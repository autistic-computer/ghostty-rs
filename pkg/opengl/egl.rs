//! Port of `pkg/opengl/egl.zig`.
//!
//! STATUS: ported
//!
//! Thin EGL bindings via GLAD.
//!
//! Only types and functions used in Ghostty are modelled,
//! although the API design is rather flexible and can be extended
//! to add whatever is required. Drop down to `gl::egl::c` to access
//! raw EGL functions.
//!
//! Call `load()` once before using any EGL extension functions.
//!
//! Zig's sentinel-terminated attribute lists (`[:c.EGL_NONE]const T`) are
//! slices whose last element is `EGL_NONE` here (checked at runtime);
//! [`AttribsBuilder::items`] returns such a slice.

#![cfg(not(target_os = "macos"))]

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr::NonNull;

pub use super::c;

/// Errors from [`load`] (Zig: `error.EglInitFailed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    EglInitFailed,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for LoadError {}

pub fn load() -> Result<(), LoadError> {
    // SAFETY: no preconditions.
    if unsafe { c::gladLoadEGL() } == 0 {
        return Err(LoadError::EglInitFailed);
    }
    Ok(())
}

/// Wraps `eglGetProcAddress` for GLAD.
pub unsafe extern "C" fn get_proc_address(name: *const c_char) -> c::GLADapiproc {
    // `__eglMustCastToProperFunctionPointerType` and `GLADapiproc` are both
    // nullable `void (*)(void)`.
    c::eglGetProcAddress(name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NotInitialized,
    BadAccess,
    BadAlloc,
    BadAttribute,
    BadContext,
    BadConfig,
    BadCurrentSurface,
    BadDisplay,
    BadSurface,
    BadMatch,
    BadParameter,
    BadNativePixmap,
    BadNativeWindow,
    ContextLost,
    Unknown,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

pub fn get_error() -> Result<(), Error> {
    // SAFETY: no preconditions.
    let err = unsafe { c::eglGetError() } as u32;
    match err {
        c::EGL_SUCCESS => Ok(()),
        c::EGL_NOT_INITIALIZED => Err(Error::NotInitialized),
        c::EGL_BAD_ACCESS => Err(Error::BadAccess),
        c::EGL_BAD_ALLOC => Err(Error::BadAlloc),
        c::EGL_BAD_ATTRIBUTE => Err(Error::BadAttribute),
        c::EGL_BAD_CONTEXT => Err(Error::BadContext),
        c::EGL_BAD_CONFIG => Err(Error::BadConfig),
        c::EGL_BAD_CURRENT_SURFACE => Err(Error::BadCurrentSurface),
        c::EGL_BAD_DISPLAY => Err(Error::BadDisplay),
        c::EGL_BAD_SURFACE => Err(Error::BadSurface),
        c::EGL_BAD_MATCH => Err(Error::BadMatch),
        c::EGL_BAD_PARAMETER => Err(Error::BadParameter),
        c::EGL_BAD_NATIVE_PIXMAP => Err(Error::BadNativePixmap),
        c::EGL_BAD_NATIVE_WINDOW => Err(Error::BadNativeWindow),
        c::EGL_CONTEXT_LOST => Err(Error::ContextLost),
        _ => Err(Error::Unknown),
    }
}

/// Like [`get_error`], but always an error ([`Error::Unknown`] if EGL has
/// none recorded).
pub fn must_error() -> Error {
    match get_error() {
        Err(err) => err,
        Ok(()) => Error::Unknown,
    }
}

pub fn bind_api(api: c::EGLenum) -> Result<(), Error> {
    // SAFETY: no preconditions.
    if unsafe { c::eglBindAPI(api) } != c::EGL_TRUE {
        return Err(must_error());
    }
    Ok(())
}

/// Returns the client extensions, which are those that are independent
/// of any display, such as the platforms that can be passed to
/// `Display.initPlatform`. Returns null if the implementation does not
/// support EGL_EXT_client_extensions or no driver could be loaded.
pub fn query_client_extensions() -> Option<&'static CStr> {
    // SAFETY: EGL_NO_DISPLAY is valid for EGL_EXTENSIONS; the returned
    // string is static.
    unsafe {
        let exts = c::eglQueryString(c::EGL_NO_DISPLAY, c::EGL_EXTENSIONS as c::EGLint);
        if exts.is_null() {
            return None;
        }
        Some(CStr::from_ptr(exts))
    }
}

/// Returns true if `name` appears in the space-separated
/// extension string `exts`.
pub fn has_extension(exts: &[u8], name: &[u8]) -> bool {
    exts.split(|&b| b == b' ')
        .filter(|ext| !ext.is_empty())
        .any(|ext| ext == name)
}

/// Checks that `attrs` is terminated by `EGL_NONE` (Zig: the `:EGL_NONE`
/// sentinel in the type).
fn check_terminated<T: Copy + TryFrom<u32> + PartialEq>(attrs: &[T]) {
    let none = T::try_from(c::EGL_NONE).ok();
    assert!(
        attrs.last().copied() == none && none.is_some(),
        "EGL attribute list must end with EGL_NONE"
    );
}

/// Zig: `opaque` used as `*Display`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Display(NonNull<c_void>);

/// Zig: `Display.StringQuery`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringQuery {
    Vendor = c::EGL_VENDOR as i32,
    Extensions = c::EGL_EXTENSIONS as i32,
}

