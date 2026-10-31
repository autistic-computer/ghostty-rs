//! Port of libxev `src/posix.zig` (the subset used by the epoll backend):
//! thin wrappers over `read(2)`, `write(2)`, `close(2)` and `dup(2)` that map
//! `errno` to [`Error`] variants, plus the shared error type of this port.

use std::fmt;
use std::os::fd::RawFd;

/// The file descriptor type (Zig: `posix.fd_t`).
pub type Fd = RawFd;

/// All errors that the xev operations of this port can report.
///
/// Zig has distinct (but overlapping) error *sets* per operation
/// (`ReadError`, `WriteError`, `PollError`, `CancelError`, ...). Rust has no
/// open error-set unions, so this port uses one enum that is the union of
/// every variant those sets can hold, and the per-operation names are type
/// aliases of it (see [`ReadError`], [`WriteError`], ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    // ThreadPoolError
    ThreadPoolRequired,
    ThreadPoolUnsupported,

    // CancelError
    NotFound,

    // EpollCtlError
    FileDescriptorAlreadyPresentInSet,
    OperationCausesCircularLoop,
    FileDescriptorNotRegistered,
    SystemResources,
    UserResourceLimitReached,
    FileDescriptorIncompatibleWithEpoll,

    // Loop creation
    ProcessFdQuotaExceeded,
    SystemFdQuotaExceeded,

    // posix ReadError / WriteError
    AccessDenied,
    InputOutput,
    IsDir,
    NotOpenForReading,
    NotOpenForWriting,
    ProcessNotFound,
    SocketNotConnected,
    WouldBlock,
    ConnectionResetByPeer,
    ConnectionTimedOut,
    BrokenPipe,
    DeviceBusy,
    DiskQuota,
    FileTooBig,
    InvalidArgument,
    MessageTooBig,
    NoSpaceLeft,
    PermissionDenied,

    // xev specific
    DupFailed,
    EOF,
    Unknown,

    // Process.WaitError
    InvalidChild,

    // Loop.run
    NestedRunsNotAllowed,

    // Dynamic API
    NoAvailableBackends,

    /// Zig: `posix.UnexpectedError`.
    Unexpected,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

/// Zig: `xev.ReadError`.
pub type ReadError = Error;
/// Zig: `xev.WriteError`.
pub type WriteError = Error;
/// Zig: `xev.PollError`.
pub type PollError = Error;
/// Zig: `xev.CancelError`.
pub type CancelError = Error;
/// Zig: `xev.CloseError`.
pub type CloseError = Error;

/// The current thread's errno.
pub(crate) fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Zig: `posix.unexpectedErrno` (logs and returns `error.Unexpected`).
pub(crate) fn unexpected_errno(err: i32) -> Error {
    log::warn!(target: "xev", "unexpected errno: {err}");
    Error::Unexpected
}

#[cfg(target_os = "linux")]
const MAX_COUNT: usize = 0x7ffff000;
#[cfg(target_os = "macos")]
const MAX_COUNT: usize = i32::MAX as usize;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const MAX_COUNT: usize = isize::MAX as usize;

/// `write(2)` with errno mapping and EINTR retry.
pub fn write(fd: Fd, bytes: &[u8]) -> Result<usize, WriteError> {
    if bytes.is_empty() {
        return Ok(0);
    }

    loop {
        // SAFETY: the pointer/length pair comes from a valid slice.
        let rc = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len().min(MAX_COUNT)) };
        if rc >= 0 {
            return Ok(rc as usize);
        }
        return Err(match errno() {
            libc::EINTR => continue,
            libc::EINVAL => Error::InvalidArgument,
            libc::ESRCH => Error::ProcessNotFound,
            libc::EAGAIN => Error::WouldBlock,
            libc::EBADF => Error::NotOpenForWriting,
            libc::EDQUOT => Error::DiskQuota,
            libc::EFBIG => Error::FileTooBig,
            libc::EIO => Error::InputOutput,
            libc::ENOSPC => Error::NoSpaceLeft,
            libc::EACCES => Error::AccessDenied,
            libc::EPERM => Error::PermissionDenied,
            libc::EPIPE => Error::BrokenPipe,
            libc::ECONNRESET => Error::ConnectionResetByPeer,
            libc::EBUSY => Error::DeviceBusy,
            libc::EMSGSIZE => Error::MessageTooBig,
            libc::ENOBUFS | libc::ENOMEM => Error::SystemResources,
            e => unexpected_errno(e),
        });
    }
}

/// `read(2)` with errno mapping and EINTR retry.
pub fn read(fd: Fd, buf: &mut [u8]) -> Result<usize, ReadError> {
    if buf.is_empty() {
        return Ok(0);
    }

    loop {
        // SAFETY: the pointer/length pair comes from a valid mutable slice.
        let rc = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len().min(MAX_COUNT)) };
        if rc >= 0 {
            return Ok(rc as usize);
        }
        return Err(match errno() {
            libc::EINTR => continue,
            libc::ESRCH => Error::ProcessNotFound,
            libc::EAGAIN => Error::WouldBlock,
            libc::EBADF => Error::NotOpenForReading,
            libc::EIO => Error::InputOutput,
            libc::EISDIR => Error::IsDir,
            libc::ENOBUFS | libc::ENOMEM => Error::SystemResources,
            libc::ENOTCONN => Error::SocketNotConnected,
            libc::ECONNRESET => Error::ConnectionResetByPeer,
            libc::ETIMEDOUT => Error::ConnectionTimedOut,
            e => unexpected_errno(e),
        });
    }
}

/// `close(2)`, ignoring errors (Zig: `xev_posix.close`).
pub fn close(fd: Fd) {
    // SAFETY: closing a descriptor has no memory-safety requirements.
    unsafe {
        libc::close(fd);
    }
}

/// `dup(2)` with CLOEXEC.
pub fn dup(fd: Fd) -> Result<Fd, Error> {
    // SAFETY: plain syscall.
    let rc = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if rc < 0 {
        return Err(unexpected_errno(errno()));
    }
    Ok(rc)
}
