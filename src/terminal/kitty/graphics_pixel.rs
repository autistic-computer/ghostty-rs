//! Port of `src/terminal/kitty/graphics_pixel.zig`.
//!
//! STATUS: ported
//!
//! Pixel-buffer operations for the Kitty graphics protocol: format
//! conversion to RGBA, background fill, and rectangle composition.
//! These are the primitives behind animation frame loading (a=f) and
//! frame composition (a=c). See graphics_animation.rs for the
//! animation model built on top of them.
//!
use super::graphics_command::{Background, CompositionMode, Format};
use crate::pkg::wuffs::swizzle;

/// Convert pixel data in the given format to a freshly allocated RGBA
/// buffer. The input is not modified.
///
/// The input length must be a multiple of the format's bytes per
/// pixel; image loading validates data length against the image
/// dimensions before storing it.
pub fn rgba_from_format(format: Format, data: &[u8]) -> Vec<u8> {
    let result = match format {
        Format::Rgba => return data.to_vec(),
        Format::Rgb => swizzle::rgb_to_rgba(data),
        Format::Gray => swizzle::g_to_rgba(data),
        Format::GrayAlpha => swizzle::ga_to_rgba(data),

        // PNG is decoded to RGBA during image loading.
        Format::Png => unreachable!(),
    };

    match result {
        Ok(v) => v,
        // Allocation failure aborts in Rust (Zig returns OutOfMemory).
        Err(crate::pkg::wuffs::Error::OutOfMemory) => {
            std::alloc::handle_alloc_error(std::alloc::Layout::new::<u8>())
        }
        // These are fixed, supported swizzles; wuffs cannot fail to
        // run them.
        Err(_) => unreachable!(),
    }
}

/// Fill an RGBA buffer with the given background color.
pub fn fill_background(data: &mut [u8], bg: Background) {
    let raw: u32 = bg.to_bits();
    if raw == 0 {
        data.fill(0);
        return;
    }
    let mut i: usize = 0;
    while i + 4 <= data.len() {
        data[i] = bg.r;
        data[i + 1] = bg.g;
        data[i + 2] = bg.b;
        data[i + 3] = bg.a;
        i += 4;
    }
}

/// Compose the `src` rectangle (src_width x src_height RGBA pixels)
/// onto the `dst` canvas (dst_width x dst_height RGBA pixels) with
/// its top-left corner at (x, y). Portions of the rectangle outside
/// the canvas are silently clipped, matching Kitty's a=f behavior.
#[allow(clippy::too_many_arguments)]
pub fn compose_rect(
    dst: &mut [u8],
    dst_width: u32,
    dst_height: u32,
    src: &[u8],
    src_width: u32,
    src_height: u32,
    x: u32,
    y: u32,
    mode: CompositionMode,
) {
    if x >= dst_width || y >= dst_height {
        return;
    }
    let width = src_width.min(dst_width - x) as usize;
    let height = src_height.min(dst_height - y) as usize;
    for row in 0..height {
        let dst_off = ((y as usize + row) * dst_width as usize + x as usize) * 4;
        let src_off = row * src_width as usize * 4;
        compose_row(
            &mut dst[dst_off..][..width * 4],
            &src[src_off..][..width * 4],
            mode,
        );
    }
}

/// Compose a width x height rectangle between two full-size RGBA
/// canvases sharing the same `canvas_width` stride, reading from
/// (src_x, src_y) in `src` and writing at (dst_x, dst_y) in `dst`.
/// The caller must have validated that both rectangles are within
/// bounds; a=c reports out-of-bounds rectangles as errors rather
/// than clipping.
///
/// Zig allows `dst` and `src` to be the same buffer; in Rust that case
/// is [`compose_canvas_rect_in_place`].
#[allow(clippy::too_many_arguments)]
pub fn compose_canvas_rect(
    dst: &mut [u8],
    src: &[u8],
    canvas_width: u32,
    width: u32,
    height: u32,
    src_x: u32,
    src_y: u32,
    dst_x: u32,
    dst_y: u32,
    mode: CompositionMode,
) {
    for row in 0..height as usize {
        let dst_off = ((dst_y as usize + row) * canvas_width as usize + dst_x as usize) * 4;
        let src_off = ((src_y as usize + row) * canvas_width as usize + src_x as usize) * 4;
        compose_row(
            &mut dst[dst_off..][..width as usize * 4],
            &src[src_off..][..width as usize * 4],
            mode,
        );
    }
}

/// [`compose_canvas_rect`] where the source and destination are the same
/// canvas (a frame composed onto itself). Rows are processed in the same
/// order as the Zig code; each row's source pixels are copied before
/// writing so overlapping rectangles behave like `@memcpy`/wuffs on
/// non-overlapping rows.
#[allow(clippy::too_many_arguments)]
pub fn compose_canvas_rect_in_place(
    canvas: &mut [u8],
    canvas_width: u32,
    width: u32,
    height: u32,
    src_x: u32,
    src_y: u32,
    dst_x: u32,
    dst_y: u32,
    mode: CompositionMode,
) {
    let row_len = width as usize * 4;
    let mut tmp = vec![0u8; row_len];
    for row in 0..height as usize {
        let dst_off = ((dst_y as usize + row) * canvas_width as usize + dst_x as usize) * 4;
        let src_off = ((src_y as usize + row) * canvas_width as usize + src_x as usize) * 4;
        tmp.copy_from_slice(&canvas[src_off..][..row_len]);
        compose_row(&mut canvas[dst_off..][..row_len], &tmp, mode);
    }
}

