//! Port of `src/pty.zig` (and the small C helper `src/pty.c`, whose only job
//! was to expose `openpty`, `ptsname_r`, `ioctl` and the termios constants to
//! Zig; the Rust port calls them through `libc` directly).
//!
//! STATUS: ported

#[cfg(unix)]
use std::ffi::CStr;

#[cfg(unix)]
const LOG_TARGET: &str = "pty";

/// Redeclare this winsize struct so we can just use a Rust struct. This
/// layout should be correct on all tested platforms. The defaults on this
/// are some reasonable screen size but you should probably not use them.
///
/// Zig: `winsize`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

impl Default for Winsize {
    fn default() -> Self {
        Self {
            ws_row: 100,
            ws_col: 80,
            ws_xpixel: 800,
            ws_ypixel: 600,
        }
    }
}

#[cfg(all(unix, not(target_os = "ios")))]
pub type Pty = PosixPty;
#[cfg(target_os = "ios")]
pub type Pty = NullPty;
#[cfg(windows)]
pub type Pty = WindowsPty;

/// The modes of a pty. Not all of these modes are supported on
/// all platforms but all platforms share the same mode struct.
///
/// The default values of fields in this struct are set to the
/// most typical values for a pty. This makes it easier for cross-platform
/// code which doesn't support all of the modes to work correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    /// ICANON on POSIX
    pub canonical: bool,

    /// ECHO on POSIX
    pub echo: bool,
}

impl Default for Mode {
    fn default() -> Self {
        Self {
            canonical: true,
            echo: true,
        }
    }
}

/// The kinds of process information [`PosixPty::get_process_info`] can
/// return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessInfo {
    /// The PID of the process that controls the PTY.
    ForegroundPid,
    /// Gets the name of the slave PTY. Returned name points to an internal buffer
    /// so it should not be modified or freed.
    TtyName,
}

/// The value returned by `get_process_info` (Zig: `ProcessInfo.Type(info)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessInfoValue<'a> {
    ForegroundPid(u64),
    TtyName(&'a str),
}

macro_rules! simple_error {
    ($(#[$m:meta])* $name:ident { $($variant:ident),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($variant),* }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Debug::fmt(self, f)
            }
        }

        impl std::error::Error for $name {}
    };
}

// A pty implementation that does nothing.
//
// TODO: This should be removed. This is only temporary until we have
// a termio that doesn't use a pty. This isn't used in any user-facing
// artifacts, this is just a stopgap to get compilation to work on iOS.
#[derive(Debug)]
pub struct NullPty {
    pub master: NullPtyFd,
    pub slave: NullPtyFd,
}

/// Zig: `NullPty.Fd`.
pub type NullPtyFd = i32;

simple_error!(
    /// Zig: `NullPty.GetModeError`.
    NullPtyGetModeError { GetModeFailed }
);

#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
impl NullPty {
    pub fn open(_size: Winsize) -> Result<NullPty, std::convert::Infallible> {
        Ok(NullPty {
            master: 0,
            slave: 0,
        })
    }

    pub fn deinit(&mut self) {}

    pub fn get_mode(&self) -> Result<Mode, NullPtyGetModeError> {
        Ok(Mode::default())
    }

    pub fn set_size(&mut self, _size: Winsize) -> Result<(), std::convert::Infallible> {
        Ok(())
    }

    pub fn child_pre_exec(&self) -> Result<(), std::convert::Infallible> {
        Ok(())
    }

    /// Get information about the process(es) attached to the PTY. Returns
    /// `None` if there was an error getting the information or the information
    /// is not available on a particular platform.
    pub fn get_process_info(&mut self, _info: ProcessInfo) -> Option<ProcessInfoValue<'_>> {
        None
    }
}

