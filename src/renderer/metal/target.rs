//! Port of `src/renderer/metal/Target.zig`.
//!
//! STATUS: ported (macOS only; type-checked, not run)
//!
//! Represents a render target.
//!
//! In this case, an IOSurface-backed MTLTexture.

use std::ffi::{c_int, c_ulong, c_void};

use crate::pkg::macos::graphics;
use crate::pkg::macos::iosurface::{self, IOSurface};
use crate::pkg::macos::video;

use super::api as mtl;
use super::objc;
use super::Error;

/// Options for initializing a Target
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// MTLDevice
    pub device: objc::Object,

    /// Desired width
    pub width: usize,
    /// Desired height
    pub height: usize,

    /// Pixel format for the MTLTexture
    pub pixel_format: mtl::MTLPixelFormat,
    /// Storage mode for the MTLTexture
    pub storage_mode: mtl::StorageMode,
}

#[derive(Clone, Copy)]
pub struct Target {
    /// The underlying IOSurface.
    pub surface: &'static IOSurface,

    /// The underlying MTLTexture.
    pub texture: objc::Object,

    /// Current width of this target.
    pub width: usize,
    /// Current height of this target.
    pub height: usize,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("surface", &self.surface.as_ptr())
            .field("texture", &self.texture)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

impl Target {
    pub fn init(opts: Options) -> Result<Target, Error> {
        // We set our surface's color space to Display P3.
        // This allows us to have "Apple-style" alpha blending,
        // since it seems to be the case that Apple apps like
        // Terminal and TextEdit render text in the display's
        // color space using converted colors, which reduces,
        // but does not fully eliminate blending artifacts.
        let colorspace = graphics::ColorSpace::create_named(graphics::color_space::Name::DisplayP3)?;

        let surface = IOSurface::init(iosurface::Properties {
            width: opts.width as c_int,
            height: opts.height as c_int,
            pixel_format: video::PixelFormat::_32BGRA,
            bytes_per_element: 4,
            colorspace: Some(colorspace),
        });
        colorspace.release();
        let surface = surface?;

        // SAFETY: standard Metal object creation.
        unsafe {
            // Create our descriptor
            let class = objc::get_class(c"MTLTextureDescriptor").ok_or(Error::MetalFailed)?;
            let id_alloc: objc::Object = class.msg_send(objc::sel(c"alloc"), ());
            let desc: objc::Object = id_alloc.msg_send(objc::sel(c"init"), ());

            // Set our properties
            desc.set_property(c"width", opts.width as c_ulong);
            desc.set_property(c"height", opts.height as c_ulong);
            desc.set_property(c"pixelFormat", opts.pixel_format as c_ulong);
            desc.set_property(
                c"usage",
                mtl::MTLTextureUsage {
                    render_target: true,
                    ..Default::default()
                }
                .to_bits(),
            );
            desc.set_property(
                c"resourceOptions",
                mtl::MTLResourceOptions {
                    // Indicate that the CPU writes to this resource but never reads it.
                    cpu_cache_mode: mtl::CPUCacheMode::WriteCombined,
                    storage_mode: opts.storage_mode,
                    hazard_tracking_mode: mtl::HazardTrackingMode::Default,
                }
                .to_bits(),
            );

            let id: *mut c_void = opts.device.msg_send(
                objc::sel(c"newTextureWithDescriptor:iosurface:plane:"),
                (desc.value, surface.as_ptr(), 0 as c_ulong),
            );
            desc.release();
            if id.is_null() {
                surface.deinit();
                return Err(Error::MetalFailed);
            }

            let texture = objc::Object::from_id(id);

            Ok(Target {
                surface,
                texture,
                width: opts.width,
                height: opts.height,
            })
        }
    }

    pub fn deinit(&mut self) {
        self.surface.deinit();
        self.texture.release();
    }
}
