//! Port of `src/renderer/opengl/Target.zig`.
//!
//! STATUS: ported
//!
//! Represents a render target.
//!
//! In this case, a texture-backed framebuffer. The color attachment is a
//! `GL_TEXTURE_2D` texture instead of a renderbuffer so that we can create
//! an EGLImage from it and export the rendered frame as a dma-buf for
//! presentation by the apprt.
//!
//! We use two textures:
//!
//!   - `texture`: `GL_SRGB8_ALPHA8`. We render to this. With
//!     `GL_FRAMEBUFFER_SRGB` enabled, the GPU automatically converts linear
//!     shader output to sRGB on write. This is required for the
//!     linear-blending color pipeline to produce correct output.
//!
//!   - `export_texture`: `GL_RGBA8`. This is the texture we actually export
//!     as a dma-buf. Mesa cannot export `GL_SRGB8_ALPHA8` textures to
//!     dma-buf, so we blit the rendered sRGB texture into this plain RGBA8
//!     texture (the blit copies the already-sRGB-encoded pixel values
//!     verbatim) and export that instead.

use std::os::raw::c_void;

use crate::pkg::opengl as gl;
use crate::pkg::opengl::egl;
use crate::renderer::dmabuf::{self, Dmabuf};

/// Options for initializing a Target
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Desired width
    pub width: usize,
    /// Desired height
    pub height: usize,
}

/// Errors from [`Target`] operations (Zig: the inferred error sets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// An OpenGL call failed.
    Gl(gl::errors::Error),
    /// An EGL call failed.
    Egl(egl::Error),
    FramebufferIncomplete,
    DmabufTooManyPlanes,
    BadDmabuf,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Gl(e) => std::fmt::Display::fmt(e, f),
            Error::Egl(e) => std::fmt::Display::fmt(e, f),
            _ => std::fmt::Debug::fmt(self, f),
        }
    }
}

impl std::error::Error for Error {}

impl From<gl::errors::Error> for Error {
    fn from(e: gl::errors::Error) -> Self {
        Error::Gl(e)
    }
}

impl From<egl::Error> for Error {
    fn from(e: egl::Error) -> Self {
        Error::Egl(e)
    }
}