simple_error!(
    /// Zig: `PosixPty.OpenError`.
    OpenError { OpenptyFailed }
);
simple_error!(
    /// Zig: `PosixPty.GetModeError`.
    GetModeError { GetModeFailed }
);
simple_error!(
    /// Zig: `PosixPty.GetSizeError` / `SetSizeError` (both `error{IoctlFailed}`).
    IoctlError { IoctlFailed }
);
/// Zig: `PosixPty.GetSizeError`.
pub type GetSizeError = IoctlError;
/// Zig: `PosixPty.SetSizeError`.
pub type SetSizeError = IoctlError;
simple_error!(
    /// Zig: `PosixPty.ChildPreExecError`.
    ChildPreExecError { OperationNotSupported, ProcessGroupFailed, SetControllingTerminalFailed }
);
simple_error!(
    /// Zig: `PosixPty.Error` (the union of all the error sets above).
    Error {
        OpenptyFailed,
        GetModeFailed,
        IoctlFailed,
        OperationNotSupported,
        ProcessGroupFailed,
        SetControllingTerminalFailed,
    }
);

impl From<OpenError> for Error {
    fn from(_: OpenError) -> Self {
        Error::OpenptyFailed
    }
}
impl From<GetModeError> for Error {
    fn from(_: GetModeError) -> Self {
        Error::GetModeFailed
    }
}
impl From<IoctlError> for Error {
    fn from(_: IoctlError) -> Self {
        Error::IoctlFailed
    }
}
impl From<ChildPreExecError> for Error {
    fn from(e: ChildPreExecError) -> Self {
        match e {
            ChildPreExecError::OperationNotSupported => Error::OperationNotSupported,
            ChildPreExecError::ProcessGroupFailed => Error::ProcessGroupFailed,
            ChildPreExecError::SetControllingTerminalFailed => Error::SetControllingTerminalFailed,
        }
    }
}

/// The maximum path length (Zig: `std.Io.Dir.max_path_bytes`).
#[cfg(unix)]
const MAX_PATH_BYTES: usize = libc::PATH_MAX as usize;

/// Posix PTY creation and management. This is just a thin layer on top
/// of Posix syscalls. The caller is responsible for detail-oriented handling
/// of the returned file handles.
#[cfg(unix)]
pub struct PosixPty {
    /// The file descriptors for the master and slave side of the pty.
    /// The slave side is never closed automatically by this struct
    /// so the caller is responsible for closing it if things
    /// go wrong.
    pub master: PosixFd,
    pub slave: PosixFd,

    /// Buffer for storage of slave tty name so that we don't have to recompute
    /// it every time we need it.
    tty_name_buf: Box<[u8; MAX_PATH_BYTES + 1]>,
    /// The length of the name of slave tty in `tty_name_buf`. If `None` it
    /// has not yet been computed or may not be available. Should not be
    /// accessed directly, but through `self.get_process_info(TtyName)`.
    tty_name: Option<usize>,
}

/// Zig: `PosixPty.Fd`.
#[cfg(unix)]
pub type PosixFd = std::os::fd::RawFd;

#[cfg(unix)]
impl std::fmt::Debug for PosixPty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PosixPty")
            .field("master", &self.master)
            .field("slave", &self.slave)
            .finish_non_exhaustive()
    }
}

