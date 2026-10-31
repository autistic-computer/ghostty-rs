//! Port of `src/termio/Exec.zig`.
//!
//! STATUS: ported
//!
//! Exec implements the logic for starting and stopping a subprocess with a
//! pty as well as spinning up the necessary read thread to read from the
//! pty and forward it to the Termio instance.
//!
//! Deviations from the Zig code:
//! * Allocator parameters (and the subprocess arena) are dropped; the
//!   per-write pool is a small pointer-stable [`MemoryPool`].
//! * The read pipeline's `std.Io.Mutex`/`std.Io.Condition` pair is a
//!   `std::sync::Mutex` guarding the ring metadata plus two `Condvar`s, and
//!   the gather thread is a scoped thread (joined when the parse stage
//!   returns, like Zig's `defer gather_thread.join()`).
//! * The termio is shared with the read thread as a raw pointer, exactly
//!   like Zig's `*Termio`.
//! * A child that fails to exec exits with `_exit(1)` (Zig:
//!   `std.process.exit(1)`), since running atexit handlers in a forked child
//!   of a multi-threaded process is unsafe.
//! * The forked child's pre-exec hook does not log a pty setup failure
//!   (logging is not async-signal-safe), matching `pty.rs`.
//! * Flatpak host-command execution (`build_config::FLATPAK`) is compiled
//!   out exactly like a non-flatpak Zig build: [`FlatpakHostCommand`] is the
//!   empty struct Zig uses in that case.
//! * When the read thread's poll reports a hangup together with pending
//!   data (a child that writes and exits immediately), the data is read
//!   once before the thread exits; Zig exits right away and drops it.
//! * The apprt `pre_exec`/`post_fork` hooks are
//!   [`command::RT_PRE_EXEC`]/[`command::RT_POST_FORK`] (the GTK runtime's
//!   hooks with the `gtk` feature, none otherwise).

use std::cell::UnsafeCell;
use std::ffi::{c_void, CString};
use std::fmt;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::apprt::surface::Message as SurfaceMessage;
use crate::build_config;
use crate::command::{self, Command, EnvMap};
use crate::config::config::ShellIntegration as ConfigShellIntegration;
use crate::config::{self as configpkg, RepeatableStringMap, ShellIntegrationFeatures};
use crate::crash;
use crate::datastruct::blocking_queue::Timeout as QueueTimeout;
use crate::global;
use crate::global::xev;
use crate::lib::tinyio::TinyIo;
use crate::os as internal_os;
use crate::pty::{self as ptypkg, ProcessInfo, ProcessInfoValue, Pty};
use crate::renderer;
use crate::terminal::Terminal;
use crate::termio::backend;
use crate::termio::shell_integration;
use crate::termio::termio::{crash_surface, Termio, ThreadData as TermioThreadData};

const LOG: &str = "io_exec";

/// The termios poll rate in milliseconds.
const TERMIOS_POLL_MS: u64 = 200;

/// A file descriptor (a `HANDLE` on Windows).
pub type Fd = internal_os::pipe::Fd;

/// If we build with flatpak support then we have to keep track of
/// a potential execution on the host.
///
/// `build_config::FLATPAK` is false for the Ghostty application build, so
/// this is the empty struct Zig uses when flatpak support is not built in.
#[derive(Debug, Default)]
pub struct FlatpakHostCommand {}

/// `FlatpakHostCommand.Completion`.
#[derive(Debug, Default)]
pub struct FlatpakHostCommandCompletion {}

#[cfg(unix)]
pub use crate::os::passwd::Entry as PasswdEntry;

/// Zig: `internal_os.passwd.Entry` (the passwd module does not exist on
/// Windows; `execCommand` only uses it on macOS).
#[cfg(not(unix))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PasswdEntry {
    pub shell: Option<String>,
    pub home: Option<String>,
    pub name: Option<String>,
}

/// The errors of the exec backend (Zig: the inferred error sets of the
/// `Exec` functions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The pty could not be allocated.
    OpenptyFailed,

    /// A pty ioctl (e.g. resize) failed.
    IoctlFailed,

    /// The started command has no pid.
    ProcessNoPid,

    /// threadEnter found no started process.
    ProcessNotStarted,

    /// Returned /in the forked child/ when exec failed.
    ExecFailedInChild,

    /// Not enough system resources (fork or thread spawn failed).
    SystemResources,

    /// The apprt post fork hook failed.
    PostForkError,

    /// A path, argument or environment value contains a NUL byte.
    InvalidArgument,

    /// Killing the process group failed.
    KillFailed,

    /// Building the command failed in a way that is not recoverable.
    SystemError,

    /// Creating the read thread quit pipe failed.
    Pipe(internal_os::pipe::PipeError),

    /// An event loop error.
    Xev(xev::Error),

    /// Setting up shell integration failed.
    ShellIntegration(shell_integration::Error),

    Unexpected,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Pipe(e) => write!(f, "Pipe({e})"),
            // Zig's error set is flattened, so show the inner error name.
            Error::ShellIntegration(e) => fmt::Display::fmt(e, f),
            other => fmt::Debug::fmt(other, f),
        }
    }
}

impl std::error::Error for Error {}

impl From<command::StartError> for Error {
    fn from(e: command::StartError) -> Self {
        match e {
            command::StartError::ExecFailedInChild => Error::ExecFailedInChild,
            command::StartError::SystemResources => Error::SystemResources,
            command::StartError::PostForkError => Error::PostForkError,
            command::StartError::InvalidArgument => Error::InvalidArgument,
            command::StartError::Unexpected => Error::Unexpected,
        }
    }
}

#[cfg(unix)]
impl From<ptypkg::OpenError> for Error {
    fn from(_: ptypkg::OpenError) -> Self {
        Error::OpenptyFailed
    }
}

#[cfg(unix)]
impl From<ptypkg::IoctlError> for Error {
    fn from(_: ptypkg::IoctlError) -> Self {
        Error::IoctlFailed
    }
}

#[cfg(windows)]
impl From<ptypkg::WindowsOpenError> for Error {
    fn from(_: ptypkg::WindowsOpenError) -> Self {
        Error::OpenptyFailed
    }
}

#[cfg(windows)]
impl From<ptypkg::WindowsSetSizeError> for Error {
    fn from(_: ptypkg::WindowsSetSizeError) -> Self {
        Error::IoctlFailed
    }
}

impl From<xev::Error> for Error {
    fn from(e: xev::Error) -> Self {
        Error::Xev(e)
    }
}

impl From<shell_integration::Error> for Error {
    fn from(e: shell_integration::Error) -> Self {
        Error::ShellIntegration(e)
    }
}

/// The exec backend.
#[derive(Debug)]
pub struct Exec {
    /// The subprocess state for our exec backend.
    pub subprocess: Subprocess,
}

/// Raw termio pointer handed to the read thread (Zig: `*termio.Termio`).
struct SendPtr<T>(*mut T);

// SAFETY: the read thread only calls `Termio::process_output`, which
// synchronizes through the renderer state mutex (see `Termio`).
unsafe impl<T> Send for SendPtr<T> {}

impl<T> SendPtr<T> {
    fn get(self) -> *mut T {
        self.0
    }
}

/// The exec part of the termio thread data.
fn exec_data(td: &mut TermioThreadData) -> &mut ThreadData {
    let backend::ThreadData::Exec(exec) = &mut td.backend;
    exec
}

impl Exec {
    /// Initialize the exec state. This will NOT start it, this only sets
    /// up the internal state necessary to start it later.
    pub fn init(cfg: Config) -> Result<Exec, Error> {
        let subprocess = Subprocess::init(cfg)?;
        Ok(Exec { subprocess })
    }

    pub fn deinit(&mut self) {
        self.subprocess.deinit();
    }

    /// Call to initialize the terminal state as necessary for this backend.
    /// This is called before any termio begins. This should not be called
    /// after termio begins because it may put the internal terminal state
    /// into a bad state.
    pub fn init_terminal(&mut self, term: &mut Terminal) {
        // If we have an initial pwd requested by the subprocess, then we
        // set that on the terminal now. This allows rapidly initializing
        // new surfaces to use the proper pwd.
        if let Some(cwd) = &self.subprocess.cwd {
            if let Err(err) = term.set_pwd(cwd.as_bytes()) {
                log::warn!(target: LOG, "error setting initial pwd err={err}");
            }
        }

        // Setup our initial grid/screen size from the terminal. This
        // can't fail because the pty should not exist at this point.
        if self
            .resize(
                renderer::GridSize {
                    columns: term.cols,
                    rows: term.rows,
                },
                renderer::ScreenSize {
                    width: term.width_px,
                    height: term.height_px,
                },
            )
            .is_err()
        {
            unreachable!();
        }
    }

    /// Start the subprocess, the read thread and the event loop watchers.
    ///
    /// `io` is shared with the read thread (Zig: `*termio.Termio`); it must
    /// stay valid until `thread_exit` returns. `td` must not move until
    /// `thread_exit` returns since completions point into it.
    pub fn thread_enter(&mut self, io: *mut Termio, td: &mut TermioThreadData) -> Result<(), Error> {
        // Start our subprocess
        let pty_fds = match self.subprocess.start() {
            Ok(v) => v,
            // If we specifically got this error then we are in the forked
            // process and our child failed to execute. If we DIDN'T
            // get this specific error then we're in the parent and
            // we need to bubble it up.
            Err(Error::ExecFailedInChild) => {
                // We're in the child. Nothing more we can do but abnormal exit.
                // The Command will output some additional information.
                // SAFETY: terminating the forked child.
                unsafe { libc::_exit(1) }
            }
            Err(err) => return Err(err),
        };

        // Watcher to detect subprocess exit
        let mut process: Option<xev::Process> = match &self.subprocess.process {
            Some(Process::ForkExec(cmd)) => match cmd.pid {
                Some(pid) => match xev::Process::init(pid) {
                    Ok(p) => Some(p),
                    Err(err) => {
                        self.subprocess.stop();
                        return Err(err.into());
                    }
                },
                None => {
                    self.subprocess.stop();
                    return Err(Error::ProcessNoPid);
                }
            },

            // If we're executing via Flatpak then we can't do
            // traditional process watching (its implemented
            // as a special case in os/flatpak.zig) since the
            // command is on the host.
            Some(Process::Flatpak(_)) => None,

            None => {
                self.subprocess.stop();
                return Err(Error::ProcessNotStarted);
            }
        };

        // Track our process start time for abnormal exits
        let process_start = Instant::now();

        // Create our pipe that we'll use to kill our read thread.
        // pipe[0] is the read end, pipe[1] is the write end.
        let pipe = match internal_os::pipe() {
            Ok(p) => p,
            Err(err) => {
                if let Some(p) = &mut process {
                    p.deinit();
                }
                self.subprocess.stop();
                return Err(Error::Pipe(err));
            }
        };

        // Setup our stream so that we can write.
        let stream = xev::Stream::init_fd(pty_fds.write);

        // Start our timer to read termios state changes. This is used
        // to detect things such as when password input is being done
        // so we can render the terminal in a different way.
        let termios_timer = match xev::Timer::init() {
            Ok(t) => t,
            Err(err) => {
                stream.deinit();
                close_fd(pipe[0]);
                close_fd(pipe[1]);
                if let Some(p) = &mut process {
                    p.deinit();
                }
                self.subprocess.stop();
                return Err(err.into());
            }
        };

        // Start our read thread
        let read_fd = pty_fds.read;
        let quit_fd = pipe[0];
        let io_ptr = SendPtr(io);
        let read_thread = match std::thread::Builder::new()
            .name("io-reader".into())
            .spawn(move || {
                let io = io_ptr.get();
                #[cfg(windows)]
                ReadThread::thread_main_windows(read_fd, io, quit_fd);
                #[cfg(not(windows))]
                ReadThread::thread_main_posix(read_fd, io, quit_fd);
            }) {
            Ok(t) => t,
            Err(err) => {
                log::warn!(target: LOG, "failed to spawn read thread err={err}");
                termios_timer.deinit();
                stream.deinit();
                close_fd(pipe[0]);
                close_fd(pipe[1]);
                if let Some(p) = &mut process {
                    p.deinit();
                }
                self.subprocess.stop();
                return Err(Error::SystemResources);
            }
        };

        // Setup our threadata backend state to be our own
        td.backend = backend::ThreadData::Exec(ThreadData {
            start: process_start,
            write_stream: stream,
            process,
            read_thread: Some(read_thread),
            read_thread_pipe: pipe[1],
            read_thread_fd: pty_fds.read,
            termios_timer,
            ..ThreadData::default()
        });

        let td_ptr: *mut TermioThreadData = td;
        let l = td.r#loop;
        let exec = exec_data(td);

        // Start our process watcher. If we have an xev.Process use it.
        if let Some(p) = &exec.process {
            // SAFETY: the completion lives in the thread data, which does
            // not move until threadExit; the loop outlives the thread data.
            unsafe { p.wait(&mut *l, &mut exec.process_wait_c, td_ptr, process_exit) };
        } else if build_config::FLATPAK {
            // If we're in flatpak and we have a flatpak command then we
            // would run the special flatpak logic for watching. Flatpak
            // support is not built in (see `FlatpakHostCommand`).
        }

        // Start our termios timer. We don't support this on Windows.
        // Fundamentally, we could support this on Windows so we're just
        // waiting for someone to implement it.
        #[cfg(not(windows))]
        // SAFETY: as above.
        unsafe {
            exec.termios_timer.run(
                &mut *l,
                &mut exec.termios_timer_c,
                TERMIOS_POLL_MS,
                td_ptr,
                termios_timer_callback,
            )
        };

        Ok(())
    }

