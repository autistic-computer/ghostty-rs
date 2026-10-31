//! Port of `pkg/harfbuzz/version.zig`.
//!
//! STATUS: ported

use std::ffi::CStr;
use std::os::raw::c_uint;

use super::c;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub micro: u32,
}

/// Returns library version as three integer components.
pub fn version() -> Version {
    let mut major: c_uint = 0;
    let mut minor: c_uint = 0;
    let mut micro: c_uint = 0;
    // SAFETY: valid out pointers.
    unsafe { c::hb_version(&mut major, &mut minor, &mut micro) };
    Version {
        major,
        minor,
        micro,
    }
}

/// Tests the library version against a minimum value, as three integer components.
pub fn version_at_least(vsn: Version) -> bool {
    // SAFETY: no preconditions.
    unsafe { c::hb_version_atleast(vsn.major, vsn.minor, vsn.micro) > 0 }
}

/// Returns library version as a string with three components.
pub fn version_string() -> &'static CStr {
    // SAFETY: returns a static NUL-terminated string.
    unsafe { CStr::from_ptr(c::hb_version_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_basics() {
        // Should be able to get the version
        let vsn = version();
        assert!(vsn.major > 0);

        // Should be at least version 1
        assert!(version_at_least(Version {
            major: 1,
            minor: 0,
            micro: 0,
        }));
    }
}
