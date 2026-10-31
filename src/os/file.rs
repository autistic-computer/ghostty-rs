//! Port of `src/os/file.zig`.
//!
//! STATUS: ported

use crate::lib::tinyio::TinyIo;

use super::EnvironMap;

/// Zig: `posix.rlimit` (an empty struct where unsupported).
#[cfg(unix)]
pub type Rlimit = libc::rlimit;
#[cfg(not(unix))]
#[derive(Debug, Clone, Copy, Default)]
pub struct Rlimit {}

/// This maximizes the number of file descriptors we can have open. We
/// need to do this because each window consumes at least a handful of fds.
/// This is extracted from the Zig compiler source code.
#[cfg(unix)]
pub fn fix_max_files() -> Option<Rlimit> {
    let mut old: libc::rlimit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid out pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut old) } != 0 {
        log::warn!(target: "os", "failed to query file handle limit, may limit max windows");
        return None; // Oh well; we tried.
    }

    // If we're already at the max, we're done.
    if old.rlim_cur >= old.rlim_max {
        log::debug!(target: "os", "file handle limit already maximized value={}", old.rlim_cur);
        return Some(old);
    }

    // Do a binary search for the limit.
    let mut lim = old;
    let mut min: libc::rlim_t = lim.rlim_cur;
    let mut max: libc::rlim_t = 1 << 20;
    // But if there's a defined upper bound, don't search, just set it.
    if lim.rlim_max != libc::RLIM_INFINITY {
        min = lim.rlim_max;
        max = lim.rlim_max;
    }

    loop {
        lim.rlim_cur = min + (max - min) / 2; // on freebsd rlim_t is signed
                                              // SAFETY: valid pointer.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } == 0 {
            min = lim.rlim_cur;
        } else {
            max = lim.rlim_cur;
        }
        if min + 1 >= max {
            break;
        }
    }

    log::debug!(target: "os", "file handle limit raised value={}", lim.rlim_cur);
    Some(old)
}

#[cfg(not(unix))]
pub fn fix_max_files() -> Option<Rlimit> {
    None
}

#[cfg(unix)]
pub fn restore_max_files(lim: Rlimit) {
    // SAFETY: valid pointer.
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) };
}

#[cfg(not(unix))]
pub fn restore_max_files(_lim: Rlimit) {}

/// Return the recommended path for temporary files. Any trailing
/// path separator is stripped so callers can safely join with their
/// own separator (e.g. `"{tmp}/{name}"`).
///
/// On Windows this calls `GetTempPathW` (or falls back to a hard-coded
/// path if the syscall fails). On POSIX this returns `$TMPDIR`/`$TMP` (or
/// `"/tmp"` as a fallback) from `environ`. The Zig version only allocates
/// on Windows and has a matching `freeTmpDir`; the Rust port always
/// returns an owned `String`, so no free function is needed.
pub fn alloc_tmp_dir(environ: &EnvironMap) -> String {
    #[cfg(windows)]
    {
        let _ = environ;
        use super::windows;
        // GetTempPathW guarantees the result fits in MAX_PATH+1.
        let mut buf = [0u16; windows::MAX_PATH + 1];
        // SAFETY: buffer is valid for MAX_PATH+1 u16s.
        let len =
            unsafe { windows::exp::kernel32::GetTempPathW(buf.len() as u32, buf.as_mut_ptr()) }
                as usize;
        if len > 0 {
            // Trim the UTF-16 string before encoding as UTF-8.
            let mut trimmed = &buf[..len];
            while let Some((&last, rest)) = trimmed.split_last() {
                if last != b'\\' as u16 {
                    break;
                }
                trimmed = rest;
            }
            match String::from_utf16(trimmed) {
                Ok(utf8) => return utf8,
                Err(e) => {
                    log::warn!(target: "os", "failed to convert temp dir path from windows string: {e}")
                }
            }
        }
        return "C:\\Windows\\Temp".to_string();
    }

    #[allow(unreachable_code)]
    {
        let tmpdir = environ
            .get("TMPDIR")
            .or_else(|| environ.get("TMP"))
            .map(String::as_str)
            .unwrap_or("/tmp");
        tmpdir
            .trim_end_matches(std::path::MAIN_SEPARATOR)
            .to_string()
    }
}

const RANDOM_BASENAME_BYTES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RandomBasenameError {
    BufferTooSmall,
}

