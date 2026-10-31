//! Port of `src/sentry/level.zig`.
//!
//! STATUS: ported

use std::ffi::c_int;

/// sentry_level_t
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug = -1,
    Info = 0,
    Warning = 1,
    Err = 2,
    Fatal = 3,
}

impl Level {
    /// `@backingInt`
    pub const fn to_int(self) -> c_int {
        self as c_int
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::sentry::c;

    #[test]
    fn matches_c_constants() {
        assert_eq!(Level::Debug.to_int(), c::SENTRY_LEVEL_DEBUG);
        assert_eq!(Level::Info.to_int(), c::SENTRY_LEVEL_INFO);
        assert_eq!(Level::Warning.to_int(), c::SENTRY_LEVEL_WARNING);
        assert_eq!(Level::Err.to_int(), c::SENTRY_LEVEL_ERROR);
        assert_eq!(Level::Fatal.to_int(), c::SENTRY_LEVEL_FATAL);
    }
}