    pub fn thread_exit(&mut self, td: &mut TermioThreadData) {
        let exec = exec_data(td);

        if exec.exited {
            self.subprocess.external_exit();
        }
        self.subprocess.stop();

        // Quit our read thread after exiting the subprocess so that
        // we don't get stuck waiting for data to stop flowing if it is
        // a particularly noisy process.
        #[cfg(unix)]
        {
            // SAFETY: writing one byte from a valid buffer.
            let rc = unsafe { libc::write(exec.read_thread_pipe, b"x".as_ptr() as *const c_void, 1) };
            if rc < 0 {
                match errno() {
                    // EPIPE means that our read thread is closed already, which is
                    // completely fine since that is what we were trying to achieve.
                    libc::EPIPE => {}
                    e => log::warn!(target: LOG, "error writing to read thread quit pipe err=E{e}"),
                }
            }
        }

        #[cfg(windows)]
        {
            use crate::os::windows::{self as windows, exp::kernel32};
            let mut written: windows::DWORD = 0;
            // SAFETY: writing one byte from a valid buffer to our pipe.
            unsafe {
                kernel32::WriteFile(
                    exec.read_thread_pipe,
                    b"x".as_ptr().cast(),
                    1,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };

            // Interrupt the blocking read so the thread can see the quit message
            // SAFETY: plain call on our handle.
            if unsafe { kernel32::CancelIoEx(exec.read_thread_fd, std::ptr::null_mut()) }
                == windows::FALSE
            {
                match windows::get_last_error() {
                    windows::ERROR_NOT_FOUND => {}
                    err => log::warn!(target: LOG, "error interrupting read thread err={err}"),
                }
            }
        }

        if let Some(t) = exec.read_thread.take() {
            let _ = t.join();
        }
    }

    pub fn focus_gained(&mut self, td: &mut TermioThreadData, focused: bool) -> Result<(), Error> {
        // Windows has no termios, so there is nothing to poll.
        if cfg!(windows) {
            return Ok(());
        }

        let td_ptr: *mut TermioThreadData = td;
        let l = td.r#loop;
        let execdata = exec_data(td);

        if !focused {
            // Flag the timer to end on the next iteration. This is
            // a lot cheaper than doing full timer cancellation.
            execdata.termios_timer_running = false;
        } else {
            // Always set this to true. There is a race condition if we lose
            // focus and regain focus before the termios timer ticks where
            // if we don't set this unconditionally the timer will end on
            // the next iteration.
            execdata.termios_timer_running = true;

            // If we're focused, we want to start our termios timer. We
            // only do this if it isn't already running. We use the termios
            // callback because that'll trigger an immediate state check AND
            // start the timer.
            if execdata.termios_timer_c.state() != xev::CompletionState::Active {
                // SAFETY: the loop outlives the thread data.
                let _ = termios_timer_tick(td_ptr, unsafe { &mut *l });
            }
        }

        Ok(())
    }

    pub fn resize(
        &mut self,
        grid_size: renderer::GridSize,
        screen_size: renderer::ScreenSize,
    ) -> Result<(), Error> {
        self.subprocess.resize(grid_size, screen_size)
    }

    pub fn queue_write(
        &mut self,
        td: &mut TermioThreadData,
        data: &[u8],
        linefeed: bool,
    ) -> Result<(), Error> {
        let l = td.r#loop;
        let exec = exec_data(td);

        // If our process is exited then we don't send any more writes.
        if exec.exited {
            return Ok(());
        }
        let exec_ptr: *mut ThreadData = exec;

        // We go through and chunk the data if necessary to fit into
        // our cached buffers that we can queue to the stream.
        let mut i: usize = 0;
        while i < data.len() {
            // SAFETY: pool items are valid, pointer-stable allocations.
            let w = unsafe { &mut *exec.write_pool.create() };
            w.td = exec_ptr;
            let buf = &mut w.buf;
            let len = 'slice: {
                // The maximum end index is either the end of our data or
                // the end of our buffer, whichever is smaller.
                let max = data.len().min(i + buf.len());

                // Fast
                if !linefeed {
                    let len = max - i;
                    buf[..len].copy_from_slice(&data[i..max]);
                    i = max;
                    break 'slice len;
                }

                // Slow, have to replace \r with \r\n
                let mut buf_i: usize = 0;
                while i < data.len() && buf_i < buf.len() - 1 {
                    let ch = data[i];
                    i += 1;

                    if ch != b'\r' {
                        buf[buf_i] = ch;
                        buf_i += 1;
                        continue;
                    }

                    // CRLF
                    buf[buf_i] = b'\r';
                    buf[buf_i + 1] = b'\n';
                    buf_i += 2;
                }

                buf_i
            };

            let slice = xev::WriteBuffer::slice(&w.buf[..len]);
            let w_ptr: *mut Write = w;
            // SAFETY: the write state (request and buffer) is pool memory
            // that stays put until the write callback returns it; the queue
            // lives in the thread data and the loop outlives both.
            unsafe {
                exec.write_stream.queue_write(
                    &mut *l,
                    &mut exec.write_queue,
                    &mut (*w_ptr).req,
                    slice,
                    w_ptr,
                    tty_write,
                )
            };
        }

        Ok(())
    }

    /// Get information about the process(es) running within the backend. Returns
    /// `null` if there was an error getting the information or the information is
    /// not available on a particular platform.
    pub fn get_process_info(&mut self, info: ProcessInfo) -> Option<ProcessInfoValue<'_>> {
        self.subprocess.get_process_info(info)
    }
}

fn process_exit_common(td: &mut TermioThreadData, exit_code: u32) {
    let execdata = exec_data(td);
    execdata.exited = true;

    // Determine how long the process was running for.
    let runtime_ms: u64 = execdata.start.elapsed().as_millis() as u64;
    log::debug!(target: LOG, "child process exited status={exit_code} runtime={runtime_ms}ms");

    // We always notify the surface immediately that the child has
    // exited and some metadata about the exit.
    let _ = td.surface_mailbox.push(
        SurfaceMessage::ChildExited(crate::apprt::surface::ChildExited {
            exit_code,
            runtime_ms,
        }),
        QueueTimeout::Forever,
    );
}

fn process_exit(
    td_: *mut TermioThreadData,
    _l: &mut xev::Loop,
    _c: &mut xev::Completion,
    r: Result<u32, xev::process::WaitError>,
) -> xev::CallbackAction {
    let Ok(exit_code) = r else { unreachable!() };
    // SAFETY: the thread data outlives its armed completions.
    process_exit_common(unsafe { &mut *td_ }, exit_code);
    xev::CallbackAction::Disarm
}

fn termios_timer_callback(
    td_: *mut TermioThreadData,
    l: &mut xev::Loop,
    _c: &mut xev::Completion,
    r: Result<(), xev::timer::RunError>,
) -> xev::CallbackAction {
    // log.debug("termios timer fired", .{});

    // This should never happen because we guard starting our
    // timer on windows but we want this assertion to fire if
    // we ever do start the timer on windows.
    // TODO: support on windows
    if cfg!(windows) {
        panic!("termios timer not implemented on Windows");
    }

    match r {
        Ok(()) => {}
        // This is sent when our timer is canceled. That's fine.
        Err(xev::timer::RunError::Canceled) => return xev::CallbackAction::Disarm,
        Err(err) => {
            log::warn!(target: LOG, "error in termios timer callback err={err}");
            panic!("crash in termios timer callback");
        }
    }

    termios_timer_tick(td_, l)
}

/// The body of the termios timer callback. `focusGained` calls this
/// directly (Zig: `termiosTimer(td, undefined, undefined, {})`).
fn termios_timer_tick(td_: *mut TermioThreadData, l: &mut xev::Loop) -> xev::CallbackAction {
    // SAFETY: the thread data outlives its armed completions.
    let td = unsafe { &mut *td_ };
    let renderer_state = td.renderer_state;
    let surface_mailbox = td.surface_mailbox;
    let exec = exec_data(td);

    // This is kind of hacky but we get the termios data straight from
    // the pty master fd (Zig rebuilds a Pty struct for this).
    let mode: ptypkg::Mode = get_mode(exec.read_thread_fd).unwrap_or_else(|err| {
        log::warn!(target: LOG, "error getting termios mode err={err}");

        // If we have an error we return the default mode values
        // which are the likely values.
        ptypkg::Mode::default()
    });

    // If the mode changed, then we process it.
    if mode != exec.termios_mode {
        'mode_change: {
            log::debug!(target: LOG, "termios change mode={mode:?}");
            exec.termios_mode = mode;

            // We assume we're in some sort of password input if we're
            // in canonical mode and not echoing. This is a heuristic.
            let password_input = mode.canonical && !mode.echo;

            // If our password input state changed on the terminal then
            // we notify the surface.
            {
                // SAFETY: the render state (and its mutex and terminal)
                // outlive the IO thread.
                let state = unsafe { &*renderer_state };
                let mutex = unsafe { state.mutex.as_ref() };
                mutex.lock_uncancelable(TinyIo);
                let t = unsafe { state.terminal.as_ref() };
                let current = t.flags.password_input;
                mutex.unlock(TinyIo);
                if current == password_input {
                    break 'mode_change;
                }
            }

            // We have to notify the surface that we're in password input.
            // We must block on this because the balanced true/false state
            // of this is critical to apprt behavior.
            let _ = surface_mailbox.push(
                SurfaceMessage::PasswordInput(password_input),
                QueueTimeout::Forever,
            );
        }
    }

    // Repeat the timer
    if exec.termios_timer_running {
        // SAFETY: the completion lives in the thread data, which does not
        // move while the loop runs.
        unsafe {
            exec.termios_timer.run(
                l,
                &mut exec.termios_timer_c,
                TERMIOS_POLL_MS,
                td_,
                termios_timer_callback,
            )
        };
    }

    xev::CallbackAction::Disarm
}

/// Read the termios mode of a pty master fd (Zig: `Pty.getMode` on a Pty
/// rebuilt from `read_thread_fd`).
#[cfg(unix)]
fn get_mode(master: Fd) -> Result<ptypkg::Mode, ptypkg::GetModeError> {
    // SAFETY: zeroed termios is a valid out value.
    let mut attrs: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: valid out pointer.
    if unsafe { libc::tcgetattr(master, &mut attrs) } != 0 {
        return Err(ptypkg::GetModeError::GetModeFailed);
    }

    Ok(ptypkg::Mode {
        canonical: (attrs.c_lflag & libc::ICANON) != 0,
        echo: (attrs.c_lflag & libc::ECHO) != 0,
    })
}

#[cfg(not(unix))]
fn get_mode(_master: Fd) -> Result<ptypkg::Mode, Error> {
    Ok(ptypkg::Mode::default())
}

fn tty_write(
    w_: *mut Write,
    _l: &mut xev::Loop,
    _c: &mut xev::Completion,
    _s: xev::Stream,
    _b: xev::WriteBuffer,
    r: Result<usize, xev::WriteError>,
) -> xev::CallbackAction {
    // SAFETY: `w_` is a live pool item whose thread data outlives it.
    unsafe { (*(*w_).td).write_pool.destroy(w_) };

    match r {
        Ok(_d) => {
            //log.info("WROTE: {d}", .{d});
        }
        Err(err) => {
            log::error!(target: LOG, "write error: {err:?}");
        }
    }

    xev::CallbackAction::Disarm
}

/// A pool of pointer-stable allocations (Zig: `std.heap.MemoryPool`).
/// Items handed out by [`MemoryPool::create`] stay valid until
/// [`MemoryPool::deinit`], even after they are returned with
/// [`MemoryPool::destroy`] (they are then reused by later creates).
pub struct MemoryPool<T: Default> {
    free: Vec<*mut T>,
    all: Vec<*mut T>,
}

impl<T: Default> Default for MemoryPool<T> {
    fn default() -> Self {
        Self {
            free: Vec::new(),
            all: Vec::new(),
        }
    }
}

impl<T: Default> fmt::Debug for MemoryPool<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryPool")
            .field("allocated", &self.all.len())
            .field("free", &self.free.len())
            .finish()
    }
}

impl<T: Default> MemoryPool<T> {
    /// Check out an item from the pool.
    pub fn create(&mut self) -> *mut T {
        if let Some(p) = self.free.pop() {
            return p;
        }
        let p = Box::into_raw(Box::<T>::default());
        self.all.push(p);
        p
    }

    /// Return an item to the pool.
    pub fn destroy(&mut self, p: *mut T) {
        debug_assert!(self.all.contains(&p));
        self.free.push(p);
    }

    /// Free all items, including those that are still checked out.
    pub fn deinit(&mut self) {
        self.free.clear();
        for p in self.all.drain(..) {
            // SAFETY: every pointer in `all` came from `Box::into_raw`.
            drop(unsafe { Box::from_raw(p) });
        }
    }
}

/// The state for a single queued pty write. The write request and
/// the buffer it writes from must both remain pointer-stable until
/// the write completes, so they're pooled together and checked out
/// per write.
pub struct Write {
    /// Backpointer to the thread data so the write completion
    /// callback can put this back into the pool.
    pub td: *mut ThreadData,

    /// The libxev write request.
    pub req: xev::WriteRequest,

    /// The buffer for the data being written.
    pub buf: [u8; 64],
}

impl Default for Write {
    fn default() -> Self {
        Self {
            td: std::ptr::null_mut(),
            req: xev::WriteRequest::default(),
            buf: [0; 64],
        }
    }
}

/// The thread local data for the exec implementation.
pub struct ThreadData {
    /// Process start time and boolean of whether its already exited.
    pub start: Instant,
    pub exited: bool,

    /// The data stream is the main IO for the pty.
    pub write_stream: xev::Stream,

    /// The process watcher
    pub process: Option<xev::Process>,

    /// This is the pool of available (unused) write states. If you grab
    /// one from the pool, you must put it back when you're done!
    pub write_pool: MemoryPool<Write>,

    /// The write queue for the data stream.
    pub write_queue: xev::WriteQueue,

    /// This is used for both waiting for the process to exit and then
    /// subsequently to wait for the data_stream to close.
    pub process_wait_c: xev::Completion,

    /// The completion specific to Flatpak process waiting. If
    /// we aren't compiling with Flatpak support this is zero-sized.
    pub flatpak_wait_c: FlatpakHostCommandCompletion,

    /// Reader thread state
    pub read_thread: Option<JoinHandle<()>>,
    pub read_thread_pipe: Fd,
    pub read_thread_fd: Fd,

