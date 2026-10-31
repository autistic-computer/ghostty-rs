//! Port of `src/lib/TinyIo.zig`.
//!
//! STATUS: ported
//!
//! TinyIo: a tiny, blocking I/O implementation optimized for binary size.
//!
//! In Zig this is a `std.Io` vtable implementation: compared to
//! `std.Io.Threaded`, it is much smaller in binary size and runtime
//! footprint because Threaded's vtable architecture prevents the linker from
//! pruning uncalled functions and Threaded requires 256KB of TLS.
//!
//! TinyIo implements exactly the operations we need as plain blocking
//! syscalls. It doesn't support cancelation. It doesn't support concurrency
//! operations, but all our APIs are direct syscalls so it supports the
//! concurrency of calls that those syscalls support (which is usually
//! safe on POSIX systems).
//!
//! The Rust port has no `std.Io` interface to plug into, so [`TinyIo`] is a
//! zero-sized handle whose methods are the implemented vtable entries
//! (`dir_open_file`, `file_read_positional`, `futex_wait`, ...), together
//! with the `std.Io.File`/`std.Io.Dir` handle types ([`File`], [`Dir`]) and
//! the `std.Io.Mutex` futex mutex ([`Mutex`]). Vtable entries that are
//! "failing"/"no" stubs in Zig are not represented, except the few that the
//! shared tests exercise.
//!
//! The platform arms live in `tinyio/`: `posix.rs` is direct syscalls
//! cribbed from Zig's std.posix, and `windows.rs` is direct ntdll and
//! kernel32 calls modeled on the Windows arms of `std.Io.Threaded`. Shared
//! tests are in `tinyio/test.rs`.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "tinyio/posix.rs"]
pub mod posix;
#[path = "tinyio/test.rs"]
pub mod test;
#[path = "tinyio/windows.rs"]
pub mod windows;
// @generated-mods-end

use std::fmt;
use std::io;
use std::sync::atomic::{AtomicU32, Ordering};

pub use crate::lib::compat::thread::Timeout;

#[cfg(unix)]
use self::posix as imp;
#[cfg(windows)]
use self::windows as imp;

/// The native handle type (a file descriptor on POSIX, a `HANDLE` on
/// Windows).
#[cfg(unix)]
pub type Handle = std::os::fd::RawFd;
#[cfg(windows)]
pub type Handle = *mut std::ffi::c_void;
#[cfg(not(any(unix, windows)))]
pub type Handle = i32;

/// Zig: `std.Io.Threaded.max_iovecs_len`.
pub const MAX_IOVECS_LEN: usize = 8;

/// True if this platform has a real TinyIo implementation. On
/// unsupported platforms every operation fails like `std.Io.failing`.
pub const SUPPORTED: bool = cfg!(any(unix, windows));

/// Errors from TinyIo operations (the union of the Zig error sets used by
/// the implemented vtable entries).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    AccessDenied,
    AntivirusInterference,
    BadPathName,
    ConcurrencyUnavailable,
    ConnectionResetByPeer,
    DeviceBusy,
    EndOfStream,
    EntropyUnavailable,
    FileBusy,
    FileLocksUnsupported,
    FileNotFound,
    FileSystem,
    FileTooBig,
    InputOutput,
    IsDir,
    LockViolation,
    NameTooLong,
    NetworkNotFound,
    NoDevice,
    NoSpaceLeft,
    NotDir,
    NotOpenForReading,
    OperationUnsupported,
    PathAlreadyExists,
    PermissionDenied,
    PipeBusy,
    ProcessFdQuotaExceeded,
    ReadOnlyFileSystem,
    SocketUnconnected,
    Streaming,
    SymLinkLoop,
    SystemFdQuotaExceeded,
    SystemResources,
    Unexpected,
    Unseekable,
    WouldBlock,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

