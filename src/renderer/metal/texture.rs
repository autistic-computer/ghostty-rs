//! Port of `src/renderer/metal/Texture.zig`.
//!
//! STATUS: ported (macOS only; type-checked, not run)
//!
//! Wrapper for handling textures.

use std::ffi::{c_ulong, c_void};
use std::fmt;

use super::api as mtl;
use super::objc;

/// Options for initializing a texture.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// MTLDevice
    pub device: objc::Object,
    pub pixel_format: mtl::MTLPixelFormat,
    pub resource_options: mtl::MTLResourceOptions,
    pub usage: mtl::MTLTextureUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Texture {
    /// The underlying MTLTexture Object.
    pub texture: objc::Object,

    /// The width of this texture.
    pub width: usize,
    /// The height of this texture.
    pub height: usize,

    /// Bytes per pixel for this texture.
    pub bpp: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A Metal API call failed.
    MetalFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MetalFailed")
    }
}

impl std::error::Error for Error {}

impl Texture {
    /// Initialize a texture
    pub fn init(opts: Options, width: usize, height: usize, data: Option<&[u8]>) -> Result<Texture, Error> {
        // SAFETY: standard Metal object creation.
        let id: *mut c_void = unsafe {
            // Create our descriptor
            let class = objc::get_class(c"MTLTextureDescriptor").ok_or(Error::MetalFailed)?;
            let id_alloc: objc::Object = class.msg_send(objc::sel(c"alloc"), ());
            let desc: objc::Object = id_alloc.msg_send(objc::sel(c"init"), ());

            // Set our properties
            desc.set_property(c"pixelFormat", opts.pixel_format as c_ulong);
            desc.set_property(c"width", width as c_ulong);
            desc.set_property(c"height", height as c_ulong);
            desc.set_property(c"resourceOptions", opts.resource_options.to_bits());
            desc.set_property(c"usage", opts.usage.to_bits());

            // Initialize
            let id = opts
                .device
                .msg_send(objc::sel(c"newTextureWithDescriptor:"), (desc.value,));
            desc.release();
            id
        };
        if id.is_null() {
            return Err(Error::MetalFailed);
        }

        let this = Texture {
            texture: objc::Object::from_id(id),
            width,
            height,
            bpp: bpp_of(opts.pixel_format),
        };

        // If we have data, we set it here.
        if let Some(d) = data {
            debug_assert_eq!(d.len(), width * height * this.bpp);
            this.replace_region(0, 0, width, height, d);
        }

        Ok(this)
    }

    pub fn deinit(&self) {
        self.texture.release();
    }

    /// Replace a region of the texture with the provided data.
    ///
    /// Does NOT check the dimensions of the data to ensure correctness.
    pub fn replace_region(&self, x: usize, y: usize, width: usize, height: usize, data: &[u8]) {
        // SAFETY: valid MTLTexture; `data` covers the region (caller
        // contract, like Zig).
        unsafe {
            self.texture.msg_send::<(), _>(
                objc::sel(c"replaceRegion:mipmapLevel:withBytes:bytesPerRow:"),
                (
                    mtl::MTLRegion {
                        origin: mtl::MTLOrigin {
                            x: x as c_ulong,
                            y: y as c_ulong,
                            z: 0,
                        },
                        size: mtl::MTLSize {
                            width: width as c_ulong,
                            height: height as c_ulong,
                            depth: 1,
                        },
                    },
                    0 as c_ulong,
                    data.as_ptr() as *const c_void,
                    (self.bpp * width) as c_ulong,
                ),
            )
        }
    }
}

/// Returns the bytes per pixel for the provided pixel format
fn bpp_of(pixel_format: mtl::MTLPixelFormat) -> usize {
    use mtl::MTLPixelFormat as F;
    match pixel_format {
        // Invalid
        F::Invalid => panic!("invalid pixel format"),

        // 8-bit pixel formats
        F::A8unorm
        | F::R8unorm
        | F::R8unormSrgb
        | F::R8snorm
        | F::R8uint
        | F::R8sint
        | F::Rg8unorm
        | F::Rg8unormSrgb
        | F::Rg8snorm
        | F::Rg8uint
        | F::Rg8sint
        | F::Stencil8 => 1,

        // 16-bit pixel formats
        F::R16unorm
        | F::R16snorm
        | F::R16uint
        | F::R16sint
        | F::R16float
        | F::Rg16unorm
        | F::Rg16snorm
        | F::Rg16uint
        | F::Rg16sint
        | F::Rg16float
        | F::B5g6r5unorm
        | F::A1bgr5unorm
        | F::Abgr4unorm
        | F::Bgr5a1unorm
        | F::Depth16unorm => 2,

        // 32-bit pixel formats
        F::Rgba8unorm
        | F::Rgba8unormSrgb
        | F::Rgba8snorm
        | F::Rgba8uint
        | F::Rgba8sint
        | F::Bgra8unorm
        | F::Bgra8unormSrgb
        | F::Rgb10a2unorm
        | F::Rgb10a2uint
        | F::Rg11b10float
        | F::Rgb9e5float
        | F::Bgr10a2unorm
        | F::Bgr10Xr
        | F::Bgr10XrSrgb
        | F::R32uint
        | F::R32sint
        | F::R32float
        | F::Depth32float
        | F::Depth24unormStencil8 => 4,

        // 64-bit pixel formats
        F::Rg32uint
        | F::Rg32sint
        | F::Rg32float
        | F::Rgba16unorm
        | F::Rgba16snorm
        | F::Rgba16uint
        | F::Rgba16sint
        | F::Rgba16float
        | F::Bgra10Xr
        | F::Bgra10XrSrgb => 8,

        // 128-bit pixel formats,
        F::Rgba32uint | F::Rgba32sint | F::Rgba32float => 128,

        // Weird formats I was too lazy to get the sizes of
        _ => panic!(
            "pixel format size unknown (unlikely that this format was actually used, could be memory corruption)"
        ),
    }
}