impl Display {
    /// Wraps a raw `EGLDisplay` obtained elsewhere (e.g. from GDK).
    ///
    /// # Safety
    /// `ptr` must be a valid, initialized `EGLDisplay`.
    pub unsafe fn from_raw(ptr: NonNull<c_void>) -> Display {
        Display(ptr)
    }

    /// `attribs`, if given, must end with `EGL_NONE`.
    pub fn init_platform(
        platform: c::EGLenum,
        id: c::EGLNativeDisplayType,
        attribs: Option<&[c::EGLAttrib]>,
    ) -> Result<Display, Error> {
        if let Some(a) = attribs {
            check_terminated(a);
        }
        // SAFETY: attribs is EGL_NONE terminated.
        let display = unsafe {
            c::eglGetPlatformDisplay(
                platform,
                id,
                attribs.map_or(std::ptr::null(), |a| a.as_ptr()),
            )
        };
        if display.is_null() {
            return Err(must_error());
        }
        Self::initialize(display)
    }

    fn initialize(display: c::EGLDisplay) -> Result<Display, Error> {
        let mut major: c::EGLint = 0;
        let mut minor: c::EGLint = 0;
        // SAFETY: valid display and out pointers.
        if unsafe { c::eglInitialize(display, &mut major, &mut minor) } != c::EGL_TRUE {
            return Err(must_error());
        }
        log::debug!(target: "opengl_egl", "EGL initialized {major}.{minor}");
        Ok(Display(NonNull::new(display).expect("non-null display")))
    }

    pub fn terminate(self) {
        // SAFETY: valid display.
        unsafe { c::eglTerminate(self.cval()) };
    }

