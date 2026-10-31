//! Port of `src/renderer/OpenGL.zig`.
//!
//! STATUS: ported
//!
//! Graphics API wrapper for OpenGL.
//!
//! Deviations:
//! * The module is not built on macOS (it needs EGL, which macOS lacks;
//!   Zig never compiles it there either since the macOS build uses Metal).
//! * The allocator field and parameters are dropped.
//! * `ExportedFrame` releases its resources on drop (`deinit` drops it).

#![cfg(not(target_os = "macos"))]

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "opengl/buffer.rs"]
pub mod buffer;
#[path = "opengl/device.rs"]
pub mod device;
#[path = "opengl/frame.rs"]
pub mod frame;
#[path = "opengl/pipeline.rs"]
pub mod pipeline;
#[path = "opengl/render_pass.rs"]
pub mod render_pass;
#[path = "opengl/sampler.rs"]
pub mod sampler;
#[path = "opengl/shaders.rs"]
pub mod shaders;
#[path = "opengl/target.rs"]
pub mod target;
#[path = "opengl/texture.rs"]
pub mod texture;
// @generated-mods-end

use std::ffi::{c_void, CStr};
use std::fmt;

use crate::apprt;
use crate::config::config::AlphaBlending;
use crate::font;
use crate::pkg::opengl as gl;
use crate::pkg::opengl::egl;
use crate::renderer::dmabuf::Dmabuf;
use crate::renderer::shadertoy;
use crate::renderer::generic::{self, GraphicsApi};
use crate::renderer::{Health, Options};

pub use self::buffer::Buffer;
pub use self::device::Device;
pub use self::frame::Frame;
pub use self::pipeline::Pipeline;
pub use self::render_pass::RenderPass;
pub use self::sampler::Sampler;
pub use self::target::Target;
pub use self::texture::Texture;

/// The primitive types for draw steps (Zig: `gl.Primitive`).
pub use crate::pkg::opengl::Primitive;

/// Zig: `const Renderer = rendererpkg.GenericRenderer(OpenGL)`.
pub type Renderer = generic::Renderer<OpenGL>;

/// Zig: `pub const GraphicsAPI = OpenGL`.
pub type GraphicsAPI = OpenGL;

pub const CUSTOM_SHADER_TARGET: shadertoy::Target = shadertoy::Target::Glsl;
/// The fragCoord for OpenGL shaders is +Y = up.
pub const CUSTOM_SHADER_Y_IS_DOWN: bool = false;

/// Triple-buffering gives the GPU room to pipeline renders without
/// having to wait on the apprt consuming previous frames.
pub const SWAP_CHAIN_COUNT: usize = 3;

const LOG: &str = "opengl";

/// We require at least OpenGL 4.3
pub const MIN_VERSION_MAJOR: u32 = 4;
pub const MIN_VERSION_MINOR: u32 = 3;

/// Errors from the OpenGL graphics API (Zig: inferred error sets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Egl(egl::Error),
    Gl(gl::errors::Error),
    /// GLAD failed to load (Zig: `error.GLInitFailed`).
    GLInitFailed,
    OpenGLOutdated,
    Target(target::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Egl(e) => fmt::Display::fmt(e, f),
            Error::Gl(e) => fmt::Display::fmt(e, f),
            Error::Target(e) => fmt::Display::fmt(e, f),
            _ => fmt::Debug::fmt(self, f),
        }
    }
}

impl std::error::Error for Error {}

impl From<egl::Error> for Error {
    fn from(e: egl::Error) -> Self {
        Error::Egl(e)
    }
}

impl From<gl::errors::Error> for Error {
    fn from(e: gl::errors::Error) -> Self {
        Error::Gl(e)
    }
}

impl From<gl::glad::Error> for Error {
    fn from(_: gl::glad::Error) -> Self {
        Error::GLInitFailed
    }
}

impl From<target::Error> for Error {
    fn from(e: target::Error) -> Self {
        Error::Target(e)
    }
}

#[derive(Debug)]
pub struct OpenGL {
    /// Alpha blending mode
    pub blending: AlphaBlending,

    /// The shared render device.
    pub device: *const Device,

    pub egl_context: egl::Context,
}

// SAFETY: the device is app-scoped and outlives every renderer; the EGL
// context is only made current on one thread at a time (see `init` and
// `thread_enter`).
unsafe impl Send for OpenGL {}

