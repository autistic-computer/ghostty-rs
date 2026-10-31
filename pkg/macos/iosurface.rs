//! Port of `src/macos/iosurface.zig`.
//!
//! STATUS: ported

#![allow(clippy::module_inception)]

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "iosurface/c.rs"]
pub mod c;
#[path = "iosurface/iosurface.rs"]
pub mod iosurface;
// @generated-mods-end

pub use self::iosurface::{IOSurface, Properties};