#[cfg(unix)]
fn last_errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[cfg(unix)]
impl PosixPty {
    /// Open a new PTY with the given initial size.
    pub fn open(size: Winsize) -> Result<PosixPty, OpenError> {
        // Need to copy so that it becomes non-const.
        let mut size_copy = libc::winsize {
            ws_row: size.ws_row,
            ws_col: size.ws_col,
            ws_xpixel: size.ws_xpixel,
            ws_ypixel: size.ws_ypixel,
        };

        let mut master_fd: PosixFd = -1;
        let mut slave_fd: PosixFd = -1;
        // SAFETY: valid out pointers; name and termios may be null.
        if unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size_copy as *mut libc::winsize as _,
            )
        } < 0
        {
            return Err(OpenError::OpenptyFailed);
        }
        let close_both = || {
            // SAFETY: closing fds we own.
            unsafe {
                libc::close(master_fd);
                libc::close(slave_fd);
            }
        };

        // Set CLOEXEC on the master fd, only the slave fd should be inherited
        // by the child process (shell/command).
        'cloexec: {
            // SAFETY: plain fcntl.
            let flags = unsafe { libc::fcntl(master_fd, libc::F_GETFD) };
            if flags == -1 {
                log::warn!(target: LOG_TARGET, "error getting flags for master fd err=E{}", last_errno());
                break 'cloexec;
            }

            // SAFETY: plain fcntl.
            if unsafe { libc::fcntl(master_fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0 {
                log::warn!(target: LOG_TARGET, "error setting CLOEXEC on master fd err=E{}", last_errno());
            }
        }

        // Enable UTF-8 mode. I think this is on by default on Linux but it
        // is NOT on by default on macOS so we ensure that it is always set.
        // SAFETY: zeroed termios is a valid out value.
        let mut attrs: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: valid out pointer.
        if unsafe { libc::tcgetattr(master_fd, &mut attrs) } != 0 {
            close_both();
            return Err(OpenError::OpenptyFailed);
        }
        attrs.c_iflag |= libc::IUTF8;
        // SAFETY: valid termios pointer.
        if unsafe { libc::tcsetattr(master_fd, libc::TCSANOW, &attrs) } != 0 {
            close_both();
            return Err(OpenError::OpenptyFailed);
        }

        Ok(PosixPty {
            master: master_fd,
            slave: slave_fd,
            tty_name_buf: Box::new([0; MAX_PATH_BYTES + 1]),
            tty_name: None,
        })
    }

    /// Close the master side. The slave side is the caller's responsibility.
    pub fn deinit(&mut self) {
        // SAFETY: closing an fd we own.
        unsafe { libc::close(self.master) };
        self.master = -1;
    }

    pub fn get_mode(&self) -> Result<Mode, GetModeError> {
        // SAFETY: zeroed termios is a valid out value.
        let mut attrs: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: valid out pointer.
        if unsafe { libc::tcgetattr(self.master, &mut attrs) } != 0 {
            return Err(GetModeError::GetModeFailed);
        }

        Ok(Mode {
            canonical: (attrs.c_lflag & libc::ICANON) != 0,
            echo: (attrs.c_lflag & libc::ECHO) != 0,
        })
    }

    /// Return the size of the pty.
    pub fn get_size(&self) -> Result<Winsize, GetSizeError> {
        let mut ws = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: valid out pointer for TIOCGWINSZ.
        if unsafe { libc::ioctl(self.master, libc::TIOCGWINSZ, &mut ws) } < 0 {
            return Err(IoctlError::IoctlFailed);
        }

        Ok(Winsize {
            ws_row: ws.ws_row,
            ws_col: ws.ws_col,
            ws_xpixel: ws.ws_xpixel,
            ws_ypixel: ws.ws_ypixel,
        })
    }

    /// Set the size of the pty.
    pub fn set_size(&mut self, size: Winsize) -> Result<(), SetSizeError> {
        let ws = libc::winsize {
            ws_row: size.ws_row,
            ws_col: size.ws_col,
            ws_xpixel: size.ws_xpixel,
            ws_ypixel: size.ws_ypixel,
        };
        // SAFETY: valid pointer for TIOCSWINSZ.
        if unsafe { libc::ioctl(self.master, libc::TIOCSWINSZ, &ws) } < 0 {
            return Err(IoctlError::IoctlFailed);
        }
        Ok(())
    }

    /// This should be called prior to exec in the forked child process
    /// in order to setup the tty properly.
    ///
    /// Only async-signal-safe calls are made (this runs after fork).
    pub fn child_pre_exec(&self) -> Result<(), ChildPreExecError> {
        // Reset our signals
        // SAFETY: a zeroed sigaction with SIG_DFL and an empty mask is valid.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = 0;
            for sig in [
                libc::SIGABRT,
                libc::SIGALRM,
                libc::SIGBUS,
                libc::SIGCHLD,
                libc::SIGFPE,
                libc::SIGHUP,
                libc::SIGILL,
                libc::SIGINT,
                libc::SIGPIPE,
                libc::SIGSEGV,
                libc::SIGTRAP,
                libc::SIGTERM,
                libc::SIGQUIT,
            ] {
                libc::sigaction(sig, &sa, std::ptr::null_mut());
            }
        }

        // Create a new process group
        // SAFETY: plain syscall.
        if unsafe { libc::setsid() } < 0 {
            return Err(ChildPreExecError::ProcessGroupFailed);
        }

        // Set controlling terminal
        // SAFETY: TIOCSCTTY takes an int argument.
        if unsafe { libc::ioctl(self.slave, libc::TIOCSCTTY as _, 0 as libc::c_ulong) } < 0 {
            // NOTE: the Zig code logs the errno here; we don't, because we
            // are in the forked child and the logger may allocate or lock.
            return Err(ChildPreExecError::SetControllingTerminalFailed);
        }

        // Can close master/slave pair now
        // SAFETY: closing fds we own.
        unsafe {
            libc::close(self.slave);
            libc::close(self.master);
        }
        Ok(())
    }

    /// Get information about the process(es) attached to the PTY. Returns
    /// `None` if there was an error getting the information or the information
    /// is not available on a particular platform.
    pub fn get_process_info(&mut self, info: ProcessInfo) -> Option<ProcessInfoValue<'_>> {
        match info {
            ProcessInfo::ForegroundPid => {
                self.foreground_pid().map(ProcessInfoValue::ForegroundPid)
            }
            ProcessInfo::TtyName => self.tty_name().map(ProcessInfoValue::TtyName),
        }
    }

    /// `get_process_info(.foreground_pid)`.
    pub fn foreground_pid(&self) -> Option<u64> {
        // SAFETY: plain call.
        let rc = unsafe { libc::tcgetpgrp(self.master) };
        if rc < 0 {
            return None;
        }
        Some(rc as u64)
    }

    /// `get_process_info(.tty_name)`.
    pub fn tty_name(&mut self) -> Option<&str> {
        if self.tty_name.is_none() {
            self.tty_name = self.compute_tty_name();
        }
        let len = self.tty_name?;
        std::str::from_utf8(&self.tty_name_buf[..len]).ok()
    }

    #[cfg(target_os = "macos")]
    fn compute_tty_name(&mut self) -> Option<usize> {
        // The macOS TIOCPTYGNAME ioctl does not allow us to
        // specify the length of the buffer passed to it, but
        // expects it to be at least 128 bytes long.
        const TIOCPTYGNAME: libc::c_ulong = 0x40807453;
        debug_assert!(self.tty_name_buf.len() >= 128);
        // SAFETY: the buffer is at least 128 bytes.
        if unsafe { libc::ioctl(self.master, TIOCPTYGNAME, self.tty_name_buf.as_mut_ptr()) } != 0 {
            log::error!(target: LOG_TARGET, "error getting name of slave PTY errno={}", last_errno());
            return None;
        }
        CStr::from_bytes_until_nul(&self.tty_name_buf[..])
            .ok()
            .map(|s| s.to_bytes().len())
    }

    #[cfg(target_os = "linux")]
    fn compute_tty_name(&mut self) -> Option<usize> {
        let len = self.tty_name_buf.len();
        // SAFETY: the buffer is valid for `len` bytes.
        if unsafe { libc::ptsname_r(self.master, self.tty_name_buf.as_mut_ptr().cast(), len) } != 0
        {
            return None;
        }
        CStr::from_bytes_until_nul(&self.tty_name_buf[..])
            .ok()
            .map(|s| s.to_bytes().len())
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn compute_tty_name(&mut self) -> Option<usize> {
        None
    }
}