impl std::fmt::Display for RandomBasenameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BufferTooSmall")
    }
}

impl std::error::Error for RandomBasenameError {}

/// Length of the basename produced by `random_basename`
/// (url-safe base64 without padding of 16 bytes).
pub const RANDOM_BASENAME_LEN: usize = (RANDOM_BASENAME_BYTES * 4).div_ceil(3);

/// Zig: `std.base64.url_safe_no_pad.Encoder.encode`.
fn base64_url_no_pad_encode<'a>(dest: &'a mut [u8], source: &[u8]) -> &'a [u8] {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = 0;
    for chunk in source.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let chars = chunk.len() + 1;
        for i in 0..chars {
            dest[out] = ALPHABET[((n >> (18 - 6 * i)) & 0x3F) as usize];
            out += 1;
        }
    }
    &dest[..out]
}

/// Write a random filesystem-safe base64 basename of length
/// `RANDOM_BASENAME_LEN` into `buf` and return a slice over the
/// written bytes. Returns `BufferTooSmall` if `buf` is too short.
pub fn random_basename(buf: &mut [u8]) -> Result<&str, RandomBasenameError> {
    if buf.len() < RANDOM_BASENAME_LEN {
        return Err(RandomBasenameError::BufferTooSmall);
    }
    let mut rand_buf = [0u8; RANDOM_BASENAME_BYTES];
    // Zig uses the global Io's (non-secure) `random`; TinyIo only has a
    // secure source, which is a strict improvement.
    if TinyIo::INIT.random_secure(&mut rand_buf).is_err() {
        // Fall back to something unique-ish rather than failing.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        rand_buf.copy_from_slice(&(nanos ^ (std::process::id() as u128) << 64).to_le_bytes());
    }
    let encoded = base64_url_no_pad_encode(&mut buf[..RANDOM_BASENAME_LEN], &rand_buf);
    // SAFETY-free: the base64 alphabet is ASCII.
    Ok(std::str::from_utf8(encoded).unwrap())
}

/// Return a freshly-allocated path of the form `{TMPDIR}/{prefix}{random}`.
///
/// Nothing is created on disk; this only builds the path string. Useful
/// for one-shot temporary file/socket paths where a full `TempDir` is
/// overkill.
pub fn random_tmp_path(prefix: &str) -> String {
    let mut name_buf = [0u8; RANDOM_BASENAME_LEN];
    let basename = random_basename(&mut name_buf).unwrap();
    // Zig: `global.tmpDirPath()`, the process temp dir computed at startup.
    let tmp = alloc_tmp_dir(&super::environ_map());
    format!("{}{}{}{}", tmp, std::path::MAIN_SEPARATOR, prefix, basename)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_basename_test() {
        let mut buf = [0u8; RANDOM_BASENAME_LEN];
        let name = random_basename(&mut buf).unwrap();
        assert_eq!(RANDOM_BASENAME_LEN, name.len());
        for c in name.bytes() {
            let ok = c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
            assert!(ok);
        }

        let mut small = [0u8; RANDOM_BASENAME_LEN - 1];
        assert_eq!(
            Err(RandomBasenameError::BufferTooSmall),
            random_basename(&mut small)
        );
    }

    #[test]
    fn base64_matches_reference() {
        let mut out = [0u8; 32];
        assert_eq!(b"Zm9vYmFy", base64_url_no_pad_encode(&mut out, b"foobar"));
        assert_eq!(b"Zm9vYg", base64_url_no_pad_encode(&mut out, b"foob"));
        assert_eq!(b"-_8", base64_url_no_pad_encode(&mut out, &[0xfb, 0xff]));
        assert_eq!(22, RANDOM_BASENAME_LEN);
    }

    #[cfg(unix)]
    #[test]
    fn tmp_dir_from_environ() {
        let mut env = EnvironMap::new();
        assert_eq!("/tmp", alloc_tmp_dir(&env));
        env.insert("TMP".into(), "/var/tmp/".into());
        assert_eq!("/var/tmp", alloc_tmp_dir(&env));
        env.insert("TMPDIR".into(), "/x//".into());
        assert_eq!("/x", alloc_tmp_dir(&env));
        assert!(random_tmp_path("ghostty-").contains("ghostty-"));
    }

    #[test]
    fn fix_and_restore_max_files() {
        if let Some(old) = fix_max_files() {
            restore_max_files(old);
        }
    }
}
