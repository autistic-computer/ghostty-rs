//! Port of `src/apprt/gtk/gtk_version.zig`.
//!
//! STATUS: ported
//!
//! Zig imports `gtk/gtk.h` to read the `GTK_*_VERSION` macros of the headers
//! it builds against. The port gets the same values from `build.rs`, which
//! asks pkg-config for the version of the `gtk4` it links against and exports
//! it as `GHOSTTY_GTK_VERSION`.
//!
//! Zig refuses to compile against headers older than [`MINIMUM_VERSION`]
//! (a `@compileError`). The port targets GTK 4.14 (the gtk-rs `v4_14`
//! feature), so that check is not done at build time: [`log_version`] still
//! reports [`GtkMinimumVersionError`] at runtime, exactly as in Zig.
#![cfg(feature = "gtk")]

use std::cmp::Ordering;

use crate::terminal::build_options::SemanticVersion;

/// Parses a `major.minor.patch` version string at compile time.
///
/// Panics (a compile error in const context) on malformed input.
pub(crate) const fn parse_version(s: &str) -> SemanticVersion {
    let bytes = s.as_bytes();
    let mut parts = [0usize; 3];
    let mut part = 0;
    let mut i = 0;
    let mut digits = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'.' {
            assert!(digits > 0, "malformed version");
            part += 1;
            assert!(part < 3, "malformed version");
            digits = 0;
        } else {
            assert!(c >= b'0' && c <= b'9', "malformed version");
            parts[part] = parts[part] * 10 + (c - b'0') as usize;
            digits += 1;
        }
        i += 1;
    }
    assert!(digits > 0 && part == 2, "malformed version");
    SemanticVersion {
        major: parts[0],
        minor: parts[1],
        patch: parts[2],
        pre: None,
        build: None,
    }
}

/// `std.SemanticVersion.order` restricted to the numeric components (the
/// versions here never have a pre-release or build part).
pub(crate) fn order(a: SemanticVersion, b: SemanticVersion) -> Ordering {
    (a.major, a.minor, a.patch).cmp(&(b.major, b.minor, b.patch))
}

/// Shorthand for a numeric `SemanticVersion`.
pub(crate) const fn version(major: usize, minor: usize, patch: usize) -> SemanticVersion {
    SemanticVersion {
        major,
        minor,
        patch,
        pre: None,
        build: None,
    }
}

/// The version of the GTK headers (pkg-config module `gtk4`) we build
/// against.
pub const COMPTIME_VERSION: SemanticVersion = parse_version(env!("GHOSTTY_GTK_VERSION"));

pub const MINIMUM_VERSION: SemanticVersion = version(4, 18, 0);

pub fn get_runtime_version() -> SemanticVersion {
    version(
        gtk4::major_version() as usize,
        gtk4::minor_version() as usize,
        gtk4::micro_version() as usize,
    )
}

/// `error{GtkMinimumVersion}`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GtkMinimumVersionError;

impl std::fmt::Display for GtkMinimumVersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GtkMinimumVersion")
    }
}

impl std::error::Error for GtkMinimumVersionError {}

pub fn log_version() -> Result<(), GtkMinimumVersionError> {
    let runtime_version = get_runtime_version();
    log::info!(
        target: "gtk",
        "GTK version build={} runtime={}",
        COMPTIME_VERSION,
        runtime_version,
    );
    if order(runtime_version, MINIMUM_VERSION) == Ordering::Less {
        return Err(GtkMinimumVersionError);
    }
    Ok(())
}

/// Verifies that the GTK version is at least the given version.
///
/// This can be run in both a comptime and runtime context. If it is run in a
/// comptime context, it will only check the version in the headers. If it is
/// run in a runtime context, it will check the actual version of the library we
/// are linked against.
///
/// This function should be used in cases where the version check would affect
/// code generation, such as using symbols that are only available beyond a
/// certain version. For checks which only depend on GTK's runtime behavior,
/// use `runtimeAtLeast`.
///
/// This is inlined so that the comptime checks will disable the runtime checks
/// if the comptime checks fail.
///
/// (Rust has no `@inComptime()`; the port always does the runtime check once
/// the header check passes. In Rust, code generation that depends on the GTK
/// version is selected with the gtk-rs `v4_*` Cargo features instead.)
#[inline]
pub fn at_least(major: u16, minor: u16, micro: u16) -> bool {
    // If our header has lower versions than the given version,
    // we can return false immediately. This prevents us from
    // compiling against unknown symbols and makes runtime checks
    // very slightly faster.
    if order(
        COMPTIME_VERSION,
        version(major as usize, minor as usize, micro as usize),
    ) == Ordering::Less
    {
        return false;
    }

    runtime_at_least(major, minor, micro)
}

/// Verifies that the GTK version at runtime is at least the given version.
///
/// This function should be used in cases where the only the runtime behavior
/// is affected by the version check. For checks which would affect code
/// generation, use `atLeast`.
#[inline]
pub fn runtime_at_least(major: u16, minor: u16, micro: u16) -> bool {
    // We use the functions instead of the constants such as c.GTK_MINOR_VERSION
    // because the function gets the actual runtime version.
    let runtime_version = get_runtime_version();
    order(
        runtime_version,
        version(major as usize, minor as usize, micro as usize),
    ) != Ordering::Less
}

#[inline]
pub fn runtime_until(major: u16, minor: u16, micro: u16) -> bool {
    let runtime_version = get_runtime_version();
    order(
        runtime_version,
        version(major as usize, minor as usize, micro as usize),
    ) == Ordering::Less
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAJ: u16 = COMPTIME_VERSION.major as u16;
    const MIN: u16 = COMPTIME_VERSION.minor as u16;
    const MIC: u16 = COMPTIME_VERSION.patch as u16;

    #[test]
    fn at_least() {
        let funs: [fn(u16, u16, u16) -> bool; 2] = [super::at_least, runtime_at_least];
        for fun in funs {
            assert!(fun(MAJ, MIN, MIC));

            assert!(!fun(MAJ, MIN, MIC + 1));
            assert!(!fun(MAJ, MIN + 1, MIC));
            assert!(!fun(MAJ + 1, MIN, MIC));

            assert!(fun(MAJ - 1, MIN, MIC));
            assert!(fun(MAJ - 1, MIN + 1, MIC));
            assert!(fun(MAJ - 1, MIN, MIC + 1));

            assert!(fun(MAJ, MIN - 1, MIC + 1));
        }
    }

    #[test]
    fn runtime_until() {
        // This is an array in case we add a comptime variant.
        let funs: [fn(u16, u16, u16) -> bool; 1] = [super::runtime_until];
        for fun in funs {
            assert!(!fun(MAJ, MIN, MIC));

            assert!(fun(MAJ, MIN, MIC + 1));
            assert!(fun(MAJ, MIN + 1, MIC));
            assert!(fun(MAJ + 1, MIN, MIC));

            assert!(!fun(MAJ - 1, MIN, MIC));
            assert!(!fun(MAJ - 1, MIN + 1, MIC));
            assert!(!fun(MAJ - 1, MIN, MIC + 1));

            assert!(!fun(MAJ, MIN - 1, MIC + 1));
        }
    }

    #[test]
    fn parse_version_components() {
        let v = parse_version("4.14.5");
        assert_eq!((v.major, v.minor, v.patch), (4, 14, 5));
    }
}
