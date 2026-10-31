//! Port of `src/macos/video.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "video/c.rs"]
pub mod c;
#[path = "video/display_link.rs"]
pub mod display_link;
#[path = "video/pixel_format.rs"]
pub mod pixel_format;
// @generated-mods-end

pub use display_link::DisplayLink;
pub use pixel_format::PixelFormat;
