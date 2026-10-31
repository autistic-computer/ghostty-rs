//! Port of `src/config/edit.zig`.
//!
//! STATUS: ported

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use crate::cli::args::StringZ;
use crate::config::file_load;

/// The path to the configuration that should be opened for editing.
///
/// On Linux, this will use the file at the XDG config path. This is the
/// only valid path for Linux so we don't need to check for other paths.
///
/// On macOS, both XDG and AppSupport paths are valid. Because Ghostty
/// prioritizes AppSupport over XDG, we will use AppSupport if it exists,
/// followed by XDG if it exists, and finally AppSupport if neither exist.
/// For the existence check, we also prefer non-empty files over empty
/// files.
pub fn open_path() -> io::Result<StringZ> {
    // Get the path we should open
    let config_path = config_path()?;

    if !config_path.exists {
        if let Some(config_dir) = Path::new(&config_path.name).parent() {
            // Check to see if dir exists.
            match std::fs::metadata(config_dir) {
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    // Create config directory recursively.
                    std::fs::create_dir_all(config_dir)?;
                }
                Err(err) => return Err(err),
            }
        }

        // Try to create file and go on if it already exists
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config_path.name)
        {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }

    Ok(StringZ::from(config_path.name))
}

struct ConfigPathResult {
    name: String,
    exists: bool,
}

/// Returns the config path to use for open for the current OS.
fn config_path() -> io::Result<ConfigPathResult> {
    let paths = config_path_candidates()?;
    debug_assert!(!paths.is_empty());

    // Find the first path that exists and is non-empty. If no paths are
    // non-empty but at least one exists, we will return the first path that
    // exists.
    let mut exists: Option<&String> = None;
    for path in &paths {
        let f = match File::open(path) {
            Ok(f) => f,
            Err(err) => match err.kind() {
                // File doesn't exist, continue.
                io::ErrorKind::NotFound
                | io::ErrorKind::InvalidInput
                | io::ErrorKind::InvalidFilename => continue,

                // Some other error, assume it exists and return it.
                _ => return Err(err),
            },
        };

        // We expect stat to succeed because we just opened the file.
        let stat = f.metadata()?;

        // If the file is non-empty, return it.
        if stat.len() > 0 {
            return Ok(ConfigPathResult {
                name: path.clone(),
                exists: true,
            });
        }

        // If the file is empty, remember it exists.
        if exists.is_none() {
            exists = Some(path);
        }
    }

    // No paths are non-empty, return the first path that exists.
    if let Some(v) = exists {
        return Ok(ConfigPathResult {
            name: v.clone(),
            exists: true,
        });
    }

    // No paths are non-empty or exist, return the first path.
    Ok(ConfigPathResult {
        name: paths[0].clone(),
        exists: false,
    })
}

/// Returns a list of possible paths the main config file could be
/// in for the current OS.
fn config_path_candidates() -> io::Result<Vec<String>> {
    let mut paths: Vec<String> = Vec::with_capacity(4);

    #[cfg(target_os = "macos")]
    {
        paths.push(file_load::default_app_support_path().map_err(io::Error::other)?);
        paths.push(file_load::legacy_default_app_support_path().map_err(io::Error::other)?);
    }

    paths.push(file_load::default_xdg_path().map_err(io::Error::other)?);
    paths.push(file_load::legacy_default_xdg_path().map_err(io::Error::other)?);

    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_candidates_are_absolute_or_error() {
        if let Ok(paths) = config_path_candidates() {
            assert!(paths.len() >= 2);
            assert!(paths.iter().any(|p| p.ends_with("ghostty/config.ghostty")));
        }
    }
}
