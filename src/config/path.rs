//! Port of `src/config/path.zig`.
//!
//! STATUS: ported

use std::ffi::c_char;
use std::io;

use crate::cli::args::{Error, ParseCli, StringZ};
use crate::cli::diagnostics::{Diagnostic, DiagnosticList};
use crate::config::formatter::{self, EntryFormatter, FormatEntry};

/// Zig `ParseError = error{ValueRequired} || Allocator.Error`; both are
/// variants of the common arg parsing error.
pub type ParseError = Error;

/// Zig `std.Io.Dir.max_path_bytes` on Linux/macOS.
const MAX_PATH_BYTES: usize = 4096;

/// Path is like a string that represents a path value. The difference is that
/// when loading the configuration the value for this will be automatically
/// expanded relative to the path of the config file (or the home directory).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Path {
    /// No error if the file does not exist.
    Optional(StringZ),

    /// The file is required to exist.
    Required(StringZ),
}

/// ghostty_config_path_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct C {
    pub path: *const c_char,
    pub optional: bool,
}

impl Path {
    /// The path string, regardless of the tag.
    pub fn path(&self) -> &StringZ {
        match self {
            Path::Optional(path) | Path::Required(path) => path,
        }
    }

    fn path_mut(&mut self) -> &mut StringZ {
        match self {
            Path::Optional(path) | Path::Required(path) => path,
        }
    }

    pub fn len(&self) -> usize {
        self.path().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn equal(&self, other: &Path) -> bool {
        self == other
    }

    /// Returns the path as a C-compatible struct. The pointer is valid as
    /// long as this value lives.
    pub fn cval(&self) -> C {
        match self {
            Path::Optional(path) => C {
                path: path.as_ptr(),
                optional: true,
            },
            Path::Required(path) => C {
                path: path.as_ptr(),
                optional: false,
            },
        }
    }

    /// Parse the input and return a Path. A leading `?` indicates that the path
    /// is _optional_ and an error should not be logged or displayed to the user
    /// if that path does not exist. Otherwise the path is required and an error
    /// should be logged if the path does not exist.
    pub fn parse(input: Option<&str>) -> Result<Option<Path>, ParseError> {
        let mut value = input.ok_or(Error::ValueRequired)?;

        if value.is_empty() {
            return Ok(None);
        }

        let optional = if let Some(rest) = value.strip_prefix('?') {
            value = rest;
            true
        } else {
            false
        };

        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            value = &value[1..value.len() - 1];
        }

        if optional {
            Ok(Some(Path::Optional(StringZ::from(value))))
        } else {
            Ok(Some(Path::Required(StringZ::from(value))))
        }
    }

    /// The formatted value: optional paths are prefixed with `?`.
    fn format_value(&self) -> Result<String, formatter::Error> {
        match self {
            Path::Optional(path) => {
                // Zig prints into a `max_path_bytes + 1` buffer.
                if path.len() + 1 > MAX_PATH_BYTES + 1 {
                    return Err(formatter::Error::OutOfMemory);
                }
                Ok(format!("?{}", path))
            }
            Path::Required(path) => Ok(path.to_string()),
        }
    }