impl From<Error> for io::Error {
    fn from(e: Error) -> io::Error {
        let kind = match e {
            Error::FileNotFound => io::ErrorKind::NotFound,
            Error::AccessDenied | Error::PermissionDenied => io::ErrorKind::PermissionDenied,
            Error::EndOfStream => io::ErrorKind::UnexpectedEof,
            Error::WouldBlock => io::ErrorKind::WouldBlock,
            Error::PathAlreadyExists => io::ErrorKind::AlreadyExists,
            _ => io::ErrorKind::Other,
        };
        io::Error::new(kind, e)
    }
}

/// Zig: `std.Io.File.Kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    BlockDevice,
    CharacterDevice,
    Directory,
    NamedPipe,
    SymLink,
    File,
    UnixDomainSocket,
    Whiteout,
    Door,
    EventPort,
    Unknown,
}

/// Zig: `std.Io.File.Stat`. Times are nanoseconds since the epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub inode: u64,
    pub size: u64,
    /// Permission bits (`mode & 0o7777` on POSIX).
    pub permissions: u32,
    pub kind: FileKind,
    pub atime: i128,
    pub mtime: i128,
    pub ctime: i128,
    pub nlink: u64,
    pub block_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileFlags {
    pub nonblocking: bool,
}

/// Zig: `std.Io.File`. A plain handle; it is not closed on drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct File {
    pub handle: Handle,
    pub flags: FileFlags,
}

/// Zig: `std.Io.Dir`. A plain handle; it is not closed on drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dir {
    pub handle: Handle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenMode {
    #[default]
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lock {
    #[default]
    None,
    Shared,
    Exclusive,
}

/// Zig: `std.Io.Dir.OpenFileOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFileOptions {
    pub mode: OpenMode,
    pub follow_symlinks: bool,
    pub allow_ctty: bool,
    pub path_only: bool,
    pub resolve_beneath: bool,
    pub lock: Lock,
    pub allow_directory: bool,
}

impl Default for OpenFileOptions {
    fn default() -> Self {
        OpenFileOptions {
            mode: OpenMode::ReadOnly,
            follow_symlinks: true,
            allow_ctty: false,
            path_only: false,
            resolve_beneath: false,
            lock: Lock::None,
            allow_directory: true,
        }
    }
}

impl OpenFileOptions {
    pub fn is_read(&self) -> bool {
        self.mode != OpenMode::WriteOnly
    }

    pub fn is_write(&self) -> bool {
        self.mode != OpenMode::ReadOnly
    }
}

/// Zig: `std.Io.CancelProtection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelProtection {
    Unblocked,
    Blocked,
}

/// Zig: `std.Io.Operation`, restricted to the variants the tests use.
pub enum Operation<'a, 'b> {
    FileReadStreaming {
        file: File,
        data: &'a mut [&'b mut [u8]],
    },
    FileWriteStreaming {
        file: File,
        data: &'a [&'b [u8]],
    },
}

/// Zig: `std.Io.Operation.Result`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationResult {
    FileReadStreaming(Result<usize, Error>),
    FileWriteStreaming(Result<usize, Error>),
}

/// The result of [`TinyIo::async_`]. `noAsync` runs the task synchronously,
/// so the value is already available.
pub struct Future<T> {
    result: T,
}

impl<T> Future<T> {
    pub fn await_(self, _io: TinyIo) -> T {
        self.result
    }
}

/// A tiny, blocking I/O implementation. Stateless (zero-sized).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TinyIo;

impl TinyIo {
    /// Initialize a TinyIo. TinyIo is stateless (zero-sized), so this
    /// exists purely to mirror the `std.Io.Threaded` initialization shape.
    pub const INIT: TinyIo = TinyIo;

    /// Returns the I/O interface for this implementation (itself).
    pub fn io(self) -> TinyIo {
        self
    }

    // `noAsync` runs the task synchronously and returns its result, which
    // means `await`/`cancel` can never block. `concurrent` reports that
    // concurrency is unavailable, which callers must handle anyway.

    pub fn async_<T>(self, f: impl FnOnce() -> T) -> Future<T> {
        Future { result: f() }
    }

