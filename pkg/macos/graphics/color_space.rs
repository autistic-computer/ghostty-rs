//! Port of `src/macos/graphics/color_space.zig`.
//!
//! STATUS: ported

use super::c;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CGColorSpaceRef`
    ColorSpace
);

impl ColorSpace {
    pub fn create_device_gray() -> Result<&'static ColorSpace, Error> {
        // SAFETY: plain constructor.
        unsafe { created(c::CGColorSpaceCreateDeviceGray()) }
    }

    pub fn create_device_rgb() -> Result<&'static ColorSpace, Error> {
        // SAFETY: plain constructor.
        unsafe { created(c::CGColorSpaceCreateDeviceRGB()) }
    }

    pub fn create_named(name: Name) -> Result<&'static ColorSpace, Error> {
        // SAFETY: name is a framework color space name.
        unsafe { created(c::CGColorSpaceCreateWithName(name.cfstring())) }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CGColorSpaceRelease(self.as_ptr()) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Name {
    /// This color space uses the DCI P3 primaries, a D65 white point, and
    /// the sRGB transfer function.
    DisplayP3,
    /// The Display P3 color space with a linear transfer function and
    /// extended-range values.
    ExtendedLinearDisplayP3,
    /// The sRGB colorimetry and non-linear transfer function are specified
    /// in IEC 61966-2-1.
    SRGB,
    /// This color space has the same colorimetry as `sRGB`, but uses a
    /// linear transfer function.
    LinearSRGB,
    /// This color space has the same colorimetry as `sRGB`, but you can
    /// encode component values below `0.0` and above `1.0`. Negative values
    /// are encoded as the signed reflection of the original encoding
    /// function, as shown in the formula below:
    /// ```text
    /// extendedTransferFunction(x) = sign(x) * sRGBTransferFunction(abs(x))
    /// ```
    ExtendedSRGB,
    /// This color space has the same colorimetry as `sRGB`; in addition,
    /// you may encode component values below `0.0` and above `1.0`.
    ExtendedLinearSRGB,
    /// ...
    GenericGrayGamma2_2,
    /// ...
    LinearGray,
    /// This color space has the same colorimetry as `genericGrayGamma2_2`,
    /// but you can encode component values below `0.0` and above `1.0`.
    /// Negative values are encoded as the signed reflection of the
    /// original encoding function, as shown in the formula below:
    /// ```text
    /// extendedGrayTransferFunction(x) = sign(x) * gamma22Function(abs(x))
    /// ```
    ExtendedGray,
    /// This color space has the same colorimetry as `linearGray`; in
    /// addition, you may encode component values below `0.0` and above `1.0`.
    ExtendedLinearGray,
}

impl Name {
    fn cfstring(self) -> c::CFStringRef {
        // SAFETY: reading framework constants.
        unsafe {
            match self {
                Name::DisplayP3 => c::kCGColorSpaceDisplayP3,
                Name::ExtendedLinearDisplayP3 => c::kCGColorSpaceExtendedLinearDisplayP3,
                Name::SRGB => c::kCGColorSpaceSRGB,
                Name::ExtendedSRGB => c::kCGColorSpaceExtendedSRGB,
                Name::LinearSRGB => c::kCGColorSpaceLinearSRGB,
                Name::ExtendedLinearSRGB => c::kCGColorSpaceExtendedLinearSRGB,
                Name::GenericGrayGamma2_2 => c::kCGColorSpaceGenericGrayGamma2_2,
                Name::ExtendedGray => c::kCGColorSpaceExtendedGray,
                Name::LinearGray => c::kCGColorSpaceLinearGray,
                Name::ExtendedLinearGray => c::kCGColorSpaceExtendedLinearGray,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_gray() {
        let space = ColorSpace::create_device_gray().unwrap();
        space.release();
    }

    #[test]
    fn device_rgb() {
        let space = ColorSpace::create_device_rgb().unwrap();
        space.release();
    }

    #[test]
    fn named() {
        let space = ColorSpace::create_named(Name::DisplayP3).unwrap();
        space.release();
    }
}
