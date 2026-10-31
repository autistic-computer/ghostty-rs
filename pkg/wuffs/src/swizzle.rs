//! Port of `src/wuffs/src/swizzle.zig`.
//!
//! STATUS: ported
//!
//! Deviation: instead of preparing a `wuffs_base__pixel_swizzler`, the
//! conversions are implemented in Rust with the same per-pixel semantics
//! (and, for [`rgba_src_over`], the same 16-bit integer arithmetic) as the
//! Wuffs swizzlers they replace. Wuffs pixel format names describe the byte
//! order in memory, e.g. `BGR` is the bytes B, G, R.

use super::error::Error;

const LOG: &str = "wuffs_swizzler";

/// The subset of Wuffs pixel formats used by the swizzles in this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PixelFormat {
    /// `WUFFS_BASE__PIXEL_FORMAT__Y`
    Y,
    /// `WUFFS_BASE__PIXEL_FORMAT__YA_NONPREMUL`
    YaNonpremul,
    /// `WUFFS_BASE__PIXEL_FORMAT__RGB`
    Rgb,
    /// `WUFFS_BASE__PIXEL_FORMAT__BGR`
    Bgr,
    /// `WUFFS_BASE__PIXEL_FORMAT__BGRA_PREMUL`
    BgraPremul,
    /// `WUFFS_BASE__PIXEL_FORMAT__RGBA_PREMUL`
    RgbaPremul,
    /// `WUFFS_BASE__PIXEL_FORMAT__RGBA_NONPREMUL`
    RgbaNonpremul,
}

impl PixelFormat {
    fn bits_per_pixel(self) -> usize {
        match self {
            PixelFormat::Y => 8,
            PixelFormat::YaNonpremul => 16,
            PixelFormat::Rgb | PixelFormat::Bgr => 24,
            PixelFormat::BgraPremul | PixelFormat::RgbaPremul | PixelFormat::RgbaNonpremul => 32,
        }
    }

    /// Whether the color channels are premultiplied by alpha. Formats
    /// without an alpha channel are opaque, so this doesn't matter for them.
    fn premul(self) -> bool {
        matches!(self, PixelFormat::BgraPremul | PixelFormat::RgbaPremul)
    }

    fn has_alpha(self) -> bool {
        matches!(
            self,
            PixelFormat::YaNonpremul
                | PixelFormat::BgraPremul
                | PixelFormat::RgbaPremul
                | PixelFormat::RgbaNonpremul
        )
    }

    /// Read a pixel as `[r, g, b, a]`.
    fn read(self, px: &[u8]) -> [u8; 4] {
        match self {
            PixelFormat::Y => [px[0], px[0], px[0], 0xFF],
            PixelFormat::YaNonpremul => [px[0], px[0], px[0], px[1]],
            PixelFormat::Rgb => [px[0], px[1], px[2], 0xFF],
            PixelFormat::Bgr => [px[2], px[1], px[0], 0xFF],
            PixelFormat::BgraPremul => [px[2], px[1], px[0], px[3]],
            PixelFormat::RgbaPremul | PixelFormat::RgbaNonpremul => [px[0], px[1], px[2], px[3]],
        }
    }

    /// Write a `[r, g, b, a]` pixel.
    fn write(self, px: &mut [u8], v: [u8; 4]) {
        let [r, g, b, a] = v;
        match self {
            // Gray conversion isn't needed by any swizzle here.
            PixelFormat::Y => px[0] = r,
            PixelFormat::YaNonpremul => {
                px[0] = r;
                px[1] = a;
            }
            PixelFormat::Rgb => px.copy_from_slice(&[r, g, b]),
            PixelFormat::Bgr => px.copy_from_slice(&[b, g, r]),
            PixelFormat::BgraPremul => px.copy_from_slice(&[b, g, r, a]),
            PixelFormat::RgbaPremul | PixelFormat::RgbaNonpremul => {
                px.copy_from_slice(&[r, g, b, a])
            }
        }
    }
}

pub fn g_to_rgba(src: &[u8]) -> Result<Vec<u8>, Error> {
    swizzle(src, PixelFormat::Y, PixelFormat::RgbaPremul)
}

pub fn ga_to_rgba(src: &[u8]) -> Result<Vec<u8>, Error> {
    // Wuffs doesn't support YA_PREMUL as a swizzle source. The nonpremul
    // pair produces the same bytes (r=g=b=y, a=a, no alpha math), which
    // is what we want: alpha semantics are preserved as-is, matching the
    // other conversions here.
    swizzle(src, PixelFormat::YaNonpremul, PixelFormat::RgbaNonpremul)
}

pub fn rgb_to_rgba(src: &[u8]) -> Result<Vec<u8>, Error> {
    swizzle(src, PixelFormat::Rgb, PixelFormat::RgbaPremul)
}

pub fn bgr_to_rgba(src: &[u8]) -> Result<Vec<u8>, Error> {
    swizzle(src, PixelFormat::Bgr, PixelFormat::RgbaPremul)
}

pub fn bgra_to_rgba(src: &[u8]) -> Result<Vec<u8>, Error> {
    swizzle(src, PixelFormat::BgraPremul, PixelFormat::RgbaPremul)
}

