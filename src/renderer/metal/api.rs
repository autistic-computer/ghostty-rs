//! Port of `src/renderer/metal/api.zig`.
//!
//! STATUS: ported
//!
//! This file contains the definitions of the Metal API that we use.
//!
//! Because the online Apple developer docs have recently (as of January 2025)
//! been changed to hide enum values, `Metal-cpp` has been used as a reference
//! source instead.
//!
//! Ref: https://developer.apple.com/metal/cpp/
//!
//! Zig `enum(c_ulong)`s are `#[repr(u64)]` enums (`c_ulong` is 64 bits on
//! Apple platforms); pass them to Objective-C with `as c_ulong`. The
//! non-exhaustive `MTLCommandBufferStatus` is a newtype over its integer.
//! The `packed struct(c_ulong)` option sets have `to_bits`.

use std::ffi::{c_ulong, c_void};

/// https://developer.apple.com/documentation/metal/mtlcommandbufferstatus?language=objc
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MTLCommandBufferStatus(pub c_ulong);

impl MTLCommandBufferStatus {
    pub const NOT_ENQUEUED: MTLCommandBufferStatus = MTLCommandBufferStatus(0);
    pub const ENQUEUED: MTLCommandBufferStatus = MTLCommandBufferStatus(1);
    pub const COMMITTED: MTLCommandBufferStatus = MTLCommandBufferStatus(2);
    pub const SCHEDULED: MTLCommandBufferStatus = MTLCommandBufferStatus(3);
    pub const COMPLETED: MTLCommandBufferStatus = MTLCommandBufferStatus(4);
    pub const ERROR: MTLCommandBufferStatus = MTLCommandBufferStatus(5);
}

/// https://developer.apple.com/documentation/metal/mtlresourceoptions?language=objc
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MTLResourceOptions {
    /// https://developer.apple.com/documentation/metal/mtlcpucachemode?language=objc
    pub cpu_cache_mode: CPUCacheMode,
    /// https://developer.apple.com/documentation/metal/mtlstoragemode?language=objc
    pub storage_mode: StorageMode,
    /// https://developer.apple.com/documentation/metal/mtlhazardtrackingmode?language=objc
    pub hazard_tracking_mode: HazardTrackingMode,
}

impl MTLResourceOptions {
    /// Options with the Zig field defaults for everything but the storage
    /// mode.
    pub const fn new(storage_mode: StorageMode) -> MTLResourceOptions {
        MTLResourceOptions {
            cpu_cache_mode: CPUCacheMode::Default,
            storage_mode,
            hazard_tracking_mode: HazardTrackingMode::Default,
        }
    }

    /// The `packed struct(c_ulong)` bits: cache mode in bits 0-3, storage
    /// mode in bits 4-7, hazard tracking mode in bits 8-9.
    pub const fn to_bits(self) -> c_ulong {
        (self.cpu_cache_mode as c_ulong)
            | ((self.storage_mode as c_ulong) << 4)
            | ((self.hazard_tracking_mode as c_ulong) << 8)
    }
}

/// Zig: `MTLResourceOptions.CPUCacheMode` (`enum(u4)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CPUCacheMode {
    #[default]
    Default = 0,
    WriteCombined = 1,
}

/// Zig: `MTLResourceOptions.StorageMode` (`enum(u4)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageMode {
    Shared = 0,
    Managed = 1,
    Private = 2,
    Memoryless = 3,
}

/// Zig: `MTLResourceOptions.HazardTrackingMode` (`enum(u2)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HazardTrackingMode {
    #[default]
    Default = 0,
    Untracked = 1,
    Tracked = 2,
}

/// https://developer.apple.com/documentation/metal/mtltextureusage?language=objc
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MTLTextureUsage {
    /// https://developer.apple.com/documentation/metal/mtltextureusage/shaderread?language=objc
    pub shader_read: bool, // TextureUsageShaderRead = 1,

    /// https://developer.apple.com/documentation/metal/mtltextureusage/shaderwrite?language=objc
    pub shader_write: bool, // TextureUsageShaderWrite = 2,

    /// https://developer.apple.com/documentation/metal/mtltextureusage/rendertarget?language=objc
    pub render_target: bool, // TextureUsageRenderTarget = 4,

    // The enum skips from 4 to 16, 8 has no documented use.
    /// https://developer.apple.com/documentation/metal/mtltextureusage/pixelformatview?language=objc
    pub pixel_format_view: bool, // TextureUsagePixelFormatView = 16,

    /// https://developer.apple.com/documentation/metal/mtltextureusage/shaderatomic?language=objc
    pub shader_atomic: bool, // TextureUsageShaderAtomic = 32,
}

