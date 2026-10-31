//! Port of `src/os/stderr.zig`.
//!
//! STATUS: ported
//!
//! Raw, best-effort stderr writing that bypasses buffered/locked I/O
//! entirely (in Zig, `std.debug.lockStderr` pulls in `std.Io.Threaded`).
//! Useful from logging and panic paths.

/// Write bytes to stderr using the most primitive mechanism available
/// for the target. This is best-effort: errors are ignored, since this
/// is only used for diagnostics (logging and panic messages).
///
/// Freestanding targets (e.g. wasm) have no stderr, so this is a no-op
/// there.
pub fn write(bytes: &[u8]) {
    #[cfg(windows)]
    {
        use super::windows;
        let handle = windows::peb_std_error_handle();
        let mut iosb = windows::IO_STATUS_BLOCK::default();
        // SAFETY: valid handle (or null, which fails harmlessly) and buffer.
        unsafe {
            windows::exp::ntdll::NtWriteFile(
                handle,
                std::ptr::null_mut(), // event
                std::ptr::null(),     // APC routine
                std::ptr::null_mut(), // APC context
                &mut iosb,
                bytes.as_ptr().cast(),
                bytes.len() as u32,
                std::ptr::null(), // byte offset
                std::ptr::null(), // key
            )
        };
        return;
    }

    #[cfg(unix)]
    {
        let mut i = 0usize;
        while i < bytes.len() {
            // SAFETY: the remaining slice is valid for reads.
            let rc = unsafe {
                libc::write(
                    libc::STDERR_FILENO,
                    bytes[i..].as_ptr().cast(),
                    bytes.len() - i,
                )
            };
            if rc >= 0 {
                i += rc as usize;
                continue;
            }
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) => continue,
                _ => return,
            }
        }
    }

    #[allow(unreachable_code)]
    {
        let _ = bytes;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn write() {
        // Smoke test: must not crash. We can't assert on the output without
        // capturing stderr, which isn't worth the complexity here.
        super::write(b"");
    }
}
