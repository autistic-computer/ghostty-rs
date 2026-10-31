//! Port of `src/terminal/kitty/graphics_render.zig`.
//!
//! STATUS: ported

use crate::terminal::page_list::Pin;

/// A render placement is a way to position a Kitty graphics image onto
/// the screen. It is broken down into the fields that make it easier to
/// position the image using a renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// The top-left corner of the image in grid coordinates.
    pub top_left: Pin,

    /// The offset in pixels from the top-left corner of the grid cell.
    pub offset_x: u32,
    pub offset_y: u32,

    /// The source rectangle of the image to render. This doesn't have to
    /// match the size the destination size and the renderer is expected
    /// to scale the image to fit the destination size.
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,

    /// The final width/height of the image in pixels.
    pub dest_width: u32,
    pub dest_height: u32,
}

impl Placement {
    /// A placement at `top_left` with every other field zero (Zig: a
    /// struct literal with defaults).
    pub const fn new(top_left: Pin) -> Placement {
        Placement {
            top_left,
            offset_x: 0,
            offset_y: 0,
            source_x: 0,
            source_y: 0,
            source_width: 0,
            source_height: 0,
            dest_width: 0,
            dest_height: 0,
        }
    }
}
