//! Port of `test/esctest/src/main.zig`.
//!
//! Runs esctest (https://github.com/ThomasDickey/esctest2) against
//! libghostty-vt, with no GUI in the way.
//!
//! `run.sh` installs esctest in share/esctest beside the bin directory
//! holding this executable. It runs under a pty. Everything it writes is
//! fed through a libghostty-vt terminal, and the terminal's replies
//! (DECRQCRA checksums, device attributes, cursor position and size
//! reports, and so on) are written back to it. When esctest exits, its
//! log is copied to stdout.

use std::ffi::CString;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use ghostty::terminal::device_attributes::{
    Attributes, ConformanceLevel, DeviceType, Feature, Primary, Secondary,
};
use ghostty::terminal::size_report;
use ghostty::terminal::stream_terminal::{Handler, ResizeWindow};
use ghostty::terminal::terminal::{Options, Resize};
use ghostty::terminal::xt_checksum;
use ghostty::terminal::Terminal;

/// The size esctest resets the terminal to before every test, and so the
/// size it starts at.
const ROWS: u16 = 25;
const COLS: u16 = 80;

/// The xterm checksum calculation esctest expects with --xterm-checksum
/// 334 and later, which is xterm's `checksumExtension: 23`.
const XT_CHECKSUM: xt_checksum::Flags = xt_checksum::Flags {
    positive: true,
    no_attributes: true,
    no_trim: true,
    undrawn: false,
    full: true,
};

/// Arguments always passed to esctest, before the user's own so that
/// theirs win. esctest only knows how to check a terminal that answers
/// like some real one, and xterm is the closest to libghostty-vt.
const ESCTEST_ARGS: &[&str] = &[
    "--expected-terminal=xterm",
    "--xterm-checksum=411",
    "--xterm-reverse-wrap=411",
    "--timeout=0.2",
];

const USAGE: &str = "Usage: esctest-libghostty [--verbose] [esctest arguments...]

Arguments are passed to esctest, for example --include=DECSTR.
The log goes to stdout.

--verbose shows libghostty-vt's own log on stderr, which names each
sequence it ignores.
";

/// Whether to show libghostty-vt's log. Without --verbose there is a
/// warning for every sequence esctest sends that isn't implemented,
/// which buries everything else.
static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Zig's `logFn`: errors always, everything else only with --verbose.
struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        VERBOSE.load(Ordering::Relaxed) || metadata.level() == log::Level::Error
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "{}({}): {}",
                record.level().as_str().to_lowercase(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

/// The pty master. Effects only get the handler, so this is how replies
/// reach esctest.
static PTY: AtomicI32 = AtomicI32::new(-1);

fn main() -> ExitCode {
    match run_main() {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

fn run_main() -> io::Result<u8> {
    log::set_logger(&Logger).expect("no other logger is installed");
    log::set_max_level(log::LevelFilter::Info);

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--verbose") {
        VERBOSE.store(true, Ordering::Relaxed);
        args.remove(0);
    }
    if args.first().is_some_and(|a| a == "--help") {
        eprint!("{USAGE}");
        return Ok(1);
    }

    // Without this check, a missing python3 only shows up as esctest
    // writing no log, since the child's own error goes to the pty.
    if !on_path(&std::env::var_os("PATH").unwrap_or_default(), "python3") {
        eprintln!("warning: python3 was not found on PATH, and esctest needs it");
        return Ok(1);
    }

    let exe = std::env::current_exe()?;
    let exe_dir = exe.parent().expect("the executable is in a directory");
    let esctest_dir = exe_dir.join("..").join("share").join("esctest");
    let esctest_dir_c = CString::new(esctest_dir.as_os_str().as_bytes()).expect("no NUL in path");

    // esctest writes its log to a file rather than the terminal, so we
    // give it one to copy from afterwards.
    let log_path = format!("/tmp/esctest-libghostty-{}.log", std::process::id());
    let _remove_log = RemoveOnDrop(&log_path);

    let mut argv: Vec<CString> = vec![c"python3".into(), c"esctest.py".into()];
    for arg in ESCTEST_ARGS {
        argv.push(CString::new(*arg).unwrap());
    }
    argv.push(CString::new(format!("--logfile={log_path}")).unwrap());
    for arg in &args {
        argv.push(CString::new(arg.as_str()).map_err(|_| io::ErrorKind::InvalidInput)?);
    }
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());

    let ws = libc::winsize { ws_row: ROWS, ws_col: COLS, ws_xpixel: 0, ws_ypixel: 0 };
    let mut pty: libc::c_int = -1;
    // SAFETY: forkpty with valid out pointers; the child only calls
    // async-signal-safe functions (chdir, execvp, write, _exit) before exec.
    let pid = unsafe { libc::forkpty(&mut pty, std::ptr::null_mut(), std::ptr::null(), &ws) };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        unsafe {
            if libc::chdir(esctest_dir_c.as_ptr()) != 0 {
                let msg = b"cannot change to the esctest directory\n";
                libc::write(2, msg.as_ptr().cast(), msg.len());
                libc::_exit(127);
            }
            libc::execvp(c"python3".as_ptr(), argv_ptrs.as_ptr());
            let msg = b"cannot run python3\n";
            libc::write(2, msg.as_ptr().cast(), msg.len());
            libc::_exit(127);
        }
    }
    PTY.store(pty, Ordering::Relaxed);

    run()?;

    let mut status: libc::c_int = 0;
    // SAFETY: waiting for our own child.
    unsafe { libc::waitpid(pid, &mut status, 0) };
    let exit_status = libc::WEXITSTATUS(status);

    // The child's own message about this went to the pty, not to us.
    if exit_status == 127 {
        eprintln!(
            "warning: couldn't start esctest with python3 in {}",
            esctest_dir.display()
        );
        return Ok(1);
    }

    copy_log(&log_path)?;
    Ok(exit_status as u8)
}

