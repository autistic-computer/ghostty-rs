//! Port of `src/macos/graphics/image.zig`.
//!
//! STATUS: ported

use super::c;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageAlphaInfo {
    None = c::kCGImageAlphaNone,
    PremultipliedLast = c::kCGImageAlphaPremultipliedLast,
    PremultipliedFirst = c::kCGImageAlphaPremultipliedFirst,
    Last = c::kCGImageAlphaLast,
    First = c::kCGImageAlphaFirst,
    NoneSkipLast = c::kCGImageAlphaNoneSkipLast,
    NoneSkipFirst = c::kCGImageAlphaNoneSkipFirst,
    Only = c::kCGImageAlphaOnly,
}

/// `CGBitmapInfo` (a non-exhaustive enum in Zig, since values are combined
/// with `ImageAlphaInfo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitmapInfo(pub u32);

impl BitmapInfo {
    pub const ALPHA_MASK: BitmapInfo = BitmapInfo(c::kCGBitmapAlphaInfoMask);
    pub const FLOAT_MASK: BitmapInfo = BitmapInfo(c::kCGBitmapFloatInfoMask);
    pub const FLOAT_COMPONENTS: BitmapInfo = BitmapInfo(c::kCGBitmapFloatComponents);
    pub const BYTE_ORDER_MASK: BitmapInfo = BitmapInfo(c::kCGBitmapByteOrderMask);
    pub const BYTE_ORDER_DEFAULT: BitmapInfo = BitmapInfo(c::kCGBitmapByteOrderDefault);
    pub const BYTE_ORDER_16_LITTLE: BitmapInfo = BitmapInfo(c::kCGBitmapByteOrder16Little);
    pub const BYTE_ORDER_32_LITTLE: BitmapInfo = BitmapInfo(c::kCGBitmapByteOrder32Little);
    pub const BYTE_ORDER_16_BIG: BitmapInfo = BitmapInfo(c::kCGBitmapByteOrder16Big);
    pub const BYTE_ORDER_32_BIG: BitmapInfo = BitmapInfo(c::kCGBitmapByteOrder32Big);
}
