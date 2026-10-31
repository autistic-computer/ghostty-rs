//! Port of `src/terminal/c/build_info.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: Zig's `builtin.mode` maps to `cfg!(debug_assertions)`:
//! debug builds report `debug` and release builds `release_fast` (Cargo
//! profiles have no ReleaseSafe/ReleaseSmall equivalent).

use std::ffi::{c_int, c_void};

use super::result::Result;
use crate::lib;
use crate::terminal::build_options;

const LOG_TARGET: &str = "build_info_c";

crate::c_api_enum! {
    /// C: GhosttyOptimizeMode
    pub enum OptimizeMode {
        Debug = 0,
        ReleaseSafe = 1,
        ReleaseSmall = 2,
        ReleaseFast = 3,
    }
}

crate::c_api_enum! {
    /// C: GhosttyBuildInfo
    pub enum BuildInfo {
        Invalid = 0,
        Simd = 1,
        KittyGraphics = 2,
        TmuxControlMode = 3,
        Optimize = 4,
        VersionString = 5,
        VersionMajor = 6,
        VersionMinor = 7,
        VersionPatch = 8,
        VersionPre = 9,
        VersionBuild = 10,
    }
}

/// The optimize mode of this build (Zig: `builtin.mode`).
fn optimize_mode() -> OptimizeMode {
    if cfg!(debug_assertions) {
        OptimizeMode::Debug
    } else {
        OptimizeMode::ReleaseFast
    }
}

/// C: ghostty_build_info
///
/// `out` must point to storage of the output type for `data`
/// (Zig: `BuildInfo.OutType`): `bool` for simd/kitty_graphics/
/// tmux_control_mode, `GhosttyOptimizeMode` for optimize, `GhosttyString`
/// for version_string/version_pre/version_build and `usize` for the
/// numeric version parts.
#[export_name = "ghostty_build_info"]
pub extern "C" fn get(data: c_int, out: *mut c_void) -> Result {
    let Some(data) = BuildInfo::from_int(data) else {
        log::warn!(target: LOG_TARGET, "build_info invalid data value={data}");
        return Result::InvalidValue;
    };

    // SAFETY: the caller provides storage of the documented output type.
    unsafe { get_typed(data, out) }
}

unsafe fn get_typed(data: BuildInfo, out: *mut c_void) -> Result {
    fn string(s: &'static str) -> lib::String {
        lib::String::init(s.as_bytes())
    }

    match data {
        BuildInfo::Invalid => return Result::InvalidValue,
        BuildInfo::Simd => *(out as *mut bool) = build_options::SIMD,
        BuildInfo::KittyGraphics => *(out as *mut bool) = build_options::KITTY_GRAPHICS,
        BuildInfo::TmuxControlMode => *(out as *mut bool) = build_options::TMUX_CONTROL_MODE,
        BuildInfo::Optimize => *(out as *mut OptimizeMode) = optimize_mode(),
        BuildInfo::VersionString => *(out as *mut lib::String) = string(build_options::VERSION_STRING),
        BuildInfo::VersionMajor => *(out as *mut usize) = build_options::VERSION_MAJOR,
        BuildInfo::VersionMinor => *(out as *mut usize) = build_options::VERSION_MINOR,
        BuildInfo::VersionPatch => *(out as *mut usize) = build_options::VERSION_PATCH,
        BuildInfo::VersionPre => {
            *(out as *mut lib::String) = string(build_options::VERSION_PRE.unwrap_or(""));
        }
        BuildInfo::VersionBuild => {
            *(out as *mut lib::String) = string(build_options::VERSION_BUILD.unwrap_or(""));
        }
    }

    Result::Success
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out<T>(v: &mut T) -> *mut c_void {
        v as *mut T as *mut c_void
    }

    #[test]
    fn get_simd() {
        let mut value = false;
        assert_eq!(Result::Success, get(BuildInfo::Simd as c_int, out(&mut value)));
        assert_eq!(build_options::SIMD, value);
    }

    #[test]
    fn get_kitty_graphics() {
        let mut value = false;
        assert_eq!(Result::Success, get(BuildInfo::KittyGraphics as c_int, out(&mut value)));
        assert_eq!(build_options::KITTY_GRAPHICS, value);
    }

    #[test]
    fn get_tmux_control_mode() {
        let mut value = false;
        assert_eq!(Result::Success, get(BuildInfo::TmuxControlMode as c_int, out(&mut value)));
        assert_eq!(build_options::TMUX_CONTROL_MODE, value);
    }

    #[test]
    fn get_optimize() {
        let mut value = OptimizeMode::ReleaseSmall;
        assert_eq!(Result::Success, get(BuildInfo::Optimize as c_int, out(&mut value)));
        assert_eq!(optimize_mode(), value);
    }

    #[test]
    fn get_version_string() {
        let mut value = lib::String::init(b"");
        assert_eq!(Result::Success, get(BuildInfo::VersionString as c_int, out(&mut value)));
        assert!(value.len > 0);
    }

    #[test]
    fn get_version_major() {
        let mut value = usize::MAX;
        assert_eq!(Result::Success, get(BuildInfo::VersionMajor as c_int, out(&mut value)));
        assert_eq!(build_options::VERSION_MAJOR, value);
    }

    #[test]
    fn get_version_minor() {
        let mut value = usize::MAX;
        assert_eq!(Result::Success, get(BuildInfo::VersionMinor as c_int, out(&mut value)));
        assert_eq!(build_options::VERSION_MINOR, value);
    }

    #[test]
    fn get_version_patch() {
        let mut value = usize::MAX;
        assert_eq!(Result::Success, get(BuildInfo::VersionPatch as c_int, out(&mut value)));
        assert_eq!(build_options::VERSION_PATCH, value);
    }

    #[test]
    fn get_version_pre() {
        let mut value = lib::String::init(b"");
        assert_eq!(Result::Success, get(BuildInfo::VersionPre as c_int, out(&mut value)));
    }

    #[test]
    fn get_version_build() {
        let mut value = lib::String::init(b"");
        assert_eq!(Result::Success, get(BuildInfo::VersionBuild as c_int, out(&mut value)));
    }

    #[test]
    fn get_invalid() {
        assert_eq!(Result::InvalidValue, get(BuildInfo::Invalid as c_int, std::ptr::null_mut()));
        // Rust port: out-of-range values are rejected like Zig's runtime
        // safety check.
        assert_eq!(Result::InvalidValue, get(99, std::ptr::null_mut()));
    }
}
