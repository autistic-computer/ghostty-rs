//! Port of `src/os/pipe.zig`.
//!
//! STATUS: ported

/// The pipe handle type (fd on POSIX, `HANDLE` on Windows).
#[cfg(unix)]
pub type Fd = std::os::fd::RawFd;
#[cfg(windows)]
pub type Fd = super::windows::HANDLE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeError {
    #[cfg(unix)]
    Posix(crate::lib::compat::fd::PipeError),
    /// An unexpected Windows error (Zig: `windows.unexpectedError`).
    Unexpected(u32),
}

impl std::fmt::Display for PipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(unix)]
            PipeError::Posix(e) => write!(f, "{e}"),
            PipeError::Unexpected(e) => write!(f, "unexpected error {e}"),
        }
    }
}

impl std::error::Error for PipeError {}

/// pipe() that works on Windows and POSIX. For POSIX systems, this sets
/// CLOEXEC on the file descriptors.
pub fn pipe() -> Result<[Fd; 2], PipeError> {
    #[cfg(unix)]
    return crate::lib::compat::fd::pipe2(libc::O_CLOEXEC).map_err(PipeError::Posix);

    #[cfg(windows)]
    {
        use super::windows;
        let mut read: windows::HANDLE = std::ptr::null_mut();
        let mut write: windows::HANDLE = std::ptr::null_mut();
        // SAFETY: valid out pointers.
        if unsafe { windows::exp::kernel32::CreatePipe(&mut read, &mut write, std::ptr::null(), 0) }
            == windows::FALSE
        {
            return Err(PipeError::Unexpected(windows::get_last_error()));
        }
        return Ok([read, write]);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn pipe_is_cloexec() {
        let fds = super::pipe().unwrap();
        for fd in fds {
            // SAFETY: fd is valid.
            assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC != 0);
            crate::lib::compat::fd::close(fd);
        }
    }
}
