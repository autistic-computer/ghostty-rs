//! Port of `src/config/file_load.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::fs::File;
use std::path::Path;

/// Errors from looking up the default config directories (Zig: the
/// inferred error sets of `xdg.config` and `macos.appSupportDir`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XdgError {
    Xdg(crate::os::xdg::Error),
    #[cfg(target_os = "macos")]
    AppSupportDir(crate::os::macos::AppSupportDirError),
}

impl fmt::Display for XdgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            XdgError::Xdg(e) => write!(f, "{e}"),
            #[cfg(target_os = "macos")]
            XdgError::AppSupportDir(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for XdgError {}

/// Zig `std.fs.path.join`: joins non-empty components with `/` without
/// doubling separators.
pub(crate) fn path_join(parts: &[&str]) -> String {
    let mut result = String::new();
    for part in parts.iter().filter(|p| !p.is_empty()) {
        if result.is_empty() {
            result.push_str(part);
            continue;
        }
        let ends = result.ends_with('/');
        let starts = part.starts_with('/');
        if ends && starts {
            result.push_str(&part[1..]);
        } else if ends || starts {
            result.push_str(part);
        } else {
            result.push('/');
            result.push_str(part);
        }
    }
    result
}

/// Get the XDG user config directory joined with `subdir`
/// (`internal_os.xdg.config` with the global environment).
pub(crate) fn xdg_config(subdir: &str) -> Result<String, XdgError> {
    let environ_map = crate::global::environ_map();
    crate::os::xdg::config(
        &environ_map,
        crate::os::xdg::Options {
            subdir: Some(subdir),
            ..Default::default()
        },
    )
    .map_err(XdgError::Xdg)
}

/// Default path for the XDG home configuration file.
pub fn default_xdg_path() -> Result<String, XdgError> {
    xdg_config("ghostty/config.ghostty")
}

/// Ghostty <1.3.0 default path for the XDG home configuration file.
pub fn legacy_default_xdg_path() -> Result<String, XdgError> {
    xdg_config("ghostty/config")
}

/// Preferred default path for the XDG home configuration file.
pub fn preferred_xdg_path() -> Result<String, XdgError> {
    // If the XDG path exists, use that.
    let xdg_path = default_xdg_path()?;
    if open(&xdg_path).is_ok() {
        return Ok(xdg_path);
    }

    // Try the legacy path
    let legacy_xdg_path = legacy_default_xdg_path()?;
    if open(&legacy_xdg_path).is_ok() {
        return Ok(legacy_xdg_path);
    }

    // Legacy path and XDG path both don't exist. Return the
    // new one.
    Ok(xdg_path)
}

/// Return the path to the macOS Application Support directory with the
/// given sub path joined (`internal_os.macos.appSupportDir`).
#[cfg(target_os = "macos")]
fn app_support_dir(sub_path: &str) -> Result<String, XdgError> {
    crate::os::macos::app_support_dir(sub_path).map_err(XdgError::AppSupportDir)
}

/// Default path for the macOS Application Support configuration file.
#[cfg(target_os = "macos")]
pub fn default_app_support_path() -> Result<String, XdgError> {
    app_support_dir("config.ghostty")
}

/// Ghostty <1.3.0 default path for the macOS Application Support
/// configuration file.
#[cfg(target_os = "macos")]
pub fn legacy_default_app_support_path() -> Result<String, XdgError> {
    app_support_dir("config")
}

/// Preferred default path for the macOS Application Support configuration file.
#[cfg(target_os = "macos")]
pub fn preferred_app_support_path() -> Result<String, XdgError> {
    // If the app support path exists, use that.
    let app_support_path = default_app_support_path()?;
    if open(&app_support_path).is_ok() {
        return Ok(app_support_path);
    }

    // Try the legacy path
    let legacy_app_support_path = legacy_default_app_support_path()?;
    if open(&legacy_app_support_path).is_ok() {
        return Ok(legacy_app_support_path);
    }

    // Legacy path and app support path both don't exist. Return the
    // new one.
    Ok(app_support_path)
}

/// Returns the path to the preferred default configuration file.
/// This is the file where users should place their configuration.
///
/// This doesn't create or populate the file with any default
/// contents; downstream callers must handle this.
pub fn preferred_default_file_path() -> Result<String, XdgError> {
    #[cfg(target_os = "macos")]
    {
        // macOS prefers the Application Support directory
        // if it exists.
        let app_support_path = preferred_app_support_path()?;
        if open(&app_support_path).is_err() {
            // Try the XDG path if it exists
            let xdg_path = preferred_xdg_path()?;
            if open(&xdg_path).is_err() {
                // If neither file exists, use app support
                return Ok(app_support_path);
            }
            return Ok(xdg_path);
        }
        Ok(app_support_path)
    }

    // All other platforms use XDG only
    #[cfg(not(target_os = "macos"))]
    {
        preferred_xdg_path()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenFileError {
    FileNotFound,
    FileIsEmpty,
    FileOpenFailed,
    NotAFile,
}

impl fmt::Display for OpenFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            OpenFileError::FileNotFound => "FileNotFound",
            OpenFileError::FileIsEmpty => "FileIsEmpty",
            OpenFileError::FileOpenFailed => "FileOpenFailed",
            OpenFileError::NotAFile => "NotAFile",
        };
        write!(f, "error.{}", name)
    }
}

