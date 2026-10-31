//! Port of `src/os/homedir.zig`.
//!
//! STATUS: ported

use std::fmt;

use super::EnvironMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The buffer used for output is not large enough to store the value.
    BufferTooSmall,
    /// Looking up the home directory failed (passwd or the shell fallback).
    HomeDetectionFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

fn copy_into<'a>(buf: &'a mut [u8], result: &str) -> Result<&'a str, Error> {
    if buf.len() < result.len() {
        return Err(Error::BufferTooSmall);
    }
    buf[..result.len()].copy_from_slice(result.as_bytes());
    // The bytes were copied from a str.
    Ok(std::str::from_utf8(&buf[..result.len()]).unwrap())
}

/// Determine the home directory for the currently executing user. This
/// is generally an expensive process so the value should be cached.
pub fn home<'a>(environ_map: &EnvironMap, buf: &'a mut [u8]) -> Result<Option<&'a str>, Error> {
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
    return home_unix(environ_map, buf);

    #[cfg(windows)]
    return home_windows(environ_map, buf).map_err(|_| Error::BufferTooSmall);

    // iOS doesn't have a user-writable home directory
    #[allow(unreachable_code)]
    Ok(None)
}

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
fn home_unix<'a>(environ_map: &EnvironMap, buf: &'a mut [u8]) -> Result<Option<&'a str>, Error> {
    // First: if we have a HOME env var, then we use that.
    if let Some(result) = environ_map.get("HOME") {
        return copy_into(buf, result).map(Some);
    }

    // On macOS: [NSFileManager defaultManager].homeDirectoryForCurrentUser.path
    #[cfg(target_os = "macos")]
    {
        if let Some(result) = super::macos::home_directory_for_current_user() {
            return copy_into(buf, &result).map(Some);
        }
    }

    // We try passwd. This doesn't work on multi-user mac but we try it
    // anyways.
    let pw = super::passwd::get().map_err(|_| Error::HomeDetectionFailed)?;
    if let Some(result) = &pw.home {
        return copy_into(buf, result).map(Some);
    }

    // If all else fails, have the shell tell us...
    let run = std::process::Command::new("/bin/sh")
        .args(["-c", "cd && pwd"])
        .env_clear()
        .envs(environ_map)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|_| Error::HomeDetectionFailed)?;

    // Zig limits stdout/stderr to half of a 1024 byte buffer each.
    if run.stdout.len() > 512 || run.stderr.len() > 512 {
        return Err(Error::HomeDetectionFailed);
    }

    if run.status.code() == Some(0) {
        let stdout = String::from_utf8_lossy(&run.stdout);
        let result = trim_space(&stdout);
        return copy_into(buf, result).map(Some);
    }

    Ok(None)
}

#[cfg(windows)]
fn home_windows<'a>(environ_map: &EnvironMap, buf: &'a mut [u8]) -> Result<Option<&'a str>, Error> {
    let Some(drive) = environ_map.get("HOMEDRIVE") else {
        return Ok(None);
    };
    let Some(path) = environ_map.get("HOMEPATH") else {
        return Ok(None);
    };
    let total = drive.len() + path.len();
    if total > buf.len() {
        return Err(Error::BufferTooSmall);
    }
    buf[..drive.len()].copy_from_slice(drive.as_bytes());
    buf[drive.len()..total].copy_from_slice(path.as_bytes());
    Ok(Some(std::str::from_utf8(&buf[..total]).unwrap()))
}

fn trim_space(input: &str) -> &str {
    input.trim_matches([' ', '\n', '\t'])
}

/// Expands a path that starts with a tilde (~) to the home directory of
/// the current user.
///
/// Errors if `home` fails or if the size of the expanded path is larger
/// than `buf.len()`.
pub fn expand_home<'a>(
    environ_map: &EnvironMap,
    path: &'a str,
    buf: &'a mut [u8],
) -> Result<&'a str, Error> {
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
    return expand_home_unix(environ_map, path, buf);

    // `~/` is not an idiom generally used on Windows, and iOS doesn't have
    // a user-writable home directory.
    #[allow(unreachable_code)]
    {
        let _ = (environ_map, buf);
        Ok(path)
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
fn expand_home_unix<'a>(
    environ_map: &EnvironMap,
    path: &'a str,
    buf: &'a mut [u8],
) -> Result<&'a str, Error> {
    if !path.starts_with("~/") {
        return Ok(path);
    }
    let home_len = match home(environ_map, buf) {
        Ok(Some(home)) => home.len(),
        Ok(None) | Err(_) => return Err(Error::HomeDetectionFailed),
    };
    let rest = &path[1..]; // Skip the ~
    let expanded_len = home_len + rest.len();

    if expanded_len > buf.len() {
        return Err(Error::BufferTooSmall);
    }
    buf[home_len..expanded_len].copy_from_slice(rest.as_bytes());

    // Both parts are valid UTF-8.
    Ok(std::str::from_utf8(&buf[..expanded_len]).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
    #[test]
    fn expand_home_unix_test() {
        let environ_map = super::super::environ_map();
        let mut buf = vec![0u8; 4096];
        let home_dir = expand_home_unix(&environ_map, "~/", &mut buf)
            .unwrap()
            .to_string();
        // Joining the home directory `~` with the path `/`
        // the result should end with a separator here. (e.g. `/home/user/`)
        assert!(home_dir.ends_with('/'));

        let downloads = expand_home_unix(&environ_map, "~/Downloads/shader.glsl", &mut buf)
            .unwrap()
            .to_string();
        let expected_downloads = format!("{home_dir}Downloads/shader.glsl");
        assert_eq!(expected_downloads, downloads);

        assert_eq!("~", expand_home_unix(&environ_map, "~", &mut buf).unwrap());
        assert_eq!(
            "~abc/",
            expand_home_unix(&environ_map, "~abc/", &mut buf).unwrap()
        );
        assert_eq!(
            "/home/user",
            expand_home_unix(&environ_map, "/home/user", &mut buf).unwrap()
        );
        assert_eq!("", expand_home_unix(&environ_map, "", &mut buf).unwrap());

        // Expect an error if the buffer is large enough to hold the home
        // directory, but not the expanded path
        let mut small_buf = vec![0u8; home_dir.len()];
        assert_eq!(
            Err(Error::BufferTooSmall),
            expand_home_unix(&environ_map, "~/Downloads", &mut small_buf)
        );
    }

    #[test]
    fn home_test() {
        let environ_map = super::super::environ_map();
        let mut buf = [0u8; 1024];
        let result = home(&environ_map, &mut buf).unwrap();
        assert!(result.is_some());
        assert!(!result.unwrap().is_empty());
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn home_without_home_env_uses_passwd() {
        let mut buf = [0u8; 1024];
        let result = home(&EnvironMap::new(), &mut buf).unwrap();
        assert!(result.is_some_and(|h| !h.is_empty()));
    }
}