impl OpenGL {
    pub fn init(device: *const Device, opts: &Options) -> Result<OpenGL, Error> {
        // SAFETY: the device outlives every renderer created from it.
        let dev = unsafe { &*device };
        let display = dev.display;

        // Bind to OpenGL API explicitly before we create the context,
        // as otherwise it might fallback to OpenGL ES
        egl::bind_api(egl::c::EGL_OPENGL_API)?;

        // Create our context. Contexts are surface-scoped because a
        // context can only be current on one thread at a time.
        let context = egl::Context::create(
            display,
            dev.config,
            None,
            &[
                egl::c::EGL_CONTEXT_MAJOR_VERSION as egl::c::EGLint,
                MIN_VERSION_MAJOR as egl::c::EGLint,
                egl::c::EGL_CONTEXT_MINOR_VERSION as egl::c::EGLint,
                MIN_VERSION_MINOR as egl::c::EGLint,
                egl::c::EGL_CONTEXT_OPENGL_PROFILE_MASK as egl::c::EGLint,
                egl::c::EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT as egl::c::EGLint,
                egl::c::EGL_NONE as egl::c::EGLint,
            ],
        )
        .inspect_err(|err| {
            log::warn!(target: LOG, "failed to create EGL context err={err}");
        })?;

        if let Err(err) = display.make_current(None, None, Some(context)) {
            log::warn!(target: LOG, "failed to make EGL context current err={err}");
            let _ = context.destroy(display);
            return Err(err.into());
        }

        // Release current so that the main thread
        // doesn't hold onto the GL context forever.
        display.release_current();

        Ok(OpenGL {
            blending: opts.config.blending,
            device,
            egl_context: context,
        })
    }

    fn dev(&self) -> &Device {
        // SAFETY: the device outlives every renderer created from it.
        unsafe { &*self.device }
    }

    pub fn deinit(&mut self) {
        let display = self.dev().display;
        display.release_current();
        let _ = self.egl_context.destroy(display);
    }

    /// Callback called by renderer.Thread when it begins. Called on the render
    /// thread. The EGL context was created at `init` time on the main thread;
    /// here we (re)bind it to this thread and load the thread-local glad
    /// function pointers so all subsequent GL work on this thread is valid.
    pub fn thread_enter(&mut self, surface: *mut apprt::Surface) -> Result<(), Error> {
        let _ = surface;
        self.dev()
            .display
            .make_current(None, None, Some(self.egl_context))?;
        // Load our function pointers for this thread's threadlocal.
        prepare_context(Some(egl::get_proc_address))
    }

    /// Callback called by renderer.Thread when it exits. Called on the render
    /// thread; unbinds the context from this thread so it can be destroyed on
    /// the main thread.
    pub fn thread_exit(&mut self) {
        self.dev().display.release_current();
        gl::glad::unload();
    }

    /// Get the current size of the runtime surface.
    pub fn surface_size(&self) -> Result<SurfaceSize, Error> {
        let mut viewport: [gl::c::GLint; 4] = [0; 4];
        gl::glad::call!(GetIntegerv(gl::c::GL_VIEWPORT, viewport.as_mut_ptr()));
        Ok(SurfaceSize {
            width: viewport[2] as u32,
            height: viewport[3] as u32,
        })
    }

    /// Set the GL viewport to cover the given size in device pixels.
    ///
    /// This used to be automatically called by the GtkGLArea upon resizing,
    /// but now we need to do this manually.
    pub fn set_viewport(&self, width: u32, height: u32) {
        if let Err(err) = gl::viewport(0, 0, width as _, height as _) {
            log::warn!(target: LOG, "failed to set OpenGL viewport err={err}");
        }
    }

    /// Actions taken before doing anything in `drawFrame`.
    ///
    /// Right now there's nothing we need to do for OpenGL.
    pub fn draw_frame_start(&mut self) {}

    /// Actions taken after `drawFrame` is done.
    ///
    /// Right now there's nothing we need to do for OpenGL.
    pub fn draw_frame_end(&mut self) {}

    pub fn init_shaders(&self, custom_shaders: &[&CStr]) -> Result<shaders::Shaders, pipeline::Error> {
        shaders::Shaders::init(custom_shaders)
    }

    /// Initialize a new render target which can be presented by this API.
    pub fn init_target(&self, width: usize, height: usize) -> Result<Target, target::Error> {
        Target::init(target::Options { width, height })
    }