impl MTLTextureUsage {
    /// https://developer.apple.com/documentation/metal/mtltextureusage/unknown?language=objc
    #[allow(dead_code)]
    const UNKNOWN: MTLTextureUsage = MTLTextureUsage {
        shader_read: false,
        shader_write: false,
        render_target: false,
        pixel_format_view: false,
        shader_atomic: false,
    }; // TextureUsageUnknown = 0,

    /// The `packed struct(c_ulong)` bits.
    pub const fn to_bits(self) -> c_ulong {
        (self.shader_read as c_ulong)
            | ((self.shader_write as c_ulong) << 1)
            | ((self.render_target as c_ulong) << 2)
            | ((self.pixel_format_view as c_ulong) << 4)
            | ((self.shader_atomic as c_ulong) << 5)
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MTLClearColor {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MTLViewport {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub znear: f64,
    pub zfar: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MTLRegion {
    pub origin: MTLOrigin,
    pub size: MTLSize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MTLOrigin {
    pub x: c_ulong,
    pub y: c_ulong,
    pub z: c_ulong,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MTLSize {
    pub width: c_ulong,
    pub height: c_ulong,
    pub depth: c_ulong,
}

#[link(name = "Metal", kind = "framework")]
extern "C" {
    /// https://developer.apple.com/documentation/metal/1433367-mtlcopyalldevices
    pub fn MTLCopyAllDevices() -> *mut c_void;

    /// https://developer.apple.com/documentation/metal/1433401-mtlcreatesystemdefaultdevice
    pub fn MTLCreateSystemDefaultDevice() -> *mut c_void;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Not a Zig test: checks the packed option set layouts.
    #[test]
    fn packed_bits() {
        let opts = MTLResourceOptions {
            cpu_cache_mode: CPUCacheMode::WriteCombined,
            storage_mode: StorageMode::Managed,
            hazard_tracking_mode: HazardTrackingMode::Tracked,
        };
        assert_eq!(0b10_0001_0001, opts.to_bits());
        let usage = MTLTextureUsage {
            shader_read: true,
            render_target: true,
            ..Default::default()
        };
        assert_eq!(0b101, usage.to_bits());
        assert_eq!(81, MTLPixelFormat::Bgra8unormSrgb as u64);
    }
}

// Generated from the Zig `enum(c_ulong)`/`enum(c_long)` declarations.

/// https://developer.apple.com/documentation/metal/mtlloadaction?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLLoadAction {
    DontCare = 0,
    Load = 1,
    Clear = 2,
}

/// https://developer.apple.com/documentation/metal/mtlstoreaction?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLStoreAction {
    DontCare = 0,
    Store = 1,
    MultisampleResolve = 2,
    StoreAndMultisampleResolve = 3,
    Unknown = 4,
    CustomSampleDepthStore = 5,
}

/// https://developer.apple.com/documentation/metal/mtlprimitivetype?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLPrimitiveType {
    Point = 0,
    Line = 1,
    LineStrip = 2,
    Triangle = 3,
    TriangleStrip = 4,
}

/// https://developer.apple.com/documentation/metal/mtlindextype?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLIndexType {
    Uint16 = 0,
    Uint32 = 1,
}

/// https://developer.apple.com/documentation/metal/mtlvertexformat?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLVertexFormat {
    Invalid = 0,
    Uchar2 = 1,
    Uchar3 = 2,
    Uchar4 = 3,
    Char2 = 4,
    Char3 = 5,
    Char4 = 6,
    Uchar2normalized = 7,
    Uchar3normalized = 8,
    Uchar4normalized = 9,
    Char2normalized = 10,
    Char3normalized = 11,
    Char4normalized = 12,
    Ushort2 = 13,
    Ushort3 = 14,
    Ushort4 = 15,
    Short2 = 16,
    Short3 = 17,
    Short4 = 18,
    Ushort2normalized = 19,
    Ushort3normalized = 20,
    Ushort4normalized = 21,
    Short2normalized = 22,
    Short3normalized = 23,
    Short4normalized = 24,
    Half2 = 25,
    Half3 = 26,
    Half4 = 27,
    Float = 28,
    Float2 = 29,
    Float3 = 30,
    Float4 = 31,
    Int = 32,
    Int2 = 33,
    Int3 = 34,
    Int4 = 35,
    Uint = 36,
    Uint2 = 37,
    Uint3 = 38,
    Uint4 = 39,
    Int1010102normalized = 40,
    Uint1010102normalized = 41,
    Uchar4normalizedBgra = 42,
    Uchar = 45,
    Char = 46,
    Ucharnormalized = 47,
    Charnormalized = 48,
    Ushort = 49,
    Short = 50,
    Ushortnormalized = 51,
    Shortnormalized = 52,
    Half = 53,
    Floatrg11b10 = 54,
    Floatrgb9e5 = 55,
}

/// https://developer.apple.com/documentation/metal/mtlvertexstepfunction?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLVertexStepFunction {
    Constant = 0,
    PerVertex = 1,
    PerInstance = 2,
    PerPatch = 3,
    PerPatchControlPoint = 4,
}