    /// The timer to detect termios state changes.
    pub termios_timer: xev::Timer,
    pub termios_timer_c: xev::Completion,
    pub termios_timer_running: bool,

    /// The last known termios mode. Used for change detection
    /// to prevent unnecessary locking of expensive mutexes.
    pub termios_mode: ptypkg::Mode,
}

impl fmt::Debug for ThreadData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadData")
            .field("exited", &self.exited)
            .field("write_stream", &self.write_stream)
            .field("process", &self.process)
            .field("read_thread_fd", &self.read_thread_fd)
            .field("termios_timer_running", &self.termios_timer_running)
            .field("termios_mode", &self.termios_mode)
            .finish_non_exhaustive()
    }
}

/// The placeholder state before `threadEnter` fills it in (Zig fills the
/// required fields at once; this provides the defaulted ones).
impl Default for ThreadData {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            exited: false,
            write_stream: xev::Stream::init_fd(invalid_fd()),
            process: None,
            write_pool: MemoryPool::default(),
            write_queue: xev::WriteQueue::default(),
            process_wait_c: xev::Completion::default(),
            flatpak_wait_c: FlatpakHostCommandCompletion::default(),
            read_thread: None,
            read_thread_pipe: invalid_fd(),
            read_thread_fd: invalid_fd(),
            termios_timer: xev::Timer {},
            termios_timer_c: xev::Completion::default(),
            termios_timer_running: true,
            termios_mode: ptypkg::Mode::default(),
        }
    }
}

impl ThreadData {
    pub fn deinit(&mut self) {
        close_fd(self.read_thread_pipe);
        self.read_thread_pipe = invalid_fd();

        // Clear our write pool. We know we aren't ever going to do
        // any more IO since we stop our data stream below so we can just
        // drop this.
        self.write_pool.deinit();

        // Stop our process watcher
        if let Some(p) = &mut self.process {
            p.deinit();
        }

        // Stop our write stream
        self.write_stream.deinit();

        // Stop our termios timer
        self.termios_timer.deinit();
    }
}

#[cfg(unix)]
fn invalid_fd() -> Fd {
    -1
}

#[cfg(windows)]
fn invalid_fd() -> Fd {
    std::ptr::null_mut()
}

#[cfg(unix)]
fn close_fd(fd: Fd) {
    if fd >= 0 {
        // SAFETY: closing an fd we own.
        unsafe { libc::close(fd) };
    }
}

#[cfg(windows)]
fn close_fd(fd: Fd) {
    if !fd.is_null() {
        // SAFETY: closing a handle we own.
        unsafe { crate::os::windows::exp::kernel32::CloseHandle(fd) };
    }
}

#[cfg(unix)]
fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct Config {
    pub command: Option<configpkg::Command>,

    /// The environment for the subprocess. init takes ownership of this
    /// whether or not it succeeds, so the caller must not free it after
    /// calling init.
    pub env: EnvMap,
    pub env_override: RepeatableStringMap,
    pub shell_integration: ConfigShellIntegration,
    pub shell_integration_features: ShellIntegrationFeatures,
    pub cursor_blink: Option<bool>,
    pub working_directory: Option<String>,
    pub resources_dir: Option<String>,
    pub term: String,

    pub rt_pre_exec_info: command::RtPreExecInfo,
    pub rt_post_fork_info: command::RtPostForkInfo,
}

/// Zig declares defaults for the optional fields only; `env`,
/// `resources_dir` and `term` default to empty here.
impl Default for Config {
    fn default() -> Self {
        Self {
            command: None,
            env: EnvMap::new(),
            env_override: RepeatableStringMap::default(),
            shell_integration: ConfigShellIntegration::Detect,
            shell_integration_features: ShellIntegrationFeatures::default(),
            cursor_blink: None,
            working_directory: None,
            resources_dir: None,
            term: String::new(),
            rt_pre_exec_info: command::rt_pre_exec_info_unset(),
            rt_post_fork_info: command::rt_post_fork_info_unset(),
        }
    }
}

/// Union that represents the running process type.
#[derive(Debug)]
pub enum Process {
    /// Standard POSIX fork/exec
    ForkExec(Command),

    /// Flatpak DBus command
    Flatpak(FlatpakHostCommand),
}

/// The pty fds returned by [`Subprocess::start`].
#[derive(Debug, Clone, Copy)]
pub struct StartFds {
    pub read: Fd,
    pub write: Fd,
}

/// Format args for logging (Zig: `ArgsFormatter`).
fn format_args_list(args: &[String]) -> String {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('`');
        out.push_str(a);
        out.push('`');
    }
    out
}

#[derive(Debug)]
pub struct Subprocess {
    pub cwd: Option<String>,
    pub env: Option<EnvMap>,
    pub args: Vec<String>,
    pub grid_size: renderer::GridSize,
    pub screen_size: renderer::ScreenSize,
    pub pty: Option<Pty>,
    pub process: Option<Process>,

    pub rt_pre_exec_info: command::RtPreExecInfo,
    pub rt_post_fork_info: command::RtPostForkInfo,
}

impl Subprocess {
    /// Initialize the subprocess. This will NOT start it, this only sets
    /// up the internal state necessary to start it later.
    pub fn init(cfg: Config) -> Result<Subprocess, Error> {
        // Get our env. If a default env isn't provided by the caller
        // then we get it ourselves.
        let mut env = cfg.env;

        // If we have a resources dir then set our env var
        if let Some(dir) = &cfg.resources_dir {
            log::info!(target: LOG, "found Ghostty resources dir: {dir}");
            env.insert("GHOSTTY_RESOURCES_DIR".into(), dir.clone());
        }

        // Set our TERM var. This is a bit complicated because we want to use
        // the ghostty TERM value but we want to only do that if we have
        // ghostty in the TERMINFO database.
        //
        // For now, we just look up a bundled dir but in the future we should
        // also load the terminfo database and look for it.
        if let Some(base) = &cfg.resources_dir {
            env.insert("TERM".into(), cfg.term.clone());
            env.insert("COLORTERM".into(), "truecolor".into());

            // Assume that the resources directory is adjacent to the terminfo
            // database
            let parent = dirname(base).unwrap_or_else(|| unreachable!());
            env.insert("TERMINFO".into(), format!("{parent}/terminfo"));
        } else {
            if cfg!(target_vendor = "apple") {
                log::warn!(target: LOG, "ghostty terminfo not found, using xterm-256color");
                log::warn!(target: LOG, "the terminfo SHOULD exist on macos, please ensure");
                log::warn!(target: LOG, "you're using a valid app bundle.");
            }

            env.insert("TERM".into(), "xterm-256color".into());
            env.insert("COLORTERM".into(), "truecolor".into());
        }

        // Add our binary to the path if we can find it.
        'ghostty_path: {
            // Skip this for flatpak since host cannot reach them
            if build_config::FLATPAK && internal_os::is_flatpak() {
                break 'ghostty_path;
            }

            let exe_bin_path = match std::env::current_exe() {
                Ok(p) => p,
                Err(err) => {
                    log::warn!(target: LOG, "failed to get ghostty exe path err={err}");
                    break 'ghostty_path;
                }
            };
            let Some(exe_dir) = exe_bin_path.parent().and_then(|p| p.to_str()) else {
                break 'ghostty_path;
            };
            log::debug!(target: LOG, "appending ghostty bin to path dir={exe_dir}");

            // We always set this so that if the shell overwrites the path
            // scripts still have a way to find the Ghostty binary when
            // running in Ghostty.
            env.insert("GHOSTTY_BIN_DIR".into(), exe_dir.into());

            // Append if we have a path. We want to append so that ghostty is
            // the last priority in the path. If we don't have a path set
            // then we just set it to the directory of the binary.
            if let Some(path) = env.get("PATH") {
                // Verify that our path doesn't already contain this entry
                if path
                    .split(PATH_DELIMITER)
                    .filter(|e| !e.is_empty())
                    .any(|entry| entry == exe_dir)
                {
                    break 'ghostty_path;
                }

                let new_path = append_env(path, exe_dir);
                env.insert("PATH".into(), new_path);
            } else {
                env.insert("PATH".into(), exe_dir.into());
            }
        }

        // On macOS, export additional data directories from our
        // application bundle.
        #[cfg(target_os = "macos")]
        'darwin: {
            let Some(resources_dir) = &cfg.resources_dir else {
                break 'darwin;
            };

            let xdg_data_dir_key = "XDG_DATA_DIRS";
            let data_dir = format!("{resources_dir}/..");
            let current = env
                .get(xdg_data_dir_key)
                .cloned()
                .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            env.insert(xdg_data_dir_key.into(), append_env(&current, &data_dir));