    /// Expand relative paths or paths prefixed with `~/`. The path will be
    /// overwritten.
    ///
    /// `base` is the base directory to expand relative paths. It must be an
    /// absolute path. Errors will be added to the list of diagnostics if
    /// they occur. Failing to open the base directory is returned as an
    /// error.
    pub fn expand(&mut self, base: &str, diags: &mut DiagnosticList) -> io::Result<()> {
        debug_assert!(std::path::Path::new(base).is_absolute());

        let path = self.path().as_str().to_owned();

        // If it is already absolute we can ignore it.
        if path.is_empty() || std::path::Path::new(&path).is_absolute() {
            return Ok(());
        }

        // Check if the path starts with a tilde and expand it to the
        // home directory on Linux/macOS. We explicitly look for "~/"
        // because we don't support alternate users such as "~alice/"
        if path.starts_with("~/") && !cfg!(windows) {
            match expand_home(&path) {
                Ok(expanded) => {
                    log::debug!(
                        target: "config",
                        "expanding file path from home directory: path={}",
                        expanded
                    );
                    *self.path_mut() = StringZ::from(expanded);
                }
                Err(err) => {
                    diags.append(Diagnostic {
                        message: format!(
                            "error expanding home directory for path {}: {}",
                            path, err
                        )
                        .into(),
                        ..Default::default()
                    });

                    // Blank this path so that we don't attempt to resolve it
                    // again
                    *self = Path::Required(StringZ::new());
                }
            }

            return Ok(());
        }

        // If it isn't absolute, we need to make it absolute relative
        // to the base.
        let meta = std::fs::metadata(base)?;
        if !meta.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "not a directory"));
        }

        let abs = match std::fs::canonicalize(std::path::Path::new(base).join(&path)) {
            Ok(abs) => abs.to_string_lossy().into_owned(),
            // The file doesn't exist. Try to resolve the relative path
            // another way.
            Err(err) if err.kind() == io::ErrorKind::NotFound => resolve(base, &path),
            Err(err) => {
                diags.append(Diagnostic {
                    message: format!(
                        "error resolving file path {}: error.{}",
                        path,
                        io_error_name(&err)
                    )
                    .into(),
                    ..Default::default()
                });

                // Blank this path so that we don't attempt to resolve it again
                *self = Path::Required(StringZ::new());

                return Ok(());
            }
        };

        log::debug!(
            target: "config",
            "expanding file path relative={} abs={}",
            path,
            abs
        );

        *self.path_mut() = StringZ::from(abs);
        Ok(())
    }
}

impl ParseCli for Path {
    /// Parse CLI option. The value will be overwritten (zero-length values
    /// are ignored).
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        debug_assert!(input.is_some());
        let Some(item) = Path::parse(input)? else {
            return Ok(());
        };
        if item.len() == 0 {
            return Ok(());
        }
        *self = item;
        Ok(())
    }

    /// Zig parses into an `undefined` path when an optional path field is
    /// null; we start from an empty required path.
    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        let mut v = Path::Required(StringZ::new());
        v.parse_cli(input)?;
        Ok(v)
    }
}

impl FormatEntry for Path {
    /// Used by formatter.
    fn format_entry(&self, formatter: &mut EntryFormatter<'_>) -> Result<(), formatter::Error> {
        let value = self.format_value()?;
        formatter.format_entry(value.as_str())
    }
}

/// RepeatablePath is like repeatable string but represents a path value. The
/// difference is that when loading the configuration any values for this will
/// be automatically expanded relative to the path of the config file (or the home
/// directory).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepeatablePath {
    pub value: Vec<Path>,
}

impl ParseCli for RepeatablePath {
    fn parse_cli(&mut self, input: Option<&str>) -> Result<(), Error> {
        let Some(item) = Path::parse(input)? else {
            self.value.clear();
            return Ok(());
        };

        if item.len() == 0 {
            // This handles the case of zero length paths after removing any ?
            // prefixes or surrounding quotes. In this case, we don't reset the
            // list.
            return Ok(());
        }

        self.value.push(item);
        Ok(())
    }

    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        crate::cli::args::parse_cli_new_from_default(input)
    }
}

impl RepeatablePath {
    /// Compare if two of our value are equal. Required by Config.
    pub fn equal(&self, other: &RepeatablePath) -> bool {
        if self.value.len() != other.value.len() {
            return false;
        }
        self.value.iter().zip(other.value.iter()).all(|(a, b)| a.equal(b))
    }

    /// Expand all the paths relative to the base directory.
    pub fn expand(&mut self, base: &str, diags: &mut DiagnosticList) -> io::Result<()> {
        for path in &mut self.value {
            path.expand(base, diags)?;
        }
        Ok(())
    }
}

