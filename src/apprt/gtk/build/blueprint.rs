//! Port of `src/apprt/gtk/build/blueprint.zig`.
//!
//! STATUS: ported
//!
//! Checks that `libadwaita` is at least the given version and that
//! `blueprint-compiler` is on the PATH and new enough. The blueprints
//! themselves are compiled by `blueprint-compiler` directly from the build
//! system; see `gtkDistResources` in `src/build/SharedDeps.zig`.
//!
//! Usage: blueprint.zig <major> <minor> <stamp>
//!
//! Example: blueprint.zig 1 5 blueprint-check.stamp
//!
//! `<stamp>` is written when every check passes, so the build system has an
//! output to cache this step by.
//!
//! In the port the blueprints are compiled ahead of time by
//! `tools/gen_gtk_ui.sh` (which performs the same `blueprint-compiler`
//! version check) and the `.ui` output is committed, so Cargo builds never
//! run this. [`main`] is kept as a library function taking the arguments
//! (without argv0); it returns an error instead of exiting the process.

use std::cmp::Ordering;
use std::process::Command;

use crate::apprt::gtk::adw_version;
use crate::apprt::gtk::gtk_version::{order, version};
use crate::terminal::build_options::SemanticVersion;

pub const BLUEPRINT_COMPILER_HELP: &str = "
When building from a Git checkout, Ghostty requires
version {} or newer of `blueprint-compiler` as a
build-time dependency. Please install it, ensure that it
is available on your PATH, and then retry building Ghostty.
See `HACKING.md` for more details.

This message should *not* appear for normal users, who
should build Ghostty from official release tarballs instead.
Please consult https://ghostty.org/docs/install/build for
more information on the recommended build instructions.
";

const ADWAITA_VERSION: SemanticVersion = adw_version::COMPTIME_VERSION;

const REQUIRED_BLUEPRINT_VERSION: SemanticVersion = version(0, 20, 0);

/// The errors of the check binary. Where Zig prints a message and calls
/// `std.process.exit(1)`, the port prints the same message and returns
/// [`BlueprintError::Exit`].
#[derive(Debug)]
pub enum BlueprintError {
    NoMajorVersion,
    NoMinorVersion,
    NoStamp,
    InvalidVersion,
    /// The check failed; the reason has been printed to stderr.
    Exit,
    Io(std::io::Error),
}

impl std::fmt::Display for BlueprintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlueprintError::NoMajorVersion => f.write_str("NoMajorVersion"),
            BlueprintError::NoMinorVersion => f.write_str("NoMinorVersion"),
            BlueprintError::NoStamp => f.write_str("NoStamp"),
            BlueprintError::InvalidVersion => f.write_str("InvalidVersion"),
            BlueprintError::Exit => f.write_str("blueprint check failed"),
            BlueprintError::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for BlueprintError {}

fn help() -> String {
    BLUEPRINT_COMPILER_HELP.replacen("{}", &REQUIRED_BLUEPRINT_VERSION.to_string(), 1)
}

/// `std.SemanticVersion.parse` for the plain `major.minor.patch` form.
fn parse_semver(s: &str) -> Option<SemanticVersion> {
    let mut it = s.splitn(3, '.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch_str = it.next()?;
    // Drop any pre-release/build suffix.
    let end = patch_str
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(patch_str.len());
    let patch = patch_str[..end].parse().ok()?;
    Some(version(major, minor, patch))
}

pub fn main(args: &[&str]) -> Result<(), BlueprintError> {
    let mut it = args.iter();
    let arg_major = it.next().ok_or(BlueprintError::NoMajorVersion)?;
    let arg_minor = it.next().ok_or(BlueprintError::NoMinorVersion)?;
    let stamp = it.next().ok_or(BlueprintError::NoStamp)?;

    let required_adwaita_version = version(
        arg_major.parse::<u8>().map_err(|_| BlueprintError::InvalidVersion)? as usize,
        arg_minor.parse::<u8>().map_err(|_| BlueprintError::InvalidVersion)? as usize,
        0,
    );
    if order(ADWAITA_VERSION, required_adwaita_version) == Ordering::Less {
        eprint!(
            "`libadwaita` is too old.\n\
             \n\
             Ghostty requires a version {required_adwaita_version} or newer of `libadwaita` to\n\
             compile its blueprints. Please install it, ensure that it is\n\
             available on your PATH, and then retry building Ghostty."
        );
        return Err(BlueprintError::Exit);
    }

    // Version checks
    {
        let blueprint_compiler = match Command::new("blueprint-compiler").arg("--version").output() {
            Ok(v) => v,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                eprint!("`blueprint-compiler` not found.{}", help());
                return Err(BlueprintError::Exit);
            }
            Err(err) => return Err(BlueprintError::Io(err)),
        };

        if !blueprint_compiler.status.success() {
            return Err(BlueprintError::Exit);
        }

        let stdout = String::from_utf8_lossy(&blueprint_compiler.stdout);
        let version = parse_semver(stdout.trim()).ok_or(BlueprintError::InvalidVersion)?;
        if order(version, REQUIRED_BLUEPRINT_VERSION) == Ordering::Less {
            eprint!("`blueprint-compiler` is the wrong version.{}", help());
            return Err(BlueprintError::Exit);
        }
    }

    // Everything passed.
    std::fs::File::create(stamp).map_err(BlueprintError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_args() {
        assert!(matches!(main(&[]), Err(BlueprintError::NoMajorVersion)));
        assert!(matches!(main(&["1"]), Err(BlueprintError::NoMinorVersion)));
        assert!(matches!(main(&["1", "5"]), Err(BlueprintError::NoStamp)));
    }

    #[test]
    fn adwaita_too_old() {
        assert!(matches!(
            main(&["99", "0", "/nonexistent/stamp"]),
            Err(BlueprintError::Exit)
        ));
    }

    #[test]
    fn semver_parse() {
        let v = parse_semver("0.20.4").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (0, 20, 4));
        assert!(parse_semver("uninstalled").is_none());
        assert!(help().contains("version 0.20.0 or newer"));
    }
}
