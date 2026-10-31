//! Port of `src/macos/video/pixel_format.zig`.
//!
//! STATUS: ported
//!
//! `PixelFormat` is a non-exhaustive `enum(c_int)` in Zig; here it is a
//! newtype with one associated constant per `kCVPixelFormatType_*` value.
//! The constant names keep the CoreVideo spelling (with a leading `_` when
//! it starts with a digit).

#![allow(non_upper_case_globals)]

/// A CoreVideo pixel format type (`OSType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelFormat(pub i32);

const fn fourcc(v: &[u8; 4]) -> i32 {
    (((v[0] as u32) << 24) | ((v[1] as u32) << 16) | ((v[2] as u32) << 8) | (v[3] as u32)) as i32
}

impl PixelFormat {
    /// 1 bit indexed
    pub const _1Monochrome: PixelFormat = PixelFormat(0x00000001);
    /// 2 bit indexed
    pub const _2Indexed: PixelFormat = PixelFormat(0x00000002);
    /// 4 bit indexed
    pub const _4Indexed: PixelFormat = PixelFormat(0x00000004);
    /// 8 bit indexed
    pub const _8Indexed: PixelFormat = PixelFormat(0x00000008);
    /// 1 bit indexed gray, white is zero
    pub const _1IndexedGray_WhiteIsZero: PixelFormat = PixelFormat(0x00000021);
    /// 2 bit indexed gray, white is zero
    pub const _2IndexedGray_WhiteIsZero: PixelFormat = PixelFormat(0x00000022);
    /// 4 bit indexed gray, white is zero
    pub const _4IndexedGray_WhiteIsZero: PixelFormat = PixelFormat(0x00000024);
    /// 8 bit indexed gray, white is zero
    pub const _8IndexedGray_WhiteIsZero: PixelFormat = PixelFormat(0x00000028);
    /// 16 bit BE RGB 555
    pub const _16BE555: PixelFormat = PixelFormat(0x00000010);
    /// 16 bit LE RGB 555
    pub const _16LE555: PixelFormat = PixelFormat(fourcc(b"L555"));
    /// 16 bit LE RGB 5551
    pub const _16LE5551: PixelFormat = PixelFormat(fourcc(b"5551"));
    /// 16 bit BE RGB 565
    pub const _16BE565: PixelFormat = PixelFormat(fourcc(b"B565"));
    /// 16 bit LE RGB 565
    pub const _16LE565: PixelFormat = PixelFormat(fourcc(b"L565"));
    /// 24 bit RGB
    pub const _24RGB: PixelFormat = PixelFormat(0x00000018);
    /// 24 bit BGR
    pub const _24BGR: PixelFormat = PixelFormat(fourcc(b"24BG"));
    /// 32 bit ARGB
    pub const _32ARGB: PixelFormat = PixelFormat(0x00000020);
    /// 32 bit BGRA
    pub const _32BGRA: PixelFormat = PixelFormat(fourcc(b"BGRA"));
    /// 32 bit ABGR
    pub const _32ABGR: PixelFormat = PixelFormat(fourcc(b"ABGR"));
    /// 32 bit RGBA
    pub const _32RGBA: PixelFormat = PixelFormat(fourcc(b"RGBA"));
    /// 64 bit ARGB, 16-bit big-endian samples
    pub const _64ARGB: PixelFormat = PixelFormat(fourcc(b"b64a"));
    /// 64 bit RGBA, 16-bit little-endian full-range (0-65535) samples
    pub const _64RGBALE: PixelFormat = PixelFormat(fourcc(b"l64r"));
    /// 48 bit RGB, 16-bit big-endian samples
    pub const _48RGB: PixelFormat = PixelFormat(fourcc(b"b48r"));
    /// 32 bit AlphaGray, 16-bit big-endian samples, black is zero
    pub const _32AlphaGray: PixelFormat = PixelFormat(fourcc(b"b32a"));
    /// 16 bit Grayscale, 16-bit big-endian samples, black is zero
    pub const _16Gray: PixelFormat = PixelFormat(fourcc(b"b16g"));
    /// 30 bit RGB, 10-bit big-endian samples, 2 unused padding bits (at least significant end).
    pub const _30RGB: PixelFormat = PixelFormat(fourcc(b"R10k"));
    /// 30 bit RGB, 10-bit big-endian samples, 2 unused padding bits (at most significant end), video-range (64-940).
    pub const _30RGB_r210: PixelFormat = PixelFormat(fourcc(b"r210"));
    /// Component Y'CbCr 8-bit 4:2:2, ordered Cb Y'0 Cr Y'1
    pub const _422YpCbCr8: PixelFormat = PixelFormat(fourcc(b"2vuy"));
    /// Component Y'CbCrA 8-bit 4:4:4:4, ordered Cb Y' Cr A
    pub const _4444YpCbCrA8: PixelFormat = PixelFormat(fourcc(b"v408"));
    /// Component Y'CbCrA 8-bit 4:4:4:4, rendering format. full range alpha, zero biased YUV, ordered A Y' Cb Cr
    pub const _4444YpCbCrA8R: PixelFormat = PixelFormat(fourcc(b"r408"));
    /// Component Y'CbCrA 8-bit 4:4:4:4, ordered A Y' Cb Cr, full range alpha, video range Y'CbCr.
    pub const _4444AYpCbCr8: PixelFormat = PixelFormat(fourcc(b"y408"));
    /// Component Y'CbCrA 16-bit 4:4:4:4, ordered A Y' Cb Cr, full range alpha, video range Y'CbCr, 16-bit little-endian samples.
    pub const _4444AYpCbCr16: PixelFormat = PixelFormat(fourcc(b"y416"));
    /// Component AY'CbCr single precision floating-point 4:4:4:4
    pub const _4444AYpCbCrFloat: PixelFormat = PixelFormat(fourcc(b"r4fl"));
    /// Component Y'CbCr 8-bit 4:4:4, ordered Cr Y' Cb, video range Y'CbCr
    pub const _444YpCbCr8: PixelFormat = PixelFormat(fourcc(b"v308"));
    /// Component Y'CbCr 10,12,14,16-bit 4:2:2
    pub const _422YpCbCr16: PixelFormat = PixelFormat(fourcc(b"v216"));
    /// Component Y'CbCr 10-bit 4:2:2
    pub const _422YpCbCr10: PixelFormat = PixelFormat(fourcc(b"v210"));
    /// Component Y'CbCr 10-bit 4:4:4
    pub const _444YpCbCr10: PixelFormat = PixelFormat(fourcc(b"v410"));
    /// Planar Component Y'CbCr 8-bit 4:2:0.  baseAddr points to a big-endian CVPlanarPixelBufferInfo_YCbCrPlanar struct
    pub const _420YpCbCr8Planar: PixelFormat = PixelFormat(fourcc(b"y420"));
    /// Planar Component Y'CbCr 8-bit 4:2:0, full range.  baseAddr points to a big-endian CVPlanarPixelBufferInfo_YCbCrPlanar struct
    pub const _420YpCbCr8PlanarFullRange: PixelFormat = PixelFormat(fourcc(b"f420"));
    /// First plane: Video-range Component Y'CbCr 8-bit 4:2:2, ordered Cb Y'0 Cr Y'1; second plane: alpha 8-bit 0-255
    pub const _422YpCbCr_4A_8BiPlanar: PixelFormat = PixelFormat(fourcc(b"a2vy"));
    /// Bi-Planar Component Y'CbCr 8-bit 4:2:0, video-range (luma=[16,235] chroma=[16,240]).  baseAddr points to a big-endian CVPlanarPixelBufferInfo_YCbCrBiPlanar struct
    pub const _420YpCbCr8BiPlanarVideoRange: PixelFormat = PixelFormat(fourcc(b"420v"));
    /// Bi-Planar Component Y'CbCr 8-bit 4:2:0, full-range (luma=[0,255] chroma=[1,255]).  baseAddr points to a big-endian CVPlanarPixelBufferInfo_YCbCrBiPlanar struct
    pub const _420YpCbCr8BiPlanarFullRange: PixelFormat = PixelFormat(fourcc(b"420f"));
    /// Bi-Planar Component Y'CbCr 8-bit 4:2:2, video-range (luma=[16,235] chroma=[16,240]).  baseAddr points to a big-endian CVPlanarPixelBufferInfo_YCbCrBiPlanar struct
    pub const _422YpCbCr8BiPlanarVideoRange: PixelFormat = PixelFormat(fourcc(b"422v"));
    /// Bi-Planar Component Y'CbCr 8-bit 4:2:2, full-range (luma=[0,255] chroma=[1,255]).  baseAddr points to a big-endian CVPlanarPixelBufferInfo_YCbCrBiPlanar struct
    pub const _422YpCbCr8BiPlanarFullRange: PixelFormat = PixelFormat(fourcc(b"422f"));
    /// Bi-Planar Component Y'CbCr 8-bit 4:4:4, video-range (luma=[16,235] chroma=[16,240]).  baseAddr points to a big-endian CVPlanarPixelBufferInfo_YCbCrBiPlanar struct
    pub const _444YpCbCr8BiPlanarVideoRange: PixelFormat = PixelFormat(fourcc(b"444v"));
    /// Bi-Planar Component Y'CbCr 8-bit 4:4:4, full-range (luma=[0,255] chroma=[1,255]).  baseAddr points to a big-endian CVPlanarPixelBufferInfo_YCbCrBiPlanar struct
    pub const _444YpCbCr8BiPlanarFullRange: PixelFormat = PixelFormat(fourcc(b"444f"));
    /// Component Y'CbCr 8-bit 4:2:2, ordered Y'0 Cb Y'1 Cr
    pub const _422YpCbCr8_yuvs: PixelFormat = PixelFormat(fourcc(b"yuvs"));
    /// Component Y'CbCr 8-bit 4:2:2, full range, ordered Y'0 Cb Y'1 Cr
    pub const _422YpCbCr8FullRange: PixelFormat = PixelFormat(fourcc(b"yuvf"));
    /// 8 bit one component, black is zero
    pub const OneComponent8: PixelFormat = PixelFormat(fourcc(b"L008"));
    /// 8 bit two component, black is zero
    pub const TwoComponent8: PixelFormat = PixelFormat(fourcc(b"2C08"));
    /// little-endian RGB101010, 2 MSB are ignored, wide-gamut (384-895)
    pub const _30RGBLEPackedWideGamut: PixelFormat = PixelFormat(fourcc(b"w30r"));
    /// little-endian ARGB2101010 full-range ARGB
    pub const ARGB2101010LEPacked: PixelFormat = PixelFormat(fourcc(b"l10r"));
    /// little-endian ARGB10101010, each 10 bits in the MSBs of 16bits, wide-gamut (384-895, including alpha)
    pub const _40ARGBLEWideGamut: PixelFormat = PixelFormat(fourcc(b"w40a"));
    /// little-endian ARGB10101010, each 10 bits in the MSBs of 16bits, wide-gamut (384-895, including alpha). Alpha premultiplied
    pub const _40ARGBLEWideGamutPremultiplied: PixelFormat = PixelFormat(fourcc(b"w40m"));
    /// 10 bit little-endian one component, stored as 10 MSBs of 16 bits, black is zero
    pub const OneComponent10: PixelFormat = PixelFormat(fourcc(b"L010"));
    /// 12 bit little-endian one component, stored as 12 MSBs of 16 bits, black is zero
    pub const OneComponent12: PixelFormat = PixelFormat(fourcc(b"L012"));
    /// 16 bit little-endian one component, black is zero
    pub const OneComponent16: PixelFormat = PixelFormat(fourcc(b"L016"));
    /// 16 bit little-endian two component, black is zero
    pub const TwoComponent16: PixelFormat = PixelFormat(fourcc(b"2C16"));
    /// 16 bit one component IEEE half-precision float, 16-bit little-endian samples
    pub const OneComponent16Half: PixelFormat = PixelFormat(fourcc(b"L00h"));
    /// 32 bit one component IEEE float, 32-bit little-endian samples
    pub const OneComponent32Float: PixelFormat = PixelFormat(fourcc(b"L00f"));
    /// 16 bit two component IEEE half-precision float, 16-bit little-endian samples
    pub const TwoComponent16Half: PixelFormat = PixelFormat(fourcc(b"2C0h"));
    /// 32 bit two component IEEE float, 32-bit little-endian samples
    pub const TwoComponent32Float: PixelFormat = PixelFormat(fourcc(b"2C0f"));
    /// 64 bit RGBA IEEE half-precision float, 16-bit little-endian samples
    pub const _64RGBAHalf: PixelFormat = PixelFormat(fourcc(b"RGhA"));
    /// 128 bit RGBA IEEE float, 32-bit little-endian samples
    pub const _128RGBAFloat: PixelFormat = PixelFormat(fourcc(b"RGfA"));
    /// Bayer 14-bit Little-Endian, packed in 16-bits, ordered G R G R... alternating with B G B G...
    pub const _14Bayer_GRBG: PixelFormat = PixelFormat(fourcc(b"grb4"));
    /// Bayer 14-bit Little-Endian, packed in 16-bits, ordered R G R G... alternating with G B G B...
    pub const _14Bayer_RGGB: PixelFormat = PixelFormat(fourcc(b"rgg4"));
    /// Bayer 14-bit Little-Endian, packed in 16-bits, ordered B G B G... alternating with G R G R...
    pub const _14Bayer_BGGR: PixelFormat = PixelFormat(fourcc(b"bgg4"));
    /// Bayer 14-bit Little-Endian, packed in 16-bits, ordered G B G B... alternating with R G R G...
    pub const _14Bayer_GBRG: PixelFormat = PixelFormat(fourcc(b"gbr4"));
    /// IEEE754-2008 binary16 (half float), describing the normalized shift when comparing two images. Units are 1/meters: ( pixelShift / (pixelFocalLength * baselineInMeters) )
    pub const DisparityFloat16: PixelFormat = PixelFormat(fourcc(b"hdis"));
    /// IEEE754-2008 binary32 float, describing the normalized shift when comparing two images. Units are 1/meters: ( pixelShift / (pixelFocalLength * baselineInMeters) )
    pub const DisparityFloat32: PixelFormat = PixelFormat(fourcc(b"fdis"));
    /// IEEE754-2008 binary16 (half float), describing the depth (distance to an object) in meters
    pub const DepthFloat16: PixelFormat = PixelFormat(fourcc(b"hdep"));
    /// IEEE754-2008 binary32 float, describing the depth (distance to an object) in meters
    pub const DepthFloat32: PixelFormat = PixelFormat(fourcc(b"fdep"));
    /// 2 plane YCbCr10 4:2:0, each 10 bits in the MSBs of 16bits, video-range (luma=[64,940] chroma=[64,960])
    pub const _420YpCbCr10BiPlanarVideoRange: PixelFormat = PixelFormat(fourcc(b"x420"));
    /// 2 plane YCbCr10 4:2:2, each 10 bits in the MSBs of 16bits, video-range (luma=[64,940] chroma=[64,960])
    pub const _422YpCbCr10BiPlanarVideoRange: PixelFormat = PixelFormat(fourcc(b"x422"));
    /// 2 plane YCbCr10 4:4:4, each 10 bits in the MSBs of 16bits, video-range (luma=[64,940] chroma=[64,960])
    pub const _444YpCbCr10BiPlanarVideoRange: PixelFormat = PixelFormat(fourcc(b"x444"));
    /// 2 plane YCbCr10 4:2:0, each 10 bits in the MSBs of 16bits, full-range (Y range 0-1023)
    pub const _420YpCbCr10BiPlanarFullRange: PixelFormat = PixelFormat(fourcc(b"xf20"));
    /// 2 plane YCbCr10 4:2:2, each 10 bits in the MSBs of 16bits, full-range (Y range 0-1023)
    pub const _422YpCbCr10BiPlanarFullRange: PixelFormat = PixelFormat(fourcc(b"xf22"));
    /// 2 plane YCbCr10 4:4:4, each 10 bits in the MSBs of 16bits, full-range (Y range 0-1023)
    pub const _444YpCbCr10BiPlanarFullRange: PixelFormat = PixelFormat(fourcc(b"xf44"));
    /// first and second planes as per 420YpCbCr8BiPlanarVideoRange (420v), alpha 8 bits in third plane full-range.  No CVPlanarPixelBufferInfo struct.
    pub const _420YpCbCr8VideoRange_8A_TriPlanar: PixelFormat = PixelFormat(fourcc(b"v0a8"));
    /// Single plane Bayer 16-bit little-endian sensor element ("sensel".*) samples from full-size decoding of ProRes RAW images; Bayer pattern (sensel ordering) and other raw conversion information is described via buffer attachments
    pub const _16VersatileBayer: PixelFormat = PixelFormat(fourcc(b"bp16"));
    /// Single plane 64-bit RGBA (16-bit little-endian samples) from downscaled decoding of ProRes RAW images; components--which may not be co-sited with one another--are sensel values and require raw conversion, information for which is described via buffer attachments
    pub const _64RGBA_DownscaledProResRAW: PixelFormat = PixelFormat(fourcc(b"bp64"));
    /// 2 plane YCbCr16 4:2:2, video-range (luma=[4096,60160] chroma=[4096,61440])
    pub const _422YpCbCr16BiPlanarVideoRange: PixelFormat = PixelFormat(fourcc(b"sv22"));
    /// 2 plane YCbCr16 4:4:4, video-range (luma=[4096,60160] chroma=[4096,61440])
    pub const _444YpCbCr16BiPlanarVideoRange: PixelFormat = PixelFormat(fourcc(b"sv44"));
    /// 3 plane video-range YCbCr16 4:4:4 with 16-bit full-range alpha (luma=[4096,60160] chroma=[4096,61440] alpha=[0,65535]).  No CVPlanarPixelBufferInfo struct.
    pub const _444YpCbCr16VideoRange_16A_TriPlanar: PixelFormat = PixelFormat(fourcc(b"s4as"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_values() {
        assert_eq!(PixelFormat::_32BGRA.0, 0x42475241);
        assert_eq!(PixelFormat::_32ARGB.0, 0x20);
        assert_eq!(PixelFormat::_420YpCbCr8BiPlanarVideoRange.0, 0x34323076);
    }
}
