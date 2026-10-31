//! Port of `src/apprt/gtk/adw_version.zig`.
//!
//! STATUS: ported
//!
//! Zig imports `adwaita.h` to read the `ADW_*_VERSION` macros of the headers
//! it builds against. The port gets the same values from `build.rs`, which
//! asks pkg-config for the version of `libadwaita-1` it links against and
//! exports it as `GHOSTTY_ADW_VERSION`.
//!
//! Zig refuses to compile against headers older than [`MINIMUM_VERSION`]
//! (a `@compileError`). The port targets libadwaita 1.5 (the libadwaita-rs
//! `v1_5` feature), so that check is not done at build time:
//! [`log_version`] still reports [`AdwaitaMinimumVersionError`] at runtime,
//! exactly as in Zig.
#![cfg(feature = "gtk")]

use std::cmp::Ordering;

use super::gtk_version::{order, parse_version, version};
use crate::terminal::build_options::SemanticVersion;

/// The version of the libadwaita headers (pkg-config module `libadwaita-1`)
/// we build against.
pub const COMPTIME_VERSION: SemanticVersion = parse_version(env!("GHOSTTY_ADW_VERSION"));

pub const MINIMUM_VERSION: SemanticVersion = version(1, 7, 0);

pub fn get_runtime_version() -> SemanticVersion {
    // The safe libadwaita-rs wrappers assert that GTK is initialized, which
    // these C functions do not need.
    // SAFETY: the adw_get_*_version functions only return constants.
    unsafe {
        version(
            libadwaita::ffi::adw_get_major_version() as usize,
            libadwaita::ffi::adw_get_minor_version() as usize,
            libadwaita::ffi::adw_get_micro_version() as usize,
        )
    }
}

/// `error{AdwaitaMinimumVersion}`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdwaitaMinimumVersionError;

impl std::fmt::Display for AdwaitaMinimumVersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AdwaitaMinimumVersion")
    }
}

impl std::error::Error for AdwaitaMinimumVersionError {}

pub fn log_version() -> Result<(), AdwaitaMinimumVersionError> {
    let runtime_version = get_runtime_version();
    log::info!(
        target: "gtk",
        "libadwaita version build={} runtime={}",
        COMPTIME_VERSION,
        runtime_version,
    );
    if order(runtime_version, MINIMUM_VERSION) == Ordering::Less {
        return Err(AdwaitaMinimumVersionError);
    }
    Ok(())
}

/// Verifies that the running libadwaita version is at least the given
/// version. This will return false if Ghostty is configured to not build with
/// libadwaita.
///
/// This can be run in both a comptime and runtime context. If it is run in a
/// comptime context, it will only check the version in the headers. If it is
/// run in a runtime context, it will check the actual version of the library we
/// are linked against. So generally  you probably want to do both checks!
///
/// This is inlined so that the comptime checks will disable the runtime checks
/// if the comptime checks fail.
///
/// (Rust has no `@inComptime()`; the port always does the runtime check once
/// the header check passes.)
#[inline]
pub fn at_least(major: u16, minor: u16, micro: u16) -> bool {
    // If our header has lower versions than the given version, we can return
    // false immediately. This prevents us from compiling against unknown
    // symbols and makes runtime checks very slightly faster.
    if order(
        COMPTIME_VERSION,
        version(major as usize, minor as usize, micro as usize),
    ) == Ordering::Less
    {
        return false;
    }

    runtime_at_least(major, minor, micro)
}

/// Verifies that the libadwaita version at runtime is at least the given version.
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

#[cfg(test)]
mod tests {
    use super::*;

    const MAJ: u16 = COMPTIME_VERSION.major as u16;
    const MIN: u16 = COMPTIME_VERSION.minor as u16;
    const MIC: u16 = COMPTIME_VERSION.patch as u16;

    #[test]
    fn version_at_least() {
        let funs: [fn(u16, u16, u16) -> bool; 2] = [at_least, runtime_at_least];
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
}