            let manpath_key = "MANPATH";
            let man_dir = format!("{resources_dir}/../man");
            // Always append with colon in front, as it mean that if
            // `MANPATH` is empty, then it should be treated as an extra
            // path instead of overriding all paths set by OS.
            let current = env.get(manpath_key).cloned().unwrap_or_default();
            env.insert(manpath_key.into(), append_env_always(&current, &man_dir));
        }

        // Set environment variables used by some programs (such as neovim) to detect
        // which terminal emulator and version they're running under.
        env.insert("TERM_PROGRAM".into(), "ghostty".into());
        env.insert("TERM_PROGRAM_VERSION".into(), build_config::VERSION_STRING.into());

        // VTE_VERSION is set by gnome-terminal and other VTE-based terminals.
        // We don't want our child processes to think we're running under VTE.
        // This is not apprt-specific, so we do it here.
        env.remove("VTE_VERSION");

        // Setup our shell integration, if we can.
        let shell_command: configpkg::Command = 'shell: {
            let default_shell_command: configpkg::Command =
                cfg.command.clone().unwrap_or_else(|| {
                    configpkg::Command::Shell(if cfg!(windows) { "cmd.exe" } else { "sh" }.into())
                });

            // Always set up shell features (GHOSTTY_SHELL_FEATURES). These are
            // used by both automatic and manual shell integrations.
            shell_integration::setup_features(
                &mut env,
                cfg.shell_integration_features,
                cfg.cursor_blink.unwrap_or(true),
            )?;

            let force: Option<shell_integration::Shell> = match cfg.shell_integration {
                ConfigShellIntegration::None => {
                    // This is a source of confusion for users despite being
                    // opt-in since it results in some Ghostty features not
                    // working. We always want to log it.
                    log::info!(target: LOG, "shell integration disabled by configuration");
                    break 'shell default_shell_command;
                }

                ConfigShellIntegration::Detect => None,
                ConfigShellIntegration::Bash => Some(shell_integration::Shell::Bash),
                ConfigShellIntegration::Elvish => Some(shell_integration::Shell::Elvish),
                ConfigShellIntegration::Fish => Some(shell_integration::Shell::Fish),
                ConfigShellIntegration::Nushell => Some(shell_integration::Shell::Nushell),
                ConfigShellIntegration::Zsh => Some(shell_integration::Shell::Zsh),
            };

            let Some(dir) = &cfg.resources_dir else {
                log::warn!(target: LOG, "no resources dir set, shell integration disabled");
                break 'shell default_shell_command;
            };

            let Some(integration) =
                shell_integration::setup(dir, &default_shell_command, &mut env, force)?
            else {
                log::warn!(
                    target: LOG,
                    "shell could not be detected, no automatic shell integration will be injected"
                );
                break 'shell default_shell_command;
            };

            log::info!(
                target: LOG,
                "shell integration automatically injected shell={:?}",
                integration.shell
            );

            integration.command
        };

        // Add the environment variables that override any others.
        for (k, v) in cfg.env_override.iterator() {
            env.insert(k.as_str().into(), v.as_str().into());
        }

        // Build our args list
        let args: Vec<String> = match exec_command(&shell_command, passwd_get) {
            Ok(args) => args,

            // If we fail to allocate space for the command we want to
            // execute, we'd still like to try to run something so
            // Ghostty can launch (and maybe the user can debug this further).
            // Realistically, if you're getting OOM, I think other stuff is
            // about to crash, but we can try.
            Err(ExecCommandError::OutOfMemory) => {
                log::warn!(
                    target: LOG,
                    "failed to allocate space for command args, falling back to basic shell"
                );
                vec![if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.to_string()]
            }

            // This logs on its own, this is a bad error.
            Err(ExecCommandError::SystemError) => return Err(Error::SystemError),
        };

        // We have to copy the cwd because there is no guarantee that
        // pointers in full_config remain valid.
        let cwd: Option<String> = cfg.working_directory.clone();

        // Propagate the current working directory (CWD) to the shell, enabling
        // the shell to display the current directory name rather than the
        // resolved path for symbolic links. This is important and based
        // on the same behavior in Konsole and Kitty (see the linked issues):
        // https://bugs.kde.org/show_bug.cgi?id=242114
        // https://github.com/kovidgoyal/kitty/issues/1595
        // https://github.com/ghostty-org/ghostty/discussions/7769
        if let Some(pwd) = &cwd {
            env.insert("PWD".into(), pwd.clone());
        }

        Ok(Subprocess {
            env: Some(env),
            cwd,
            args,

            rt_pre_exec_info: cfg.rt_pre_exec_info,
            rt_post_fork_info: cfg.rt_post_fork_info,

            // Should be initialized with initTerminal call.
            grid_size: renderer::GridSize::default(),
            screen_size: renderer::ScreenSize {
                width: 1,
                height: 1,
            },
            pty: None,
            process: None,
        })
    }

    /// Clean up the subprocess. This will stop the subprocess if it is started.
    pub fn deinit(&mut self) {
        self.stop();
        if let Some(pty) = &mut self.pty {
            pty.deinit();
        }
        self.pty = None;
        self.env = None;
    }

    /// Start the subprocess. If the subprocess is already started this
    /// will crash.
    pub fn start(&mut self) -> Result<StartFds, Error> {
        assert!(self.pty.is_none() && self.process.is_none());

        // Create our pty
        let pty = Pty::open(ptypkg::Winsize {
            ws_row: self.grid_size.rows as u16,
            ws_col: self.grid_size.columns as u16,
            ws_xpixel: self.screen_size.width as u16,
            ws_ypixel: self.screen_size.height as u16,
        })?;
        self.pty = Some(pty);

        match self.start_command() {
            Ok(fds) => {
                // Cleanup we only run in our parent when we successfully start
                // the process.
                if self.process.is_some() {
                    // Once our subcommand is started we can close the slave
                    // side. This prevents the slave fd from being leaked to
                    // future children.
                    #[cfg(unix)]
                    close_fd(self.pty.as_ref().unwrap().slave);

                    // Successful start we can clear out some memory.
                    self.env = None;
                }
                Ok(fds)
            }

            // If we fail in our child we need to flag it so our
            // cleanup doesn't run.
            Err(Error::ExecFailedInChild) => Err(Error::ExecFailedInChild),

            Err(err) => {
                if let Some(mut pty) = self.pty.take() {
                    #[cfg(unix)]
                    close_fd(pty.slave);
                    pty.deinit();
                }
                Err(err)
            }
        }
    }

    /// The part of `start` after the pty is opened.
    fn start_command(&mut self) -> Result<StartFds, Error> {
        log::debug!(target: LOG, "starting command command={}", format_args_list(&self.args));

        // If we can't access the cwd, then don't set any cwd and inherit.
        // This is important because our cwd can be set by the shell (OSC 7)
        // and we don't want to break new windows.
        let cwd: Option<String> = match &self.cwd {
            Some(proposed) => 'cwd: {
                // Flatpak sandboxing prevents access to certain reserved
                // paths regardless of configured permissions; a flatpak
                // build verifies the cwd with a host command here. Flatpak
                // support is not built in (see `FlatpakHostCommand`).
                if build_config::FLATPAK && internal_os::is_flatpak() {
                    log::info!(target: LOG, "flatpak detected, will use host command to verify cwd access");
                }

                match access(proposed) {
                    Ok(()) => break 'cwd Some(proposed.clone()),
                    Err(err) => {
                        log::warn!(target: LOG, "cannot access cwd, ignoring: {err}");
                        break 'cwd None;
                    }
                }
            }
            None => None,
        };

        // In flatpak, we use the HostCommand to execute our shell.
        if internal_os::is_flatpak() && !build_config::FLATPAK {
            log::warn!(target: LOG, "flatpak detected, but flatpak support not built-in");
        }

        let (pty_master, pty_slave) = self.pty_fds();
        let to_c = |s: &str| CString::new(s).map_err(|_| Error::InvalidArgument);

        // Build our subcommand
        let mut cmd = Command {
            path: to_c(&self.args[0])?,
            args: self
                .args
                .iter()
                .map(|a| to_c(a))
                .collect::<Result<Vec<_>, _>>()?,
            env: self.env.clone(),
            cwd: cwd.as_deref().map(to_c).transpose()?,
            stdin: Some(pty_slave),
            stdout: Some(pty_slave),
            stderr: Some(pty_slave),
            #[cfg(windows)]
            pseudo_console: Some(self.pty.as_ref().unwrap().pseudo_console),
            os_pre_exec: if cfg!(windows) {
                None
            } else {
                Some(os_pre_exec_callback)
            },
            rt_pre_exec: command::RT_PRE_EXEC,
            rt_pre_exec_info: self.rt_pre_exec_info,
            rt_post_fork: command::RT_POST_FORK,
            rt_post_fork_info: self.rt_post_fork_info,
            data: self as *mut Subprocess as *mut c_void,
            pid: None,
        };

        cmd.start()?;
        log::info!(
            target: LOG,
            "started subcommand path={} pid={:?}",
            self.args[0],
            cmd.pid
        );

        self.process = Some(Process::ForkExec(cmd));

        #[cfg(windows)]
        {
            let pty = self.pty.as_ref().unwrap();
            let _ = pty_master;
            return Ok(StartFds {
                read: pty.out_pipe,
                write: pty.in_pipe,
            });
        }

        #[cfg(not(windows))]
        Ok(StartFds {
            read: pty_master,
            write: pty_master,
        })
    }

    #[cfg(unix)]
    fn pty_fds(&self) -> (Fd, Fd) {
        let pty = self.pty.as_ref().unwrap();
        (pty.master, pty.slave)
    }

    #[cfg(windows)]
    fn pty_fds(&self) -> (Fd, Fd) {
        let pty = self.pty.as_ref().unwrap();
        (pty.out_pipe, std::ptr::null_mut())
    }

    /// This should be called after fork but before exec in the child process.
    /// To repeat: this function RUNS IN THE FORKED CHILD PROCESS before
    /// exec is called; it does NOT run in the main Ghostty process.
    #[cfg(unix)]
    fn child_pre_exec(&self) -> Result<(), ptypkg::ChildPreExecError> {
        // Setup our pty
        self.pty.as_ref().unwrap().child_pre_exec()
    }

    /// Called to notify that we exited externally so we can unset our
    /// running state.
    pub fn external_exit(&mut self) {
        self.process = None;
    }

    /// Stop the subprocess. This is safe to call anytime. This will wait
    /// for the subprocess to register that it has been signalled, but not
    /// for it to terminate, so it will not block.
    /// This does not close the pty.
    pub fn stop(&mut self) {
        match &mut self.process {
            None => return,

            Some(Process::ForkExec(cmd)) => {
                // Note: this will also wait for the command to exit, so
                // DO NOT call cmd.wait
                if let Err(err) = kill_command(cmd) {
                    log::error!(target: LOG, "error sending SIGHUP to command, may hang: {err}");
                }
            }

            // Flatpak support is not built in (see `FlatpakHostCommand`).
            Some(Process::Flatpak(_)) => {}
        }

        self.process = None;
    }

    /// Resize the pty subprocess. This is safe to call anytime.
    pub fn resize(
        &mut self,
        grid_size: renderer::GridSize,
        screen_size: renderer::ScreenSize,
    ) -> Result<(), Error> {
        self.grid_size = grid_size;
        self.screen_size = screen_size;

        if let Some(pty) = &mut self.pty {
            // It is theoretically possible for the grid or screen size to
            // exceed u16, although the terminal in that case isn't very
            // usable. This should be protected upstream but we still clamp
            // in case there is a bad caller which has happened before.
            pty.set_size(ptypkg::Winsize {
                ws_row: u16::try_from(grid_size.rows).unwrap_or(u16::MAX),
                ws_col: u16::try_from(grid_size.columns).unwrap_or(u16::MAX),
                ws_xpixel: u16::try_from(screen_size.width).unwrap_or(u16::MAX),
                ws_ypixel: u16::try_from(screen_size.height).unwrap_or(u16::MAX),
            })?;
        }

        Ok(())
    }

    /// Get information about the process(es) running within the subprocess.
    /// Returns `null` if there was an error getting the information or the
    /// information is not available on a particular platform.
    pub fn get_process_info(&mut self, info: ProcessInfo) -> Option<ProcessInfoValue<'_>> {
        let pty = self.pty.as_mut()?;
        pty.get_process_info(info)
    }
}

/// The pre-exec hook run in the forked child (Zig: the `os_pre_exec`
/// callback struct in `start`).
fn os_pre_exec_callback(cmd: &mut Command) -> Option<u8> {
    let Some(sp) = cmd.get_data::<Subprocess>() else {
        unreachable!()
    };
    #[cfg(unix)]
    {
        // SAFETY: `data` is the subprocess being started; it is alive (and
        // unchanged) in the forked child.
        // The Zig code logs a failure here; we don't, because we are in the
        // forked child and the logger may allocate or lock.
        let _ = unsafe { (*sp).child_pre_exec() };
    }
    #[cfg(not(unix))]
    let _ = sp;
    None
}

/// Kill the underlying subprocess. This sends a SIGHUP to the child
/// process. This also waits for the command to exit and will return the
/// exit code.
fn kill_command(command: &mut Command) -> Result<(), Error> {
    if let Some(pid) = command.pid {
        #[cfg(windows)]
        {
            use crate::os::windows::{self as windows, exp::kernel32};
            // SAFETY: the pid is a valid process handle.
            if unsafe { kernel32::TerminateProcess(pid, 0) } == windows::FALSE {
                let _ = windows::unexpected_error(windows::get_last_error());
                return Err(Error::Unexpected);
            }

            command.wait(false).map_err(|_| Error::Unexpected)?;
        }

        #[cfg(unix)]
        kill_pid(pid)?;
    }

    Ok(())
}

