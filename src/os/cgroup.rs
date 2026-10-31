//! Port of `src/os/cgroup.zig`.
//!
//! STATUS: ported

use std::io::Read;

/// Returns the path to the cgroup for the given pid.
pub fn current(buf: &mut [u8], pid: u32) -> Option<&[u8]> {
    // Read our cgroup by opening /proc/<pid>/cgroup and reading the first
    // line. The first line will look something like this:
    // 0::/user.slice/user-1000.slice/session-1.scope
    // The cgroup path is the third field.
    let path = format!("/proc/{pid}/cgroup");
    let mut file = std::fs::File::open(path).ok()?;

    // Zig: `readSliceShort`: fill as much of buf as possible.
    let mut len = 0;
    while len < buf.len() {
        match file.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    let contents = &buf[..len];

    // Find the last ':'
    let idx = contents.iter().rposition(|&c| c == b':')?;
    let mut result = &contents[idx + 1..];
    while let Some((&last, rest)) = result.split_last() {
        if !matches!(last, b' ' | b'\r' | b'\n') {
            break;
        }
        result = rest;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn current_cgroup() {
        let mut buf = [0u8; 4096];
        if let Some(cg) = current(&mut buf, std::process::id()) {
            assert!(cg.starts_with(b"/"));
            assert!(!cg.ends_with(b"\n"));
        }
        assert!(current(&mut buf, u32::MAX).is_none());
    }
}
