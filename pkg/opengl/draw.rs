//! Port of `pkg/opengl/draw.zig`.
//!
//! STATUS: ported

use std::os::raw::c_void;

use super::c;
use super::errors::{self, Error};
use super::glad;
use super::primitives::Primitive;
use super::texture;

pub fn clear_color(r: f32, g: f32, b: f32, a: f32) {
    glad::call!(ClearColor(r, g, b, a));
}

pub fn clear(mask: c::GLbitfield) {
    glad::call!(Clear(mask));
}

pub fn draw_arrays(mode: c::GLenum, first: c::GLint, count: c::GLsizei) -> Result<(), Error> {
    glad::call!(DrawArrays(mode, first, count));
    errors::get_error()
}

pub fn draw_arrays_instanced(
    mode: Primitive,
    first: c::GLint,
    count: c::GLsizei,
    primcount: c::GLsizei,
) -> Result<(), Error> {
    glad::call!(DrawArraysInstanced(
        mode as c::GLenum,
        first,
        count,
        primcount
    ));
    errors::get_error()
}

pub fn draw_elements(
    mode: c::GLenum,
    count: c::GLsizei,
    typ: c::GLenum,
    offset: usize,
) -> Result<(), Error> {
    let offset_ptr: *const c_void = if offset == 0 {
        std::ptr::null()
    } else {
        offset as *const c_void
    };
    glad::call!(DrawElements(mode, count, typ, offset_ptr));
    errors::get_error()
}

pub fn draw_elements_instanced(
    mode: c::GLenum,
    count: c::GLsizei,
    typ: c::GLenum,
    primcount: c::GLsizei,
) -> Result<(), Error> {
    glad::call!(DrawElementsInstanced(
        mode,
        count,
        typ,
        std::ptr::null(),
        primcount
    ));
    errors::get_error()
}

pub fn enable(cap: c::GLenum) -> Result<(), Error> {
    glad::call!(Enable(cap));
    errors::get_error()
}

pub fn disable(cap: c::GLenum) -> Result<(), Error> {
    glad::call!(Disable(cap));
    errors::get_error()
}

pub fn front_face(mode: c::GLenum) -> Result<(), Error> {
    glad::call!(FrontFace(mode));
    errors::get_error()
}

pub fn blend_func(sfactor: c::GLenum, dfactor: c::GLenum) -> Result<(), Error> {
    glad::call!(BlendFunc(sfactor, dfactor));
    errors::get_error()
}

pub fn viewport(
    x: c::GLint,
    y: c::GLint,
    width: c::GLsizei,
    height: c::GLsizei,
) -> Result<(), Error> {
    glad::call!(Viewport(x, y, width, height));
    errors::get_error()
}

/// # Safety
/// `data` must be null or point to a buffer large enough for the requested
/// pixels (or be an offset into a bound pixel pack buffer).
#[allow(clippy::too_many_arguments)]
pub unsafe fn read_pixels(
    x: c::GLint,
    y: c::GLint,
    width: c::GLsizei,
    height: c::GLsizei,
    format: texture::Format,
    typ: texture::DataType,
    data: *mut c_void,
) -> Result<(), Error> {
    glad::call!(ReadPixels(x, y, width, height, format.0, typ.0, data));
    errors::get_error()
}

#[allow(clippy::too_many_arguments)]
pub fn blit_framebuffer(
    src_x0: c::GLint,
    src_y0: c::GLint,
    src_x1: c::GLint,
    src_y1: c::GLint,
    dst_x0: c::GLint,
    dst_y0: c::GLint,
    dst_x1: c::GLint,
    dst_y1: c::GLint,
    mask: BlitMask,
    filter: texture::MagFilter,
) -> Result<(), Error> {
    glad::call!(BlitFramebuffer(
        src_x0,
        src_y0,
        src_x1,
        src_y1,
        dst_x0,
        dst_y0,
        dst_x1,
        dst_y1,
        mask.to_bits(),
        filter as c::GLenum,
    ));
    errors::get_error()
}

/// Zig accepts any integer type (`anytype`).
pub fn pixel_store(mode: c::GLenum, value: c::GLint) -> Result<(), Error> {
    glad::call!(PixelStorei(mode, value));
    errors::get_error()
}

pub fn finish() {
    glad::call!(Finish());
}

pub fn flush() {
    glad::call!(Flush());
}

/// Zig: `packed struct(c.GLbitfield)`; [`BlitMask::to_bits`] is the
/// `@bitCast`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlitMask {
    /// Bit 8.
    pub depth_buffer_bit: bool,
    /// Bit 10.
    pub stencil_buffer_bit: bool,
    /// Bit 14.
    pub color_buffer_bit: bool,
}

impl BlitMask {
    pub const fn to_bits(self) -> c::GLbitfield {
        (self.depth_buffer_bit as c::GLbitfield) << 8
            | (self.stencil_buffer_bit as c::GLbitfield) << 10
            | (self.color_buffer_bit as c::GLbitfield) << 14
    }

    pub const fn from_bits(v: c::GLbitfield) -> BlitMask {
        BlitMask {
            depth_buffer_bit: v & (1 << 8) != 0,
            stencil_buffer_bit: v & (1 << 10) != 0,
            color_buffer_bit: v & (1 << 14) != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blit_mask_bits() {
        let all = BlitMask {
            depth_buffer_bit: true,
            stencil_buffer_bit: true,
            color_buffer_bit: true,
        };
        assert_eq!(
            c::GL_DEPTH_BUFFER_BIT | c::GL_STENCIL_BUFFER_BIT | c::GL_COLOR_BUFFER_BIT,
            all.to_bits()
        );
        assert_eq!(
            c::GL_COLOR_BUFFER_BIT,
            BlitMask {
                color_buffer_bit: true,
                ..Default::default()
            }
            .to_bits()
        );
        assert_eq!(all, BlitMask::from_bits(all.to_bits()));
    }
}