/// https://developer.apple.com/documentation/metal/mtlpixelformat?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLPixelFormat {
    Invalid = 0,
    A8unorm = 1,
    R8unorm = 10,
    R8unormSrgb = 11,
    R8snorm = 12,
    R8uint = 13,
    R8sint = 14,
    R16unorm = 20,
    R16snorm = 22,
    R16uint = 23,
    R16sint = 24,
    R16float = 25,
    Rg8unorm = 30,
    Rg8unormSrgb = 31,
    Rg8snorm = 32,
    Rg8uint = 33,
    Rg8sint = 34,
    B5g6r5unorm = 40,
    A1bgr5unorm = 41,
    Abgr4unorm = 42,
    Bgr5a1unorm = 43,
    R32uint = 53,
    R32sint = 54,
    R32float = 55,
    Rg16unorm = 60,
    Rg16snorm = 62,
    Rg16uint = 63,
    Rg16sint = 64,
    Rg16float = 65,
    Rgba8unorm = 70,
    Rgba8unormSrgb = 71,
    Rgba8snorm = 72,
    Rgba8uint = 73,
    Rgba8sint = 74,
    Bgra8unorm = 80,
    Bgra8unormSrgb = 81,
    Rgb10a2unorm = 90,
    Rgb10a2uint = 91,
    Rg11b10float = 92,
    Rgb9e5float = 93,
    Bgr10a2unorm = 94,
    Bgr10Xr = 554,
    Bgr10XrSrgb = 555,
    Rg32uint = 103,
    Rg32sint = 104,
    Rg32float = 105,
    Rgba16unorm = 110,
    Rgba16snorm = 112,
    Rgba16uint = 113,
    Rgba16sint = 114,
    Rgba16float = 115,
    Bgra10Xr = 552,
    Bgra10XrSrgb = 553,
    Rgba32uint = 123,
    Rgba32sint = 124,
    Rgba32float = 125,
    Bc1Rgba = 130,
    Bc1RgbaSrgb = 131,
    Bc2Rgba = 132,
    Bc2RgbaSrgb = 133,
    Bc3Rgba = 134,
    Bc3RgbaSrgb = 135,
    Bc4Runorm = 140,
    Bc4Rsnorm = 141,
    Bc5Rgunorm = 142,
    Bc5Rgsnorm = 143,
    Bc6hRgbfloat = 150,
    Bc6hRgbufloat = 151,
    Bc7Rgbaunorm = 152,
    Bc7RgbaunormSrgb = 153,
    PvrtcRgb2bpp = 160,
    PvrtcRgb2bppSrgb = 161,
    PvrtcRgb4bpp = 162,
    PvrtcRgb4bppSrgb = 163,
    PvrtcRgba2bpp = 164,
    PvrtcRgba2bppSrgb = 165,
    PvrtcRgba4bpp = 166,
    PvrtcRgba4bppSrgb = 167,
    EacR11unorm = 170,
    EacR11snorm = 172,
    EacRg11unorm = 174,
    EacRg11snorm = 176,
    EacRgba8 = 178,
    EacRgba8Srgb = 179,
    Etc2Rgb8 = 180,
    Etc2Rgb8Srgb = 181,
    Etc2Rgb8a1 = 182,
    Etc2Rgb8a1Srgb = 183,
    Astc4x4Srgb = 186,
    Astc5x4Srgb = 187,
    Astc5x5Srgb = 188,
    Astc6x5Srgb = 189,
    Astc6x6Srgb = 190,
    Astc8x5Srgb = 192,
    Astc8x6Srgb = 193,
    Astc8x8Srgb = 194,
    Astc10x5Srgb = 195,
    Astc10x6Srgb = 196,
    Astc10x8Srgb = 197,
    Astc10x10Srgb = 198,
    Astc12x10Srgb = 199,
    Astc12x12Srgb = 200,
    Astc4x4Ldr = 204,
    Astc5x4Ldr = 205,
    Astc5x5Ldr = 206,
    Astc6x5Ldr = 207,
    Astc6x6Ldr = 208,
    Astc8x5Ldr = 210,
    Astc8x6Ldr = 211,
    Astc8x8Ldr = 212,
    Astc10x5Ldr = 213,
    Astc10x6Ldr = 214,
    Astc10x8Ldr = 215,
    Astc10x10Ldr = 216,
    Astc12x10Ldr = 217,
    Astc12x12Ldr = 218,
    Astc4x4Hdr = 222,
    Astc5x4Hdr = 223,
    Astc5x5Hdr = 224,
    Astc6x5Hdr = 225,
    Astc6x6Hdr = 226,
    Astc8x5Hdr = 228,
    Astc8x6Hdr = 229,
    Astc8x8Hdr = 230,
    Astc10x5Hdr = 231,
    Astc10x6Hdr = 232,
    Astc10x8Hdr = 233,
    Astc10x10Hdr = 234,
    Astc12x10Hdr = 235,
    Astc12x12Hdr = 236,
    Gbgr422 = 240,
    Bgrg422 = 241,
    Depth16unorm = 250,
    Depth32float = 252,
    Stencil8 = 253,
    Depth24unormStencil8 = 255,
    Depth32floatStencil8 = 260,
    X32Stencil8 = 261,
    X24Stencil8 = 262,
}