/// Deletes the log file when the run ends (Zig: `defer unlink`).
struct RemoveOnDrop<'a>(&'a str);

impl Drop for RemoveOnDrop<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

/// Whether an executable with this name is in one of the directories of
/// a PATH value.
fn on_path(path: &std::ffi::OsStr, name: &str) -> bool {
    std::env::split_paths(path).filter(|dir| !dir.as_os_str().is_empty()).any(|dir| {
        let candidate = CString::new(dir.join(name).as_os_str().as_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path.
        unsafe { libc::access(candidate.as_ptr(), libc::X_OK) == 0 }
    })
}

/// Feed esctest's output through the terminal until it exits.
fn run() -> io::Result<()> {
    let mut t = Terminal::init(Options {
        default_xt_checksum: XT_CHECKSUM,
        ..Options::new(COLS, ROWS)
    })
    .map_err(|err| io::Error::other(format!("terminal init failed: {err:?}")))?;

    let mut stream = t.vt_stream();
    stream.handler.xt_checksum_report = true;
    stream.handler.effects.write_pty = Some(write_pty);
    stream.handler.effects.size = Some(size);
    stream.handler.effects.device_attributes = Some(device_attributes);
    stream.handler.effects.resize_window = Some(resize_window);

    let pty = PTY.load(Ordering::Relaxed);
    let mut buf = [0u8; 4096];
    loop {
        // SAFETY: reading into a local buffer of the given length.
        let n = unsafe { libc::read(pty, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            stream.next_slice(&buf[..n as usize]);
            continue;
        }

        // Linux reports the other end closing as EIO rather than EOF.
        if n == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EIO) => return Ok(()),
            _ => {
                eprintln!("reading the pty failed: {err}");
                return Err(err);
            }
        }
    }
}

fn write_pty(_: &mut Handler<'_>, data: &[u8]) {
    let pty = PTY.load(Ordering::Relaxed);
    let mut rest = data;
    while !rest.is_empty() {
        // SAFETY: writing from a valid slice.
        let n = unsafe { libc::write(pty, rest.as_ptr().cast(), rest.len()) };
        if n < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return;
        }
        rest = &rest[n as usize..];
    }
}

fn size(h: &mut Handler<'_>) -> Option<size_report::Size> {
    Some(size_report::Size {
        rows: h.terminal.rows,
        columns: h.terminal.cols,
        cell_width: 10,
        cell_height: 20,
    })
}

/// Resize as xterm does when esctest asks with CSI 8 t, keeping a
/// dimension it gives as zero, and tell esctest's side of the pty.
fn resize_window(h: &mut Handler<'_>, req: ResizeWindow) {
    // The enum is non-exhaustive; grid sizes are the only kind today.
    let ResizeWindow::Grid(grid) = req else { return };
    let t = &mut *h.terminal;
    let new_rows = if grid.rows == 0 { t.rows } else { grid.rows };
    let new_cols = if grid.columns == 0 { t.cols } else { grid.columns };
    if let Err(err) = t.resize(Resize { cols: new_cols, rows: new_rows, cell_size_px: None }) {
        log::error!("resizing to {new_cols}x{new_rows} failed: {err:?}");
        return;
    }

    let ws = libc::winsize { ws_row: new_rows, ws_col: new_cols, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCSWINSZ on the pty master with a valid winsize.
    unsafe { libc::ioctl(PTY.load(Ordering::Relaxed), libc::TIOCSWINSZ, &ws) };
}

/// Answer as a VT520, the terminal esctest's highest VT level tests.
fn device_attributes(_: &mut Handler<'_>) -> Attributes {
    Attributes {
        primary: Primary {
            conformance_level: ConformanceLevel::LEVEL_5,
            features: &[Feature::ANSI_COLOR],
        },
        secondary: Secondary { device_type: DeviceType::VT520, ..Default::default() },
        ..Default::default()
    }
}

fn copy_log(path: &str) -> io::Result<()> {
    let mut file = match std::fs::File::open(Path::new(path)) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("esctest wrote no log to {path}: {err}");
            return Ok(());
        }
    };
    let mut stdout = io::stdout().lock();
    io::copy(&mut file, &mut stdout)?;
    stdout.flush()
}