#[cfg(unix)]
fn kill_pid(pid: libc::pid_t) -> Result<(), Error> {
    let Some(pgid) = getpgid(pid) else {
        return Ok(());
    };

    // It is possible to send a killpg between the time that
    // our child process calls setsid but before or simultaneous
    // to calling execve. In this case, the direct child dies
    // but grandchildren survive. To work around this, we loop
    // and repeatedly kill the process group until all
    // descendents are well and truly dead. We will not rest
    // until the entire family tree is obliterated.
    loop {
        // SAFETY: plain syscall.
        if unsafe { libc::killpg(pgid, libc::SIGHUP) } == 0 {
            log::debug!(target: LOG, "process group killed pgid={pgid}");
        } else {
            let err = errno();
            if cfg!(target_vendor = "apple") && err == libc::EPERM {
                log::debug!(target: LOG, "killpg failed with EPERM, expected on Darwin and ignoring");
            } else {
                log::warn!(target: LOG, "error killing process group pgid={pgid} err={err}");
                return Err(Error::KillFailed);
            }
        }

        // See Command.zig wait for why we specify WNOHANG.
        // The gist is that it lets us detect when children
        // are still alive without blocking so that we can
        // kill them again.
        let res_pid = loop {
            // SAFETY: plain syscall with a null status pointer.
            let rc = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
            if rc == -1 && errno() == libc::EINTR {
                continue;
            }
            break rc;
        };
        log::debug!(target: LOG, "waitpid result={res_pid}");
        if res_pid != 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    Ok(())
}

#[cfg(unix)]
fn getpgid(pid: libc::pid_t) -> Option<libc::pid_t> {
    // Get our process group ID. Before the child pid calls setsid
    // the pgid will be ours because we forked it. Its possible that
    // we may be calling this before setsid if we are killing a surface
    // VERY quickly after starting it.
    // SAFETY: plain syscall.
    let my_pgid = unsafe { libc::getpgid(0) };

    // We loop while pgid == my_pgid. The expectation if we have a valid
    // pid is that setsid will eventually be called because it is the
    // FIRST thing the child process does and as far as I can tell,
    // setsid cannot fail. I'm sure that's not true, but I'd rather
    // have a bug reported than defensively program against it now.
    loop {
        // SAFETY: plain syscall.
        let pgid = unsafe { libc::getpgid(pid) };
        if pgid == my_pgid {
            log::warn!(target: LOG, "pgid is our own, retrying");
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }

        // Don't know why it would be zero but its not a valid pid
        if pgid == 0 {
            return None;
        }

        // If the pid doesn't exist then... we're done!
        // (Zig compares the result with the ESRCH constant.)
        if pgid == libc::ESRCH {
            return None;
        }

        // If we have an error we're done.
        if pgid < 0 {
            log::warn!(target: LOG, "error getting pgid for kill");
            return None;
        }

        return Some(pgid);
    }
}

/// Check that a path exists (Zig: `std.Io.Dir.cwd().access(path, .{})`).
fn access(path: &str) -> std::io::Result<()> {
    std::fs::metadata(path).map(|_| ())
}

/// `std.fs.path.dirname`: the path without its last component, or `None`
/// for a root or a path without a separator.
fn dirname(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let idx = trimmed.rfind('/')?;
    let dir = trimmed[..idx].trim_end_matches('/');
    if dir.is_empty() {
        return Some("/");
    }
    Some(dir)
}

/// The read thread works with a companion gather thread to form a two-stage
/// pipeline that moves pty output into the terminal:
///
///   io-gather:  read()/poll() the pty into one of a few rotating
///               buffers, batching bulk output.
///   io-reader:  hand each filled buffer to processOutput (terminal
///               lock, VT parse, state update, render scheduling).
///
/// This used to be a single serial loop (and still is on Windows):
///
///   while (true) { blocking_read(); exit_if_eof(); process(); }
///
/// I found on macOS that the kernel tty output queue caps every read
/// on the master at 1 KB no matter how large the read buffer is. This means
/// that producers (e.g. `cat`) stall with the above architecture because
/// there are windows in the `process()` part where we aren't draining
/// the kernel pty fd.
///
/// Instead, having a separate thread gather and drain the kernel pty
/// into a rotating set of preallocated buffers minimizes this stall
/// period to effectively zero: while the io-reader thread parses
/// one batch, the gather thread is draining the kernel queue. There is
/// still stalling (our VT parse is a bottleneck now), but we don't stall
/// between them.
///
/// Interactive latency is preserved: a batch is delivered on the
/// first EAGAIN unless the stream is saturated (>= 1 KiB gathered
/// means the writer filled the kernel queue), in which case we bridge
/// the producer's microsecond refill gaps with a short poll, bounded
/// by a small total budget per batch that is well under a display
/// frame. This means that small outputs that are more typical continue
/// to be interactive.
///
/// We use basic poll/read syscalls here because we are only
/// monitoring two fds and this is still much faster and lower
/// overhead than any async mechanism.
pub struct ReadThread;

/// The state shared between the gather and parse stages. This is
/// a fixed ring of buffers plus the metadata to rotate ownership
/// between the two threads. A buffer is owned by exactly one
/// stage at a time, so buffer contents need no locking. Only the
/// ring metadata is guarded by the mutex.
struct Pipeline {
    /// Guards the ring metadata.
    mutex: Mutex<PipelineState>,

    /// Signaled when a batch is published or the gather stage is
    /// done. Waited on by the parse stage.
    batch_ready: Condvar,

    /// Signaled when a batch has been consumed. Waited on by the
    /// gather stage when all buffers are in flight (backpressure).
    slot_free: Condvar,

    /// A self-pipe the parse stage uses to interrupt the gather
    /// stage's bridge poll the moment it runs out of batches.
    /// Bridging a refill gap is only free while the parse stage
    /// is busy; once it goes idle, every additional microsecond
    /// spent bridging is added straight to output latency. The
    /// write end is used by the parse stage, the read end is
    /// polled by the gather stage. -1 when unavailable, in which
    /// case bridge polls are bounded by their timeout only.
    idle_read_fd: libc::c_int,
    idle_write_fd: libc::c_int,

    /// The buffer storage itself.
    bufs: Box<[UnsafeCell<[u8; ReadThread::BUFFER_CAPACITY]>]>,
}

/// The ring metadata of [`Pipeline`].
struct PipelineState {
    /// The number of valid bytes in each buffer. Set at publish
    /// time by the gather stage, read by the parse stage.
    lens: [usize; ReadThread::BUFFER_COUNT],

    /// Ring state: head is the next slot the gather stage fills,
    /// tail is the next slot the parse stage consumes, count is
    /// the number of published, unconsumed batches.
    head: usize,
    tail: usize,
    count: usize,

    /// Set by the gather stage (under the mutex) while it sleeps
    /// in a bridge poll. The parse stage only writes to the idle
    /// pipe when this is set, so an interactive terminal never
    /// pays the pipe syscalls.
    bridging: bool,

    /// Set by the gather stage when the stream is over (quit
    /// signal, EOF, or pty error). The parse stage drains any
    /// remaining batches and then exits.
    done: bool,
}

// SAFETY: the buffers are only accessed by the stage that owns the slot
// (ownership is transferred under the mutex); everything else is
// synchronized.
unsafe impl Sync for Pipeline {}

impl Pipeline {
    fn new(idle_read_fd: libc::c_int, idle_write_fd: libc::c_int) -> Pipeline {
        Pipeline {
            mutex: Mutex::new(PipelineState {
                lens: [0; ReadThread::BUFFER_COUNT],
                head: 0,
                tail: 0,
                count: 0,
                bridging: false,
                done: false,
            }),
            batch_ready: Condvar::new(),
            slot_free: Condvar::new(),
            idle_read_fd,
            idle_write_fd,
            bufs: (0..ReadThread::BUFFER_COUNT)
                .map(|_| UnsafeCell::new([0u8; ReadThread::BUFFER_CAPACITY]))
                .collect(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, PipelineState> {
        self.mutex.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The subset of `std.posix.ReadError` the gather stage distinguishes.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PosixReadError {
    WouldBlock,
    NotOpenForReading,
    InputOutput,
    Other(i32),
}

/// `std.posix.read`: retries on EINTR.
#[cfg(unix)]
fn posix_read(fd: libc::c_int, buf: &mut [u8]) -> Result<usize, PosixReadError> {
    loop {
        // SAFETY: valid buffer pointer and length.
        let rc = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        if rc >= 0 {
            return Ok(rc as usize);
        }
        return Err(match errno() {
            libc::EINTR => continue,
            libc::EAGAIN => PosixReadError::WouldBlock,
            libc::EBADF => PosixReadError::NotOpenForReading,
            libc::EIO => PosixReadError::InputOutput,
            e => PosixReadError::Other(e),
        });
    }
}

/// `std.posix.poll`: retries on EINTR.
#[cfg(unix)]
fn posix_poll(fds: &mut [libc::pollfd], timeout: libc::c_int) -> Result<usize, i32> {
    loop {
        // SAFETY: valid pollfd array.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if rc >= 0 {
            return Ok(rc as usize);
        }
        match errno() {
            libc::EINTR => continue,
            e => return Err(e),
        }
    }
}

/// Clears the crash thread state when dropped.
struct CrashStateGuard;

impl Drop for CrashStateGuard {
    fn drop(&mut self) {
        crash::sentry::set_thread_state(None);
    }
}

impl ReadThread {
    /// The number of buffers rotated between the gather and parse
    /// stages. The gather stage can run at most this many batches
    /// ahead of the parse stage before it blocks, which (via the
    /// kernel pty queue) is also what preserves flow control to the
    /// child. Empirically chosen through measurements on an M4 Max.
    /// Less than 4 there are minor slowdowns, above 4 there are no
    /// improvements.
    const BUFFER_COUNT: usize = 4;

    /// The capacity of each gather buffer. One batch is also the unit
    /// of work the parse stage does per terminal lock acquisition, so
    /// this bounds both gather latency and lock hold time.
    const BUFFER_CAPACITY: usize = 64 * 1024;

    /// How many gathered bytes mark a stream as saturated. The macOS
    /// kernel tty output queue hands the master at most about 1 KiB
    /// per read, so gathering a full 1 KiB means the writer filled
    /// the queue (a bulk stream worth briefly waiting on), while
    /// anything smaller is an interactive trickle that must be
    /// delivered with no added latency.
    const BRIDGE_THRESHOLD: usize = 1024;

    /// How many times an EAGAIN on a saturated stream is retried
    /// with an immediate read before we're willing to sleep in poll.
    /// Basically, a spin retry.
    ///
    /// The writer refills the drained kernel queue within a few
    /// microseconds, while a sleep and wakeup through poll costs
    /// several more. If the gather stage sleeps on every refill gap,
    /// the whole pipeline degenerates to lockstep with the writer at
    /// about 1 KiB per wakeup. A short burst of nonblocking reads
    /// bridges nearly all refill gaps without sleeping. Measured, 8
    /// to 16 retries catches over 90% of the gaps and nearly doubles
    /// the saturated drain rate, and larger values helped little.
    /// The cost is bounded to at most this many extra ~0.5us read
    /// syscalls per gap, and we only spin on streams that already
    /// gathered a full kernel queue, so an idle or interactive
    /// terminal never spins.
    const BRIDGE_SPIN_MAX: usize = 16;

    /// How long one bridge poll waits for the writer's next refill
    /// once the spin retries above have failed. If the stream is
    /// quiet for this long the burst is over and we deliver what we
    /// have.
    const BRIDGE_POLL_TIMEOUT_MS: libc::c_int = 1;

    /// The longest one batch may spend bridging refill gaps before it
    /// is delivered regardless. This bounds output latency for
    /// streams that produce just enough to keep bridging. Three
    /// milliseconds is well under one display frame, so batching is
    /// invisible on screen.
    const GATHER_BUDGET_NS: u128 = 3 * 1_000_000;

    #[cfg(unix)]
    fn thread_main_posix(fd: Fd, io: *mut Termio, quit: Fd) {
        // Always close our end of the pipe when we exit.
        struct CloseOnDrop(Fd);
        impl Drop for CloseOnDrop {
            fn drop(&mut self) {
                close_fd(self.0);
            }
        }
        let _quit_guard = CloseOnDrop(quit);

        // Right now, on Darwin, `std.Thread.setName` can only name the current
        // thread, and we have no way to get the current thread from within it,
        // so instead we use this code to name the thread instead.
        #[cfg(target_os = "macos")]
        {
            // SAFETY: NUL-terminated name.
            unsafe { libc::pthread_setname_np(c"io-reader".as_ptr()) };
            Self::set_qos_class();
        }

        // Setup our crash metadata
        crash::sentry::set_thread_state(Some(crash::sentry::ThreadState {
            r#type: crash::sentry::ThreadType::Io,
            // SAFETY: the termio outlives the read thread.
            surface: crash_surface(unsafe { &(*io).surface_mailbox }),
        }));
        let _crash_guard = CrashStateGuard;

        // Set the fd to non-blocking so the gather stage can drain it
        // in a tight loop and fall back to poll for readiness. The
        // pipeline can't run with a blocking fd (a blocking read
        // would hang the gather stage on a quiet pty), but this also
        // can't realistically fail on a valid pty master.
        if !Self::set_nonblock(fd) {
            log::error!(target: LOG, "read thread exiting, pty fd must be non-blocking");
            return;
        }

        // The idle self-pipe (see the Pipeline field docs). If we
        // can't create it we still run correctly, bridge polls are
        // just bounded by their timeout instead of being interrupted
        // when the parse stage goes idle.
        let (idle_read_fd, idle_write_fd) =
            match crate::lib::compat::fd::pipe2(libc::O_CLOEXEC | libc::O_NONBLOCK) {
                Ok(fds) => (fds[0], fds[1]),
                Err(err) => {
                    log::warn!(target: LOG, "read thread failed to create idle pipe err={err}");
                    (-1, -1)
                }
            };

        // Shared pipeline
        let pipeline = Pipeline::new(idle_read_fd, idle_write_fd);

        std::thread::scope(|s| {
            let gather_thread = std::thread::Builder::new()
                .name("io-gather".into())
                .spawn_scoped(s, || Self::gather_main_posix(fd, quit, &pipeline));
            if let Err(err) = gather_thread {
                // If we can't spawn a thread the process is already
                // doomed (every surface spawns several), so don't try
                // to limp along.
                log::error!(target: LOG, "read thread exiting, failed to spawn gather thread err={err}");
                return;
            }

            // This thread is the parse stage. We consume batches in ring
            // order until the gather stage reports the stream is over and
            // the ring is drained.
            loop {
                let batch: &[u8] = {
                    let mut st = pipeline.lock();
                    while st.count == 0 {
                        if st.done {
                            return;
                        }
                        st = pipeline.batch_ready.wait(st).unwrap_or_else(|e| e.into_inner());
                    }
                    let slot = st.tail;
                    // SAFETY: the slot was published to us; the gather
                    // stage doesn't touch it until we advance the tail.
                    unsafe { &(&*pipeline.bufs[slot].get())[..st.lens[slot]] }
                };

                // The batch buffer is owned by this stage until we advance
                // the tail below, so it is safe to read outside the lock.
                // SAFETY: the termio outlives the read thread.
                unsafe { (*io).process_output(batch) };

                {
                    let mut st = pipeline.lock();
                    st.tail = (st.tail + 1) % Self::BUFFER_COUNT;
                    st.count -= 1;
                    let wake = st.count == 0 && st.bridging && pipeline.idle_write_fd >= 0;
                    drop(st);
                    pipeline.slot_free.notify_one();

                    // We ran out of batches while the gather stage is
                    // bridging a refill gap: interrupt its poll so it
                    // delivers what it has instead of sleeping out the
                    // timeout while we sit idle.
                    if wake {
                        // SAFETY: writing one byte from a valid buffer.
                        unsafe {
                            libc::write(pipeline.idle_write_fd, b"i".as_ptr() as *const c_void, 1)
                        };
                    }
                }

                // Batch boundary: hand the renderer state mutex off if
                // the renderer is waiting. See renderer.State.lockDemand.
                // SAFETY: the termio and its render state outlive the
                // read thread.
                unsafe { (*(*io).renderer_state).yield_to_demand(TinyIo) };
            }
        });

        if idle_read_fd >= 0 {
            crate::lib::compat::fd::close(idle_read_fd);
            crate::lib::compat::fd::close(idle_write_fd);
        }
    }

    /// The gather stage. This drains the pty into rotating buffers,
    /// bridging the kernel queue's refill gaps for saturated streams,
    /// and publishes each batch to the parse stage. This thread owns
    /// all fd monitoring, including the quit fd.
    #[cfg(unix)]
    fn gather_main_posix(fd: Fd, quit: Fd, pipeline: &Pipeline) {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: NUL-terminated name.
            unsafe { libc::pthread_setname_np(c"io-gather".as_ptr()) };
            Self::set_qos_class();
        }

        // However we exit, tell the parse stage the stream is over so
        // it drains the ring and joins us.
        struct DoneGuard<'a>(&'a Pipeline);
        impl Drop for DoneGuard<'_> {
            fn drop(&mut self) {
                self.0.lock().done = true;
                self.0.batch_ready.notify_one();
            }
        }
        let _done = DoneGuard(pipeline);

        // The fds we poll: data on the pty, our quit notification,
        // and the parse stage's idle wake. The idle fd only
        // participates in bridge polls (the parse stage only writes
        // while we're bridging), so the outer poll slices it off.
        let mut pollfds: [libc::pollfd; 3] = [
            libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: quit,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: pipeline.idle_read_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];

        // Whether we already drained the data that came with a hangup.
        let mut hup_drained = false;

        loop {
            // Claim the next free buffer. This blocks only when the
            // parse stage is a full ring behind, which is exactly when
            // we should stop reading and let the kernel queue exert
            // backpressure on the child.
            let buf: &mut [u8; Self::BUFFER_CAPACITY] = {
                let mut st = pipeline.lock();
                while st.count == Self::BUFFER_COUNT {
                    st = pipeline.slot_free.wait(st).unwrap_or_else(|e| e.into_inner());
                }
                // SAFETY: the head slot is free, so this stage owns it
                // until it is published.
                unsafe { &mut *pipeline.bufs[st.head].get() }
            };

            let mut total: usize = 0;
            let mut bridge_start: Option<Instant> = None;
            let mut spins: usize = 0;
            let mut fatal = false;

            // Fill the buffer from the pty. For a saturated stream the
            // kernel queue momentarily runs dry while the writer
            // refills it in parallel, so we bridge those gaps with
            // spin retries and a short poll instead of delivering a
            // tiny batch.
            'gather: while total < Self::BUFFER_CAPACITY {
                let n = match posix_read(fd, &mut buf[total..]) {
                    Ok(n) => n,

                    Err(PosixReadError::WouldBlock) => {
                        // Anything below the threshold is interactive.
                        if total < Self::BRIDGE_THRESHOLD {
                            break 'gather;
                        }

                        // The stream is saturated, so we bridge the
                        // gap. First retry the read directly a bounded
                        // number of times, since the refill usually
                        // lands within microseconds.
                        if spins < Self::BRIDGE_SPIN_MAX {
                            spins += 1;
                            continue 'gather;
                        }

                        // Still dry, so we want to sleep in poll for
                        // the next refill, within our latency budget.
                        let now = Instant::now();
                        match bridge_start {
                            Some(start) => {
                                if now.duration_since(start).as_nanos() >= Self::GATHER_BUDGET_NS {
                                    break 'gather;
                                }
                            }
                            None => bridge_start = Some(now),
                        }

                        // Bridging a refill gap is only free while the
                        // parse stage is busy, since the wait hides
                        // behind parse time. Once the parser is idle,
                        // every microsecond we hold this batch is
                        // added straight to output latency, and for a
                        // request/response producer (a burst ending
                        // in a query, e.g. frame + cursor position
                        // report) the writer is blocked on a reply to
                        // data sitting in this buffer, so a poll here
                        // would always sleep its full timeout. Deliver
                        // now if the parser is idle, and otherwise arm
                        // the idle wake so it can interrupt our poll
                        // the moment that changes.
                        {
                            let mut st = pipeline.lock();
                            if st.count == 0 {
                                break 'gather;
                            }
                            st.bridging = true;
                        }

                        let r = match posix_poll(&mut pollfds, Self::BRIDGE_POLL_TIMEOUT_MS) {
                            Ok(r) => r,
                            Err(poll_err) => {
                                Self::clear_bridging(pipeline);
                                log::warn!(target: LOG, "bridge poll failed err={poll_err}");
                                break 'gather;
                            }
                        };
                        Self::clear_bridging(pipeline);

                        // Quiet for a full timeout means the burst
                        // ended.
                        if r == 0 {
                            break 'gather;
                        }

                        // On a quit signal we deliver what we have
                        // and stop.
                        if pollfds[1].revents & libc::POLLIN != 0 {
                            log::info!(target: LOG, "read thread got quit signal");
                            fatal = true;
                            break 'gather;
                        }

                        // The parse stage went idle: drain the wake
                        // and deliver what we have. The pty may have
                        // data as well, but the next batch can pick
                        // that up; an idle parser means delivery must
                        // not wait.
                        if pollfds[2].revents & libc::POLLIN != 0 {
                            let mut trash = [0u8; 16];
                            loop {
                                let Ok(drained) = posix_read(pipeline.idle_read_fd, &mut trash)
                                else {
                                    break;
                                };
                                if drained < trash.len() {
                                    break;
                                }
                            }
                            break 'gather;
                        }

                        // HUP without IN means no more data is
                        // coming. Deliver and let the outer poll
                        // decide what to do.
                        if pollfds[0].revents & libc::POLLIN == 0 {
                            break 'gather;
                        }

                        continue 'gather;
                    }

                    // The pty is closed. We're probably gracefully
                    // shutting down.
                    Err(PosixReadError::NotOpenForReading | PosixReadError::InputOutput) => {
                        log::info!(target: LOG, "io gather exiting");
                        fatal = true;
                        break 'gather;
                    }

                    Err(err) => {
                        log::error!(target: LOG, "io gather error err={err:?}");
                        unreachable!();
                    }
                };

                // This happens on macOS instead of WouldBlock when the
                // child process dies. Deliver what we have and let the
                // outer poll detect HUP.
                if n == 0 {
                    break 'gather;
                }

                total += n;

                // Each refill gap gets a fresh spin budget.
                spins = 0;
            }

            // Publish the batch (if any) to the parse stage and rotate
            // to the next buffer.
            if total > 0 {
                let mut st = pipeline.lock();
                let head = st.head;
                st.lens[head] = total;
                st.head = (head + 1) % Self::BUFFER_COUNT;
                st.count += 1;
                drop(st);
                pipeline.batch_ready.notify_one();
            }

            if fatal {
                return;
            }

            // A full buffer means the stream is still hot, so go
            // claim the next buffer without an intervening poll.
            if total == Self::BUFFER_CAPACITY {
                continue;
            }

            // Wait for data. The idle fd is sliced off: the parse
            // stage only writes to it while we're bridging.
            if let Err(err) = posix_poll(&mut pollfds[0..2], -1) {
                log::warn!(target: LOG, "poll failed on read thread, exiting early err={err}");
                return;
            }

            // If our quit fd is set, we're done.
            if pollfds[1].revents & libc::POLLIN != 0 {
                log::info!(target: LOG, "read thread got quit signal");
                return;
            }

            // If our pty fd is closed, then we're also done with our
            // read thread.
            if pollfds[0].revents & libc::POLLHUP != 0 {
                // Deviation from Zig: output the child wrote right before
                // exiting arrives together with the hangup (POLLIN |
                // POLLHUP). Zig exits here and loses it; we drain it once
                // first (the next read ends in EIO, or we get here again).
                if pollfds[0].revents & libc::POLLIN == 0 || hup_drained {
                    log::info!(target: LOG, "pty fd closed, read thread exiting");
                    return;
                }
                hup_drained = true;
            }
        }
    }

    /// Clears the bridging flag armed before a bridge poll, closing
    /// the window in which the parse stage writes idle wakes.
    fn clear_bridging(pipeline: &Pipeline) {
        pipeline.lock().bridging = false;
    }

    /// Sets the QoS class of the calling thread for the read pipeline
    /// (macOS only). Both pipeline threads feed content the user is
    /// actively watching, and at default QoS the scheduler may place
    /// them on efficiency cores with wakeup latencies that are large
    /// compared to the ~10us cadence of the pty producer/consumer
    /// dance. Measured on an M4 Max, this results in a 15% throughput
    /// difference (on the change, not 15% total).
    #[cfg(target_os = "macos")]
    fn set_qos_class() {
        if let Err(err) = internal_os::macos::set_qos_class(internal_os::macos::QosClass::UserInitiated)
        {
            log::warn!(target: LOG, "error setting QoS class err={err:?}");
        }
    }

    /// Sets the fd to non-blocking mode. Returns false on failure.
    #[cfg(unix)]
    fn set_nonblock(fd: Fd) -> bool {
        // SAFETY: plain fcntl.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 {
            log::warn!(target: LOG, "read thread failed to get flags err=E{}", errno());
            return false;
        }

        // SAFETY: plain fcntl.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
            log::warn!(target: LOG, "read thread failed to set flags err=E{}", errno());
            return false;
        }
        true
    }

    #[cfg(windows)]
    fn thread_main_windows(fd: Fd, io: *mut Termio, quit: Fd) {
        use crate::os::windows::{self as windows, exp::kernel32};

        // Always close our end of the pipe when we exit.
        struct CloseOnDrop(Fd);
        impl Drop for CloseOnDrop {
            fn drop(&mut self) {
                close_fd(self.0);
            }
        }
        let _quit_guard = CloseOnDrop(quit);

        // Setup our crash metadata
        crash::sentry::set_thread_state(Some(crash::sentry::ThreadState {
            r#type: crash::sentry::ThreadType::Io,
            // SAFETY: the termio outlives the read thread.
            surface: crash_surface(unsafe { &(*io).surface_mailbox }),
        }));
        let _crash_guard = CrashStateGuard;

        let mut buf = [0u8; 1024];
        loop {
            loop {
                let mut n: windows::DWORD = 0;
                // SAFETY: valid buffer and out pointer.
                if unsafe {
                    kernel32::ReadFile(
                        fd,
                        buf.as_mut_ptr().cast(),
                        buf.len() as windows::DWORD,
                        &mut n,
                        std::ptr::null_mut(),
                    )
                } == windows::FALSE
                {
                    let err = windows::get_last_error();
                    match err {
                        // Check for a quit signal
                        windows::ERROR_OPERATION_ABORTED => break,

                        _ => {
                            log::error!(target: LOG, "io reader error err={err}");
                            unreachable!();
                        }
                    }
                }

                // SAFETY: the termio outlives the read thread.
                unsafe { (*io).process_output(&buf[..n as usize]) };

                // See threadMainPosix: hand the renderer state mutex
                // off if the renderer is waiting, since this loop
                // would otherwise starve it under heavy output.
                // SAFETY: as above.
                unsafe { (*(*io).renderer_state).yield_to_demand(TinyIo) };
            }

            let mut quit_bytes: windows::DWORD = 0;
            // SAFETY: valid out pointer.
            if unsafe {
                kernel32::PeekNamedPipe(
                    quit,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut quit_bytes,
                    std::ptr::null_mut(),
                )
            } == windows::FALSE
            {
                let err = windows::get_last_error();
                log::error!(target: LOG, "quit pipe reader error err={err}");
                unreachable!();
            }

            if quit_bytes > 0 {
                log::info!(target: LOG, "read thread got quit signal");
                return;
            }
        }
    }
}