/// https://developer.apple.com/documentation/metal/mtlpurgeablestate?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLPurgeableState {
    KeepCurrent = 1,
    NonVolatile = 2,
    Volatile = 3,
    Empty = 4,
}

/// https://developer.apple.com/documentation/metal/mtlsamplerminmagfilter?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLSamplerMinMagFilter {
    Nearest = 0,
    Linear = 1,
}

/// https://developer.apple.com/documentation/metal/mtlsampleraddressmode?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLSamplerAddressMode {
    ClampToEdge = 0,
    MirrorClampToEdge = 1,
    Repeat = 2,
    MirrorRepeat = 3,
    ClampToZero = 4,
    ClampToBorderColor = 5,
}

/// https://developer.apple.com/documentation/metal/mtlblendfactor?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLBlendFactor {
    Zero = 0,
    One = 1,
    SourceColor = 2,
    OneMinusSourceColor = 3,
    SourceAlpha = 4,
    OneMinusSourceAlpha = 5,
    DestColor = 6,
    OneMinusDestColor = 7,
    DestAlpha = 8,
    OneMinusDestAlpha = 9,
    SourceAlphaSaturated = 10,
    BlendColor = 11,
    OneMinusBlendColor = 12,
    BlendAlpha = 13,
    OneMinusBlendAlpha = 14,
    Source1Color = 15,
    OneMinusSource1Color = 16,
    Source1Alpha = 17,
    OneMinusSource1Alpha = 18,
}

/// https://developer.apple.com/documentation/metal/mtlblendoperation?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLBlendOperation {
    Add = 0,
    Subtract = 1,
    ReverseSubtract = 2,
    Min = 3,
    Max = 4,
}

/// https://developer.apple.com/documentation/metal/mtlbarrierscope?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLBarrierScope {
    Buffers = 1,
    Textures = 2,
    RenderTargets = 4,
}

/// https://developer.apple.com/documentation/metal/mtlrenderstages?language=objc
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLRenderStage {
    Vertex = 1,
    Fragment = 2,
    Tile = 4,
    Object = 8,
    Mesh = 16,
}

/// https://developer.apple.com/documentation/metal/mtlgpufamily?language=objc
#[repr(i64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MTLGPUFamily {
    Apple1 = 1001,
    Apple2 = 1002,
    Apple3 = 1003,
    Apple4 = 1004,
    Apple5 = 1005,
    Apple6 = 1006,
    Apple7 = 1007,
    Apple8 = 1008,
    Apple9 = 1009,
    Apple10 = 1010,

    Common1 = 3001,
    Common2 = 3002,
    Common3 = 3003,

    Metal3 = 5001,
    Metal4 = 5002,
}
