//! Module grouping (no Zig counterpart file; mirrors a directory).

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "src/main.rs"]
pub mod src;
// @generated-mods-end

// Re-export the package root so callers can write `pkg::wuffs::png` like the
// Zig `@import("wuffs").png`.
pub use self::src::*;