    pub fn concurrent<T>(self, _f: impl FnOnce() -> T) -> Result<Future<T>, Error> {
        Err(Error::ConcurrencyUnavailable)
    }

    // Cancelation is never requested (there is no async), but these are
    // benign no-ops instead of `unreachable` since generic code may
    // toggle cancel protection around operations.

    pub fn recancel(self) {}

    pub fn swap_cancel_protection(self, new: CancelProtection) -> CancelProtection {
        // Stateless: there is no async, so cancelation can never be
        // requested and the protection state is meaningless. Callers save
        // this return value only to restore it via another call.
        let _ = new;
        CancelProtection::Blocked
    }

    pub fn check_cancel(self) -> Result<(), Error> {
        Ok(())
    }

    /// Zig: `Io.noRandom`. There is no non-secure random source; this
    /// fills with zeros. Use [`TinyIo::random_secure`].
    pub fn random(self, buffer: &mut [u8]) {
        buffer.fill(0);
    }

    pub fn random_secure(self, buffer: &mut [u8]) -> Result<(), Error> {
        if buffer.is_empty() {
            return Ok(());
        }
        #[cfg(any(unix, windows))]
        return imp::random_secure(buffer);
        #[allow(unreachable_code)]
        Err(Error::EntropyUnavailable)
    }

    pub fn operate(self, operation: Operation<'_, '_>) -> OperationResult {
        match operation {
            // Streaming file reads are how `File.Reader` consumes files
            // that don't support positional reads (and the fallback path
            // in general).
            Operation::FileReadStreaming { file, data } => {
                OperationResult::FileReadStreaming(self.file_read_streaming(file, data))
            }

            // Everything else (streaming writes, ioctls, socket receives)
            // is unused by the terminal; fail like `Io.failing` does.
            Operation::FileWriteStreaming { .. } => {
                OperationResult::FileWriteStreaming(Err(Error::InputOutput))
            }
        }
    }

    pub fn futex_wait(self, ptr: &AtomicU32, expected: u32, timeout: Timeout) -> Result<(), Error> {
        #[cfg(any(unix, windows))]
        imp::futex_wait_inner(ptr, expected, timeout_to_ns(timeout));
        Ok(())
    }

    pub fn futex_wait_uncancelable(self, ptr: &AtomicU32, expected: u32) {
        #[cfg(any(unix, windows))]
        imp::futex_wait_inner(ptr, expected, None);
    }

    pub fn futex_wake(self, ptr: &AtomicU32, max_waiters: u32) {
        #[cfg(any(unix, windows))]
        imp::futex_wake(ptr, max_waiters);
    }

    pub fn dir_open_file(
        self,
        dir: Dir,
        sub_path: &[u8],
        options: OpenFileOptions,
    ) -> Result<File, Error> {
        #[cfg(any(unix, windows))]
        return imp::dir_open_file(dir, sub_path, options);
        #[allow(unreachable_code)]
        Err(Error::FileNotFound)
    }

    /// Zig: `Io.failingDirCreateFile`. TinyIo can't create files.
    pub fn dir_create_file(self, _dir: Dir, _sub_path: &[u8]) -> Result<File, Error> {
        Err(Error::NoSpaceLeft)
    }

    pub fn dir_close(self, dirs: &[Dir]) {
        #[cfg(any(unix, windows))]
        imp::dir_close(dirs);
    }

    pub fn dir_real_path_file(
        self,
        dir: Dir,
        sub_path: &[u8],
        out_buffer: &mut [u8],
    ) -> Result<usize, Error> {
        #[cfg(any(unix, windows))]
        return imp::dir_real_path_file(dir, sub_path, out_buffer);
        #[allow(unreachable_code)]
        Err(Error::OperationUnsupported)
    }

    pub fn dir_delete_file(self, dir: Dir, sub_path: &[u8]) -> Result<(), Error> {
        #[cfg(any(unix, windows))]
        return imp::dir_delete_file(dir, sub_path);
        #[allow(unreachable_code)]
        Err(Error::FileNotFound)
    }

