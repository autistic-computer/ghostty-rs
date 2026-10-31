//! Port of `src/apprt/gtk/build/info.zig`.
//!
//! STATUS: ported
//!
//! Zig switches on `builtin.mode` (`.debug, .safe` vs `.fast, .small`); the
//! port uses `debug_assertions`, which Cargo enables for the dev profile.
//!
//! This file is also compiled into `build.rs` (via `#[path]`), so it must
//! not depend on anything else in the crate.

/// Base application ID
pub const BASE_APPLICATION_ID: &str = "com.mitchellh.ghostty";

/// GTK application ID
pub const APPLICATION_ID: &str = if cfg!(debug_assertions) {
    "com.mitchellh.ghostty-debug"
} else {
    BASE_APPLICATION_ID
};

pub const RESOURCE_PATH: &str = "/com/mitchellh/ghostty";

/// GTK object path
pub const OBJECT_PATH: &str = if cfg!(debug_assertions) {
    "/com/mitchellh/ghostty_debug"
} else {
    RESOURCE_PATH
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_derive_from_base() {
        assert!(APPLICATION_ID.starts_with(BASE_APPLICATION_ID));
        assert!(OBJECT_PATH.starts_with(RESOURCE_PATH));
        assert_eq!(
            RESOURCE_PATH.trim_start_matches('/').replace('/', "."),
            BASE_APPLICATION_ID
        );
    }
}
