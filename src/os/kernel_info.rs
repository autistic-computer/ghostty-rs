//! Port of `src/os/kernel_info.zig`.
//!
//! STATUS: ported

/// Returns the kernel release (`/proc/sys/kernel/osrelease`) on Linux.
pub fn get_kernel_info() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let path = "/proc/sys/kernel/osrelease";
    let file = std::fs::File::open(path).ok()?;

    // 128 bytes should be enough to hold the kernel information
    #[allow(unused_mut)]
    let mut kernel_info_buf = [0u8; 128];
    #[allow(unused_mut)]
    let mut len = 0;
    while len < kernel_info_buf.len() {
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(&file, &mut kernel_info_buf[len..], len as u64);
        #[cfg(not(unix))]
        let n: std::io::Result<usize> = {
            let _ = &file;
            Ok(0)
        };
        match n {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    let kernel_info = &kernel_info_buf[..len];
    let trimmed = kernel_info.trim_ascii();
    String::from_utf8(trimmed.to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn read_proc_sys_kernel_osrelease() {
        let kernel_info = get_kernel_info().unwrap();

        // Since we can't hardcode the info in tests, just check
        // if something was read from the file
        assert!(!kernel_info.is_empty());
        assert_ne!(kernel_info, "");
    }
}