/// Windows PTY creation and management.
#[cfg(windows)]
pub struct WindowsPty {
    pub out_pipe: crate::os::windows::HANDLE,
    pub in_pipe: crate::os::windows::HANDLE,
    pub out_pipe_pty: crate::os::windows::HANDLE,
    pub in_pipe_pty: crate::os::windows::HANDLE,
    pub pseudo_console: crate::os::windows::HPCON,
    pub size: Winsize,
}

#[cfg(windows)]
simple_error!(
    /// Zig: `WindowsPty.OpenError`.
    WindowsOpenError { Unexpected }
);

#[cfg(windows)]
simple_error!(
    /// Zig: `WindowsPty.SetSizeError`.
    WindowsSetSizeError { ResizeFailed }
);

#[cfg(windows)]
impl WindowsPty {
    /// Open a new PTY with the given initial size.
    pub fn open(size: Winsize) -> Result<WindowsPty, WindowsOpenError> {
        use crate::os::windows::{self as windows, exp::kernel32};
        use std::sync::atomic::{AtomicU32, Ordering};

        // Process-wide counter for pipe names
        static PIPE_NAME_COUNTER: AtomicU32 = AtomicU32::new(1);

        let unexpected = |err| {
            let _ = windows::unexpected_error(err);
            WindowsOpenError::Unexpected
        };

        let pipe_path = format!(
            "\\\\.\\pipe\\LOCAL\\ghostty-pty-{}-{}",
            windows::get_current_process_id(),
            PIPE_NAME_COUNTER.fetch_add(1, Ordering::Relaxed),
        );
        let pipe_path_w: Vec<u16> = pipe_path.encode_utf16().chain(std::iter::once(0)).collect();

        let security_attributes = windows::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows::SECURITY_ATTRIBUTES>() as u32,
            bInheritHandle: windows::FALSE,
            lpSecurityDescriptor: std::ptr::null_mut(),
        };