    /// Export a rendered target. Caller takes ownership
    /// of the frame and is responsible for freeing it.
    ///
    /// This runs on the render thread.
    pub fn present(&mut self, target: Target, presentation_health: Health) -> Result<ExportedFrame, Error> {
        // We only export DMABUFs when the apprt can present them.
        // Otherwise, use CPU buffers.
        if presentation_health == Health::Healthy {
            match target.export_dmabuf(self.dev().display, self.egl_context) {
                Ok(dmabuf) => return Ok(ExportedFrame::Dmabuf(dmabuf)),
                Err(_) => {
                    log::warn!(
                        target: LOG,
                        "failed to export DMABUF, falling back to CPU presentation"
                    );
                }
            }
        }

        Ok(ExportedFrame::Memory(Memory {
            width: target.width as u32,
            height: target.height as u32,
            pixels: target.read_pixels_alloc()?,
        }))
    }

    /// Returns the options to use when constructing buffers.
    #[inline]
    pub fn buffer_options(&self) -> buffer::Options {
        buffer::Options {
            target: gl::buffer::Target::Array,
            usage: gl::buffer::Usage::DynamicDraw,
        }
    }

    pub fn instance_buffer_options(&self) -> buffer::Options {
        self.buffer_options()
    }
    pub fn uniform_buffer_options(&self) -> buffer::Options {
        self.buffer_options()
    }
    pub fn fg_buffer_options(&self) -> buffer::Options {
        self.buffer_options()
    }
    pub fn bg_buffer_options(&self) -> buffer::Options {
        self.buffer_options()
    }
    pub fn image_buffer_options(&self) -> buffer::Options {
        self.buffer_options()
    }
    pub fn bg_image_buffer_options(&self) -> buffer::Options {
        self.buffer_options()
    }

    /// Returns the options to use when constructing textures.
    #[inline]
    pub fn texture_options(&self) -> texture::Options {
        texture::Options {
            format: gl::texture::Format::Rgba,
            internal_format: gl::texture::InternalFormat::Srgba,
            target: gl::texture::Target::D2,
            min_filter: gl::texture::MinFilter::Linear,
            mag_filter: gl::texture::MagFilter::Linear,
            wrap_s: gl::texture::Wrap::ClampToEdge,
            wrap_t: gl::texture::Wrap::ClampToEdge,
        }
    }

    /// Returns the options to use when constructing samplers.
    #[inline]
    pub fn sampler_options(&self) -> sampler::Options {
        sampler::Options {
            min_filter: gl::texture::MinFilter::Linear,
            mag_filter: gl::texture::MagFilter::Linear,
            wrap_s: gl::texture::Wrap::ClampToEdge,
            wrap_t: gl::texture::Wrap::ClampToEdge,
        }
    }

    /// Returns the options to use when constructing textures for images.
    #[inline]
    pub fn image_texture_options(&self, format: ImageTextureFormat, srgb: bool) -> texture::Options {
        texture::Options {
            format: format.to_pixel_format(),
            internal_format: if srgb {
                gl::texture::InternalFormat::Srgba
            } else {
                gl::texture::InternalFormat::Rgba
            },
            target: gl::texture::Target::D2,
            // TODO: Generate mipmaps for image textures and use
            //       linear_mipmap_linear filtering so that they
            //       look good even when scaled way down.
            min_filter: gl::texture::MinFilter::Linear,
            mag_filter: gl::texture::MagFilter::Linear,
            // TODO: Separate out background image options, use
            //       repeating coordinate modes so we don't have
            //       to do the modulus in the shader.
            wrap_s: gl::texture::Wrap::ClampToEdge,
            wrap_t: gl::texture::Wrap::ClampToEdge,
        }
    }

    /// Initializes a Texture suitable for the provided font atlas.
    pub fn init_atlas_texture(&self, atlas: &font::Atlas) -> Result<Texture, texture::Error> {
        let (format, internal_format) = match atlas.format {
            font::atlas::Format::Grayscale => (gl::texture::Format::Red, gl::texture::InternalFormat::Red),
            font::atlas::Format::Bgra => (gl::texture::Format::Bgra, gl::texture::InternalFormat::Srgba),
            _ => panic!("unsupported atlas format for OpenGL texture"),
        };

        Texture::init(
            texture::Options {
                format,
                internal_format,
                target: gl::texture::Target::Rectangle,
                min_filter: gl::texture::MinFilter::Nearest,
                mag_filter: gl::texture::MagFilter::Nearest,
                wrap_s: gl::texture::Wrap::ClampToEdge,
                wrap_t: gl::texture::Wrap::ClampToEdge,
            },
            atlas.size as usize,
            atlas.size as usize,
            None,
        )
    }

