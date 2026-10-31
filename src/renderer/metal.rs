//! Port of `src/renderer/Metal.zig`.
//!
//! STATUS: ported (macOS only; type-checked with
//! `cargo check --target x86_64-apple-darwin`, not run).
//!
//! Graphics API wrapper for Metal.
//!
//! Deviations:
//! * Ghostty uses the zig-objc package for the Objective-C runtime. The
//!   Rust port has no such package yet, so [`objc`] below is a minimal
//!   zig-objc-shaped binding (objects, classes, selectors, `msgSend`,
//!   properties, instance variables, runtime subclasses, blocks and
//!   autorelease pools) over libobjc (NOTE(port): zig-objc is a
//!   third-party Zig package with no Rust port in this repository).
//! * The embedded apprt surface's content scale and view are read through
//!   the [`EmbeddedSurface`] trait (NOTE(port): test builds use the
//!   recording `apprt::standin::Surface`, so Metal cannot name
//!   `apprt::embedded::Surface` fields directly).
//! * `loopEnter` receives the renderer pointer explicitly (Zig:
//!   `@fieldParentPtr("api", self)`).
//! * The allocator parameters are dropped. iOS is not supported (the
//!   Rust port only targets macOS for Metal).

#![cfg(target_os = "macos")]

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "metal/api.rs"]
pub mod api;
#[path = "metal/buffer.rs"]
pub mod buffer;
#[path = "metal/device.rs"]
pub mod device;
#[path = "metal/frame.rs"]
pub mod frame;
#[path = "metal/io_surface_layer.rs"]
pub mod io_surface_layer;
#[path = "metal/pipeline.rs"]
pub mod pipeline;
#[path = "metal/render_pass.rs"]
pub mod render_pass;
#[path = "metal/sampler.rs"]
pub mod sampler;
#[path = "metal/shaders.rs"]
pub mod shaders;
#[path = "metal/target.rs"]
pub mod target;
#[path = "metal/texture.rs"]
pub mod texture;
// @generated-mods-end

use std::ffi::{c_void, CStr};
use std::fmt;

use crate::apprt;
use crate::config::config::AlphaBlending;
use crate::font;
use crate::pkg::macos::graphics;
use crate::renderer::shadertoy;
use crate::renderer::generic::{self, GraphicsApi};
use crate::renderer::Options;

use self::api as mtl;
use self::io_surface_layer::IOSurfaceLayer;

pub use self::buffer::Buffer;
pub use self::device::Device;
pub use self::frame::Frame;
pub use self::pipeline::Pipeline;
pub use self::render_pass::RenderPass;
pub use self::sampler::Sampler;
pub use self::target::Target;
pub use self::texture::Texture;

/// The primitive types for draw steps (Zig: `mtl.MTLPrimitiveType`).
pub use self::api::MTLPrimitiveType as Primitive;

/// Zig: `const Renderer = rendererpkg.GenericRenderer(Metal)`.
pub type Renderer = generic::Renderer<Metal>;

/// Zig: `pub const GraphicsAPI = Metal`.
pub type GraphicsAPI = Metal;

pub const CUSTOM_SHADER_TARGET: shadertoy::Target = shadertoy::Target::Msl;
/// The fragCoord for Metal shaders is +Y = down.
pub const CUSTOM_SHADER_Y_IS_DOWN: bool = true;

/// Triple buffering.
pub const SWAP_CHAIN_COUNT: usize = 3;

const LOG: &str = "metal";