/// The errors from [`exec_command`] (Zig: `Allocator.Error ||
/// error{SystemError}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecCommandError {
    OutOfMemory,
    SystemError,
}

/// The passwd lookup used for the real command (Zig:
/// `internal_os.passwd`).
#[cfg(unix)]
fn passwd_get() -> Result<PasswdEntry, crate::os::passwd::Error> {
    crate::os::passwd::get()
}

#[cfg(not(unix))]
fn passwd_get() -> Result<PasswdEntry, Error> {
    Ok(PasswdEntry::default())
}

/// Builds the argv array for the process we should exec for the
/// configured command. This isn't as straightforward as it seems since
/// we deal with shell-wrapping, macOS login shells, etc.
///
/// The `passwd_get` argument (Zig: the comptime `passwdpkg` type with a
/// single `get` function) returns a passwd entry. This is used by macOS
/// to determine the username and home directory for the login shell.
/// It is unused on other platforms.
pub fn exec_command<E: fmt::Display>(
    command: &configpkg::Command,
    passwd_get: impl FnOnce() -> Result<PasswdEntry, E>,
) -> Result<Vec<String>, ExecCommandError> {
    // If we're on macOS, we have to use `login(1)` to get all of
    // the proper environment variables set, a login shell, and proper
    // hushlogin behavior.
    #[cfg(target_os = "macos")]
    'darwin: {
        let passwd = match passwd_get() {
            Ok(p) => p,
            Err(err) => {
                log::warn!(target: LOG, "failed to read passwd, not using a login shell err={err}");
                break 'darwin;
            }
        };

        let Some(username) = passwd.name.clone() else {
            log::warn!(target: LOG, "failed to get username, not using a login shell");
            break 'darwin;
        };

        let hush = match &passwd.home {
            Some(home) => 'hush: {
                if let Err(err) = std::fs::read_dir(home) {
                    log::warn!(
                        target: LOG,
                        "failed to open home dir, not checking for hushlogin err={err}"
                    );
                    break 'hush false;
                }

                std::path::Path::new(home).join(".hushlogin").exists()
            }
            None => false,
        };

        // If we made it this far we're going to start building
        // the actual command.
        //
        // This capacity is chosen based on what we'd need to
        // execute a shell command (very common). We can/will
        // grow if necessary for a longer command (uncommon).
        let mut args: Vec<String> = Vec::with_capacity(9);

        // The reason for executing login this way is unclear. This
        // comment will attempt to explain but prepare for a truly
        // unhinged reality.
        //
        // The first major issue is that on macOS, a lot of users
        // put shell configurations in ~/.bash_profile instead of
        // ~/.bashrc (or equivalent for another shell). This file is only
        // loaded for a login shell so macOS users expect all their terminals
        // to be login shells. No other platform behaves this way and its
        // totally braindead but somehow the entire dev community on
        // macOS has cargo culted their way to this reality so we have to
        // do it...
        //
        // To get a login shell, you COULD just prepend argv0 with a `-`
        // but that doesn't fully work because `getlogin()` C API will
        // return the wrong value, SHELL won't be set, and various
        // other login behaviors that macOS users expect.
        //
        // The proper way is to use `login(1)`. But login(1) forces
        // the working directory to change to the home directory,
        // which we may not want. If we specify "-l" then we can avoid
        // this behavior but now the shell isn't a login shell.
        //
        // There is another issue: `login(1)` on macOS 14.3 and earlier
        // checked for ".hushlogin" in the working directory. This means
        // that if we specify "-l" then we won't get hushlogin honored
        // if its in the home directory (which is standard). To get
        // around this, we check for hushlogin ourselves and if present
        // specify the "-q" flag to login(1).
        //
        // So to get all the behaviors we want, we specify "-l" but
        // execute "bash" (which is built-in to macOS). We then use
        // the bash builtin "exec" to replace the process with a login
        // shell ("-l" on exec) with the command we really want.
        //
        // We use "bash" instead of other shells that ship with macOS
        // because as of macOS Sonoma, we found with a microbenchmark
        // that bash can `exec` into the desired command ~2x faster
        // than zsh.
        //
        // To figure out a lot of this logic I read the login.c
        // source code in the OSS distribution Apple provides for
        // macOS.
        //
        // Awesome.
        args.push("/usr/bin/login".into());
        if hush {
            args.push("-q".into());
        }
        args.push("-flp".into());
        args.push(username);

        match command {
            // Direct args can be passed directly to login, since
            // login uses execvp we don't need to worry about PATH
            // searching.
            configpkg::Command::Direct(v) => {
                args.extend(v.iter().map(|a| a.as_str().to_string()));
            }

            configpkg::Command::Shell(v) => {
                // Use "exec" to replace the bash process with
                // our intended command so we don't have a parent
                // process hanging around.
                let cmd = format!("exec -l {}", v.as_str());

                // We execute bash with "--noprofile --norc" so that it doesn't
                // load startup files so that (1) our shell integration doesn't
                // break and (2) user configuration doesn't mess this process
                // up.
                args.push("/bin/bash".into());
                args.push("--noprofile".into());
                args.push("--norc".into());
                args.push("-c".into());
                args.push(cmd);
            }
        }

        return Ok(args);
    }

    #[cfg(not(target_os = "macos"))]
    let _ = passwd_get;

    Ok(match command {
        // We need to clone the command since there's no guarantee the config remains valid.
        configpkg::Command::Direct(v) => v.iter().map(|a| a.as_str().to_string()).collect(),

        configpkg::Command::Shell(v) => {
            let v = v.as_str();
            let mut args: Vec<String> = Vec::with_capacity(4);

            #[cfg(windows)]
            {
                // On Windows we run the shell value directly rather than
                // wrapping in `cmd.exe /C <shell>`. An intermediate cmd
                // process is wasteful for the common case (`wsl ~`,
                // `pwsh -NoLogo`, etc.) and has visible side effects
                // (extra process in the tree, per-process cmd AutoRun
                // state not reaching the user's actual shell).
                //
                // Values with arguments are split on whitespace. This
                // does not honor Windows CLI quoting rules; users who
                // need quoted arguments should use the direct command
                // form, which takes an argv array as-is.
                if !v.contains([' ', '\t']) {
                    // No arguments. If the shell is literally "cmd.exe"
                    // (the default), resolve via %COMSPEC% which is the
                    // documented path to the current command processor.
                    // Other values are passed as-is and resolved by
                    // `internal_os.path.expand` in Command.startWindows.
                    let argv0 = if v.eq_ignore_ascii_case("cmd.exe") {
                        global::environ()
                            .get("COMSPEC")
                            .cloned()
                            .unwrap_or_else(|| v.to_string())
                    } else {
                        v.to_string()
                    };
                    args.push(argv0);
                } else {
                    args.extend(
                        v.split([' ', '\t'])
                            .filter(|t| !t.is_empty())
                            .map(|t| t.to_string()),
                    );
                }
                return Ok(args);
            }

            #[cfg(not(windows))]
            {
                // We run our shell wrapped in `/bin/sh` so that we don't have
                // to parse the command line ourselves if it has arguments.
                // Additionally, some environments (NixOS, I found) use /bin/sh
                // to setup some environment variables that are important to
                // have set.
                args.push("/bin/sh".into());
                if internal_os::is_flatpak() {
                    args.push("-l".into());
                }
                args.push("-c".into());
            }

            args.push(v.to_string());
            args
        }
    })
}