    pub fn file_stat(self, file: File) -> Result<Stat, Error> {
        #[cfg(any(unix, windows))]
        return imp::file_stat(file);
        #[allow(unreachable_code)]
        Err(Error::Streaming)
    }

    pub fn file_length(self, file: File) -> Result<u64, Error> {
        #[cfg(any(unix, windows))]
        return imp::file_length(file);
        #[allow(unreachable_code)]
        Err(Error::Streaming)
    }

    pub fn file_close(self, files: &[File]) {
        #[cfg(any(unix, windows))]
        imp::file_close(files);
    }

    /// Returns 0 if reading at or past the end.
    pub fn file_read_positional(
        self,
        file: File,
        data: &mut [&mut [u8]],
        offset: u64,
    ) -> Result<usize, Error> {
        #[cfg(any(unix, windows))]
        return imp::file_read_positional(file, data, offset);
        #[allow(unreachable_code)]
        Err(Error::Unseekable)
    }

    /// Returns `EndOfStream` at the end of the stream.
    pub fn file_read_streaming(self, file: File, data: &mut [&mut [u8]]) -> Result<usize, Error> {
        #[cfg(any(unix, windows))]
        return imp::file_read_streaming(file, data);
        #[allow(unreachable_code)]
        Err(Error::InputOutput)
    }

    pub fn file_seek_by(self, file: File, offset: i64) -> Result<(), Error> {
        #[cfg(any(unix, windows))]
        return imp::file_seek_by(file, offset);
        #[allow(unreachable_code)]
        Err(Error::Unseekable)
    }

    pub fn file_seek_to(self, file: File, offset: u64) -> Result<(), Error> {
        #[cfg(any(unix, windows))]
        return imp::file_seek_to(file, offset);
        #[allow(unreachable_code)]
        Err(Error::Unseekable)
    }

    pub fn file_real_path(self, file: File, out_buffer: &mut [u8]) -> Result<usize, Error> {
        #[cfg(any(unix, windows))]
        return imp::file_real_path(file, out_buffer);
        #[allow(unreachable_code)]
        Err(Error::OperationUnsupported)
    }
}

/// Convert a timeout to relative nanoseconds without consulting a clock.
/// Deadlines can't be resolved without `now` support; treat them as a short
/// poll, which is valid because futex waits are allowed to wake spuriously
/// (callers must re-check their condition and retry).
fn timeout_to_ns(timeout: Timeout) -> Option<u64> {
    match timeout {
        Timeout::None => None,
        Timeout::Duration(d) => Some(d.as_nanos().min(u64::MAX as u128) as u64),
        Timeout::Deadline(_) => Some(10 * 1_000_000),
    }
}

impl Dir {
    /// The current working directory.
    #[cfg(unix)]
    pub fn cwd() -> Dir {
        Dir {
            handle: libc::AT_FDCWD,
        }
    }

    /// The current working directory (the PEB's current directory handle).
    #[cfg(windows)]
    pub fn cwd() -> Dir {
        Dir {
            handle: windows::cwd_handle(),
        }
    }

    pub fn open_file(
        self,
        io: TinyIo,
        sub_path: &[u8],
        options: OpenFileOptions,
    ) -> Result<File, Error> {
        io.dir_open_file(self, sub_path, options)
    }

    pub fn create_file(self, io: TinyIo, sub_path: &[u8]) -> Result<File, Error> {
        io.dir_create_file(self, sub_path)
    }

    pub fn real_path_file(
        self,
        io: TinyIo,
        sub_path: &[u8],
        out_buffer: &mut [u8],
    ) -> Result<usize, Error> {
        io.dir_real_path_file(self, sub_path, out_buffer)
    }

    pub fn delete_file(self, io: TinyIo, sub_path: &[u8]) -> Result<(), Error> {
        io.dir_delete_file(self, sub_path)
    }