fn compose_row(dst: &mut [u8], src: &[u8], mode: CompositionMode) {
    match mode {
        CompositionMode::Overwrite => dst.copy_from_slice(src),
        CompositionMode::AlphaBlend => swizzle::rgba_src_over(dst, src),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_conversion() {
        {
            let rgb = [1u8, 2, 3, 4, 5, 6];
            let result = rgba_from_format(Format::Rgb, &rgb);
            assert_eq!(&[1u8, 2, 3, 255, 4, 5, 6, 255][..], &result[..]);
        }

        {
            let gray = [7u8, 8];
            let result = rgba_from_format(Format::Gray, &gray);
            assert_eq!(&[7u8, 7, 7, 255, 8, 8, 8, 255][..], &result[..]);
        }

        {
            let ga = [7u8, 100, 8, 200];
            let result = rgba_from_format(Format::GrayAlpha, &ga);
            assert_eq!(&[7u8, 7, 7, 100, 8, 8, 8, 200][..], &result[..]);
        }

        {
            let rgba = [1u8, 2, 3, 4];
            let result = rgba_from_format(Format::Rgba, &rgba);
            assert_eq!(&rgba[..], &result[..]);
        }
    }

    #[test]
    fn fill_background_test() {
        let mut buf = [0xAAu8; 8];
        fill_background(
            &mut buf,
            Background {
                r: 1,
                g: 2,
                b: 3,
                a: 4,
            },
        );
        assert_eq!([1u8, 2, 3, 4, 1, 2, 3, 4], buf);

        fill_background(&mut buf, Background::default());
        assert_eq!([0u8; 8], buf);
    }

    #[test]
    fn compose_rect_overwrite_with_clipping() {
        // 2x2 canvas, compose a 2x1 rect at (1, 1): only the first pixel
        // of the rect fits, the rest clips off the right edge.
        let mut dst = [0u8; 16];
        let src = [1u8, 2, 3, 4, 5, 6, 7, 8];
        compose_rect(&mut dst, 2, 2, &src, 2, 1, 1, 1, CompositionMode::Overwrite);

        let mut expect = [0u8; 16];
        expect[12] = 1;
        expect[13] = 2;
        expect[14] = 3;
        expect[15] = 4;
        assert_eq!(expect, dst);
    }

    #[test]
    fn compose_rect_entirely_out_of_bounds() {
        let mut dst = [9u8; 16];
        let src = [1u8; 4];
        compose_rect(&mut dst, 2, 2, &src, 1, 1, 2, 0, CompositionMode::Overwrite);
        compose_rect(&mut dst, 2, 2, &src, 1, 1, 0, 2, CompositionMode::Overwrite);
        assert_eq!([9u8; 16], dst);
    }

    #[test]
    fn alpha_blend_source_over_semantics() {
        // Opaque source overwrites exactly.
        {
            let mut dst = [10u8, 20, 30, 40];
            compose_rect(
                &mut dst,
                1,
                1,
                &[100, 110, 120, 255],
                1,
                1,
                0,
                0,
                CompositionMode::AlphaBlend,
            );
            assert_eq!([100u8, 110, 120, 255], dst);
        }

        // Fully transparent source leaves the destination untouched
        // (like Kitty; see the module doc on rounding).
        {
            let mut dst = [10u8, 20, 30, 40];
            compose_rect(
                &mut dst,
                1,
                1,
                &[100, 110, 120, 0],
                1,
                1,
                0,
                0,
                CompositionMode::AlphaBlend,
            );
            assert_eq!([10u8, 20, 30, 40], dst);
        }

        // 50% source over opaque destination.
        {
            let mut dst = [0u8, 0, 0, 255];
            compose_rect(
                &mut dst,
                1,
                1,
                &[255, 255, 255, 128],
                1,
                1,
                0,
                0,
                CompositionMode::AlphaBlend,
            );
            assert_eq!(255u8, dst[3]);
            assert_eq!(128u8, dst[0]);
        }

        // Blending over a transparent destination yields the source
        // exactly, matching Kitty: wuffs passes the source through
        // untouched when the destination is transparent.
        {
            let mut dst = [0u8, 0, 0, 0];
            compose_rect(
                &mut dst,
                1,
                1,
                &[200, 100, 50, 128],
                1,
                1,
                0,
                0,
                CompositionMode::AlphaBlend,
            );
            assert_eq!([200u8, 100, 50, 128], dst);
        }
    }

    #[test]
    fn compose_canvas_rect_test() {
        // 3x1 canvas: copy pixel at x=0 onto x=2.
        let mut canvas = [1u8, 2, 3, 4, 0, 0, 0, 0, 9, 9, 9, 9];
        compose_canvas_rect_in_place(&mut canvas, 3, 1, 1, 0, 0, 2, 0, CompositionMode::Overwrite);
        assert_eq!([1u8, 2, 3, 4, 0, 0, 0, 0, 1, 2, 3, 4], canvas);

        // Separate buffers.
        let src = [1u8, 2, 3, 4, 0, 0, 0, 0, 9, 9, 9, 9];
        let mut dst = [0u8; 12];
        compose_canvas_rect(
            &mut dst,
            &src,
            3,
            1,
            1,
            0,
            0,
            2,
            0,
            CompositionMode::Overwrite,
        );
        assert_eq!([0u8, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4], dst);
    }
}
