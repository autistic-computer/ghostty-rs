//! Port of `src/renderer/opengl/Texture.zig`.
//!
//! STATUS: ported
//!
//! Wrapper for handling textures.

use std::os::raw::c_void;

use crate::pkg::opengl as gl;

/// Options for initializing a texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub format: gl::texture::Format,
    pub internal_format: gl::texture::InternalFormat,
    pub target: gl::texture::Target,
    pub min_filter: gl::texture::MinFilter,
    pub mag_filter: gl::texture::MagFilter,
    pub wrap_s: gl::texture::Wrap,
    pub wrap_t: gl::texture::Wrap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Texture {
    pub texture: gl::Texture,

    /// The width of this texture.
    pub width: usize,
    /// The height of this texture.
    pub height: usize,

    /// Format for this texture.
    pub format: gl::texture::Format,

    /// Target for this texture.
    pub target: gl::texture::Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// An OpenGL API call failed.
    OpenGLFailed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

impl Texture {
    /// Initialize a texture
    ///
    /// `data`, if given, must hold `width * height` pixels of `opts.format`
    /// as unsigned bytes (checked in debug builds for the formats with a
    /// known pixel size).
    pub fn init(
        opts: Options,
        width: usize,
        height: usize,
        data: Option<&[u8]>,
    ) -> Result<Texture, Error> {
        if let (Some(d), Some(bpp)) = (data, bytes_per_pixel(opts.format)) {
            debug_assert!(d.len() >= width * height * bpp);
        }

        let tex = gl::Texture::create().map_err(|_| Error::OpenGLFailed)?;
        let result = (|| {
            use gl::texture::Parameter;
            let texbind = tex.bind(opts.target)?;
            let r = (|| {
                texbind.parameter(Parameter::WrapS, opts.wrap_s)?;
                texbind.parameter(Parameter::WrapT, opts.wrap_t)?;
                texbind.parameter(Parameter::MinFilter, opts.min_filter)?;
                texbind.parameter(Parameter::MagFilter, opts.mag_filter)?;
                // SAFETY: data is null or (by contract) large enough.
                unsafe {
                    texbind.image_2d(
                        0,
                        opts.internal_format,
                        width as _,
                        height as _,
                        opts.format,
                        gl::texture::DataType::UnsignedByte,
                        data.map_or(std::ptr::null(), |d| d.as_ptr() as *const c_void),
                    )
                }
            })();
            texbind.unbind();
            r
        })();
        if result.is_err() {
            // errdefer tex.destroy();
            tex.destroy();
            return Err(Error::OpenGLFailed);
        }

        Ok(Texture {
            texture: tex,
            width,
            height,
            format: opts.format,
            target: opts.target,
        })
    }

    pub fn deinit(self) {
        self.texture.destroy();
    }

    /// Replace a region of the texture with the provided data.
    ///
    /// Does NOT check the dimensions of the data to ensure correctness.
    /// (In debug builds the length is checked for formats with a known
    /// pixel size, since a short slice would be read out of bounds.)
    pub fn replace_region(
        self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        data: &[u8],
    ) -> Result<(), Error> {
        if let Some(bpp) = bytes_per_pixel(self.format) {
            debug_assert!(data.len() >= width * height * bpp);
        }

        let texbind = self
            .texture
            .bind(self.target)
            .map_err(|_| Error::OpenGLFailed)?;
        // SAFETY: the caller provides data for the region.
        let r = unsafe {
            texbind.sub_image_2d(
                0,
                x as _,
                y as _,
                width as _,
                height as _,
                self.format,
                gl::texture::DataType::UnsignedByte,
                data.as_ptr() as *const c_void,
            )
        };
        texbind.unbind();
        r.map_err(|_| Error::OpenGLFailed)
    }
}

/// Bytes per pixel for the known formats with unsigned byte data (not
/// in the Zig code; only used for debug assertions).
fn bytes_per_pixel(format: gl::texture::Format) -> Option<usize> {
    use gl::texture::Format;
    match format {
        Format::Red => Some(1),
        Format::Rgb => Some(3),
        Format::Rgba | Format::Bgra => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::opengl::device::test_context::TestContext;

    // Not a Zig test: Texture.zig has none.
    #[test]
    fn init_replace_region() {
        let _ctx = TestContext::new();
        let opts = Options {
            format: gl::texture::Format::Red,
            internal_format: gl::texture::InternalFormat::Red,
            target: gl::texture::Target::D2,
            min_filter: gl::texture::MinFilter::Nearest,
            mag_filter: gl::texture::MagFilter::Nearest,
            wrap_s: gl::texture::Wrap::ClampToEdge,
            wrap_t: gl::texture::Wrap::ClampToEdge,
        };
        gl::pixel_store(gl::c::GL_UNPACK_ALIGNMENT, 1).unwrap();

        let tex = Texture::init(opts, 4, 4, None).unwrap();
        assert_eq!(4, tex.width);
        assert_eq!(4, tex.height);
        tex.replace_region(1, 1, 2, 2, &[1, 2, 3, 4]).unwrap();
        tex.deinit();

        let tex = Texture::init(opts, 2, 2, Some(&[9, 8, 7, 6])).unwrap();
        tex.deinit();
    }
}
