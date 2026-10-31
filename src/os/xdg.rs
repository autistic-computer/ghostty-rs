//! Port of `src/os/xdg.zig`.
//!
//! STATUS: ported
//!
//! Implementation of the XDG Base Directory specification
//! (https://specifications.freedesktop.org/basedir-spec/basedir-spec-latest.html)

use std::ffi::CStr;
use std::fmt;
use std::path::Path;

use super::homedir;
use super::EnvironMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options<'a> {
    /// Subdirectories to join to the base. This avoids extra allocations
    /// when building up the directory. This is commonly the application.
    pub subdir: Option<&'a str>,

    /// The home directory for the user. If this is not set, we will attempt
    /// to look it up which is an expensive process. By setting this, you
    /// can avoid lookups.
    pub home: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NoHomeDir,
    Home(homedir::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

/// Get the XDG user config directory.
pub fn config(environ_map: &EnvironMap, opts: Options<'_>) -> Result<String, Error> {
    dir(
        environ_map,
        opts,
        InternalOptions {
            env: "XDG_CONFIG_HOME",
            windows_env: "LOCALAPPDATA",
            default_subdir: ".config",
        },
    )
}

/// Get the XDG cache directory.
pub fn cache(environ_map: &EnvironMap, opts: Options<'_>) -> Result<String, Error> {
    dir(
        environ_map,
        opts,
        InternalOptions {
            env: "XDG_CACHE_HOME",
            windows_env: "LOCALAPPDATA",
            default_subdir: ".cache",
        },
    )
}

/// Get the XDG state directory.
pub fn state(environ_map: &EnvironMap, opts: Options<'_>) -> Result<String, Error> {
    dir(
        environ_map,
        opts,
        InternalOptions {
            env: "XDG_STATE_HOME",
            windows_env: "LOCALAPPDATA",
            default_subdir: ".local/state",
        },
    )
}

struct InternalOptions {
    env: &'static str,
    windows_env: &'static str,
    default_subdir: &'static str,
}

/// Zig: `std.fs.path.join`: join non-empty parts with the separator,
/// avoiding doubled separators.
fn path_join(parts: &[&str]) -> String {
    let sep = std::path::MAIN_SEPARATOR;
    let mut out = String::new();
    for part in parts.iter().filter(|p| !p.is_empty()) {
        if !out.is_empty() {
            let out_has_sep = out.ends_with(sep) || out.ends_with('/');
            let part_has_sep = part.starts_with(sep) || part.starts_with('/');
            if out_has_sep && part_has_sep {
                out.push_str(&part[1..]);
                continue;
            }
            if !out_has_sep && !part_has_sep {
                out.push(sep);
            }
        }
        out.push_str(part);
    }
    out
}

/// Unified helper to get XDG directories that follow a common pattern.
fn dir(
    environ_map: &EnvironMap,
    opts: Options<'_>,
    internal_opts: InternalOptions,
) -> Result<String, Error> {
    // If we have a cached home dir, use that.
    if let Some(home) = opts.home {
        return Ok(path_join(&[
            home,
            internal_opts.default_subdir,
            opts.subdir.unwrap_or(""),
        ]));
    }

    // First check the env var. On Windows we treat `LOCALAPPDATA` as a
    // fallback for `XDG_CONFIG_HOME`
    let env: &str = if cfg!(windows) {
        environ_map
            .get(internal_opts.env)
            .or_else(|| environ_map.get(internal_opts.windows_env))
            .map(String::as_str)
            .unwrap_or("")
    } else {
        environ_map
            .get(internal_opts.env)
            .map(String::as_str)
            .unwrap_or("")
    };

    if !env.is_empty() {
        // If we have a subdir, then we use the env as-is to avoid a copy.
        if let Some(subdir) = opts.subdir {
            return Ok(path_join(&[env, subdir]));
        }

        return Ok(env.to_string());
    }

    // Get our home dir
    let mut buf = [0u8; 1024];
    if let Some(home) = homedir::home(environ_map, &mut buf).map_err(Error::Home)? {
        return Ok(path_join(&[
            home,
            internal_opts.default_subdir,
            opts.subdir.unwrap_or(""),
        ]));
    }

    Err(Error::NoHomeDir)
}

/// Parses the xdg-terminal-exec specification. This expects argv[0] to
/// be "xdg-terminal-exec".
pub fn parse_terminal_exec<'a, 'b>(argv: &'a [&'b CStr]) -> Option<&'a [&'b CStr]> {
    let arg0 = argv.first()?.to_bytes();
    let basename = Path::new(std::str::from_utf8(arg0).ok()?).file_name()?;
    if basename != "xdg-terminal-exec" {
        return None;
    }

    // We expect at least one argument
    if argv.len() < 2 {
        return Some(&[]);
    }

    // If the first argument is "-e" we skip it.
    let start: usize = if argv[1].to_bytes() == b"-e" { 2 } else { 1 };
    Some(&argv[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir() {
        let environ_map = super::super::environ_map();
        let value = config(&environ_map, Options::default()).unwrap();
        assert!(!value.is_empty());
    }

    #[test]
    fn cache_directory_paths() {
        let mock_home = if cfg!(windows) {
            "C:\\Users\\test"
        } else {
            "/Users/test"
        };
        let environ_map = super::super::environ_map();

        // Test when XDG_CACHE_HOME is not set
        {
            // Test base path
            {
                let cache_path = cache(
                    &environ_map,
                    Options {
                        home: Some(mock_home),
                        ..Default::default()
                    },
                )
                .unwrap();
                let expected = path_join(&[mock_home, ".cache"]);
                assert_eq!(expected, cache_path);
            }

            // Test with subdir
            {
                let cache_path = cache(
                    &environ_map,
                    Options {
                        home: Some(mock_home),
                        subdir: Some("ghostty"),
                    },
                )
                .unwrap();
                let expected = path_join(&[mock_home, ".cache", "ghostty"]);
                assert_eq!(expected, cache_path);
            }
        }
    }

    type DirFn = fn(&EnvironMap, Options<'_>) -> Result<String, Error>;

    const CASES: [(&str, DirFn, &str); 3] = [
        ("XDG_CONFIG_HOME", config, ".config"),
        ("XDG_CACHE_HOME", cache, ".cache"),
        ("XDG_STATE_HOME", state, ".local/state"),
    ];

    #[cfg(not(windows))]
    #[test]
    fn fallback_when_xdg_env_empty() {
        for (name, func, default_subdir) in CASES {
            let mut environ_map = super::super::environ_map();
            let temp_home = "/tmp/ghostty-test-home";
            environ_map.insert("HOME".into(), temp_home.into());

            let expected = path_join(&[temp_home, default_subdir]);

            // Test with empty string - should fallback to home
            environ_map.insert(name.into(), "".into());
            let actual = func(&environ_map, Options::default()).unwrap();

            assert_eq!(expected, actual);
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn fallback_when_xdg_env_empty_and_subdir() {
        for (name, func, default_subdir) in CASES {
            let mut environ_map = super::super::environ_map();
            let temp_home = "/tmp/ghostty-test-home";
            environ_map.insert("HOME".into(), temp_home.into());

            let expected = path_join(&[temp_home, default_subdir, "ghostty"]);

            // Test with empty string - should fallback to home
            environ_map.insert(name.into(), "".into());
            let actual = func(
                &environ_map,
                Options {
                    subdir: Some("ghostty"),
                    ..Default::default()
                },
            )
            .unwrap();

            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn parse_terminal_exec_test() {
        {
            let actual = parse_terminal_exec(&[c"a", c"b", c"c"]);
            assert!(actual.is_none());
        }
        {
            let actual = parse_terminal_exec(&[c"xdg-terminal-exec"]).unwrap();
            assert!(actual.is_empty());
        }
        {
            let actual = parse_terminal_exec(&[c"xdg-terminal-exec", c"a", c"b", c"c"]).unwrap();
            assert_eq!(actual, &[c"a", c"b", c"c"]);
        }
        {
            let actual =
                parse_terminal_exec(&[c"xdg-terminal-exec", c"-e", c"a", c"b", c"c"]).unwrap();
            assert_eq!(actual, &[c"a", c"b", c"c"]);
        }
        {
            let actual =
                parse_terminal_exec(&[c"xdg-terminal-exec", c"a", c"-e", c"b", c"c"]).unwrap();
            assert_eq!(actual, &[c"a", c"-e", c"b", c"c"]);
        }
    }

    #[test]
    fn path_join_matches_zig() {
        assert_eq!("/a/b", path_join(&["/a", "b", ""]));
        assert_eq!("/a/b", path_join(&["/a/", "/b"]));
    }
}
