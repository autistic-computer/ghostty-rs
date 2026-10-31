//! Port of `src/macos/graphics.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "graphics/affine_transform.rs"]
pub mod affine_transform;
#[path = "graphics/bitmap_context.rs"]
pub mod bitmap_context;
#[path = "graphics/c.rs"]
pub mod c;
#[path = "graphics/color_space.rs"]
pub mod color_space;
#[path = "graphics/context.rs"]
pub mod context;
#[path = "graphics/font.rs"]
pub mod font;
#[path = "graphics/geometry.rs"]
pub mod geometry;
#[path = "graphics/image.rs"]
pub mod image;
#[path = "graphics/path.rs"]
pub mod path;
// @generated-mods-end

pub use affine_transform::AffineTransform;
pub use bitmap_context::BitmapContext;
pub use color_space::ColorSpace;
pub use context::{Context, TextDrawingMode};
pub use font::Glyph;
pub use geometry::{Point, Rect, Size};
pub use image::{BitmapInfo, ImageAlphaInfo};
pub use path::{MutablePath, Path};
