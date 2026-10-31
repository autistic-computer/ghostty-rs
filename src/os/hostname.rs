//! Port of `src/os/hostname.zig`.
//!
//! STATUS: ported

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalHostnameValidationError {
    PermissionDenied,
    Unexpected,
}

impl fmt::Display for LocalHostnameValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for LocalHostnameValidationError {}

/// Zig: `posix.gethostname`.
#[cfg(unix)]
fn gethostname(buf: &mut [u8]) -> Result<&[u8], LocalHostnameValidationError> {
    // SAFETY: buffer is valid for writes of its length.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return Err(match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM) => LocalHostnameValidationError::PermissionDenied,
            _ => LocalHostnameValidationError::Unexpected,
        });
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Ok(&buf[..len])
}

/// Zig: `posix.HOST_NAME_MAX`.
#[cfg(unix)]
const HOST_NAME_MAX: usize = if cfg!(target_os = "linux") { 64 } else { 255 };

/// Checks if a hostname is local to the current machine. This matches
/// both "localhost" and the current hostname of the machine (as returned
/// by `gethostname`).
pub fn is_local(hostname: &[u8]) -> Result<bool, LocalHostnameValidationError> {
    // A 'localhost' hostname is always considered local.
    if hostname == b"localhost" {
        return Ok(true);
    }

    // If hostname is not "localhost" it must match our hostname.
    #[cfg(windows)]
    {
        use super::windows;
        let mut buf = [0u8; 257];
        let mut n_size: windows::DWORD = 256;
        // SAFETY: buffer and size pointer are valid.
        if unsafe { windows::exp::kernel32::GetComputerNameA(buf.as_mut_ptr(), &mut n_size) }
            == windows::FALSE
        {
            return Ok(false);
        }
        let our_hostname = &buf[..n_size as usize];
        return Ok(hostname == our_hostname);
    }

    #[cfg(unix)]
    {
        let mut buf = [0u8; HOST_NAME_MAX + 1];
        let our_hostname = gethostname(&mut buf)?;
        return Ok(hostname == our_hostname);
    }

    #[allow(unreachable_code)]
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_local_returns_true_when_provided_hostname_is_localhost() {
        assert!(is_local(b"localhost").unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn is_local_returns_true_when_hostname_is_local() {
        let mut buf = [0u8; HOST_NAME_MAX + 1];
        let local_hostname = gethostname(&mut buf).unwrap().to_vec();
        assert!(is_local(&local_hostname).unwrap());
    }

    #[test]
    fn is_local_returns_false_when_hostname_is_not_local() {
        assert_eq!(false, is_local(b"not-the-local-hostname").unwrap());
    }
}