impl std::error::Error for OpenFileError {}

/// Opens the file at the given path and returns the file handle
/// if it exists and is non-empty. This also constrains the possible
/// errors to a smaller set that we can explicitly handle.
pub fn open(path: &str) -> Result<File, OpenFileError> {
    debug_assert!(Path::new(path).is_absolute());

    let file = match File::open(path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(OpenFileError::FileNotFound)
        }
        Err(err) => {
            log::warn!(target: "config", "unexpected file open error path={} err={}", path, err);
            return Err(OpenFileError::FileOpenFailed);
        }
    };

    let stat = match file.metadata() {
        Ok(stat) => stat,
        Err(err) => {
            log::warn!(target: "config", "error getting file stat path={} err={}", path, err);
            return Err(OpenFileError::FileOpenFailed);
        }
    };
    if !stat.is_file() {
        return Err(OpenFileError::NotAFile);
    }

    if stat.len() == 0 {
        return Err(OpenFileError::FileIsEmpty);
    }

    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn path_join_works() {
        assert_eq!("a/b", path_join(&["a", "b"]));
        assert_eq!("/a/b/", path_join(&["/a/", "/b/"]));
        assert_eq!("a/b", path_join(&["a", "", "b"]));
        assert_eq!("/home/.config", path_join(&["/home", ".config", ""]));
    }

    #[test]
    fn open_errors() {
        let dir = std::env::temp_dir().join(format!("ghostty-file-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let missing = dir.join("missing");
        assert_eq!(
            OpenFileError::FileNotFound,
            open(missing.to_str().unwrap()).unwrap_err()
        );

        assert_eq!(
            OpenFileError::NotAFile,
            open(dir.to_str().unwrap()).unwrap_err()
        );

        let empty = dir.join("empty");
        File::create(&empty).unwrap();
        assert_eq!(
            OpenFileError::FileIsEmpty,
            open(empty.to_str().unwrap()).unwrap_err()
        );

        let full = dir.join("full");
        File::create(&full).unwrap().write_all(b"a = b\n").unwrap();
        assert!(open(full.to_str().unwrap()).is_ok());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn default_paths_end_with_config_file_names() {
        if let Ok(p) = default_xdg_path() {
            assert!(p.ends_with("ghostty/config.ghostty"));
        }
        if let Ok(p) = legacy_default_xdg_path() {
            assert!(p.ends_with("ghostty/config"));
        }
    }
}