/// The `PATH`-style list separator (Zig: `std.fs.path.delimiter`).
const PATH_DELIMITER: char = if cfg!(windows) { ';' } else { ':' };

/// Append a value to an environment variable such as PATH.
/// The returned value is always allocated so it must be freed.
fn append_env(current: &str, value: &str) -> String {
    // If there is no prior value, we return it as-is
    if current.is_empty() {
        return value.to_string();
    }

    // Otherwise we must prefix.
    append_env_always(current, value)
}

/// Always append value to environment, even when it is empty.
/// This is useful because some env vars (like MANPATH) want there
/// to be an empty prefix to preserve existing values.
///
/// The returned value is always allocated so it must be freed.
fn append_env_always(current: &str, value: &str) -> String {
    format!("{current}{PATH_DELIMITER}{value}")
}

/// A test harness that runs an [`Exec`] backend under a real
/// [`crate::termio::Thread`] and [`Termio`], with stand-ins for the
/// renderer (a mailbox and wakeup nobody drains) and the surface (an app
/// queue the harness drains).
#[cfg(all(test, unix))]
pub(crate) mod testing {
    use std::ptr::NonNull;
    use std::time::{Duration, Instant};

    use super::{Config, Exec};
    use crate::apprt;
    use crate::app::{Mailbox as AppMailbox, MailboxQueue as AppQueue, Message as AppMessage};
    use crate::apprt::surface::Message as SurfaceMessage;
    use crate::config::Command as ConfigCommand;
    use crate::global::xev;
    use crate::lib::tinyio::{Mutex, TinyIo};
    use crate::renderer;
    use crate::terminal::point::{Coordinate, Point};
    use crate::termio::{Backend, DerivedConfig, Mailbox, Message, Options, Termio, Thread};

    struct SendPtr<T>(*mut T);
    // SAFETY: test-only hand-off of pointers whose targets outlive the
    // spawned thread (it is joined in `Harness::stop`).
    unsafe impl<T> Send for SendPtr<T> {}
    impl<T> SendPtr<T> {
        fn get(self) -> *mut T {
            self.0
        }
    }

    /// Everything a running termio thread needs, heap-allocated so that
    /// the pointers between the pieces stay valid.
    pub struct Harness {
        pub mutex: Box<Mutex>,
        pub state: Box<renderer::State>,
        pub rt_app: Box<apprt::App>,
        pub app_queue: Box<AppQueue>,
        pub renderer_mailbox: Box<renderer::thread::Mailbox>,
        pub renderer_wakeup: xev::Async,
        pub io: Box<Termio>,
        pub thread: Box<Thread>,
        pub stop: xev::Async,
        pub handle: Option<std::thread::JoinHandle<()>>,
        /// Surface messages received so far.
        pub messages: Vec<SurfaceMessage>,
        /// Renderer messages received so far.
        pub renderer_messages: Vec<renderer::Message>,
    }