/// Composite `src` over `dst` in place. Both are straight
/// (non-premultiplied) alpha RGBA of the same length. A transparent
/// destination pixel takes the source pixel exactly; wuffs composites
/// everything else in 16-bit integer space.
pub fn rgba_src_over(dst: &mut [u8], src: &[u8]) {
    assert!(dst.len() == src.len());
    assert!(dst.len() % 4 == 0);

    // This is wuffs' `swizzle_rgba_nonpremul__rgba_nonpremul__src_over`.
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        // Extract 16-bit color components.
        let mut dr = 0x101 * u32::from(d[0]);
        let mut dg = 0x101 * u32::from(d[1]);
        let mut db = 0x101 * u32::from(d[2]);
        let mut da = 0x101 * u32::from(d[3]);
        let sr = 0x101 * u32::from(s[0]);
        let sg = 0x101 * u32::from(s[1]);
        let sb = 0x101 * u32::from(s[2]);
        let sa = 0x101 * u32::from(s[3]);

        // Convert dst from nonpremul to premul.
        dr = (dr * da) / 0xFFFF;
        dg = (dg * da) / 0xFFFF;
        db = (db * da) / 0xFFFF;

        // Calculate the inverse of the src-alpha: how much of the dst to keep.
        let ia = 0xFFFF - sa;

        // Composite src (nonpremul) over dst (premul).
        da = sa + ((da * ia) / 0xFFFF);
        dr = ((sr * sa) + (dr * ia)) / 0xFFFF;
        dg = ((sg * sa) + (dg * ia)) / 0xFFFF;
        db = ((sb * sa) + (db * ia)) / 0xFFFF;

        // Convert dst from premul to nonpremul.
        if da != 0 {
            dr = (dr * 0xFFFF) / da;
            dg = (dg * 0xFFFF) / da;
            db = (db * 0xFFFF) / da;
        }

        // Convert from 16-bit color to 8-bit color.
        d[0] = (dr >> 8) as u8;
        d[1] = (dg >> 8) as u8;
        d[2] = (db >> 8) as u8;
        d[3] = (da >> 8) as u8;
    }
}

/// Convert between alpha representations using Wuffs' 16-bit arithmetic.
fn convert_alpha(v: [u8; 4], src_premul: bool, dst_premul: bool) -> [u8; 4] {
    if src_premul == dst_premul || v[3] == 0xFF {
        return v;
    }
    let a = 0x101 * u32::from(v[3]);
    let conv = |c: u8| -> u8 {
        let c = 0x101 * u32::from(c);
        let out = if dst_premul {
            (c * a) / 0xFFFF
        } else if a == 0 {
            0
        } else {
            ((c * 0xFFFF) / a).min(0xFFFF)
        };
        (out >> 8) as u8
    };
    [conv(v[0]), conv(v[1]), conv(v[2]), v[3]]
}

fn swizzle(src: &[u8], src_fmt: PixelFormat, dst_fmt: PixelFormat) -> Result<Vec<u8>, Error> {
    debug_assert!(dst_fmt.bits_per_pixel() % 8 == 0);
    let dst_size = dst_fmt.bits_per_pixel() / 8;

    debug_assert!(src_fmt.bits_per_pixel() % 8 == 0);
    let src_size = src_fmt.bits_per_pixel() / 8;

    assert!(src.len() % src_size == 0);

    // Wuffs can't swizzle an alpha source into a gray destination.
    if dst_fmt == PixelFormat::Y && src_fmt != PixelFormat::Y {
        log::warn!(target: LOG, "#base: unsupported pixel swizzler option");
        return Err(Error::WuffsError);
    }

    let dst_len = src.len() / src_size * dst_size;
    let mut dst = Vec::new();
    dst.try_reserve_exact(dst_len)
        .map_err(|_| Error::OutOfMemory)?;
    dst.resize(dst_len, 0);

    let alpha_math = src_fmt.has_alpha() && dst_fmt.has_alpha();
    for (s, d) in src
        .chunks_exact(src_size)
        .zip(dst.chunks_exact_mut(dst_size))
    {
        let mut v = src_fmt.read(s);
        if alpha_math {
            v = convert_alpha(v, src_fmt.premul(), dst_fmt.premul());
        }
        dst_fmt.write(d, v);
    }

    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ga_to_rgba_test() {
        let rgba = ga_to_rgba(&[7, 100, 8, 200]).unwrap();

        assert_eq!(&[7u8, 7, 7, 100, 8, 8, 8, 200][..], &rgba[..]);
    }

    #[test]
    fn rgba_src_over_test() {
        // 50% white over opaque black, opaque over anything, transparent
        // source over anything, and anything over a transparent
        // destination (exact source passthrough).
        #[rustfmt::skip]
        let mut dst = [
            0,  0,  0,  255,
            10, 20, 30, 40,
            10, 20, 30, 40,
            0,  0,  0,  0,
        ];
        #[rustfmt::skip]
        rgba_src_over(&mut dst, &[
            255, 255, 255, 128,
            100, 110, 120, 255,
            100, 110, 120, 0,
            200, 100, 50,  128,
        ]);
        #[rustfmt::skip]
        assert_eq!(
            [
                128, 128, 128, 255,
                100, 110, 120, 255,
                10,  20,  30,  40,
                200, 100, 50,  128,
            ],
            dst
        );
    }

    #[test]
    fn other_swizzles() {
        assert_eq!(g_to_rgba(&[9]).unwrap(), vec![9, 9, 9, 255]);
        assert_eq!(rgb_to_rgba(&[1, 2, 3]).unwrap(), vec![1, 2, 3, 255]);
        assert_eq!(bgr_to_rgba(&[1, 2, 3]).unwrap(), vec![3, 2, 1, 255]);
        assert_eq!(bgra_to_rgba(&[1, 2, 3, 4]).unwrap(), vec![3, 2, 1, 4]);
    }
}