    /// Begin a frame.
    ///
    /// Once the frame has been completed, the `frameCompleted` method
    /// on the renderer is called with the health status of the frame.
    /// The target is presented via the provided renderer's API when completed.
    #[inline]
    pub fn begin_frame(&self, renderer: *mut Renderer, target: *mut Target) -> Result<Frame, Error> {
        Ok(Frame::begin(frame::Options {}, renderer, target))
    }
}

impl GraphicsApi for OpenGL {
    type ExportedFrame = ExportedFrame;
    const EXPORTS_FRAMES: bool = true;
    const CUSTOM_SHADER_TARGET: shadertoy::Target = CUSTOM_SHADER_TARGET;
    const CUSTOM_SHADER_Y_IS_DOWN: bool = CUSTOM_SHADER_Y_IS_DOWN;
    const SWAP_CHAIN_COUNT: usize = SWAP_CHAIN_COUNT;

    fn init(device: *const Device, options: &Options) -> Result<Self, generic::Error> {
        Ok(OpenGL::init(device, options)?)
    }

    fn deinit(&mut self) {
        OpenGL::deinit(self)
    }

    fn thread_enter(&mut self, surface: *mut apprt::Surface) -> Result<(), generic::Error> {
        Ok(OpenGL::thread_enter(self, surface)?)
    }

    fn thread_exit(&mut self) {
        OpenGL::thread_exit(self)
    }

    fn set_viewport(&self, width: u32, height: u32) {
        OpenGL::set_viewport(self, width, height)
    }

    fn set_blending(&mut self, blending: AlphaBlending) {
        self.blending = blending;
    }

    fn surface_size(&self) -> Result<SurfaceSize, generic::Error> {
        Ok(OpenGL::surface_size(self)?)
    }

    fn draw_frame_start(&mut self) {
        OpenGL::draw_frame_start(self)
    }

    fn draw_frame_end(&mut self) {
        OpenGL::draw_frame_end(self)
    }

    fn init_shaders(&self, custom_shaders: &[&CStr]) -> Result<shaders::Shaders, generic::Error> {
        Ok(OpenGL::init_shaders(self, custom_shaders)?)
    }

    fn init_target(&self, width: usize, height: usize) -> Result<Target, generic::Error> {
        Ok(OpenGL::init_target(self, width, height)?)
    }

    fn uniform_buffer_options(&self) -> buffer::Options {
        OpenGL::uniform_buffer_options(self)
    }

    fn fg_buffer_options(&self) -> buffer::Options {
        OpenGL::fg_buffer_options(self)
    }

    fn bg_buffer_options(&self) -> buffer::Options {
        OpenGL::bg_buffer_options(self)
    }

    fn image_buffer_options(&self) -> buffer::Options {
        OpenGL::image_buffer_options(self)
    }

    fn bg_image_buffer_options(&self) -> buffer::Options {
        OpenGL::bg_image_buffer_options(self)
    }

    fn texture_options(&self) -> texture::Options {
        OpenGL::texture_options(self)
    }

    fn sampler_options(&self) -> sampler::Options {
        OpenGL::sampler_options(self)
    }

    fn image_texture_options(&self, format: ImageTextureFormat, srgb: bool) -> texture::Options {
        OpenGL::image_texture_options(self, format, srgb)
    }

    fn init_atlas_texture(&self, atlas: &font::Atlas) -> Result<Texture, generic::Error> {
        Ok(OpenGL::init_atlas_texture(self, atlas)?)
    }

    fn begin_frame(&self, renderer: *mut Renderer, target: *mut Target) -> Result<Frame, generic::Error> {
        Ok(OpenGL::begin_frame(self, renderer, target)?)
    }
}

/// Zig: the anonymous `struct { width: u32, height: u32 }` returned by
/// `surfaceSize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceSize {
    pub width: u32,
    pub height: u32,
}

