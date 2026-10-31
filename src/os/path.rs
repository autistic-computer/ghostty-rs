//! Port of `src/os/path.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::io;

use super::EnvironMap;

/// Zig: `std.Io.Dir.max_path_bytes`.
const MAX_PATH_BYTES: usize = if cfg!(windows) { 32767 * 3 + 1 } else { 4096 };

#[derive(Debug)]
pub enum ExpandError {
    PathTooLong,
    AccessDenied,
    Io(io::Error),
}

impl fmt::Display for ExpandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExpandError::PathTooLong => f.write_str("PathTooLong"),
            ExpandError::AccessDenied => f.write_str("AccessDenied"),
            ExpandError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ExpandError {}

/// Search for "cmd" in the PATH and return the absolute path. This will
/// always allocate if there is a non-null result.
pub fn expand(environ_map: &EnvironMap, cmd: &str) -> Result<Option<String>, ExpandError> {
    // If the command already contains a slash, then we return it as-is
    // because it is assumed to be absolute or relative.
    if cmd.contains('/') {
        return Ok(Some(cmd.to_string()));
    }

    let Some(path_var) = environ_map.get("PATH") else {
        return Ok(None);
    };

    let delimiter = if cfg!(windows) { ';' } else { ':' };
    let mut seen_eacces = false;
    for search_path in path_var.split(delimiter).filter(|s| !s.is_empty()) {
        // We need enough space in our path buffer to store this
        let path_len = search_path.len() + cmd.len() + 1;
        if MAX_PATH_BYTES < path_len {
            return Err(ExpandError::PathTooLong);
        }

        // Copy in the full path
        let full_path = format!("{}{}{}", search_path, std::path::MAIN_SEPARATOR, cmd);

        // Stat it
        let f = match std::fs::File::open(&full_path) {
            Ok(f) => f,
            Err(err) => match err.kind() {
                io::ErrorKind::NotFound => continue,
                io::ErrorKind::PermissionDenied => {
                    // Accumulate this and return it later so we can try other
                    // paths that we have access to.
                    seen_eacces = true;
                    continue;
                }
                _ => return Err(ExpandError::Io(err)),
            },
        };
        let stat = f.metadata().map_err(ExpandError::Io)?;
        if !stat.is_dir() && is_executable(&stat) {
            return Ok(Some(full_path));
        }
    }

    if seen_eacces {
        return Err(ExpandError::AccessDenied);
    }

    Ok(None)
}

fn is_executable(stat: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return stat.permissions().mode() & 0o0111 != 0;
    }
    #[allow(unreachable_code)]
    {
        let _ = stat;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `uname -n` is the *nix equivalent of `hostname.exe` on Windows
    #[test]
    fn expand_hostname() {
        let environ_map = super::super::environ_map();
        let executable = if cfg!(windows) {
            "hostname.exe"
        } else {
            "uname"
        };
        let path = expand(&environ_map, executable).unwrap().unwrap();
        assert!(path.len() > executable.len());
    }

    #[test]
    fn expand_does_not_exist() {
        let environ_map = super::super::environ_map();
        let path = expand(&environ_map, "thisreallyprobablydoesntexist123").unwrap();
        assert!(path.is_none());
    }

    #[test]
    fn expand_slash() {
        let environ_map = super::super::environ_map();
        let path = expand(&environ_map, "foo/env").unwrap().unwrap();
        assert_eq!(7, path.len());
    }
}
