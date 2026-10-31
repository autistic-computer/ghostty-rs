//! Port of `src/lib/compat/fd.zig`.
//!
//! STATUS: ported
//!
//! Code taken from Zig 0.15.2 `std.posix`. See README.md for license and
//! details. Rust's `std` has no raw `pipe2`/`close` wrappers so these stay
//! thin `libc` shims with the same error mapping.

#![cfg(unix)]

use std::fmt;
use std::os::fd::RawFd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeError {
    SystemFdQuotaExceeded,
    ProcessFdQuotaExceeded,
    /// An unexpected errno (Zig: `std.posix.UnexpectedError`).
    Unexpected(i32),
}

impl fmt::Display for PipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PipeError::SystemFdQuotaExceeded => f.write_str("SystemFdQuotaExceeded"),
            PipeError::ProcessFdQuotaExceeded => f.write_str("ProcessFdQuotaExceeded"),
            PipeError::Unexpected(e) => write!(f, "unexpected errno {e}"),
        }
    }
}

impl std::error::Error for PipeError {}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

pub fn close(fd: RawFd) {
    // SAFETY: closing an fd has no memory-safety preconditions.
    if unsafe { libc::close(fd) } != 0 {
        match errno() {
            libc::EBADF => unreachable!(), // Always a race condition
            _ => {}                        // Includes EINTR, see std.posix source in 0.15.2
        }
    }
}

pub fn pipe() -> Result<[RawFd; 2], PipeError> {
    let mut fds: [RawFd; 2] = [-1, -1];
    // SAFETY: fds is a valid 2-element array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } == 0 {
        return Ok(fds);
    }
    match errno() {
        libc::EINVAL => unreachable!(), // Invalid parameters to pipe()
        libc::EFAULT => unreachable!(), // Invalid fds pointer
        libc::ENFILE => Err(PipeError::SystemFdQuotaExceeded),
        libc::EMFILE => Err(PipeError::ProcessFdQuotaExceeded),
        e => Err(PipeError::Unexpected(e)),
    }
}

/// `pipe2` with the given `O_*` flags (e.g. `libc::O_CLOEXEC`).
pub fn pipe2(flags: libc::c_int) -> Result<[RawFd; 2], PipeError> {
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
        target_os = "illumos"
    ))]
    {
        let mut fds: [RawFd; 2] = [-1, -1];
        // SAFETY: fds is a valid 2-element array.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), flags) } == 0 {
            return Ok(fds);
        }
        return match errno() {
            libc::EINVAL => unreachable!(), // Invalid flags
            libc::EFAULT => unreachable!(), // Invalid fds pointer
            libc::ENFILE => Err(PipeError::SystemFdQuotaExceeded),
            libc::EMFILE => Err(PipeError::ProcessFdQuotaExceeded),
            e => Err(PipeError::Unexpected(e)),
        };
    }

    #[allow(unreachable_code)]
    {
        let fds = pipe()?;
        let fail = |e: PipeError| {
            close(fds[0]);
            close(fds[1]);
            Err(e)
        };

        if flags == 0 {
            return Ok(fds);
        }

        // CLOEXEC is special, it's a file descriptor flag and must be set
        // using F_SETFD.
        if flags & libc::O_CLOEXEC != 0 {
            for fd in fds {
                // SAFETY: fd is valid.
                if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                    match errno() {
                        libc::EINVAL => unreachable!(), // Invalid flags
                        libc::EBADF => unreachable!(),  // Always a race condition
                        e => return fail(PipeError::Unexpected(e)),
                    }
                }
            }
        }

        let new_flags = flags & !libc::O_CLOEXEC;
        // Set every other flag affecting the file status using F_SETFL.
        if new_flags != 0 {
            for fd in fds {
                // SAFETY: fd is valid.
                if unsafe { libc::fcntl(fd, libc::F_SETFL, new_flags) } != 0 {
                    match errno() {
                        libc::EINVAL => unreachable!(), // Invalid flags
                        libc::EBADF => unreachable!(),  // Always a race condition
                        e => return fail(PipeError::Unexpected(e)),
                    }
                }
            }
        }

        Ok(fds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe2_cloexec() {
        let fds = pipe2(libc::O_CLOEXEC).unwrap();
        for fd in fds {
            // SAFETY: fd is valid.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(flags & libc::FD_CLOEXEC != 0);
        }
        let msg = b"hi";
        // SAFETY: valid fds and buffers.
        unsafe {
            assert_eq!(2, libc::write(fds[1], msg.as_ptr().cast(), 2));
            let mut buf = [0u8; 2];
            assert_eq!(2, libc::read(fds[0], buf.as_mut_ptr().cast(), 2));
            assert_eq!(msg, &buf);
        }
        close(fds[0]);
        close(fds[1]);
    }

    #[test]
    fn plain_pipe() {
        let fds = pipe().unwrap();
        close(fds[0]);
        close(fds[1]);
    }
}