fn gl_debug_message_callback_inner(src: u32, typ: u32, id: u32, severity: u32, msg_str: &str) {
    let src_str = match src {
        gl::c::GL_DEBUG_SOURCE_API => "OpenGL API",
        gl::c::GL_DEBUG_SOURCE_WINDOW_SYSTEM => "Window System",
        gl::c::GL_DEBUG_SOURCE_SHADER_COMPILER => "Shader Compiler",
        gl::c::GL_DEBUG_SOURCE_THIRD_PARTY => "Third Party",
        gl::c::GL_DEBUG_SOURCE_APPLICATION => "User",
        gl::c::GL_DEBUG_SOURCE_OTHER => "Other",
        _ => "Unknown",
    };

    let typ_str = match typ {
        gl::c::GL_DEBUG_TYPE_ERROR => "Error",
        gl::c::GL_DEBUG_TYPE_DEPRECATED_BEHAVIOR => "Deprecated Behavior",
        gl::c::GL_DEBUG_TYPE_UNDEFINED_BEHAVIOR => "Undefined Behavior",
        gl::c::GL_DEBUG_TYPE_PORTABILITY => "Portability Issue",
        gl::c::GL_DEBUG_TYPE_PERFORMANCE => "Performance Issue",
        gl::c::GL_DEBUG_TYPE_MARKER => "Marker",
        gl::c::GL_DEBUG_TYPE_PUSH_GROUP => "Group Push",
        gl::c::GL_DEBUG_TYPE_POP_GROUP => "Group Pop",
        gl::c::GL_DEBUG_TYPE_OTHER => "Other",
        _ => "Unknown",
    };

    match severity {
        gl::c::GL_DEBUG_SEVERITY_HIGH => {
            log::error!(target: LOG, "[{id}] ({src_str}: {typ_str}) {msg_str}")
        }
        gl::c::GL_DEBUG_SEVERITY_MEDIUM => {
            log::warn!(target: LOG, "[{id}] ({src_str}: {typ_str}) {msg_str}")
        }
        gl::c::GL_DEBUG_SEVERITY_LOW => {
            log::info!(target: LOG, "[{id}] ({src_str}: {typ_str}) {msg_str}")
        }
        gl::c::GL_DEBUG_SEVERITY_NOTIFICATION => {
            log::debug!(target: LOG, "[{id}] ({src_str}: {typ_str}) {msg_str}")
        }
        _ => log::warn!(
            target: LOG,
            "UNKNOWN SEVERITY [{id}] ({src_str}: {typ_str}) {msg_str}"
        ),
    }
}

unsafe extern "C" fn gl_debug_message_callback(
    src: gl::c::GLenum,
    typ: gl::c::GLenum,
    id: gl::c::GLuint,
    severity: gl::c::GLenum,
    len: gl::c::GLsizei,
    msg: *const gl::c::GLchar,
    _user_param: *const c_void,
) {
    let msg_str = if msg.is_null() || len <= 0 {
        std::borrow::Cow::Borrowed("")
    } else {
        // SAFETY: GL passes a message of `len` bytes.
        let bytes = unsafe { std::slice::from_raw_parts(msg.cast::<u8>(), len as usize) };
        String::from_utf8_lossy(bytes)
    };
    gl_debug_message_callback_inner(src, typ, id, severity, &msg_str);
}

/// Prepares the provided GL context, loading it with glad.
fn prepare_context(get_proc_address: gl::c::GLADloadfunc) -> Result<(), Error> {
    let version = gl::glad::load(get_proc_address)?;
    let major = gl::glad::version_major(version as u32);
    let minor = gl::glad::version_minor(version as u32);
    log::info!(target: LOG, "loaded OpenGL {major}.{minor}");

    let r = (|| {
        // Need to check version before trying to enable it
        if major < MIN_VERSION_MAJOR || (major == MIN_VERSION_MAJOR && minor < MIN_VERSION_MINOR) {
            log::warn!(
                target: LOG,
                "OpenGL version is too old. Ghostty requires OpenGL {MIN_VERSION_MAJOR}.{MIN_VERSION_MINOR}"
            );
            return Err(Error::OpenGLOutdated);
        }

        // Enable debug output for the context.
        gl::enable(gl::c::GL_DEBUG_OUTPUT)?;

        // Register our debug message callback with the OpenGL context.
        gl::glad::call!(DebugMessageCallback(
            Some(gl_debug_message_callback),
            std::ptr::null()
        ));

        // Enable SRGB framebuffer for linear blending support.
        gl::enable(gl::c::GL_FRAMEBUFFER_SRGB)?;
        Ok(())
    })();
    if r.is_err() {
        gl::glad::unload();
    }
    r
}

/// A finished frame exported for presentation by the apprt.
#[derive(Debug)]
pub enum ExportedFrame {
    Dmabuf(Dmabuf),
    Memory(Memory),
}

