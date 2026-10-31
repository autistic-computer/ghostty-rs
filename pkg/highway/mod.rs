//! Module grouping (no Zig counterpart file; mirrors a directory).

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "src/main.rs"]
pub mod src;
// @generated-mods-end

// Re-export the package root (`@import("highway")`).
pub use self::src::*;