        // SAFETY: Win32 calls with valid arguments; handles are closed on
        // every error path below.
        unsafe {
            let in_pipe = kernel32::CreateNamedPipeW(
                pipe_path_w.as_ptr(),
                windows::PIPE_ACCESS_OUTBOUND
                    | windows::FILE_FLAG_FIRST_PIPE_INSTANCE
                    | windows::FILE_FLAG_OVERLAPPED,
                windows::PIPE_TYPE_BYTE,
                1,
                4096,
                4096,
                0,
                &security_attributes,
            );
            if in_pipe == windows::INVALID_HANDLE_VALUE {
                return Err(unexpected(windows::get_last_error()));
            }

            let mut security_attributes_read = security_attributes;
            let in_pipe_pty = kernel32::CreateFileW(
                pipe_path_w.as_ptr(),
                windows::GENERIC_READ,
                0,
                &mut security_attributes_read,
                windows::OPEN_EXISTING,
                windows::FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            );
            if in_pipe_pty == windows::INVALID_HANDLE_VALUE {
                let err = windows::get_last_error();
                kernel32::CloseHandle(in_pipe);
                return Err(unexpected(err));
            }

            // The in_pipe needs to be created as a named pipe, since anonymous
            // pipes created with CreatePipe do not support overlapped operations,
            // and the IOCP backend of libxev only uses overlapped operations on files.
            //
            // It would be ideal to use CreatePipe here, so that our pipe isn't
            // visible to any other processes.

            let mut out_pipe: windows::HANDLE = std::ptr::null_mut();
            let mut out_pipe_pty: windows::HANDLE = std::ptr::null_mut();
            if kernel32::CreatePipe(&mut out_pipe, &mut out_pipe_pty, std::ptr::null(), 0)
                == windows::FALSE
            {
                let err = windows::get_last_error();
                kernel32::CloseHandle(in_pipe_pty);
                kernel32::CloseHandle(in_pipe);
                return Err(unexpected(err));
            }

            let close_all = || {
                kernel32::CloseHandle(out_pipe);
                kernel32::CloseHandle(out_pipe_pty);
                kernel32::CloseHandle(in_pipe_pty);
                kernel32::CloseHandle(in_pipe);
            };

            for h in [in_pipe, in_pipe_pty, out_pipe, out_pipe_pty] {
                if kernel32::SetHandleInformation(h, windows::HANDLE_FLAG_INHERIT, 0)
                    == windows::FALSE
                {
                    let err = windows::get_last_error();
                    close_all();
                    return Err(unexpected(err));
                }
            }

            let mut pseudo_console: windows::HPCON = std::ptr::null_mut();
            let result = kernel32::CreatePseudoConsole(
                windows::COORD {
                    X: size.ws_col as i16,
                    Y: size.ws_row as i16,
                },
                in_pipe_pty,
                out_pipe_pty,
                0,
                &mut pseudo_console,
            );
            if result != windows::S_OK {
                close_all();
                return Err(WindowsOpenError::Unexpected);
            }

            Ok(WindowsPty {
                out_pipe,
                in_pipe,
                out_pipe_pty,
                in_pipe_pty,
                pseudo_console,
                size,
            })
        }
    }

    pub fn deinit(&mut self) {
        use crate::os::windows::exp::kernel32;
        // SAFETY: closing handles we own.
        unsafe {
            kernel32::CloseHandle(self.in_pipe_pty);
            kernel32::CloseHandle(self.in_pipe);
            kernel32::CloseHandle(self.out_pipe_pty);
            kernel32::CloseHandle(self.out_pipe);
            kernel32::ClosePseudoConsole(self.pseudo_console);
        }
    }

    /// Return the size of the pty.
    pub fn get_size(&self) -> Result<Winsize, std::convert::Infallible> {
        Ok(self.size)
    }

    /// Set the size of the pty.
    pub fn set_size(&mut self, size: Winsize) -> Result<(), WindowsSetSizeError> {
        use crate::os::windows::{self as windows, exp::kernel32};
        // SAFETY: the pseudo console handle is valid.
        let result = unsafe {
            kernel32::ResizePseudoConsole(
                self.pseudo_console,
                windows::COORD {
                    X: size.ws_col as i16,
                    Y: size.ws_row as i16,
                },
            )
        };

        if result != windows::S_OK {
            return Err(WindowsSetSizeError::ResizeFailed);
        }
        self.size = size;
        Ok(())
    }

    /// Get information about the process(es) attached to the PTY. Returns
    /// `None` if there was an error getting the information or the information
    /// is not available on a particular platform.
    pub fn get_process_info(&mut self, _info: ProcessInfo) -> Option<ProcessInfoValue<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty() {
        let mut ws = Winsize {
            ws_row: 50,
            ws_col: 80,
            ws_xpixel: 1,
            ws_ypixel: 1,
        };

        let mut pty = Pty::open(ws).unwrap();

        // Initialize size should match what we gave it
        assert_eq!(ws, pty.get_size().unwrap());

        // Can set and read new sizes
        ws.ws_row *= 2;
        pty.set_size(ws).unwrap();
        assert_eq!(ws, pty.get_size().unwrap());

        if cfg!(any(target_os = "freebsd", target_os = "macos")) {
            let Some(ProcessInfoValue::TtyName(name)) = pty.get_process_info(ProcessInfo::TtyName)
            else {
                panic!("no tty name");
            };
            assert!(name.starts_with("/dev/"));
        } else if cfg!(target_os = "linux") {
            let Some(ProcessInfoValue::TtyName(name)) = pty.get_process_info(ProcessInfo::TtyName)
            else {
                panic!("no tty name");
            };
            assert!(name.starts_with("/dev/pts/"));
        } else {
            assert!(pty.get_process_info(ProcessInfo::TtyName).is_none());
        }

        #[cfg(unix)]
        // SAFETY: closing the slave fd we own.
        unsafe {
            libc::close(pty.slave)
        };
        pty.deinit();
    }

    #[cfg(unix)]
    #[test]
    fn pty_mode_defaults() {
        let mut pty = Pty::open(Winsize::default()).unwrap();
        // A fresh pty is in canonical mode with echo, like Mode's defaults.
        assert_eq!(pty.get_mode().unwrap(), Mode::default());
        // Nothing controls this pty, so this must not crash (it may or may
        // not report a process group depending on the platform).
        let _ = pty.get_process_info(ProcessInfo::ForegroundPid);
        // SAFETY: closing the slave fd we own.
        unsafe { libc::close(pty.slave) };
        pty.deinit();
    }
}
