//! Port of `src/macos/graphics/bitmap_context.zig`.
//!
//! STATUS: ported

use std::ffi::{c_uint, c_void};
use std::ptr;

use super::c;
use super::color_space::ColorSpace;
use super::context::Context;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CGContextRef` created by `CGBitmapContextCreate`.
    BitmapContext
);

/// `pub const context = Context(BitmapContext);`
impl Context for BitmapContext {}

impl BitmapContext {
    /// Create a bitmap context. If `data` is given it must outlive the
    /// context.
    pub fn create(
        data: Option<&mut [u8]>,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: &ColorSpace,
        opts: c_uint,
    ) -> Result<&'static BitmapContext, Error> {
        // SAFETY: data (if any) is a valid buffer; space is live.
        unsafe {
            created(c::CGBitmapContextCreate(
                data.map_or(ptr::null_mut(), |d| d.as_mut_ptr() as *mut c_void),
                width,
                height,
                bits_per_component,
                bytes_per_row,
                space.as_ptr(),
                opts,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{AffineTransform, TextDrawingMode};
    use super::*;

    #[test]
    fn bitmap_context() {
        let cs = ColorSpace::create_device_gray().unwrap();
        let ctx = BitmapContext::create(None, 80, 80, 8, 80, cs, 0).unwrap();
        ctx.set_should_antialias(true);
        ctx.set_should_smooth_fonts(false);
        ctx.set_gray_fill_color(1.0, 1.0);
        ctx.set_gray_stroke_color(1.0, 1.0);
        ctx.set_text_drawing_mode(TextDrawingMode::Fill);
        ctx.set_text_matrix(AffineTransform::identity());
        ctx.set_text_position(0.0, 0.0);
        Context::release(ctx);
        cs.release();
    }
}