impl FormatEntry for RepeatablePath {
    /// Used by Formatter
    fn format_entry(&self, formatter: &mut EntryFormatter<'_>) -> Result<(), formatter::Error> {
        if self.value.is_empty() {
            return formatter.format_entry(&());
        }

        for item in &self.value {
            let value = item.format_value()?;
            formatter.format_entry(value.as_str())?;
        }
        Ok(())
    }
}

/// Zig-style error name for an I/O error (as printed by Zig's `{}` on an
/// error value, minus the `error.` prefix).
pub(crate) fn io_error_name(err: &io::Error) -> &'static str {
    match err.kind() {
        io::ErrorKind::NotFound => "FileNotFound",
        io::ErrorKind::PermissionDenied => "AccessDenied",
        io::ErrorKind::IsADirectory => "IsDir",
        io::ErrorKind::NotADirectory => "NotDir",
        io::ErrorKind::AlreadyExists => "PathAlreadyExists",
        io::ErrorKind::InvalidFilename | io::ErrorKind::InvalidInput => "BadPathName",
        _ if err.raw_os_error() == Some(40) && cfg!(target_os = "linux") => "SymLinkLoop",
        _ => "Unexpected",
    }
}

/// Lexically resolve `path` relative to `base` (Zig
/// `std.fs.path.resolve(alloc, &.{ base, path })` on POSIX).
fn resolve(base: &str, path: &str) -> String {
    let components: Vec<&str> = if path.starts_with('/') {
        path.split('/').collect()
    } else {
        base.split('/').chain(path.split('/')).collect()
    };
    let mut parts: Vec<&str> = Vec::new();
    for component in components {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Expands a `~/`-prefixed path to the user's home directory
/// (`internal_os.expandHome` with the global environment).
fn expand_home(path: &str) -> Result<String, crate::os::homedir::Error> {
    let environ_map = crate::global::environ_map();
    let mut buf = [0u8; MAX_PATH_BYTES];
    crate::os::expand_home(&environ_map, path, &mut buf).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::formatter::entry_formatter;

    #[test]
    fn path_parse() {
        {
            let item = Path::parse(Some("config.1")).unwrap().unwrap();
            assert_eq!(Path::Required("config.1".into()), item);
        }

        {
            let item = Path::parse(Some("?config.2")).unwrap().unwrap();
            assert_eq!(Path::Optional("config.2".into()), item);
        }

        {
            let item = Path::parse(Some("\"?config.3\"")).unwrap().unwrap();
            assert_eq!(Path::Required("?config.3".into()), item);
        }

        {
            let item = Path::parse(Some("?\"config.4\"")).unwrap().unwrap();
            assert_eq!(Path::Optional("config.4".into()), item);
        }

        {
            let item = Path::parse(Some("?")).unwrap().unwrap();
            assert_eq!(Path::Optional("".into()), item);
        }

        {
            let item = Path::parse(Some("\"\"")).unwrap().unwrap();
            assert_eq!(Path::Required("".into()), item);
        }

        {
            let item = Path::parse(Some("?\"\"")).unwrap().unwrap();
            assert_eq!(Path::Optional("".into()), item);
        }
    }

    #[test]
    fn path_parse_cli() {
        let mut item = Path::Required(StringZ::new());

        item.parse_cli(Some("config.1")).unwrap();
        assert_eq!(Path::Required("config.1".into()), item);

        item.parse_cli(Some("?config.2")).unwrap();
        assert_eq!(Path::Optional("config.2".into()), item);

        item.parse_cli(Some("\"?config.3\"")).unwrap();
        assert_eq!(Path::Required("?config.3".into()), item);

        // Zero-length values, ignored

        item.parse_cli(Some("?")).unwrap();
        assert_eq!(Path::Required("?config.3".into()), item);

        item.parse_cli(Some("\"\"")).unwrap();
        assert_eq!(Path::Required("?config.3".into()), item);

        item.parse_cli(Some("?\"\"")).unwrap();
        assert_eq!(Path::Required("?config.3".into()), item);
    }

    #[test]
    fn path_format_config_single_item() {
        let mut buf = String::new();

        let mut item = Path::Required(StringZ::new());
        item.parse_cli(Some("A")).unwrap();
        item.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
        assert_eq!("a = A\n", buf);
    }

    #[test]
    fn path_format_config_multiple_items() {
        let mut buf = String::new();

        let mut item = Path::Required(StringZ::new());
        item.parse_cli(Some("A")).unwrap();
        item.parse_cli(Some("?B")).unwrap();
        item.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
        assert_eq!("a = ?B\n", buf);
    }

    #[test]
    fn path_cval() {
        let item = Path::Optional("x".into());
        let c = item.cval();
        assert!(c.optional);
        // SAFETY: the pointer is NUL-terminated and `item` is alive.
        let s = unsafe { std::ffi::CStr::from_ptr(c.path) };
        assert_eq!(c"x", s);
    }

    #[test]
    fn path_expand() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let base = dir.to_str().unwrap();
        let mut diags = DiagnosticList::default();

        // Absolute paths are unchanged.
        let mut p = Path::Required("/abs/path".into());
        p.expand(base, &mut diags).unwrap();
        assert_eq!(Path::Required("/abs/path".into()), p);

        // Non-existent relative paths are resolved lexically.
        let mut p = Path::Optional("a/../ghostty-does-not-exist".into());
        p.expand(base, &mut diags).unwrap();
        assert_eq!(
            Path::Optional(format!("{}/ghostty-does-not-exist", base).into()),
            p
        );

        // Existing relative paths are resolved.
        let mut p = Path::Required(".".into());
        p.expand(base, &mut diags).unwrap();
        assert_eq!(Path::Required(base.into()), p);
        assert!(diags.empty());
    }

    #[test]
    fn path_resolve() {
        assert_eq!("/a/c", resolve("/a/b", "../c"));
        assert_eq!("/x", resolve("/a/b", "/x"));
        assert_eq!("/", resolve("/", ".."));
    }

    #[test]
    fn repeatable_path_parse_cli() {
        let mut list = RepeatablePath::default();
        list.parse_cli(Some("config.1")).unwrap();
        list.parse_cli(Some("?config.2")).unwrap();
        list.parse_cli(Some("\"?config.3\"")).unwrap();

        assert_eq!(3, list.value.len());

        // Zero-length values, ignored
        list.parse_cli(Some("?")).unwrap();
        list.parse_cli(Some("\"\"")).unwrap();

        assert_eq!(3, list.value.len());

        assert_eq!(Path::Required("config.1".into()), list.value[0]);
        assert_eq!(Path::Optional("config.2".into()), list.value[1]);
        assert_eq!(Path::Required("?config.3".into()), list.value[2]);

        list.parse_cli(Some("")).unwrap();
        assert_eq!(0, list.value.len());
    }

    #[test]
    fn repeatable_path_format_config_empty() {
        let mut buf = String::new();

        let list = RepeatablePath::default();
        list.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
        assert_eq!("a = \n", buf);
    }

    #[test]
    fn repeatable_path_format_config_single_item() {
        let mut buf = String::new();

        let mut list = RepeatablePath::default();
        list.parse_cli(Some("A")).unwrap();
        list.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
        assert_eq!("a = A\n", buf);
    }

    #[test]
    fn repeatable_path_format_config_multiple_items() {
        let mut buf = String::new();

        let mut list = RepeatablePath::default();
        list.parse_cli(Some("A")).unwrap();
        list.parse_cli(Some("?B")).unwrap();
        list.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
        assert_eq!("a = A\na = ?B\n", buf);
        assert!(list.equal(&list.clone()));
    }
}