/// Errors from the Metal graphics API (Zig: inferred error sets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    MetalFailed,
    ObjCFailed,
    NoMetalDevice,
    Macos(crate::pkg::macos::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

impl From<crate::pkg::macos::Error> for Error {
    fn from(e: crate::pkg::macos::Error) -> Self {
        Error::Macos(e)
    }
}

/// The parts of the embedded apprt surface Metal reads (Zig reads
/// `rt_surface.content_scale.x` and `rt_surface.platform` directly). This
/// is a trait so that test builds, whose `apprt::Surface` is the recording
/// stand-in, still type-check.
pub trait EmbeddedSurface {
    fn content_scale_x(&self) -> f64;
    fn nsview(&self) -> objc::Id;
}

#[cfg(not(test))]
impl EmbeddedSurface for apprt::embedded::Surface {
    fn content_scale_x(&self) -> f64 {
        self.content_scale.x as f64
    }

    fn nsview(&self) -> objc::Id {
        match self.platform {
            apprt::embedded::Platform::Macos(v) => v.nsview.value,
            apprt::embedded::Platform::Ios(v) => v.uiview.value,
        }
    }
}

#[cfg(test)]
impl EmbeddedSurface for apprt::Surface {
    fn content_scale_x(&self) -> f64 {
        self.content_scale.x as f64
    }

    fn nsview(&self) -> objc::Id {
        std::ptr::null_mut()
    }
}

#[derive(Debug)]
pub struct Metal {
    pub layer: IOSurfaceLayer,

    /// The shared render device.
    pub device: *const Device,

    /// MTLCommandQueue
    pub queue: objc::Object,

    /// Alpha blending mode
    pub blending: AlphaBlending,

    /// We start an AutoreleasePool before `drawFrame` and end it afterwards.
    pub autorelease_pool: Option<objc::AutoreleasePool>,
}

// SAFETY: like Zig, the Metal objects are used from the render thread
// (and the main thread for display callbacks) as Metal permits.
unsafe impl Send for Metal {}

struct ViewInfo {
    view: objc::Object,
    scale_factor: f64,
}

impl Metal {
    pub fn init(device: *const Device, opts: &Options) -> Result<Metal, Error> {
        // SAFETY: the device outlives every renderer created from it.
        let dev = unsafe { &*device };

        // Create a MTLCommandQueue for our app-scoped MTLDevice.
        let mtl_device = dev.device;
        // SAFETY: valid MTLDevice.
        let queue: objc::Object = unsafe { mtl_device.msg_send(objc::sel(c"newCommandQueue"), ()) };

        // Get the metadata about our underlying view that we'll be rendering to.
        // SAFETY: the apprt surface outlives the renderer.
        let info = unsafe {
            let rt_surface = &*opts.rt_surface;
            ViewInfo {
                scale_factor: rt_surface.content_scale_x(),
                view: objc::Object::from_id(rt_surface.nsview()),
            }
        };

        // Create an IOSurfaceLayer which we can assign to the view to make
        // it in to a "layer-hosting view", so that we can manually control
        // the layer contents.
        let layer = match IOSurfaceLayer::init() {
            Ok(l) => l,
            Err(err) => {
                queue.release();
                return Err(err);
            }
        };

        // SAFETY: valid objects (messages to a nil view are no-ops).
        unsafe {
            // Add our layer to the view.
            //
            // On macOS we do this by making the view "layer-hosting"
            // by assigning it to the view's `layer` property BEFORE
            // setting `wantsLayer` to `true`.
            info.view.set_property(c"layer", layer.layer.value);
            info.view.set_property(c"wantsLayer", objc::YES);

            // Ensure that if our layer is oversized it
            // does not overflow the bounds of the view.
            info.view.set_property(c"clipsToBounds", objc::YES);

            // Ensure that our layer has a content scale set to
            // match the scale factor of the window. This avoids
            // magnification issues leading to blurry rendering.
            layer.layer.set_property(c"contentsScale", info.scale_factor);

            // This makes it so that our display callback will actually be called.
            layer.layer.set_property(c"needsDisplayOnBoundsChange", objc::YES);
        }

        Ok(Metal {
            layer,
            device,
            queue,
            blending: opts.config.blending,
            autorelease_pool: None,
        })
    }

    fn dev(&self) -> &Device {
        // SAFETY: the device outlives every renderer created from it.
        unsafe { &*self.device }
    }

    pub fn deinit(&mut self) {
        self.queue.release();

        // The MTLDevice is app-scoped and destroyed
        // by the render device, not by us.

        self.layer.release();
    }

    pub fn loop_enter(&mut self, renderer: *mut Renderer) {
        self.layer
            .set_display_callback(Some(display_callback), renderer.cast::<c_void>());
    }

    /// Actions taken before doing anything in `drawFrame`.
    ///
    /// Right now we use this to start an AutoreleasePool.
    pub fn draw_frame_start(&mut self) {
        debug_assert!(self.autorelease_pool.is_none());
        self.autorelease_pool = Some(objc::AutoreleasePool::init());
    }

    /// Actions taken after `drawFrame` is done.
    ///
    /// Right now we use this to end our AutoreleasePool.
    pub fn draw_frame_end(&mut self) {
        debug_assert!(self.autorelease_pool.is_some());
        if let Some(pool) = self.autorelease_pool.take() {
            pool.deinit();
        }
    }

    pub fn init_shaders(&self, custom_shaders: &[&CStr]) -> Result<shaders::Shaders, Error> {
        shaders::Shaders::init(
            self.dev().device,
            custom_shaders,
            // Using an `*_srgb` pixel format makes Metal gamma encode
            // the pixels written to it *after* blending, which means
            // we get linear alpha blending rather than gamma-incorrect
            // blending.
            if self.blending.is_linear() {
                mtl::MTLPixelFormat::Bgra8unormSrgb
            } else {
                mtl::MTLPixelFormat::Bgra8unorm
            },
        )
    }

    /// Get the current size of the runtime surface.
    pub fn surface_size(&self) -> Result<SurfaceSize, Error> {
        // SAFETY: valid layer.
        let (bounds, scale): (graphics::Rect, f64) = unsafe {
            (
                self.layer.layer.get_property(c"bounds"),
                self.layer.layer.get_property(c"contentsScale"),
            )
        };

        // We need to clamp our runtime surface size to the maximum
        // possible texture size since we can't create a screen buffer (texture)
        // larger than that.
        Ok(SurfaceSize {
            width: ((bounds.size.width * scale) as u32).min(self.dev().max_texture_size),
            height: ((bounds.size.height * scale) as u32).min(self.dev().max_texture_size),
        })
    }

    /// Initialize a new render target which can be presented by this API.
    pub fn init_target(&self, width: usize, height: usize) -> Result<Target, Error> {
        Target::init(target::Options {
            device: self.dev().device,
            // Using an `*_srgb` pixel format makes Metal gamma encode the pixels
            // written to it *after* blending, which means we get linear alpha
            // blending rather than gamma-incorrect blending.
            pixel_format: if self.blending.is_linear() {
                mtl::MTLPixelFormat::Bgra8unormSrgb
            } else {
                mtl::MTLPixelFormat::Bgra8unorm
            },
            storage_mode: self.dev().default_storage_mode,
            width,
            height,
        })
    }

    /// Present the provided target.
    #[inline]
    pub fn present(&mut self, target: Target, sync: bool) -> Result<(), Error> {
        if sync {
            self.layer.set_surface_sync(target.surface);
        } else {
            self.layer.set_surface(target.surface)?;
        }
        Ok(())
    }

    /// Returns the options to use when constructing buffers.
    #[inline]
    pub fn buffer_options(&self) -> buffer::Options {
        buffer::Options {
            device: self.dev().device,
            resource_options: mtl::MTLResourceOptions {
                // Indicate that the CPU writes to this resource but never reads it.
                cpu_cache_mode: mtl::CPUCacheMode::WriteCombined,
                storage_mode: self.dev().default_storage_mode,
                hazard_tracking_mode: mtl::HazardTrackingMode::Default,
            },
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
            device: self.dev().device,
            // Using an `*_srgb` pixel format makes Metal gamma encode the pixels
            // written to it *after* blending, which means we get linear alpha
            // blending rather than gamma-incorrect blending.
            pixel_format: if self.blending.is_linear() {
                mtl::MTLPixelFormat::Bgra8unormSrgb
            } else {
                mtl::MTLPixelFormat::Bgra8unorm
            },
            resource_options: mtl::MTLResourceOptions {
                // Indicate that the CPU writes to this resource but never reads it.
                cpu_cache_mode: mtl::CPUCacheMode::WriteCombined,
                storage_mode: self.dev().default_storage_mode,
                hazard_tracking_mode: mtl::HazardTrackingMode::Default,
            },
            usage: mtl::MTLTextureUsage {
                // textureOptions is currently only used for custom shaders,
                // which require both the shader read (for when multiple shaders
                // are chained) and render target (for the final output) usage.
                // Disabling either of these will lead to metal validation
                // errors in Xcode.
                shader_read: true,
                render_target: true,
                ..Default::default()
            },
        }
    }

    #[inline]
    pub fn sampler_options(&self) -> sampler::Options {
        sampler::Options {
            device: self.dev().device,

            // These parameters match Shadertoy behaviors.
            min_filter: mtl::MTLSamplerMinMagFilter::Linear,
            mag_filter: mtl::MTLSamplerMinMagFilter::Linear,
            s_address_mode: mtl::MTLSamplerAddressMode::ClampToEdge,
            t_address_mode: mtl::MTLSamplerAddressMode::ClampToEdge,
        }
    }

    /// Returns the options to use when constructing textures for images.
    #[inline]
    pub fn image_texture_options(&self, format: ImageTextureFormat, srgb: bool) -> texture::Options {
        texture::Options {
            device: self.dev().device,
            pixel_format: format.to_pixel_format(srgb),
            resource_options: mtl::MTLResourceOptions {
                // Indicate that the CPU writes to this resource but never reads it.
                cpu_cache_mode: mtl::CPUCacheMode::WriteCombined,
                storage_mode: self.dev().default_storage_mode,
                hazard_tracking_mode: mtl::HazardTrackingMode::Default,
            },
            usage: mtl::MTLTextureUsage {
                // We only need to read from this texture from a shader.
                shader_read: true,
                ..Default::default()
            },
        }
    }

    /// Initializes a Texture suitable for the provided font atlas.
    pub fn init_atlas_texture(&self, atlas: &font::Atlas) -> Result<Texture, texture::Error> {
        let pixel_format = match atlas.format {
            font::atlas::Format::Grayscale => mtl::MTLPixelFormat::R8unorm,
            font::atlas::Format::Bgra => mtl::MTLPixelFormat::Bgra8unormSrgb,
            _ => panic!("unsupported atlas format for Metal texture"),
        };

        Texture::init(
            texture::Options {
                device: self.dev().device,
                pixel_format,
                resource_options: mtl::MTLResourceOptions {
                    // Indicate that the CPU writes to this resource but never reads it.
                    cpu_cache_mode: mtl::CPUCacheMode::WriteCombined,
                    storage_mode: self.dev().default_storage_mode,
                    hazard_tracking_mode: mtl::HazardTrackingMode::Default,
                },
                usage: mtl::MTLTextureUsage {
                    // We only need to read from this texture from a shader.
                    shader_read: true,
                    ..Default::default()
                },
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
        Ok(Frame::begin(frame::Options { queue: self.queue }, renderer, target))
    }
}

/// The IOSurfaceLayer display callback: draw a frame synchronously.
fn display_callback(renderer: *mut c_void) {
    let renderer = renderer.cast::<Renderer>();
    // SAFETY: the callback is cleared before the renderer is freed (see
    // `IOSurfaceLayer::release`).
    if let Err(err) = unsafe { (*renderer).draw_frame(true) } {
        log::warn!(target: LOG, "Error drawing frame in display callback, err={err}");
    }
}

impl GraphicsApi for Metal {
    type ExportedFrame = ();
    const EXPORTS_FRAMES: bool = false;
    const CUSTOM_SHADER_TARGET: shadertoy::Target = CUSTOM_SHADER_TARGET;
    const CUSTOM_SHADER_Y_IS_DOWN: bool = CUSTOM_SHADER_Y_IS_DOWN;
    const SWAP_CHAIN_COUNT: usize = SWAP_CHAIN_COUNT;

    fn init(device: *const Device, options: &Options) -> Result<Self, generic::Error> {
        Ok(Metal::init(device, options)?)
    }

    fn deinit(&mut self) {
        Metal::deinit(self)
    }

    fn loop_enter(&mut self, renderer: *mut Renderer) {
        Metal::loop_enter(self, renderer)
    }

    fn set_blending(&mut self, blending: AlphaBlending) {
        self.blending = blending;
    }

    fn surface_size(&self) -> Result<SurfaceSize, generic::Error> {
        Ok(Metal::surface_size(self)?)
    }

    fn draw_frame_start(&mut self) {
        Metal::draw_frame_start(self)
    }

    fn draw_frame_end(&mut self) {
        Metal::draw_frame_end(self)
    }

    fn init_shaders(&self, custom_shaders: &[&CStr]) -> Result<shaders::Shaders, generic::Error> {
        Ok(Metal::init_shaders(self, custom_shaders)?)
    }

    fn init_target(&self, width: usize, height: usize) -> Result<Target, generic::Error> {
        Ok(Metal::init_target(self, width, height)?)
    }

    fn uniform_buffer_options(&self) -> buffer::Options {
        Metal::uniform_buffer_options(self)
    }

    fn fg_buffer_options(&self) -> buffer::Options {
        Metal::fg_buffer_options(self)
    }

    fn bg_buffer_options(&self) -> buffer::Options {
        Metal::bg_buffer_options(self)
    }

    fn image_buffer_options(&self) -> buffer::Options {
        Metal::image_buffer_options(self)
    }

    fn bg_image_buffer_options(&self) -> buffer::Options {
        Metal::bg_image_buffer_options(self)
    }

    fn texture_options(&self) -> texture::Options {
        Metal::texture_options(self)
    }

    fn sampler_options(&self) -> sampler::Options {
        Metal::sampler_options(self)
    }

    fn image_texture_options(&self, format: ImageTextureFormat, srgb: bool) -> texture::Options {
        Metal::image_texture_options(self, format, srgb)
    }

    fn init_atlas_texture(&self, atlas: &font::Atlas) -> Result<Texture, generic::Error> {
        Ok(Metal::init_atlas_texture(self, atlas)?)
    }

    fn begin_frame(&self, renderer: *mut Renderer, target: *mut Target) -> Result<Frame, generic::Error> {
        Ok(Metal::begin_frame(self, renderer, target)?)
    }
}

/// Zig: the anonymous `struct { width: u32, height: u32 }` returned by
/// `surfaceSize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceSize {
    pub width: u32,
    pub height: u32,
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
    fn to_pixel_format(self, srgb: bool) -> mtl::MTLPixelFormat {
        use mtl::MTLPixelFormat as F;
        match self {
            ImageTextureFormat::Gray => {
                if srgb {
                    F::R8unormSrgb
                } else {
                    F::R8unorm
                }
            }
            ImageTextureFormat::Rgba => {
                if srgb {
                    F::Rgba8unormSrgb
                } else {
                    F::Rgba8unorm
                }
            }
            ImageTextureFormat::Bgra => {
                if srgb {
                    F::Bgra8unormSrgb
                } else {
                    F::Bgra8unorm
                }
            }
        }
    }
}

/// Check an `NSError**` out-parameter, logging and failing if set
/// (Zig: `checkError` in Pipeline.zig and shaders.zig).
pub(crate) fn check_error(err: objc::Id) -> Result<(), Error> {
    if err.is_null() {
        return Ok(());
    }
    let nserr = objc::Object::from_id(err);
    // SAFETY: an NSError's localizedDescription is an NSString, which is
    // toll-free bridged with CFString.
    unsafe {
        let desc: objc::Id = nserr.get_property(c"localizedDescription");
        if let Some(s) = crate::pkg::macos::foundation::String::from_ptr(desc) {
            let msg = s
                .cstring_ptr(crate::pkg::macos::foundation::StringEncoding::Ascii)
                .map_or("(unknown)".into(), |c| c.to_string_lossy());
            log::error!(target: LOG, "metal error={msg}");
        }
    }
    Err(Error::MetalFailed)
}

/// A minimal zig-objc-shaped Objective-C runtime binding.
///
/// NOTE(port): zig-objc is a third-party Zig package with no Rust port;
/// this binding is shared by the Metal renderer and the embedded apprt.
pub mod objc {
    use std::ffi::{c_char, c_int, c_ulong, c_void, CStr, CString};

    /// `id`
    pub type Id = *mut c_void;
    /// `SEL`
    pub type SEL = *const c_void;
    /// `IMP`
    pub type IMP = *const c_void;
    /// `BOOL` as transmitted in messages (`signed char` on x86_64, `bool`
    /// on arm64; both are one byte holding 0 or 1).
    pub type BOOL = i8;
    pub const YES: BOOL = 1;
    pub const NO: BOOL = 0;

    #[link(name = "objc")]
    extern "C" {
        fn objc_msgSend();
        #[cfg(target_arch = "x86_64")]
        fn objc_msgSend_stret();
        fn sel_registerName(name: *const c_char) -> SEL;
        fn objc_getClass(name: *const c_char) -> Id;
        fn objc_retain(obj: Id) -> Id;
        fn objc_release(obj: Id);
        fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra_bytes: usize) -> Id;
        fn objc_registerClassPair(cls: Id);
        fn objc_disposeClassPair(cls: Id);
        fn class_addIvar(cls: Id, name: *const c_char, size: usize, alignment: u8, types: *const c_char) -> BOOL;
        fn class_replaceMethod(cls: Id, name: SEL, imp: IMP, types: *const c_char) -> IMP;
        fn object_setInstanceVariable(obj: Id, name: *const c_char, value: *mut c_void) -> *mut c_void;
        fn object_getInstanceVariable(obj: Id, name: *const c_char, out_value: *mut *mut c_void) -> *mut c_void;
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    // libclosure (part of libSystem).
    extern "C" {
        static _NSConcreteStackBlock: [*const c_void; 32];
        fn _Block_object_assign(dest_addr: *mut c_void, object: *const c_void, flags: c_int);
        fn _Block_object_dispose(object: *const c_void, flags: c_int);
    }

    /// `BLOCK_FIELD_IS_OBJECT`
    pub const BLOCK_FIELD_IS_OBJECT: c_int = 3;
    /// `BLOCK_HAS_COPY_DISPOSE`
    pub const BLOCK_HAS_COPY_DISPOSE: c_int = 1 << 25;

    /// Register (or look up) a selector.
    pub fn sel(name: &CStr) -> SEL {
        // SAFETY: valid C string.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    /// Look up a class by name.
    pub fn get_class(name: &CStr) -> Option<Class> {
        // SAFETY: valid C string.
        let cls = unsafe { objc_getClass(name.as_ptr()) };
        (!cls.is_null()).then_some(Class { value: cls })
    }

    /// Argument tuples that can be passed through `objc_msgSend`.
    pub trait MessageArguments: Copy {
        /// # Safety
        ///
        /// `imp` must be `objc_msgSend` (or its stret variant matching
        /// `R`) and the receiver must respond to `sel` with these
        /// argument and return types.
        unsafe fn invoke<R>(imp: unsafe extern "C" fn(), obj: Id, sel: SEL, args: Self) -> R;
    }

    macro_rules! message_args {
        ($($a:ident : $t:ident),*) => {
            impl<$($t: Copy),*> MessageArguments for ($($t,)*) {
                unsafe fn invoke<R>(imp: unsafe extern "C" fn(), obj: Id, sel: SEL, args: Self) -> R {
                    let ($($a,)*) = args;
                    // SAFETY: caller guarantee; all fn pointers have the
                    // same size.
                    unsafe {
                        let f: unsafe extern "C" fn(Id, SEL $(, $t)*) -> R =
                            std::mem::transmute_copy(&imp);
                        f(obj, sel $(, $a)*)
                    }
                }
            }
        };
    }

    message_args!();
    message_args!(a: A);
    message_args!(a: A, b: B);
    message_args!(a: A, b: B, c: C);
    message_args!(a: A, b: B, c: C, d: D);

    /// Send a message (zig-objc `msgSend`).
    ///
    /// # Safety
    ///
    /// The receiver must respond to `sel` with these argument and return
    /// types.
    pub unsafe fn msg_send<R, A: MessageArguments>(obj: Id, sel: SEL, args: A) -> R {
        // On x86_64, structs returned in memory use objc_msgSend_stret.
        #[cfg(target_arch = "x86_64")]
        let imp: unsafe extern "C" fn() = if std::mem::size_of::<R>() > 16 {
            objc_msgSend_stret
        } else {
            objc_msgSend
        };
        #[cfg(not(target_arch = "x86_64"))]
        let imp: unsafe extern "C" fn() = objc_msgSend;
        // SAFETY: caller guarantee.
        unsafe { A::invoke(imp, obj, sel, args) }
    }

    /// The setter selector for a property name: `foo` -> `setFoo:`.
    fn setter(name: &CStr) -> SEL {
        let bytes = name.to_bytes();
        let mut s = Vec::with_capacity(bytes.len() + 5);
        s.extend_from_slice(b"set");
        if let Some((first, rest)) = bytes.split_first() {
            s.push(first.to_ascii_uppercase());
            s.extend_from_slice(rest);
        }
        s.push(b':');
        let c = CString::new(s).expect("property names have no NUL");
        sel(&c)
    }

    /// An Objective-C object (zig-objc `Object`).
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Object {
        pub value: Id,
    }

    impl Object {
        pub fn from_id(id: Id) -> Object {
            Object { value: id }
        }

        /// # Safety
        ///
        /// See [`msg_send`].
        pub unsafe fn msg_send<R, A: MessageArguments>(self, sel: SEL, args: A) -> R {
            // SAFETY: caller guarantee.
            unsafe { msg_send(self.value, sel, args) }
        }

        /// Get a property by sending its getter message.
        ///
        /// # Safety
        ///
        /// The property must exist with type `R`.
        pub unsafe fn get_property<R>(self, name: &CStr) -> R {
            // SAFETY: caller guarantee.
            unsafe { msg_send(self.value, sel(name), ()) }
        }

        /// Set a property by sending its setter message.
        ///
        /// # Safety
        ///
        /// The property must exist with type `T`.
        pub unsafe fn set_property<T: Copy>(self, name: &CStr, value: T) {
            // SAFETY: caller guarantee.
            unsafe { msg_send::<(), _>(self.value, setter(name), (value,)) }
        }

        pub fn retain(self) -> Object {
            // SAFETY: retaining a valid (or nil) object.
            Object::from_id(unsafe { objc_retain(self.value) })
        }

        pub fn release(self) {
            // SAFETY: releasing an owned (or nil) object.
            unsafe { objc_release(self.value) }
        }

        /// # Safety
        ///
        /// The object must have an instance variable with this name.
        pub unsafe fn set_instance_variable(self, name: &CStr, value: Object) {
            // SAFETY: caller guarantee.
            unsafe { object_setInstanceVariable(self.value, name.as_ptr(), value.value) };
        }

        /// # Safety
        ///
        /// The object must have an instance variable with this name.
        pub unsafe fn get_instance_variable(self, name: &CStr) -> Object {
            let mut out: *mut c_void = std::ptr::null_mut();
            // SAFETY: caller guarantee.
            unsafe { object_getInstanceVariable(self.value, name.as_ptr(), &mut out) };
            Object::from_id(out)
        }
    }

    /// An Objective-C class (zig-objc `Class`).
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Class {
        pub value: Id,
    }

    impl Class {
        /// # Safety
        ///
        /// See [`msg_send`].
        pub unsafe fn msg_send<R, A: MessageArguments>(self, sel: SEL, args: A) -> R {
            // SAFETY: caller guarantee.
            unsafe { msg_send(self.value, sel, args) }
        }

        /// Add an `id`-typed instance variable. Only valid between
        /// [`allocate_class_pair`] and [`register_class_pair`].
        pub fn add_ivar(self, name: &CStr) -> bool {
            // SAFETY: valid class and C strings.
            unsafe {
                class_addIvar(
                    self.value,
                    name.as_ptr(),
                    std::mem::size_of::<Id>(),
                    std::mem::align_of::<Id>().trailing_zeros() as u8,
                    c"@".as_ptr(),
                ) != NO
            }
        }

        /// Replace (or add) a method implementation.
        ///
        /// # Safety
        ///
        /// `imp` must be an `extern "C"` function matching `types`.
        pub unsafe fn replace_method(self, name: &CStr, imp: IMP, types: &CStr) {
            // SAFETY: caller guarantee.
            unsafe { class_replaceMethod(self.value, sel(name), imp, types.as_ptr()) };
        }
    }

    pub fn allocate_class_pair(superclass: Class, name: &CStr) -> Option<Class> {
        // SAFETY: valid class and C string.
        let cls = unsafe { objc_allocateClassPair(superclass.value, name.as_ptr(), 0) };
        (!cls.is_null()).then_some(Class { value: cls })
    }

    pub fn register_class_pair(cls: Class) {
        // SAFETY: a class from `allocate_class_pair`.
        unsafe { objc_registerClassPair(cls.value) }
    }

    pub fn dispose_class_pair(cls: Class) {
        // SAFETY: a class from `allocate_class_pair`.
        unsafe { objc_disposeClassPair(cls.value) }
    }

    /// An autorelease pool (zig-objc `AutoreleasePool`).
    #[derive(Debug)]
    pub struct AutoreleasePool(*mut c_void);

    impl AutoreleasePool {
        pub fn init() -> AutoreleasePool {
            // SAFETY: plain runtime call.
            AutoreleasePool(unsafe { objc_autoreleasePoolPush() })
        }

        pub fn deinit(self) {
            // SAFETY: the pool was pushed by `init`.
            unsafe { objc_autoreleasePoolPop(self.0) }
        }
    }

    /// A block descriptor (`Block_descriptor_1` + copy/dispose helpers).
    #[repr(C)]
    #[derive(Debug)]
    pub struct BlockDescriptor {
        pub reserved: c_ulong,
        pub size: c_ulong,
        pub copy_helper: Option<unsafe extern "C" fn(dst: *mut c_void, src: *const c_void)>,
        pub dispose_helper: Option<unsafe extern "C" fn(src: *const c_void)>,
    }

    // SAFETY: descriptors are immutable statics.
    unsafe impl Sync for BlockDescriptor {}

    /// A stack block literal with captured context `C` (zig-objc
    /// `Block(Captures, Args, Ret).Context`). Captures are accessible as
    /// `block.captures`.
    #[repr(C)]
    #[derive(Debug)]
    pub struct Block<C> {
        pub isa: *const c_void,
        pub flags: c_int,
        pub reserved: c_int,
        pub invoke: *const c_void,
        pub descriptor: *const BlockDescriptor,
        pub captures: C,
    }

    impl<C> Block<C> {
        /// Create a stack block. `invoke` must be an `extern "C"` function
        /// taking `*const Block<C>` followed by the block's arguments, and
        /// `descriptor.size` must be `size_of::<Block<C>>()`.
        pub fn init(captures: C, invoke: *const c_void, descriptor: &'static BlockDescriptor) -> Block<C> {
            debug_assert_eq!(descriptor.size as usize, std::mem::size_of::<Block<C>>());
            let flags = if descriptor.copy_helper.is_some() {
                BLOCK_HAS_COPY_DISPOSE
            } else {
                0
            };
            Block {
                isa: std::ptr::addr_of!(_NSConcreteStackBlock).cast(),
                flags,
                reserved: 0,
                invoke,
                descriptor,
                captures,
            }
        }
    }

    /// `_Block_object_assign` for an object captured in a block
    /// copy helper.
    ///
    /// # Safety
    ///
    /// Called from a block copy helper with valid addresses.
    pub unsafe fn block_retain_object(dest_addr: *mut Id, object: Id) {
        // SAFETY: caller guarantee.
        unsafe { _Block_object_assign(dest_addr.cast(), object, BLOCK_FIELD_IS_OBJECT) }
    }

    /// `_Block_object_dispose` for an object captured in a block.
    ///
    /// # Safety
    ///
    /// Called from a block dispose helper.
    pub unsafe fn block_release_object(object: Id) {
        // SAFETY: caller guarantee.
        unsafe { _Block_object_dispose(object, BLOCK_FIELD_IS_OBJECT) }
    }
}
