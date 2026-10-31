//! Port of `src/macos/animation.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "animation/c.rs"]
pub mod c;
// @generated-mods-end

use std::ffi::c_void;

// https://developer.apple.com/documentation/quartzcore/calayer/contents_gravity_values?language=objc
#[link(name = "QuartzCore", kind = "framework")]
extern "C" {
    pub static kCAGravityTopLeft: *const c_void;
    pub static kCAGravityBottomLeft: *const c_void;
    pub static kCAGravityCenter: *const c_void;
}