impl From<dmabuf::PlanesError> for Error {
    fn from(_: dmabuf::PlanesError) -> Self {
        Error::BadDmabuf
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// The framebuffer we render to.
    pub framebuffer: gl::Framebuffer,

    /// The sRGB color attachment texture we render to.
    pub texture: gl::Texture,

    /// A plain RGBA8 texture + framebuffer that we blit `texture` into for
    /// dma-buf export. Mesa can't export sRGB textures, so we blit the
    /// already-sRGB-encoded pixels into this non-sRGB texture and export it.
    pub export_texture: gl::Texture,
    pub export_framebuffer: gl::Framebuffer,

    /// Current width of this target.
    pub width: usize,
    /// Current height of this target.
    pub height: usize,
}

/// Configure the currently bound texture as a nearest-filtered,
/// edge-clamped, single-level 2D texture of the given size.
fn setup_texture(
    texture: gl::Texture,
    internal_format: gl::texture::InternalFormat,
    width: usize,
    height: usize,
) -> Result<(), Error> {
    use gl::texture::{MagFilter, MinFilter, Parameter, Wrap};

    let bound_tex = texture.bind(gl::texture::Target::D2)?;
    let r = (|| {
        bound_tex.parameter(Parameter::MinFilter, MinFilter::Nearest)?;
        bound_tex.parameter(Parameter::MagFilter, MagFilter::Nearest)?;
        bound_tex.parameter(Parameter::WrapS, Wrap::ClampToEdge)?;
        bound_tex.parameter(Parameter::WrapT, Wrap::ClampToEdge)?;
        // SAFETY: null data allocates uninitialized storage.
        unsafe {
            bound_tex.image_2d(
                0,
                internal_format,
                width as _,
                height as _,
                gl::texture::Format::Rgba,
                gl::texture::DataType::UnsignedByte,
                std::ptr::null(),
            )
        }?;
        bound_tex.parameter(Parameter::BaseLevel, 0i32)?;
        bound_tex.parameter(Parameter::MaxLevel, 0i32)?;
        Ok(())
    })();
    bound_tex.unbind();
    r
}

/// Attach `texture` as the color attachment of `fbo` and check that it
/// is complete.
fn setup_framebuffer(fbo: gl::Framebuffer, texture: gl::Texture, what: &str) -> Result<(), Error> {
    let bound_fbo = fbo.bind(gl::framebuffer::Target::Framebuffer)?;
    let r = (|| {
        bound_fbo.texture_2d(
            gl::framebuffer::Attachment::Color0,
            gl::texture::Target::D2,
            texture,
            0,
        )?;
        match bound_fbo.check_status() {
            gl::framebuffer::Status::Complete => Ok(()),
            status => {
                log::warn!(target: "opengl", "{what} framebuffer incomplete status={status:?}");
                Err(Error::FramebufferIncomplete)
            }
        }
    })();
    bound_fbo.unbind();
    r
}

impl Target {
    pub fn init(opts: Options) -> Result<Target, Error> {
        let texture = gl::Texture::create()?;
        if let Err(e) = setup_texture(
            texture,
            gl::texture::InternalFormat::Srgba,
            opts.width,
            opts.height,
        ) {
            texture.destroy();
            return Err(e);
        }

        let fbo = match gl::Framebuffer::create() {
            Ok(f) => f,
            Err(e) => {
                texture.destroy();
                return Err(e.into());
            }
        };
        if let Err(e) = setup_framebuffer(fbo, texture, "render") {
            fbo.destroy();
            texture.destroy();
            return Err(e);
        }

        // --- Export texture (plain RGBA8, for dma-buf export) ---
        let export_texture = match gl::Texture::create() {
            Ok(t) => t,
            Err(e) => {
                fbo.destroy();
                texture.destroy();
                return Err(e.into());
            }
        };
        if let Err(e) = setup_texture(
            export_texture,
            gl::texture::InternalFormat::Rgba,
            opts.width,
            opts.height,
        ) {
            export_texture.destroy();
            fbo.destroy();
            texture.destroy();
            return Err(e);
        }

        let export_fbo = match gl::Framebuffer::create() {
            Ok(f) => f,
            Err(e) => {
                export_texture.destroy();
                fbo.destroy();
                texture.destroy();
                return Err(e.into());
            }
        };
        if let Err(e) = setup_framebuffer(export_fbo, export_texture, "export") {
            export_fbo.destroy();
            export_texture.destroy();
            fbo.destroy();
            texture.destroy();
            return Err(e);
        }

        Ok(Target {
            framebuffer: fbo,
            texture,
            export_framebuffer: export_fbo,
            export_texture,
            width: opts.width,
            height: opts.height,
        })
    }

    pub fn deinit(&mut self) {
        self.framebuffer.destroy();
        self.texture.destroy();
        self.export_framebuffer.destroy();
        self.export_texture.destroy();
    }

    pub fn export_dmabuf(
        &self,
        display: egl::Display,
        context: egl::Context,
    ) -> Result<Dmabuf, Error> {
        // We disable GL_FRAMEBUFFER_SRGB while doing this blit, otherwise
        // the values may be linearized as they're copied, but even though
        // the draw framebuffer has a linear internal format, the values in
        // it should be sRGB, not linear!
        gl::disable(gl::c::GL_FRAMEBUFFER_SRGB)?;
        let r = self.export_dmabuf_inner(display, context);
        if let Err(err) = gl::enable(gl::c::GL_FRAMEBUFFER_SRGB) {
            log::error!(target: "opengl", "Error re-enabling GL_FRAMEBUFFER_SRGB, err={err}");
        }
        r
    }

    fn export_dmabuf_inner(
        &self,
        display: egl::Display,
        context: egl::Context,
    ) -> Result<Dmabuf, Error> {
        let read_bind = self.framebuffer.bind(gl::framebuffer::Target::Read)?;
        let r = (|| {
            let draw_bind = self
                .export_framebuffer
                .bind(gl::framebuffer::Target::Draw)?;
            let r = self.blit_and_export(display, context);
            draw_bind.unbind();
            r
        })();
        read_bind.unbind();
        r
    }

