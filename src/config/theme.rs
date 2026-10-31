//! Port of `src/config/theme.zig`.
//!
//! STATUS: ported

use std::fs::File;
use std::io;
use std::path::Path;

use crate::cli::diagnostics::{Diagnostic, DiagnosticList};
use crate::config::file_load::{path_join, xdg_config};
use crate::config::path::io_error_name;

/// Location of possible themes. The order of this enum matters because it
/// defines the priority of theme search (from top to bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Location {
    /// XDG config dir
    User,
    /// Ghostty resources dir
    Resources,
}

impl Location {
    /// All locations in priority order.
    const ALL: [Location; 2] = [Location::User, Location::Resources];

    /// Returns the directory for the given theme based on this location type.
    ///
    /// This will return null if the directory type doesn't exist or is
    /// invalid for any reason. For example, it is perfectly valid to
    /// install and run Ghostty without the resources directory.
    pub fn dir(self) -> Option<String> {
        match self {
            Location::User => {
                let subdir = path_join(&["ghostty", "themes"]);

                // Any error we treat as the XDG directory not existing.
                xdg_config(&subdir).ok()
            }

            Location::Resources => Some(path_join(&[&resources_dir_app()?, "themes"])),
        }
    }
}

/// The Ghostty resources directory, if any.
fn resources_dir_app() -> Option<String> {
    crate::global::resources_dir().app().map(str::to_owned)
}

/// An entry returned by [`LocationIterator::next`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationEntry {
    pub location: Location,
    pub dir: String,
}

/// An iterator that returns all possible directories for finding themes in
/// order of priority.
#[derive(Debug, Clone, Default)]
pub struct LocationIterator {
    pub i: usize,
}

impl LocationIterator {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<LocationEntry> {
        let max = Location::ALL.len();
        while self.i < max {
            let location = Location::ALL[self.i];
            self.i += 1;
            if let Some(dir) = location.dir() {
                return Some(LocationEntry { location, dir });
            }
        }
        None
    }

    pub fn reset(&mut self) {
        self.i = 0;
    }
}

/// The result of [`open`].
#[derive(Debug)]
pub struct OpenedTheme {
    pub path: String,
    pub file: File,
}

/// Zig `@tagName(stat.kind)` for a file type.
fn kind_name(ft: &std::fs::FileType) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if ft.is_block_device() {
            return "block_device";
        }
        if ft.is_char_device() {
            return "character_device";
        }
        if ft.is_fifo() {
            return "named_pipe";
        }
        if ft.is_socket() {
            return "unix_domain_socket";
        }
    }
    if ft.is_dir() {
        "directory"
    } else if ft.is_symlink() {
        "sym_link"
    } else if ft.is_file() {
        "file"
    } else {
        "unknown"
    }
}

fn add_diag(diags: &mut DiagnosticList, message: String) {
    diags.append(Diagnostic {
        message: message.into(),
        ..Default::default()
    });
}

/// Checks that the opened theme is a regular file, adding a diagnostic
/// if it isn't.
fn check_regular_file(theme: &str, file: &File, diags: &mut DiagnosticList) -> bool {
    let stat = match file.metadata() {
        Ok(stat) => stat,
        Err(err) => {
            add_diag(
                diags,
                format!(
                    "not reading theme from \"{}\": error.{}",
                    theme,
                    io_error_name(&err)
                ),
            );
            return false;
        }
    };
    if !stat.file_type().is_file() {
        add_diag(
            diags,
            format!(
                "not reading theme from \"{}\": it is a {}",
                theme,
                kind_name(&stat.file_type())
            ),
        );
        return false;
    }
    true
}

/// Open the given named theme. If there are any errors then messages
/// will be appended to the given error list and null is returned. If
/// a non-null return value is returned, there are never any errors added.
///
/// This will never return anything other than a handle to a regular file. If
/// the theme resolves to something other than a regular file a diagnostic entry
/// will be added to the list and null will be returned.
pub fn open(theme: &str, diags: &mut DiagnosticList) -> Option<OpenedTheme> {
    // Absolute themes are loaded a different path.
    if Path::new(theme).is_absolute() {
        let file = open_absolute(theme, diags)?;
        if !check_regular_file(theme, &file, diags) {
            return None;
        }
        return Some(OpenedTheme {
            path: theme.to_owned(),
            file,
        });
    }

    let basename = theme.rsplit('/').next().unwrap_or(theme);
    if theme != basename {
        add_diag(
            diags,
            format!(
                "theme \"{}\" cannot include path separators unless it is an absolute path",
                theme
            ),
        );
        return None;
    }

    // Iterate over the possible locations to try to find the
    // one that exists.
    let mut it = LocationIterator::default();
    while let Some(loc) = it.next() {
        let path = path_join(&[&loc.dir, theme]);
        match File::open(&path) {
            Ok(file) => {
                if !check_regular_file(theme, &file, diags) {
                    return None;
                }
                return Some(OpenedTheme { path, file });
            }

            // Not an error, just continue to the next location.
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}

            // Anything else is an error we log and give up on.
            Err(err) => {
                add_diag(
                    diags,
                    format!(
                        "failed to load theme \"{}\" from the file \"{}\": error.{}",
                        theme,
                        path,
                        io_error_name(&err)
                    ),
                );

                return None;
            }
        }
    }

    // Unlikely scenario: the theme doesn't exist. In this case, we reset
    // our iterator, reiterate over in order to build a better error message.
    it.reset();
    while let Some(loc) = it.next() {
        let path = path_join(&[&loc.dir, theme]);
        add_diag(
            diags,
            format!("theme \"{}\" not found, tried path \"{}\"", theme, path),
        );
    }

    None
}

/// Open the given theme from an absolute path. If there are any errors
/// then messages will be appended to the given error list and null is
/// returned. If a non-null return value is returned, there are never any
/// errors added.
pub fn open_absolute(theme: &str, diags: &mut DiagnosticList) -> Option<File> {
    match File::open(theme) {
        Ok(file) => Some(file),
        Err(err) => {
            if err.kind() == io::ErrorKind::NotFound {
                add_diag(
                    diags,
                    format!("failed to load theme from the path \"{}\"", theme),
                );
            } else {
                add_diag(
                    diags,
                    format!(
                        "failed to load theme from the path \"{}\": error.{}",
                        theme,
                        io_error_name(&err)
                    ),
                );
            }

            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn open_absolute_theme() {
        let dir = std::env::temp_dir().join(format!("ghostty-theme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let theme = dir.join("mytheme");
        File::create(&theme).unwrap().write_all(b"background = #000000\n").unwrap();

        let mut diags = DiagnosticList::default();
        let opened = open(theme.to_str().unwrap(), &mut diags).unwrap();
        assert_eq!(theme.to_str().unwrap(), opened.path);
        assert!(diags.empty());

        // A directory is not a theme.
        assert!(open(dir.to_str().unwrap(), &mut diags).is_none());
        assert_eq!(1, diags.items().len());
        assert!(diags.items()[0].message.ends_with("it is a directory"));

        // A missing absolute theme.
        let missing = dir.join("missing");
        assert!(open(missing.to_str().unwrap(), &mut diags).is_none());
        assert_eq!(2, diags.items().len());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_theme_with_separator() {
        let mut diags = DiagnosticList::default();
        assert!(open("foo/bar", &mut diags).is_none());
        assert_eq!(
            "theme \"foo/bar\" cannot include path separators unless it is an absolute path",
            diags.items()[0].message.as_str()
        );
    }
}