    pub fn query_string(self, name: StringQuery) -> Option<&'static CStr> {
        // SAFETY: valid display; the returned string is static.
        unsafe {
            let s = c::eglQueryString(self.cval(), name as c::EGLint);
            if s.is_null() {
                return None;
            }
            Some(CStr::from_ptr(s))
        }
    }

    /// Make the EGL context current on the calling thread and (re)load
    /// the thread-local GLAD function pointers so subsequent GL work is
    /// valid. Returns an error if the context can't be made current.
    pub fn make_current(
        self,
        draw: Option<Surface>,
        read: Option<Surface>,
        context: Option<Context>,
    ) -> Result<(), Error> {
        // SAFETY: valid handles (or null).
        if unsafe {
            c::eglMakeCurrent(
                self.cval(),
                draw.map_or(std::ptr::null_mut(), Surface::cval),
                read.map_or(std::ptr::null_mut(), Surface::cval),
                context.map_or(std::ptr::null_mut(), Context::cval),
            )
        } != c::EGL_TRUE
        {
            return Err(must_error());
        }
        Ok(())
    }

    pub fn release_current(self) {
        // SAFETY: valid display.
        unsafe {
            c::eglMakeCurrent(
                self.cval(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
    }

    #[inline]
    pub fn cval(self) -> c::EGLDisplay {
        self.0.as_ptr()
    }
}

/// Zig: `opaque` used as `*Config`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config(NonNull<c_void>);

impl Config {
    /// `attrs` must end with `EGL_NONE`.
    pub fn choose(display: Display, attrs: &[c::EGLint]) -> Result<Config, Error> {
        check_terminated(attrs);
        let mut config: c::EGLConfig = std::ptr::null_mut();
        let mut num_config: c::EGLint = 0;
        // SAFETY: attrs is EGL_NONE terminated; valid out pointers.
        if unsafe {
            c::eglChooseConfig(
                display.cval(),
                attrs.as_ptr(),
                &mut config,
                1,
                &mut num_config,
            )
        } != c::EGL_TRUE
            || num_config == 0
        {
            return Err(must_error());
        }
        Ok(Config(NonNull::new(config).ok_or(Error::Unknown)?))
    }

    #[inline]
    pub fn cval(self) -> c::EGLConfig {
        self.0.as_ptr()
    }
}

/// Zig: `opaque` used as `*Surface`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surface(NonNull<c_void>);

impl Surface {
    #[inline]
    pub fn cval(self) -> c::EGLSurface {
        self.0.as_ptr()
    }
}

/// Zig: `opaque` used as `*Context`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context(NonNull<c_void>);

impl Context {
    /// `attrs` must end with `EGL_NONE`.
    ///
    /// The Zig version passes `surface` (a `?*Surface`) as the share
    /// context argument of `eglCreateContext`; this mirrors that.
    pub fn create(
        display: Display,
        config: Config,
        surface: Option<Surface>,
        attrs: &[c::EGLint],
    ) -> Result<Context, Error> {
        check_terminated(attrs);
        // SAFETY: valid handles; attrs is EGL_NONE terminated.
        let context = unsafe {
            c::eglCreateContext(
                display.cval(),
                config.cval(),
                surface.map_or(std::ptr::null_mut(), Surface::cval),
                attrs.as_ptr(),
            )
        };
        match NonNull::new(context) {
            Some(ctx) => Ok(Context(ctx)),
            None => Err(must_error()),
        }
    }

    pub fn destroy(self, display: Display) -> Result<(), Error> {
        // SAFETY: valid handles.
        if unsafe { c::eglDestroyContext(display.cval(), self.cval()) } != c::EGL_TRUE {
            return Err(must_error());
        }
        Ok(())
    }

    #[inline]
    pub fn cval(self) -> c::EGLContext {
        self.0.as_ptr()
    }
}

/// Zig: `AttribsBuilder(comptime cap)`: a fixed-capacity, always
/// `EGL_NONE`-terminated attribute list.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AttribsBuilder<const CAP: usize> {
    attribs: [c::EGLAttrib; CAP],
    /// The sentinel slot after `attribs` (Zig: `[capacity:EGL_NONE]`).
    sentinel: c::EGLAttrib,
    len: usize,
}

impl<const CAP: usize> AttribsBuilder<CAP> {
    pub const CAPACITY: usize = CAP;

    pub const EMPTY: Self = AttribsBuilder {
        // Save ourselves the trouble of manually terminating
        // the array with an `EGL_NONE`
        attribs: [c::EGL_NONE as c::EGLAttrib; CAP],
        sentinel: c::EGL_NONE as c::EGLAttrib,
        len: 0,
    };

    pub fn add(&mut self, k: c::EGLAttrib, v: c::EGLAttrib) {
        assert!(Self::CAPACITY - self.len >= 2);
        self.attribs[self.len] = k;
        self.attribs[self.len + 1] = v;
        self.len += 2;
    }

    /// The attributes followed by their `EGL_NONE` terminator.
    pub fn items(&self) -> &[c::EGLAttrib] {
        // SAFETY: `attribs` and `sentinel` are laid out contiguously
        // (repr(C), same element type, no padding), the pointer is derived
        // from the whole struct, and attribs[len] (or the sentinel when
        // len == CAP) is EGL_NONE.
        unsafe {
            std::slice::from_raw_parts(self as *const Self as *const c::EGLAttrib, self.len + 1)
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Zig: `opaque` used as `*Image`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image(NonNull<c_void>);

impl Image {
    /// `attrs`, if given, must end with `EGL_NONE`.
    pub fn create(
        display: Display,
        context: Option<Context>,
        target: ImageTarget,
        attrs: Option<&[c::EGLAttrib]>,
    ) -> Result<Image, Error> {
        if let Some(a) = attrs {
            check_terminated(a);
        }
        // SAFETY: valid handles; attrs is EGL_NONE terminated.
        let image = unsafe {
            c::eglCreateImage(
                display.cval(),
                context.map_or(std::ptr::null_mut(), Context::cval),
                target.tag() as c::EGLenum,
                target.to_client_buffer(),
                attrs.map_or(std::ptr::null(), |a| a.as_ptr()),
            )
        };
        match NonNull::new(image) {
            Some(img) => Ok(Image(img)),
            None => Err(must_error()),
        }
    }

    pub fn destroy(self, display: Display) -> Result<(), Error> {
        // SAFETY: valid handles.
        if unsafe { c::eglDestroyImage(display.cval(), self.cval()) } != c::EGL_TRUE {
            return Err(must_error());
        }
        Ok(())
    }

    /// Requires [`load`] (EGL_MESA_image_dma_buf_export).
    pub fn export_dmabuf_query(self, display: Display) -> Result<DmabufQuery, Error> {
        let mut query = DmabufQuery {
            fourcc: 0,
            num_planes: 0,
            modifier: 0,
        };
        // SAFETY: reading the glad-loaded function pointer.
        let f = unsafe { c::glad_eglExportDMABUFImageQueryMESA }
            .expect("eglExportDMABUFImageQueryMESA not loaded (call egl.load())");
        // SAFETY: valid handles and out pointers.
        if unsafe {
            f(
                display.cval(),
                self.cval(),
                &mut query.fourcc,
                &mut query.num_planes,
                &mut query.modifier,
            )
        } != c::EGL_TRUE
        {
            return Err(must_error());
        }
        Ok(query)
    }

    /// Requires [`load`] (EGL_MESA_image_dma_buf_export). The slices must
    /// have room for `num_planes` entries.
    pub fn export_dmabuf(
        self,
        display: Display,
        fds: &mut [c_int],
        strides: &mut [c_int],
        offsets: &mut [c_int],
    ) -> Result<(), Error> {
        // SAFETY: reading the glad-loaded function pointer.
        let f = unsafe { c::glad_eglExportDMABUFImageMESA }
            .expect("eglExportDMABUFImageMESA not loaded (call egl.load())");
        // SAFETY: valid handles; the caller sized the slices.
        if unsafe {
            f(
                display.cval(),
                self.cval(),
                fds.as_mut_ptr(),
                strides.as_mut_ptr(),
                offsets.as_mut_ptr(),
            )
        } != c::EGL_TRUE
        {
            return Err(must_error());
        }
        Ok(())
    }

    #[inline]
    pub fn cval(self) -> c::EGLImage {
        self.0.as_ptr()
    }
}

/// Zig: `union(ImageTarget.Tag)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageTarget {
    /// OpenGL 2D Texture with the given name.
    Texture2d(u32),
    // Many other variants, add when needed
}

/// Zig: `ImageTarget.Tag`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageTargetTag {
    Texture2d = c::EGL_GL_TEXTURE_2D,
}

impl ImageTarget {
    pub fn tag(self) -> ImageTargetTag {
        match self {
            ImageTarget::Texture2d(_) => ImageTargetTag::Texture2d,
        }
    }

    pub fn to_client_buffer(self) -> c::EGLClientBuffer {
        match self {
            // Yes this really is that cursed
            ImageTarget::Texture2d(v) => v as usize as c::EGLClientBuffer,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmabufQuery {
    pub fourcc: c_int,
    pub num_planes: c_int,
    pub modifier: u64,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// EGL displays are per-platform singletons; serialize the tests that
    /// initialize/terminate one. (Also taken by the renderer's GL tests,
    /// `renderer::opengl::device::test_context`.)
    pub(crate) static EGL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn has_extension_tokens() {
        let exts = b"EGL_EXT_a  EGL_MESA_platform_surfaceless EGL_KHR_b";
        assert!(has_extension(exts, b"EGL_MESA_platform_surfaceless"));
        assert!(has_extension(exts, b"EGL_KHR_b"));
        assert!(!has_extension(exts, b"EGL_KHR"));
        assert!(!has_extension(exts, b""));
    }

    #[test]
    fn attribs_builder() {
        let mut b = AttribsBuilder::<4>::EMPTY;
        assert_eq!(&[c::EGL_NONE as c::EGLAttrib], b.items());
        b.add(1, 2);
        b.add(3, 4);
        assert_eq!(&[1, 2, 3, 4, c::EGL_NONE as c::EGLAttrib], b.items());
        check_terminated(b.items());
    }

    #[test]
    fn image_target_client_buffer() {
        let t = ImageTarget::Texture2d(42);
        assert_eq!(42usize, t.to_client_buffer() as usize);
        assert_eq!(c::EGL_GL_TEXTURE_2D, t.tag() as u32);
    }

    #[test]
    #[ignore = "requires an EGL driver"]
    fn surfaceless_display() {
        let _guard = EGL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        load().unwrap();
        bind_api(c::EGL_OPENGL_API).unwrap();
        let display = Display::init_platform(
            c::EGL_PLATFORM_SURFACELESS_MESA,
            c::EGL_DEFAULT_DISPLAY,
            None,
        )
        .unwrap();
        assert!(display.query_string(StringQuery::Vendor).is_some());
        display.terminate();
    }

    /// Exercises the GL wrappers against a surfaceless EGL context (not a
    /// Zig test; the Zig package has no GL tests).
    #[test]
    #[ignore = "requires an EGL driver with EGL_MESA_platform_surfaceless and GL 4.3"]
    fn gl_smoke() {
        let _guard = EGL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use crate::pkg::opengl as gl;

        load().unwrap();
        bind_api(c::EGL_OPENGL_API).unwrap();
        let display = Display::init_platform(
            c::EGL_PLATFORM_SURFACELESS_MESA,
            c::EGL_DEFAULT_DISPLAY,
            None,
        )
        .unwrap();
        let config = Config::choose(
            display,
            &[
                c::EGL_SURFACE_TYPE as c::EGLint,
                0,
                c::EGL_RENDERABLE_TYPE as c::EGLint,
                c::EGL_OPENGL_BIT as c::EGLint,
                c::EGL_RED_SIZE as c::EGLint,
                8,
                c::EGL_GREEN_SIZE as c::EGLint,
                8,
                c::EGL_BLUE_SIZE as c::EGLint,
                8,
                c::EGL_ALPHA_SIZE as c::EGLint,
                8,
                c::EGL_NONE as c::EGLint,
            ],
        )
        .unwrap();
        let ctx = Context::create(
            display,
            config,
            None,
            &[
                c::EGL_CONTEXT_MAJOR_VERSION as c::EGLint,
                4,
                c::EGL_CONTEXT_MINOR_VERSION as c::EGLint,
                3,
                c::EGL_CONTEXT_OPENGL_PROFILE_MASK as c::EGLint,
                c::EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT as c::EGLint,
                c::EGL_NONE as c::EGLint,
            ],
        )
        .unwrap();
        display.make_current(None, None, Some(ctx)).unwrap();

        let res = gl::glad::load(Some(get_proc_address)).unwrap();
        assert!(gl::glad::version_major(res as u32) >= 4);

        let mut it = gl::ext::iterator().unwrap();
        let mut n = 0;
        while let Some(ext) = it.next().unwrap() {
            assert!(!ext.is_empty());
            n += 1;
        }
        assert_eq!(n, gl::ext::len().unwrap());

        // Render target.
        let tex = gl::Texture::create().unwrap();
        let tb = tex.bind(gl::texture::Target::D2).unwrap();
        tb.parameter(
            gl::texture::Parameter::MinFilter,
            gl::texture::MinFilter::Nearest,
        )
        .unwrap();
        tb.parameter(
            gl::texture::Parameter::MagFilter,
            gl::texture::MagFilter::Nearest,
        )
        .unwrap();
        // SAFETY: null data allocates uninitialized storage.
        unsafe {
            tb.image_2d(
                0,
                gl::texture::InternalFormat::Rgba,
                4,
                4,
                gl::texture::Format::Rgba,
                gl::texture::DataType::UnsignedByte,
                std::ptr::null(),
            )
        }
        .unwrap();
        let fbo = gl::Framebuffer::create().unwrap();
        let fb = fbo.bind(gl::framebuffer::Target::Framebuffer).unwrap();
        fb.texture_2d(
            gl::framebuffer::Attachment::Color0,
            gl::texture::Target::D2,
            tex,
            0,
        )
        .unwrap();
        assert_eq!(gl::framebuffer::Status::Complete, fb.check_status());
        gl::viewport(0, 0, 4, 4).unwrap();

        // A program that fills everything with a uniform color.
        let program = gl::Program::create_vf(
            c"#version 430 core
            layout(location = 0) in vec2 pos;
            void main() { gl_Position = vec4(pos, 0.0, 1.0); }",
            c"#version 430 core
            uniform vec4 color;
            out vec4 frag;
            void main() { frag = color; }",
        )
        .unwrap();
        let pb = program.r#use().unwrap();
        program
            .set_uniform(c"color", [0.0f32, 1.0, 0.0, 1.0])
            .unwrap();
        assert_eq!(
            Err(gl::errors::Error::UniformNameInvalid),
            program.set_uniform(c"nope", 1.0f32)
        );

        let vao = gl::VertexArray::create().unwrap();
        let vb = vao.bind().unwrap();
        let buf = gl::Buffer::create().unwrap();
        let bb = buf.bind(gl::buffer::Target::Array).unwrap();
        let verts: [[f32; 2]; 4] = [[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]];
        bb.set_data(&verts, gl::buffer::Usage::StaticDraw).unwrap();
        bb.attribute::<[f32; 2]>(0, 2, 0).unwrap();

        gl::clear_color(1.0, 0.0, 0.0, 1.0);
        gl::clear(c::GL_COLOR_BUFFER_BIT);
        gl::draw_arrays(c::GL_TRIANGLE_STRIP, 0, 4).unwrap();
        gl::finish();

        let mut pixels = [0u8; 4 * 4 * 4];
        // SAFETY: pixels has room for 4x4 RGBA8.
        unsafe {
            gl::read_pixels(
                0,
                0,
                4,
                4,
                gl::texture::Format::Rgba,
                gl::texture::DataType::UnsignedByte,
                pixels.as_mut_ptr() as *mut c_void,
            )
        }
        .unwrap();
        for px in pixels.chunks(4) {
            assert_eq!(&[0, 255, 0, 255], px);
        }

        bb.unbind();
        buf.destroy();
        vb.unbind();
        vao.destroy();
        pb.unbind();
        program.destroy();
        fb.unbind();
        fbo.destroy();
        tb.unbind();
        tex.destroy();
        gl::glad::unload();

        display.release_current();
        ctx.destroy(display).unwrap();
        display.terminate();
    }
}