/// RGBA8 pixel data with premultiplied alpha, tightly packed
/// (`width * 4` bytes per row), in CPU memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Memory {
    pub fn deinit(self) {}
}

impl ExportedFrame {
    pub fn deinit(self) {
        drop(self)
    }
}

impl Drop for ExportedFrame {
    fn drop(&mut self) {
        match self {
            ExportedFrame::Dmabuf(v) => v.deinit(),
            ExportedFrame::Memory(_) => {}
        }
    }
}

/// Pixel format for image texture options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageTextureFormat {
    /// 1 byte per pixel grayscale.
    Gray,
    /// 4 bytes per pixel RGBA.
    Rgba,
    /// 4 bytes per pixel BGRA.
    Bgra,
}

impl ImageTextureFormat {
    fn to_pixel_format(self) -> gl::texture::Format {
        match self {
            ImageTextureFormat::Gray => gl::texture::Format::Red,
            ImageTextureFormat::Rgba => gl::texture::Format::Rgba,
            ImageTextureFormat::Bgra => gl::texture::Format::Bgra,
        }
    }
}

/// Test support shared by the OpenGL renderer tests.
#[cfg(test)]
pub(crate) mod test_util {
    use super::*;
    use crate::app::{Mailbox as AppMailbox, MailboxQueue as AppQueue};

    /// Renderer options for `device` whose surface mailbox points at
    /// `app`/`queue` (or nowhere when they are null).
    pub(crate) fn test_options_with_mailbox(
        device: *mut Device,
        app: *const apprt::App,
        queue: *const AppQueue,
    ) -> Options {
        Options {
            device,
            config: generic::DerivedConfig::default(),
            font_grid: crate::renderer::generic::test_util::test_grid(),
            size: crate::renderer::Size::default(),
            surface_mailbox: crate::apprt::surface::Mailbox {
                surface: std::ptr::null_mut(),
                app: AppMailbox {
                    rt_app: app,
                    mailbox: queue,
                },
            },
            rt_surface: std::ptr::null_mut(),
            thread: std::ptr::null_mut(),
        }
    }

    pub(crate) fn test_options(device: *mut Device) -> Options {
        test_options_with_mailbox(device, std::ptr::null(), std::ptr::null())
    }
}

#[cfg(test)]
mod tests {
    // Not Zig tests: OpenGL.zig has none. These exercise the API against a
    // surfaceless EGL device.
    use super::*;
    use crate::renderer::opengl::device::test_context::egl_lock;
    use crate::renderer::opengl::test_util::test_options;

    #[test]
    fn init_thread_enter_present() {
        let _guard = egl_lock();
        let mut device = Device::init(device::Options::default()).unwrap();
        let opts = test_options(&mut *device);
        let mut api = OpenGL::init(&*device, &opts).unwrap();
        api.thread_enter(std::ptr::null_mut()).unwrap();

        api.set_viewport(16, 8);
        let size = api.surface_size().unwrap();
        assert_eq!(SurfaceSize { width: 16, height: 8 }, size);

        let mut target = api.init_target(4, 2).unwrap();
        // Presenting while unhealthy always uses CPU memory.
        let frame = api.present(target, Health::Unhealthy).unwrap();
        match &frame {
            ExportedFrame::Memory(m) => {
                assert_eq!(4, m.width);
                assert_eq!(2, m.height);
                assert_eq!(4 * 2 * 4, m.pixels.len());
            }
            ExportedFrame::Dmabuf(_) => panic!("expected memory frame"),
        }
        frame.deinit();
        target.deinit();

        api.thread_exit();
        api.deinit();
        device.deinit();
    }

    #[test]
    fn options() {
        let opts = OpenGL {
            blending: AlphaBlending::Native,
            device: std::ptr::null(),
            egl_context: unsafe { std::mem::transmute::<usize, egl::Context>(1) },
        };
        assert_eq!(gl::texture::Format::Red, opts.image_texture_options(ImageTextureFormat::Gray, false).format);
        assert_eq!(
            gl::texture::InternalFormat::Srgba,
            opts.image_texture_options(ImageTextureFormat::Rgba, true).internal_format
        );
        assert_eq!(gl::texture::Target::D2, opts.texture_options().target);
        assert_eq!(gl::buffer::Usage::DynamicDraw, opts.uniform_buffer_options().usage);
    }
}