    /// An exec config running `argv` directly with a minimal environment.
    pub fn direct_config(argv: &[&str]) -> Config {
        let mut env = super::EnvMap::new();
        env.insert("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into());
        Config {
            command: Some(ConfigCommand::Direct(
                argv.iter().map(|a| (*a).into()).collect(),
            )),
            env,
            shell_integration: crate::config::config::ShellIntegration::None,
            term: "xterm-256color".into(),
            ..Config::default()
        }
    }

    /// The size of a `cols`x`rows` grid of 10x20 cells.
    pub fn grid_size(cols: u32, rows: u32) -> renderer::Size {
        renderer::Size {
            screen: renderer::ScreenSize {
                width: cols * 10,
                height: rows * 20,
            },
            cell: renderer::CellSize {
                width: 10,
                height: 20,
            },
            padding: renderer::Padding::default(),
        }
    }

    impl Harness {
        /// Build the termio for `cfg` (80x24) without starting the thread.
        pub fn new(cfg: Config) -> Harness {
            Harness::with_config(cfg, &crate::config::Config::default())
        }

        /// Build the termio for `cfg` and the app config `full_config`.
        pub fn with_config(cfg: Config, full_config: &crate::config::Config) -> Harness {
            let mut mutex = Box::new(Mutex::new());
            // The terminal is owned by the termio; it is set below.
            let mut state = Box::new(renderer::State::new(
                NonNull::from(&mut *mutex),
                NonNull::dangling(),
            ));
            let rt_app = Box::new(apprt::App::default());
            let app_queue = Box::new(AppQueue::new());
            let renderer_mailbox = renderer::thread::Mailbox::create();
            let renderer_wakeup = xev::Async::init().unwrap();

            let backend = Backend::Exec(Exec::init(cfg).unwrap());
            let mut io = Termio::init(Options {
                size: grid_size(80, 24),
                full_config,
                config: DerivedConfig::init(full_config),
                backend,
                mailbox: Mailbox::init_spsc().unwrap(),
                renderer_state: &mut *state,
                renderer_wakeup,
                renderer_mailbox: &*renderer_mailbox,
                surface_mailbox: apprt::surface::Mailbox {
                    surface: std::ptr::null_mut(),
                    app: AppMailbox {
                        rt_app: &*rt_app,
                        mailbox: &*app_queue,
                    },
                },
            })
            .unwrap();
            state.terminal = NonNull::from(&mut io.terminal);

            let thread = Box::new(Thread::init().unwrap());
            let stop = thread.stop;
            Harness {
                mutex,
                state,
                rt_app,
                app_queue,
                renderer_mailbox,
                renderer_wakeup,
                io,
                thread,
                stop,
                handle: None,
                messages: Vec::new(),
                renderer_messages: Vec::new(),
            }
        }

        /// Start the termio thread.
        pub fn start(&mut self) {
            let thread = SendPtr(&mut *self.thread as *mut Thread);
            let io = SendPtr(&mut *self.io as *mut Termio);
            self.handle = Some(
                std::thread::Builder::new()
                    .name("io".into())
                    .spawn(move || {
                        let thread = thread.get();
                        let io = io.get();
                        // SAFETY: both outlive the thread (joined in `stop`).
                        unsafe { (*thread).thread_main(io) };
                    })
                    .unwrap(),
            );
        }

        pub fn io_ptr(&mut self) -> *mut Termio {
            &mut *self.io
        }

        /// Queue a message to the termio thread (from the "surface").
        pub fn send(&mut self, msg: Message) {
            let io = self.io_ptr();
            // SAFETY: the mailbox is thread-safe.
            let mailbox = unsafe { &(*io).mailbox };
            mailbox.send(msg, None);
            mailbox.notify();
        }

        /// Run `f` on the termio while holding the renderer mutex (which
        /// protects the terminal).
        pub fn with_io<R>(&mut self, f: impl FnOnce(&mut Termio) -> R) -> R {
            let io = self.io_ptr();
            self.mutex.lock_uncancelable(TinyIo);
            // SAFETY: the inspected fields are protected by the mutex.
            let r = f(unsafe { &mut *io });
            self.mutex.unlock(TinyIo);
            r
        }

        /// The whole screen contents (history included, soft wraps joined)
        /// of the terminal.
        pub fn screen(&mut self) -> String {
            self.with_io(|io| {
                io.terminal
                    .screens
                    .active
                    .dump_string_alloc_unwrapped(Point::Screen(Coordinate::default()))
                    .unwrap()
            })
        }

        /// Wait until the screen contains `needle`.
        pub fn wait_output(&mut self, needle: &str) -> String {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let out = self.screen();
                if out.contains(needle) {
                    return out;
                }
                assert!(Instant::now() < deadline, "timed out waiting for {needle:?}, got {out:?}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Move the queued surface messages into `messages`. Like the
        /// surface, a password input report updates the terminal flag.
        pub fn collect_messages(&mut self) {
            while let Some(m) = self.app_queue.pop() {
                match m {
                    AppMessage::SurfaceMessage { message, .. } => {
                        if let SurfaceMessage::PasswordInput(v) = message {
                            self.with_io(|io| io.terminal.flags.password_input = v);
                        }
                        self.messages.push(message)
                    }
                    other => panic!("unexpected app message {other:?}"),
                }
            }
        }

        /// Move the queued renderer messages into `renderer_messages`.
        pub fn collect_renderer_messages(&mut self) {
            while let Some(m) = self.renderer_mailbox.pop() {
                self.renderer_messages.push(m);
            }
        }

        /// Collect surface messages, waiting until `pred` matches one.
        pub fn wait_message(&mut self, pred: impl Fn(&SurfaceMessage) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                self.collect_messages();
                if self.messages.iter().any(&pred) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for a message, got {:?}",
                    self.messages
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Wait until `pred` holds on the termio (under the renderer mutex).
        pub fn wait_io(&mut self, pred: impl Fn(&mut Termio) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.with_io(&pred) {
                assert!(Instant::now() < deadline, "timed out waiting for termio state");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Stop and join the termio thread.
        pub fn stop(&mut self) {
            if let Some(h) = self.handle.take() {
                self.stop.notify().unwrap();
                h.join().unwrap();
            }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.stop();
            self.io.deinit();
            self.renderer_wakeup.deinit();
            self.thread.deinit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fail_passwd() -> Result<PasswdEntry, &'static str> {
        // Failed passwd entry means we can't construct a macOS
        // login command and falls back to POSIX behavior.
        Err("Fail")
    }

    #[cfg(target_os = "macos")]
    fn testuser_passwd() -> Result<PasswdEntry, &'static str> {
        Ok(PasswdEntry {
            name: Some("testuser".into()),
            ..Default::default()
        })
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn exec_command_darwin_shell_command() {
        let result =
            exec_command(&configpkg::Command::Shell("foo bar baz".into()), testuser_passwd).unwrap();

        assert_eq!(8, result.len());
        assert_eq!(result[0], "/usr/bin/login");
        assert_eq!(result[1], "-flp");
        assert_eq!(result[2], "testuser");
        assert_eq!(result[3], "/bin/bash");
        assert_eq!(result[4], "--noprofile");
        assert_eq!(result[5], "--norc");
        assert_eq!(result[6], "-c");
        assert_eq!(result[7], "exec -l foo bar baz");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn exec_command_darwin_direct_command() {
        let result = exec_command(
            &configpkg::Command::Direct(vec!["foo".into(), "bar baz".into()]),
            testuser_passwd,
        )
        .unwrap();

        assert_eq!(5, result.len());
        assert_eq!(result[0], "/usr/bin/login");
        assert_eq!(result[1], "-flp");
        assert_eq!(result[2], "testuser");
        assert_eq!(result[3], "foo");
        assert_eq!(result[4], "bar baz");
    }

    #[cfg(not(windows))]
    #[test]
    fn exec_command_shell_command_empty_passwd() {
        let result = exec_command(&configpkg::Command::Shell("foo bar baz".into()), || {
            // Empty passwd entry means we can't construct a macOS
            // login command and falls back to POSIX behavior.
            Ok::<_, &str>(PasswdEntry::default())
        })
        .unwrap();

        assert_eq!(3, result.len());
        assert_eq!(result[0], "/bin/sh");
        assert_eq!(result[1], "-c");
        assert_eq!(result[2], "foo bar baz");
    }

    #[cfg(not(windows))]
    #[test]
    fn exec_command_shell_command_error_passwd() {
        let result =
            exec_command(&configpkg::Command::Shell("foo bar baz".into()), fail_passwd).unwrap();

        assert_eq!(3, result.len());
        assert_eq!(result[0], "/bin/sh");
        assert_eq!(result[1], "-c");
        assert_eq!(result[2], "foo bar baz");
    }

    #[cfg(not(windows))]
    #[test]
    fn exec_command_direct_command_error_passwd() {
        let result = exec_command(
            &configpkg::Command::Direct(vec!["foo".into(), "bar baz".into()]),
            fail_passwd,
        )
        .unwrap();

        assert_eq!(2, result.len());
        assert_eq!(result[0], "foo");
        assert_eq!(result[1], "bar baz");
    }

    #[cfg(not(windows))]
    #[test]
    fn exec_command_direct_command_config_freed() {
        let command_arena = configpkg::Command::Direct(vec!["foo".into(), "bar baz".into()]);
        let command = command_arena.clone();

        let result = exec_command(&command, fail_passwd).unwrap();

        drop(command);
        drop(command_arena);

        assert_eq!(2, result.len());
        assert_eq!(result[0], "foo");
        assert_eq!(result[1], "bar baz");
    }

    #[cfg(windows)]
    fn empty_passwd() -> Result<PasswdEntry, &'static str> {
        Ok(PasswdEntry::default())
    }

    #[cfg(windows)]
    #[test]
    fn exec_command_windows_bare_cmd_exe_resolves_via_comspec() {
        let result = exec_command(&configpkg::Command::Shell("cmd.exe".into()), empty_passwd).unwrap();

        assert_eq!(1, result.len());

        // Expect COMSPEC if available, otherwise the documented fallback.
        let expected = std::env::var("COMSPEC")
            .unwrap_or_else(|_| "C:\\Windows\\System32\\cmd.exe".to_string());
        assert_eq!(expected, result[0]);
    }

    #[cfg(windows)]
    #[test]
    fn exec_command_windows_bare_non_cmd_shell_is_passed_through() {
        let result =
            exec_command(&configpkg::Command::Shell("pwsh.exe".into()), empty_passwd).unwrap();

        assert_eq!(1, result.len());
        assert_eq!("pwsh.exe", result[0]);
    }

    #[cfg(windows)]
    #[test]
    fn exec_command_windows_shell_with_args_is_split_on_whitespace() {
        let result = exec_command(&configpkg::Command::Shell("wsl ~".into()), empty_passwd).unwrap();

        assert_eq!(2, result.len());
        assert_eq!("wsl", result[0]);
        assert_eq!("~", result[1]);
    }

    #[cfg(windows)]
    #[test]
    fn exec_command_windows_direct_command_is_passed_through_unchanged() {
        let result = exec_command(
            &configpkg::Command::Direct(vec![
                "C:\\tools\\foo.exe".into(),
                "arg with spaces".into(),
            ]),
            empty_passwd,
        )
        .unwrap();

        assert_eq!(2, result.len());
        assert_eq!("C:\\tools\\foo.exe", result[0]);
        assert_eq!("arg with spaces", result[1]);
    }

    /// Zig runs init under a failing allocator to check that the env is
    /// freed on every failure path. Rust ownership makes that automatic
    /// (init takes the env by value), so this checks that init consumes
    /// the env, builds the subprocess environment from it and frees it.
    #[test]
    fn init_frees_the_env_when_it_fails() {
        let config = configpkg::Config::default();

        let mut env = EnvMap::new();
        env.insert("HOME".into(), "/home/ghostty".into());
        env.insert("SHELL".into(), "/bin/sh".into());

        let mut exec = Exec::init(Config {
            env,
            resources_dir: None,
            term: "xterm-ghostty".into(),
            rt_pre_exec_info: command::RtPreExecInfo::init(&config),
            rt_post_fork_info: command::RtPostForkInfo::init(&config),
            ..Config::default()
        })
        .unwrap();

        let env = exec.subprocess.env.as_ref().unwrap();
        assert_eq!(Some("/home/ghostty"), env.get("HOME").map(String::as_str));
        assert_eq!(Some("/bin/sh"), env.get("SHELL").map(String::as_str));
        exec.deinit();
        assert!(exec.subprocess.env.is_none());
    }

    // Extra (not in Zig): the subprocess environment setup.
    #[cfg(unix)]
    #[test]
    fn init_env() {
        let mut env = EnvMap::new();
        env.insert("VTE_VERSION".into(), "7000".into());
        env.insert("PATH".into(), "/usr/bin".into());
        let mut env_override = RepeatableStringMap::default();
        env_override.map.push(("FOO".into(), "bar".into()));

        let sp = Subprocess::init(Config {
            env,
            env_override,
            resources_dir: Some("/opt/ghostty/share/ghostty".into()),
            term: "xterm-ghostty".into(),
            working_directory: Some("/tmp".into()),
            cursor_blink: Some(false),
            ..Config::default()
        })
        .unwrap();

        let env = sp.env.as_ref().unwrap();
        let get = |k: &str| env.get(k).map(String::as_str);
        assert_eq!(Some("/opt/ghostty/share/ghostty"), get("GHOSTTY_RESOURCES_DIR"));
        assert_eq!(Some("xterm-ghostty"), get("TERM"));
        assert_eq!(Some("truecolor"), get("COLORTERM"));
        assert_eq!(Some("/opt/ghostty/share/terminfo"), get("TERMINFO"));
        assert_eq!(Some("ghostty"), get("TERM_PROGRAM"));
        assert_eq!(Some(build_config::VERSION_STRING), get("TERM_PROGRAM_VERSION"));
        assert_eq!(None, get("VTE_VERSION"));
        assert_eq!(Some("bar"), get("FOO"));
        assert_eq!(Some("/tmp"), get("PWD"));
        assert_eq!(Some("cursor:steady,path,title"), get("GHOSTTY_SHELL_FEATURES"));

        // Our binary dir is appended to the PATH.
        let exe = std::env::current_exe().unwrap();
        let exe_dir = exe.parent().unwrap().to_str().unwrap();
        assert_eq!(Some(exe_dir), get("GHOSTTY_BIN_DIR"));
        assert_eq!(Some(format!("/usr/bin:{exe_dir}").as_str()), get("PATH"));

        // Without a command, the default shell is wrapped in /bin/sh.
        if !internal_os::is_flatpak() {
            assert_eq!(vec!["/bin/sh", "-c", "sh"], sp.args);
        }
        assert_eq!(Some("/tmp"), sp.cwd.as_deref());
    }

    // Extra (not in Zig): no resources dir falls back to xterm-256color.
    #[test]
    fn init_env_no_resources_dir() {
        let sp = Subprocess::init(Config {
            term: "xterm-ghostty".into(),
            ..Config::default()
        })
        .unwrap();
        let env = sp.env.as_ref().unwrap();
        assert_eq!(Some("xterm-256color"), env.get("TERM").map(String::as_str));
        assert_eq!(None, env.get("TERMINFO"));
        assert_eq!(None, env.get("GHOSTTY_RESOURCES_DIR"));
    }

    // Extra (not in Zig): helpers.
    #[test]
    fn append_env_helpers() {
        assert_eq!("b", append_env("", "b"));
        assert_eq!(format!("a{PATH_DELIMITER}b"), append_env("a", "b"));
        assert_eq!(format!("{PATH_DELIMITER}b"), append_env_always("", "b"));
        assert_eq!(Some("/a"), dirname("/a/b"));
        assert_eq!(Some("/a"), dirname("/a/b/"));
        assert_eq!(Some("/"), dirname("/a"));
        assert_eq!(None, dirname("a"));
        assert_eq!(None, dirname("/"));
        assert_eq!("`a`, `b c`", format_args_list(&["a".into(), "b c".into()]));
    }

    // Extra (not in Zig): the write pool hands out stable, reused items.
    #[test]
    fn memory_pool_reuse() {
        let mut pool: MemoryPool<Write> = MemoryPool::default();
        let a = pool.create();
        let b = pool.create();
        assert_ne!(a, b);
        pool.destroy(a);
        assert_eq!(a, pool.create());
        pool.deinit();
    }

    // Extra (not in Zig): start a subprocess on a pty, resize it (clamping
    // to u16) and stop it.
    #[cfg(unix)]
    #[test]
    fn subprocess_start_resize_stop() {
        let mut sp =
            Subprocess::init(testing::direct_config(&["/bin/sh", "-c", "sleep 10"])).unwrap();
        sp.resize(
            renderer::GridSize {
                columns: 100,
                rows: 40,
            },
            renderer::ScreenSize {
                width: 1000,
                height: 800,
            },
        )
        .unwrap();
        let fds = sp.start().unwrap();
        assert_eq!(fds.read, fds.write);
        assert!(sp.env.is_none());
        assert!(matches!(sp.process, Some(Process::ForkExec(_))));
        let size = sp.pty.as_ref().unwrap().get_size().unwrap();
        assert_eq!(
            (40, 100, 1000, 800),
            (size.ws_row, size.ws_col, size.ws_xpixel, size.ws_ypixel)
        );

        sp.resize(
            renderer::GridSize {
                columns: 120,
                rows: 50,
            },
            renderer::ScreenSize {
                width: 100_000,
                height: 900,
            },
        )
        .unwrap();
        let size = sp.pty.as_ref().unwrap().get_size().unwrap();
        assert_eq!(
            (50, 120, u16::MAX, 900),
            (size.ws_row, size.ws_col, size.ws_xpixel, size.ws_ypixel)
        );

        assert!(matches!(
            sp.get_process_info(ProcessInfo::ForegroundPid),
            Some(ProcessInfoValue::ForegroundPid(_))
        ));

        // Stopping kills the process group and reaps the child.
        sp.stop();
        assert!(sp.process.is_none());
        sp.deinit();
    }

    // Extra (not in Zig): a command that can't be started leaves no pty
    // behind.
    #[cfg(unix)]
    #[test]
    fn subprocess_start_failure_cleans_up() {
        let mut sp = Subprocess::init(testing::direct_config(&["/bin/s\0h"])).unwrap();
        assert_eq!(Err(Error::InvalidArgument), sp.start().map(|_| ()));
        assert!(sp.pty.is_none());
        assert!(sp.process.is_none());
        sp.deinit();
    }

    // Extra (not in Zig): run a command on a pty under the termio thread,
    // read its output and observe its exit.
    #[cfg(unix)]
    #[test]
    fn exec_reads_output_and_observes_exit() {
        let mut h = testing::Harness::new(testing::direct_config(&[
            "/bin/sh",
            "-c",
            // The read thread stops at the pty hangup even if output is
            // still buffered (like Zig), so give it time to read first.
            "printf 'hello from the pty'; sleep 0.2; exit 3",
        ]));
        h.start();
        h.wait_output("hello from the pty");
        h.wait_message(|m| matches!(m, SurfaceMessage::ChildExited(e) if e.exit_code == 3));
        h.stop();
    }

    // Extra (not in Zig): bulk output goes through the gather pipeline
    // intact and in order.
    #[cfg(unix)]
    #[test]
    fn exec_reads_bulk_output() {
        let mut h = testing::Harness::new(testing::direct_config(&[
            "/bin/sh",
            "-c",
            "i=0; while [ $i -lt 5000 ]; do echo \"line $i\"; i=$((i+1)); done; echo DONE; sleep 0.5",
        ]));
        h.start();
        let out = h.wait_output("DONE");
        let lines: Vec<&str> = out.split('\n').filter(|l| l.starts_with("line ")).collect();
        assert_eq!(5000, lines.len());
        for (i, l) in lines.iter().enumerate() {
            assert_eq!(format!("line {i}"), *l);
        }
        h.wait_message(|m| matches!(m, SurfaceMessage::ChildExited(e) if e.exit_code == 0));
        h.stop();
    }

    // Extra (not in Zig): writes queued through the mailbox reach the child.
    #[cfg(unix)]
    #[test]
    fn exec_queue_write() {
        let mut h = testing::Harness::new(testing::direct_config(&[
            "/bin/sh",
            "-c",
            "read line; echo \"got:$line\"; sleep 0.5",
        ]));
        h.start();
        h.send(crate::termio::Message::write_req(b"abc\n"));
        h.wait_output("got:abc");
        h.wait_message(|m| matches!(m, SurfaceMessage::ChildExited(e) if e.exit_code == 0));

        // Writes after the exit are dropped.
        h.send(crate::termio::Message::write_req(b"ignored\n"));
        h.stop();
    }

    // Extra (not in Zig): a write larger than one pooled buffer is chunked
    // and arrives in order.
    #[cfg(unix)]
    #[test]
    fn exec_queue_write_chunks() {
        let mut h = testing::Harness::new(testing::direct_config(&[
            "/bin/sh",
            "-c",
            "read line; echo \"len:${#line}:$line\"; sleep 0.5",
        ]));
        h.start();
        let payload: String = (0..300).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        h.send(crate::termio::Message::write_req(format!("{payload}\n").as_bytes()));
        h.wait_output(&format!("len:300:{payload}"));
        h.stop();
    }

    // Extra (not in Zig): linefeed mode turns \r into \r\n.
    #[cfg(unix)]
    #[test]
    fn exec_queue_write_linefeed() {
        let mut h = testing::Harness::new(testing::direct_config(&[
            "/bin/sh",
            "-c",
            "read a; read b; echo \"[$a|$b]\"; sleep 0.5",
        ]));
        h.start();
        h.send(crate::termio::Message::LinefeedMode(true));
        // With linefeed mode "1\r" is "1\r\n": two lines, "1" and "".
        h.send(crate::termio::Message::write_req(b"1\r"));
        h.wait_output("[1|]");
        h.stop();
    }

    // Extra (not in Zig): the termios timer reports password input.
    #[cfg(unix)]
    #[test]
    fn exec_password_input() {
        let mut h = testing::Harness::new(testing::direct_config(&[
            "/bin/sh",
            "-c",
            "stty -echo; echo off; sleep 1; stty echo; echo on; sleep 1",
        ]));
        h.start();
        h.wait_message(|m| matches!(m, SurfaceMessage::PasswordInput(true)));
        h.wait_message(|m| matches!(m, SurfaceMessage::PasswordInput(false)));
        let inputs: Vec<bool> = h
            .messages
            .iter()
            .filter_map(|m| match m {
                SurfaceMessage::PasswordInput(v) => Some(*v),
                _ => None,
            })
            .collect();
        assert_eq!(vec![true, false], inputs);
        h.stop();
    }

    // Extra (not in Zig): stopping the thread while the child is still
    // running kills it and joins the read thread.
    #[cfg(unix)]
    #[test]
    fn exec_stop_running_child() {
        let mut h = testing::Harness::new(testing::direct_config(&[
            "/bin/sh",
            "-c",
            "echo ready; sleep 30",
        ]));
        h.start();
        h.wait_output("ready");
        let start = Instant::now();
        h.stop();
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    // Extra (not in Zig): shell features are set in the Zig order.
    #[test]
    fn shell_features() {
        let mut env = EnvMap::new();
        shell_integration::setup_features(&mut env, ShellIntegrationFeatures::default(), true)
            .unwrap();
        assert_eq!(
            Some("cursor:blink,path,title"),
            env.get("GHOSTTY_SHELL_FEATURES").map(String::as_str)
        );

        let mut env = EnvMap::new();
        let none = ShellIntegrationFeatures {
            cursor: false,
            sudo: false,
            title: false,
            ssh_env: false,
            ssh_terminfo: false,
            path: false,
        };
        shell_integration::setup_features(&mut env, none, true).unwrap();
        assert!(env.get("GHOSTTY_SHELL_FEATURES").is_none());
    }
}