    fn blit_and_export(
        &self,
        display: egl::Display,
        context: egl::Context,
    ) -> Result<Dmabuf, Error> {
        gl::blit_framebuffer(
            0,
            0,
            self.width as _,
            self.height as _,
            0,
            0,
            self.width as _,
            self.height as _,
            gl::draw::BlitMask {
                color_buffer_bit: true,
                ..Default::default()
            },
            gl::texture::MagFilter::Nearest,
        )?;

        let image = egl::Image::create(
            display,
            Some(context),
            egl::ImageTarget::Texture2d(self.export_texture.id),
            Some(&[egl::c::EGL_NONE as egl::c::EGLAttrib]),
        )?;
        let r = (|| {
            let query = image.export_dmabuf_query(display)?;

            if query.num_planes < 1 || query.num_planes as usize > dmabuf::MAX_PLANES {
                log::error!(target: "opengl", "DMABUF has too many planes={}", query.num_planes);
                return Err(Error::DmabufTooManyPlanes);
            }

            let mut planes = dmabuf::Planes::new(query.num_planes as u8);
            image.export_dmabuf(
                display,
                &mut planes.fds,
                &mut planes.strides,
                &mut planes.offsets,
            )?;
            planes.validate()?;

            Ok(Dmabuf {
                width: self.width as u32,
                height: self.height as u32,
                // bitCast instead of intCast since the numerical value of fourccs
                // is rather meaningless, and we only care about the bit pattern
                fourcc: query.fourcc as u32,
                modifier: query.modifier,
                premultiplied: true,
                planes,
            })
        })();
        let _ = image.destroy(display);
        r
    }

    /// Read the current contents of the framebuffer into CPU memory.
    ///
    /// This is used by the CPU readback presentation fallback. The
    /// returned data is tightly-packed RGBA8 with premultiplied alpha,
    /// i.e. `width * 4` bytes per row, containing sRGB-encoded values:
    /// `GL_FRAMEBUFFER_SRGB` has no effect on `ReadPixels`, so the stored
    /// values are returned verbatim.
    pub fn read_pixels_alloc(&self) -> Result<Vec<u8>, gl::errors::Error> {
        let bind = self.framebuffer.bind(gl::framebuffer::Target::Read)?;

        let mut pixels = vec![0u8; self.width * self.height * 4];

        // SAFETY: pixels has room for width x height RGBA8 pixels.
        let r = unsafe {
            gl::read_pixels(
                0,
                0,
                self.width as _,
                self.height as _,
                gl::texture::Format::Rgba,
                gl::texture::DataType::UnsignedByte,
                pixels.as_mut_ptr() as *mut c_void,
            )
        };
        bind.unbind();
        r?;

        Ok(pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::opengl::device::test_context::TestContext;

    // Not a Zig test: Target.zig has none.
    #[test]
    fn init_clear_read_pixels() {
        let _ctx = TestContext::new();
        let mut target = Target::init(Options {
            width: 3,
            height: 2,
        })
        .unwrap();

        {
            let fb = target
                .framebuffer
                .bind(gl::framebuffer::Target::Framebuffer)
                .unwrap();
            gl::viewport(0, 0, 3, 2).unwrap();
            gl::clear_color(1.0, 0.0, 1.0, 1.0);
            gl::clear(gl::c::GL_COLOR_BUFFER_BIT);
            fb.unbind();
        }
        gl::finish();

        let pixels = target.read_pixels_alloc().unwrap();
        assert_eq!(3 * 2 * 4, pixels.len());
        for px in pixels.chunks(4) {
            assert_eq!(&[255, 0, 255, 255], px);
        }

        target.deinit();
    }

    // Not a Zig test. Exporting needs a GPU driver with
    // EGL_MESA_image_dma_buf_export that supports dma-bufs (software
    // rasterizers such as llvmpipe may refuse); either outcome must be a
    // clean result, never a crash.
    #[test]
    fn export_dmabuf_does_not_crash() {
        let ctx = TestContext::new();
        let mut target = Target::init(Options {
            width: 4,
            height: 4,
        })
        .unwrap();

        match target.export_dmabuf(ctx.device.display, ctx.context) {
            Ok(dmabuf) => {
                assert_eq!(4, dmabuf.width);
                assert_eq!(4, dmabuf.height);
                assert!(dmabuf.planes.count >= 1);
                dmabuf.deinit();
            }
            Err(err) => {
                assert!(matches!(
                    err,
                    Error::Egl(_) | Error::Gl(_) | Error::DmabufTooManyPlanes | Error::BadDmabuf
                ));
            }
        }

        target.deinit();
    }
}