    pub fn close(self, io: TinyIo) {
        io.dir_close(&[self]);
    }
}

impl File {
    pub fn close(self, io: TinyIo) {
        io.file_close(&[self]);
    }

    pub fn stat(self, io: TinyIo) -> Result<Stat, Error> {
        io.file_stat(self)
    }

    pub fn length(self, io: TinyIo) -> Result<u64, Error> {
        io.file_length(self)
    }

    pub fn read_streaming(self, io: TinyIo, data: &mut [&mut [u8]]) -> Result<usize, Error> {
        io.operate(Operation::FileReadStreaming { file: self, data })
            .into_read()
    }

    pub fn real_path(self, io: TinyIo, out_buffer: &mut [u8]) -> Result<usize, Error> {
        io.file_real_path(self, out_buffer)
    }

    /// A reader over the file (Zig: `File.reader`). It uses positional
    /// reads and falls back to streaming reads for unseekable files.
    pub fn reader(self, io: TinyIo) -> FileReader {
        FileReader {
            io,
            file: self,
            pos: 0,
            mode: ReaderMode::Positional,
        }
    }
}

impl OperationResult {
    fn into_read(self) -> Result<usize, Error> {
        match self {
            OperationResult::FileReadStreaming(r) => r,
            OperationResult::FileWriteStreaming(_) => unreachable!(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReaderMode {
    Positional,
    Streaming,
}

/// Zig: `std.Io.File.Reader` (unbuffered; wrap in `std::io::BufReader`
/// for buffering).
pub struct FileReader {
    io: TinyIo,
    file: File,
    pos: u64,
    mode: ReaderMode,
}

impl io::Read for FileReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.mode {
                ReaderMode::Positional => {
                    match self
                        .io
                        .file_read_positional(self.file, &mut [&mut *buf], self.pos)
                    {
                        Ok(n) => {
                            self.pos += n as u64;
                            return Ok(n);
                        }
                        Err(Error::Unseekable) => {
                            self.mode = ReaderMode::Streaming;
                            continue;
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                ReaderMode::Streaming => {
                    return match self.file.read_streaming(self.io, &mut [&mut *buf]) {
                        Ok(n) => Ok(n),
                        Err(Error::EndOfStream) => Ok(0),
                        Err(e) => Err(e.into()),
                    };
                }
            }
        }
    }
}

/// Port of `std.Io.Mutex`: a three-state futex mutex that parks on
/// [`TinyIo::futex_wait_uncancelable`] when contended. Used by kitty
/// graphics storage in Zig.
#[derive(Debug, Default)]
pub struct Mutex {
    state: AtomicU32,
}

const UNLOCKED: u32 = 0;
const LOCKED_ONCE: u32 = 1;
const CONTENDED: u32 = 2;

impl Mutex {
    pub const INIT: Mutex = Mutex {
        state: AtomicU32::new(UNLOCKED),
    };

    pub const fn new() -> Mutex {
        Mutex::INIT
    }

    pub fn try_lock(&self) -> bool {
        self.state
            .compare_exchange(UNLOCKED, LOCKED_ONCE, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    pub fn lock_uncancelable(&self, io: TinyIo) {
        if self.try_lock() {
            return;
        }
        self.lock_slow(io);
    }

    #[cold]
    fn lock_slow(&self, io: TinyIo) {
        if self.state.swap(CONTENDED, Ordering::Acquire) == UNLOCKED {
            return;
        }
        while self.state.swap(CONTENDED, Ordering::Acquire) != UNLOCKED {
            io.futex_wait_uncancelable(&self.state, CONTENDED);
        }
    }

    pub fn unlock(&self, io: TinyIo) {
        match self.state.swap(UNLOCKED, Ordering::Release) {
            UNLOCKED => unreachable!("unlock of unlocked mutex"),
            LOCKED_ONCE => {}
            _ => io.futex_wake(&self.state, 1),
        }
    }
}
