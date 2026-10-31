//! Port of `src/font/sprite/Face.zig`.
//!
//! STATUS: ported
//!
//! This implements the built-in "sprite face". This font renders
//! the built-in glyphs for the terminal, such as box drawing fonts, as well
//! as specific sprites that are part of our rendering model such as
//! text decorations (underlines).
//!
//! This isn't really a "font face" so much as it is quacks like a font
//! face with regards to how it works with font.Group. We don't use any
//! dynamic dispatch so it isn't truly an interface but the functions
//! and behaviors are close enough to a system face that it makes it easy
//! to integrate with font.Group. This is desirable so that higher level
//! processes such as GroupCache, Shaper, etc. don't need to be aware of
//! special sprite handling and just treat it like a normal font face.
//!
//! Port note (tests): the reference PNGs in `src/font/sprite/testdata/`
//! were regenerated from this Rust implementation (set
//! `GHOSTTY_UPDATE_SPRITE_GOLDENS=1` to regenerate them), because the
//! canvas rasterizes with tiny-skia instead of z2d and anti-aliased edge
//! pixels differ slightly (by at most 64/255; rectangle-only ranges are
//! pixel-identical). The original z2d goldens are kept in
//! `src/font/sprite/testdata/z2d/` and checked with a tolerance.

use super::canvas::{Canvas, DrawError};
use super::draw::{
    block, braille, branch, geometric_shapes, powerline, r#box, special,
    symbols_for_legacy_computing as sflc, symbols_for_legacy_computing_supplement as sflcs,
};
use super::Sprite;
use crate::font::atlas::{self, Atlas};
use crate::font::glyph::{Glyph, RenderOptions};
use crate::font::metrics::Metrics;
use crate::font::Presentation;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Face {
    /// Grid metrics for rendering sprites.
    pub metrics: Metrics,
}

/// Errors that a [`DrawFn`] can return (Zig `Allocator.Error ||
/// z2d.Path.Error || z2d.painter.FillError || z2d.painter.StrokeError ||
/// error{MathError}`).
pub type DrawFnError = DrawError;

/// A function that draws a glyph on the provided canvas.
pub type DrawFn = fn(
    cp: u32,
    canvas: &mut Canvas,
    width: u32,
    height: u32,
    metrics: Metrics,
) -> Result<(), DrawFnError>;

#[derive(Clone, Copy)]
struct Range {
    min: u32,
    max: u32,
    draw: DrawFn,
}

/// Ranges for functions with names in the format `draw<CP>` or
/// `draw<MIN>_<MAX>`, sorted in ascending order.
///
/// (Zig collects these at comptime from the draw modules' declarations;
/// here the table is written out and its ordering is checked by a test.)
#[rustfmt::skip]
const RANGES: &[Range] = &[
    Range { min: 0x2500, max: 0x257F, draw: r#box::draw2500_257f },
    Range { min: 0x2580, max: 0x259F, draw: block::draw2580_259f },
    Range { min: 0x25E2, max: 0x25E5, draw: geometric_shapes::draw25e2_25e5 },
    Range { min: 0x25F8, max: 0x25FA, draw: geometric_shapes::draw25f8_25fa },
    Range { min: 0x25FF, max: 0x25FF, draw: geometric_shapes::draw25ff },
    Range { min: 0x2800, max: 0x28FF, draw: braille::draw2800_28ff },
    Range { min: 0xE0B0, max: 0xE0B0, draw: powerline::drawe0b0 },
    Range { min: 0xE0B1, max: 0xE0B1, draw: powerline::drawe0b1 },
    Range { min: 0xE0B2, max: 0xE0B2, draw: powerline::drawe0b2 },
    Range { min: 0xE0B3, max: 0xE0B3, draw: powerline::drawe0b3 },
    Range { min: 0xE0B4, max: 0xE0B4, draw: powerline::drawe0b4 },
    Range { min: 0xE0B5, max: 0xE0B5, draw: powerline::drawe0b5 },
    Range { min: 0xE0B6, max: 0xE0B6, draw: powerline::drawe0b6 },
    Range { min: 0xE0B7, max: 0xE0B7, draw: powerline::drawe0b7 },
    Range { min: 0xE0B8, max: 0xE0B8, draw: powerline::drawe0b8 },
    Range { min: 0xE0B9, max: 0xE0B9, draw: powerline::drawe0b9 },
    Range { min: 0xE0BA, max: 0xE0BA, draw: powerline::drawe0ba },
    Range { min: 0xE0BB, max: 0xE0BB, draw: powerline::drawe0bb },
    Range { min: 0xE0BC, max: 0xE0BC, draw: powerline::drawe0bc },
    Range { min: 0xE0BD, max: 0xE0BD, draw: powerline::drawe0bd },
    Range { min: 0xE0BE, max: 0xE0BE, draw: powerline::drawe0be },
    Range { min: 0xE0BF, max: 0xE0BF, draw: powerline::drawe0bf },
    Range { min: 0xE0D2, max: 0xE0D2, draw: powerline::drawe0d2 },
    Range { min: 0xE0D4, max: 0xE0D4, draw: powerline::drawe0d4 },
    Range { min: 0xF5D0, max: 0xF60D, draw: branch::drawf5d0_f60d },
    Range { min: 0x1CC1B, max: 0x1CC1E, draw: sflcs::draw1cc1b_1cc1e },
    Range { min: 0x1CC21, max: 0x1CC2F, draw: sflcs::draw1cc21_1cc2f },
    Range { min: 0x1CC30, max: 0x1CC3F, draw: sflcs::draw1cc30_1cc3f },
    Range { min: 0x1CD00, max: 0x1CDE5, draw: sflcs::draw1cd00_1cde5 },
    Range { min: 0x1CE00, max: 0x1CE00, draw: sflcs::draw1ce00 },
    Range { min: 0x1CE01, max: 0x1CE01, draw: sflcs::draw1ce01 },
    Range { min: 0x1CE0B, max: 0x1CE0B, draw: sflcs::draw1ce0b },
    Range { min: 0x1CE0C, max: 0x1CE0C, draw: sflcs::draw1ce0c },
    Range { min: 0x1CE16, max: 0x1CE19, draw: sflcs::draw1ce16_1ce19 },
    Range { min: 0x1CE51, max: 0x1CE8F, draw: sflcs::draw1ce51_1ce8f },
    Range { min: 0x1CE90, max: 0x1CEAF, draw: sflcs::draw1ce90_1ceaf },
    Range { min: 0x1FB00, max: 0x1FB3B, draw: sflc::draw1fb00_1fb3b },
    Range { min: 0x1FB3C, max: 0x1FB67, draw: sflc::draw1fb3c_1fb67 },
    Range { min: 0x1FB68, max: 0x1FB6F, draw: sflc::draw1fb68_1fb6f },
    Range { min: 0x1FB70, max: 0x1FB75, draw: sflc::draw1fb70_1fb75 },
    Range { min: 0x1FB76, max: 0x1FB7B, draw: sflc::draw1fb76_1fb7b },
    Range { min: 0x1FB7C, max: 0x1FB97, draw: sflc::draw1fb7c_1fb97 },
    Range { min: 0x1FB98, max: 0x1FB98, draw: sflc::draw1fb98 },
    Range { min: 0x1FB99, max: 0x1FB99, draw: sflc::draw1fb99 },
    Range { min: 0x1FB9A, max: 0x1FB9F, draw: sflc::draw1fb9a_1fb9f },
    Range { min: 0x1FBA0, max: 0x1FBAE, draw: sflc::draw1fba0_1fbae },
    Range { min: 0x1FBAF, max: 0x1FBAF, draw: sflc::draw1fbaf },
    Range { min: 0x1FBBD, max: 0x1FBBD, draw: sflc::draw1fbbd },
    Range { min: 0x1FBBE, max: 0x1FBBE, draw: sflc::draw1fbbe },
    Range { min: 0x1FBBF, max: 0x1FBBF, draw: sflc::draw1fbbf },
    Range { min: 0x1FBCE, max: 0x1FBCE, draw: sflc::draw1fbce },
    Range { min: 0x1FBCF, max: 0x1FBCF, draw: sflc::draw1fbcf },
    Range { min: 0x1FBD0, max: 0x1FBDF, draw: sflc::draw1fbd0_1fbdf },
    Range { min: 0x1FBE0, max: 0x1FBEF, draw: sflc::draw1fbe0_1fbef },
];

fn get_draw_fn(cp: u32) -> Option<DrawFn> {
    // For special sprites (cursors, underlines, etc.) all sprites are drawn
    // by functions from `Special` that share the name of the enum field.
    if cp >= Sprite::START {
        return Some(match Sprite::from_cp(cp)? {
            Sprite::Underline => special::underline,
            Sprite::UnderlineDouble => special::underline_double,
            Sprite::UnderlineDotted => special::underline_dotted,
            Sprite::UnderlineDashed => special::underline_dashed,
            Sprite::UnderlineCurly => special::underline_curly,
            Sprite::Strikethrough => special::strikethrough,
            Sprite::Overline => special::overline,
            Sprite::CursorRect => special::cursor_rect,
            Sprite::CursorHollowRect => special::cursor_hollow_rect,
            Sprite::CursorBar => special::cursor_bar,
            Sprite::CursorUnderline => special::cursor_underline,
        });
    }

    for range in RANGES {
        if cp >= range.min && cp <= range.max {
            return Some(range.draw);
        }
    }
    None
}

/// Errors from [`Face::render_glyph`] (the Zig inferred error set:
/// atlas errors plus [`DrawFnError`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderGlyphError {
    Atlas(atlas::Error),
    Draw(DrawFnError),
}

impl From<atlas::Error> for RenderGlyphError {
    fn from(e: atlas::Error) -> Self {
        RenderGlyphError::Atlas(e)
    }
}

impl From<DrawFnError> for RenderGlyphError {
    fn from(e: DrawFnError) -> Self {
        RenderGlyphError::Draw(e)
    }
}

impl std::fmt::Display for RenderGlyphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderGlyphError::Atlas(e) => std::fmt::Display::fmt(e, f),
            RenderGlyphError::Draw(e) => std::fmt::Display::fmt(e, f),
        }
    }
}

impl std::error::Error for RenderGlyphError {}

impl Face {
    /// Returns true if the codepoint exists in our sprite font.
    pub fn has_codepoint(&self, cp: u32, p: Option<Presentation>) -> bool {
        // We ignore presentation. No matter what presentation is
        // requested we always provide glyphs for our codepoints.
        let _ = p;
        get_draw_fn(cp).is_some()
    }

    /// Render the glyph.
    pub fn render_glyph(
        &self,
        atlas: &mut Atlas,
        cp: u32,
        opts: RenderOptions,
    ) -> Result<Glyph, RenderGlyphError> {
        if cfg!(debug_assertions) && !self.has_codepoint(cp, None) {
            log::error!(target: "font_sprite", "invalid codepoint cp={:x}", cp);
            unreachable!(); // crash
        }

        // It should be impossible for this to be null and we assert that
        // in runtime safety modes but in case it is its not worth memory
        // corruption so we return a valid, blank glyph.
        let Some(draw) = get_draw_fn(cp) else {
            return Ok(Glyph {
                width: 0,
                height: 0,
                offset_x: 0,
                offset_y: 0,
                atlas_x: 0,
                atlas_y: 0,
            });
        };

        let metrics = self.metrics;

        // We adjust our sprite width based on the cell width.
        let width = match opts.cell_width.unwrap_or(1) {
            0 | 1 => metrics.cell_width,
            width => metrics.cell_width * width as u32,
        };

        // Sprite glyphs generally get the full cell height, but cursor glyphs need
        // to be affected by `adjust-cursor-height`, so we use `cursor_height` for
        // the height if it's one of the full-height cursors.
        let height = if cp == Sprite::CursorRect.cp()
            || cp == Sprite::CursorHollowRect.cp()
            || cp == Sprite::CursorBar.cp()
        {
            metrics.cursor_height
        } else {
            metrics.cell_height
        };

        let padding_x = width / 4;
        let padding_y = height / 4;

        // Make a canvas of the desired size
        let mut canvas = Canvas::init(width, height, padding_x, padding_y);

        draw(cp, &mut canvas, width, height, metrics)?;

        // Write the drawing to the atlas
        let region = canvas.write_atlas(atlas)?;

        // The X offset is the displacement from the left edge of the cell that our
        // drawn sprite will be drawn in the grid. That's the same as the distance
        // we have calculated from the left of the canvas, minus the padding, since
        // the padding represents extra pixels to the left of the cell.
        let offset_x: i32 = canvas.clip_left as i32 - padding_x as i32;
        // Similar logic applies for the Y offset, but with the additional factor
        // that we want to re-center cursor glyphs in the cell if they were drawn
        // taller or shorter than a normal cell is; this only applies to cursors
        // currently, but could conceivably apply to other things in the future.
        let offset_y: i32 = region.height.saturating_add(canvas.clip_bottom) as i32
            - padding_y as i32
            // By adding half the difference between the cell height and the
            // height we passed to the draw function, we center it in the cell.
            + (metrics.cell_height as i32 - height as i32) / 2;

        Ok(Glyph {
            width: region.width,
            height: region.height,
            offset_x,
            offset_y,
            atlas_x: region.x,
            atlas_y: region.y,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::metrics::FaceMetrics;
    use crate::font::sprite::canvas::{Operator, Surface};
    use std::path::{Path, PathBuf};

    #[test]
    fn ranges_sorted_and_not_overlapping() {
        // Ensure there's no overlapping ranges (Zig comptime check).
        let mut i: u32 = 0;
        for (k, n) in RANGES.iter().enumerate() {
            assert!(n.min <= n.max);
            assert!(
                k == 0 || n.min > i,
                "Codepoint range {:X} overlaps the previous range ending at {:X}",
                n.min,
                i
            );
            i = n.max;
        }
    }

    #[test]
    fn has_codepoint() {
        let face = Face {
            metrics: Metrics::calc(FaceMetrics {
                px_per_em: 16.0,
                cell_width: 8.0,
                ascent: 12.0,
                descent: -4.0,
                line_gap: 0.0,
                ..Default::default()
            }),
        };
        assert!(face.has_codepoint(0x2500, None));
        assert!(face.has_codepoint(0x1FBEF, Some(Presentation::Emoji)));
        assert!(face.has_codepoint(Sprite::Underline.cp(), None));
        assert!(face.has_codepoint(Sprite::CursorUnderline.cp(), None));
        assert!(!face.has_codepoint(Sprite::CursorUnderline.cp() + 1, None));
        assert!(!face.has_codepoint('A' as u32, None));
        assert!(!face.has_codepoint(0x1FBB0, None));
    }

    fn testdata_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/font/sprite/testdata")
    }

    /// Encode an alpha8 surface as an 8-bit grayscale PNG (like z2d's
    /// PNG exporter does for alpha8 surfaces).
    fn encode_png(sfc: &Surface) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc =
                png::Encoder::new(&mut out, sfc.get_width() as u32, sfc.get_height() as u32);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&sfc.buf).unwrap();
        }
        out
    }

    /// Decode an 8-bit grayscale PNG to (width, height, pixels).
    fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(png::ColorType::Grayscale, info.color_type);
        assert_eq!(png::BitDepth::Eight, info.bit_depth);
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    /// How a rendered atlas is compared with its reference PNG.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Check {
        /// Pixel-exact comparison with the goldens in `testdata/`, which
        /// were generated by this (tiny-skia based) implementation.
        Exact,
        /// Comparison with the original Zig/z2d goldens in `testdata/z2d/`,
        /// tolerating the small anti-aliasing differences between z2d's
        /// 4x multisampling and tiny-skia's rasterizer.
        Z2d,
    }

    /// Used in `test_draw_ranges`, checks for diff between the provided atlas
    /// and the reference file for the range, returns true if there is a diff.
    ///
    /// Unlike the Zig version, this compares decoded pixels rather than the
    /// PNG bytes first (the PNG encoders differ). If the environment
    /// variable `GHOSTTY_UPDATE_SPRITE_GOLDENS` is set, the `Exact`
    /// reference file is (re)written instead.
    fn test_diff_atlas(
        atlas: &Surface,
        i: u32,
        width: u32,
        height: u32,
        thickness: u32,
        check: Check,
    ) -> bool {
        let name = format!("U+{:X}...U+{:X}-{}x{}+{}.png", i, i + 0xFF, width, height, thickness);
        let ref_path = match check {
            Check::Exact => testdata_dir().join(&name),
            Check::Z2d => testdata_dir().join("z2d").join(&name),
        };

        if check == Check::Exact && std::env::var_os("GHOSTTY_UPDATE_SPRITE_GOLDENS").is_some() {
            std::fs::write(&ref_path, encode_png(atlas)).unwrap();
            return false;
        }

        let out_dir = std::env::temp_dir().join("ghostty_sprite_face_test");
        let _ = std::fs::create_dir_all(&out_dir);
        let test_path = out_dir.join(format!("sprite_face_test-{name}"));

        let ref_bytes = match std::fs::read(&ref_path) {
            Ok(b) => b,
            Err(err) => {
                let _ = std::fs::write(&test_path, encode_png(atlas));
                eprintln!(
                    "Can't open reference file {}: {} (test={})",
                    ref_path.display(),
                    err,
                    test_path.display()
                );
                return true;
            }
        };

        let (ref_w, ref_h, ref_gray) = decode_png(&ref_bytes);
        assert_eq!(ref_w as i32, atlas.get_width());
        assert_eq!(ref_h as i32, atlas.get_height());

        // We'll make a visual representation of the diff using
        // red for removed pixels and green for added.
        let test_gray = &atlas.buf;
        let mut diff_pix = vec![0u8; test_gray.len() * 3];

        let mut differs = false;
        let mut max_diff: u8 = 0;
        let mut big_diffs: usize = 0;
        let (mut ref_sum, mut test_sum) = (0u64, 0u64);
        for j in 0..test_gray.len() {
            let t = test_gray[j];
            let r = ref_gray[j];
            ref_sum += r as u64;
            test_sum += t as u64;
            if t == r {
                // If the pixels match, write it as a faded gray.
                diff_pix[j * 3] = t / 3;
                diff_pix[j * 3 + 1] = t / 3;
                diff_pix[j * 3 + 2] = t / 3;
            } else {
                differs = true;
                let d = t.abs_diff(r);
                max_diff = max_diff.max(d);
                if d > 32 {
                    big_diffs += 1;
                }
                // Otherwise put the reference value in the red
                // channel and the new value in the green channel.
                diff_pix[j * 3] = r;
                diff_pix[j * 3 + 1] = t;
            }
        }

        let fail = match check {
            Check::Exact => differs,
            // Anti-aliasing differences only: no pixel may be off by more
            // than a quarter of the range, only a few may be off by more
            // than 1/8, and the total coverage must agree within 1%.
            Check::Z2d => {
                max_diff > 64
                    || big_diffs > 32
                    || (ref_sum as f64 - test_sum as f64).abs() > ref_sum as f64 * 0.01
            }
        };
        if !fail {
            return false;
        }

        // Drop the test image and a diff image as PNGs.
        let _ = std::fs::write(&test_path, encode_png(atlas));
        let diff_path = out_dir.join(format!("sprite_face_diff-{name}"));
        {
            let mut out = Vec::new();
            {
                let mut enc =
                    png::Encoder::new(&mut out, atlas.get_width() as u32, atlas.get_height() as u32);
                enc.set_color(png::ColorType::Rgb);
                enc.set_depth(png::BitDepth::Eight);
                let mut writer = enc.write_header().unwrap();
                writer.write_image_data(&diff_pix).unwrap();
            }
            let _ = std::fs::write(&diff_path, out);
        }
        eprintln!(
            "One or more glyphs differ from reference file in range U+{:X}...U+{:X}! \
             test={}, reference={}, diff={} (max diff {}, {} pixels off by more than 32)",
            i,
            i + 0xFF,
            test_path.display(),
            ref_path.display(),
            diff_path.display(),
            max_diff,
            big_diffs,
        );

        true
    }

    /// Draws all ranges in to a set of 16x16 glyph atlases, checks for regressions
    /// against reference files, logs errors and exposes a diff for any difference
    /// between the reference and test atlas.
    ///
    /// Returns true if there was a diff.
    fn test_draw_ranges(width: u32, ascent: u32, descent: u32, thickness: u32, check: Check) -> bool {
        let metrics = Metrics::calc(FaceMetrics {
            // Fudged number, not used in anything we care about here.
            px_per_em: 16.0,

            cell_width: width as f64,
            ascent: ascent as f64,
            descent: -(descent as f64),
            line_gap: 0.0,
            underline_thickness: Some(thickness as f64),
            strikethrough_thickness: Some(thickness as f64),
            ..Default::default()
        });

        let height = ascent + descent;

        let padding_x = width / 4;
        let padding_y = height / 4;

        // Canvas to draw glyphs on, we'll reuse this for all glyphs.
        let mut canvas = Canvas::init(width, height, padding_x, padding_y);

        // We render glyphs in batches of 256, which we copy (including padding) to
        // a 16 by 16 surface to be compared with the reference file for that range.
        let stride_x = width + 2 * padding_x;
        let stride_y = height + 2 * padding_y;
        let mut atlas = Surface::init((stride_x * 16) as i32, (stride_y * 16) as i32);

        let mut i: u32 = RANGES[0].min & !0xFF;

        // We set this to true if we have any fails so we can
        // return an error after we're done comparing all glyphs.
        let mut fail = false;

        for range in RANGES {
            for cp in range.min..=range.max {
                // If we've moved to a new batch of 256, check the
                // current one and clear the surface for the next one.
                if cp - i >= 0x100 {
                    if test_diff_atlas(&atlas, i, width, height, thickness, check) {
                        fail = true;
                    }

                    i = cp & !0xFF;
                    atlas.buf.fill(0);
                }

                (get_draw_fn(cp).unwrap())(cp, &mut canvas, width, height, metrics).unwrap();
                canvas.clear_clipping_regions();
                atlas.composite(
                    &canvas.sfc,
                    Operator::Src,
                    (stride_x * ((cp - i) % 16)) as i32,
                    (stride_y * ((cp - i) / 16)) as i32,
                );
                canvas.sfc.buf.fill(0);
                canvas.clip_top = 0;
                canvas.clip_left = 0;
                canvas.clip_right = 0;
                canvas.clip_bottom = 0;
            }
        }

        if test_diff_atlas(&atlas, i, width, height, thickness, check) {
            fail = true;
        }

        fail
    }

    fn render_all_sprites(check: Check) {
        let mut diff = false;

        // test_draw_ranges(width, ascent, descent, thickness):
        //
        // We compare 4 different sets of metrics;
        // - even cell size / even thickness
        // - even cell size / odd thickness
        // - odd cell size / even thickness
        // - odd cell size / odd thickness
        // (Also a decreasing range of sizes.)
        if test_draw_ranges(18, 30, 6, 4, check) {
            diff = true;
        }
        if test_draw_ranges(12, 20, 4, 3, check) {
            diff = true;
        }
        if test_draw_ranges(11, 19, 2, 2, check) {
            diff = true;
        }
        if test_draw_ranges(9, 15, 2, 1, check) {
            diff = true;
        }

        assert!(!diff); // There should be no diffs from reference.
    }

    #[test]
    fn sprite_face_render_all_sprites() {
        // Renders all sprites to an atlas and compares
        // it to a ground truth for regression testing.
        render_all_sprites(Check::Exact);
    }

    /// Deviation from the Zig test suite: the goldens in `testdata/` are
    /// rendered by this port (tiny-skia instead of z2d), so this also
    /// checks the renders against the original z2d goldens, allowing for
    /// anti-aliasing differences.
    #[test]
    fn sprite_face_render_all_sprites_close_to_z2d() {
        render_all_sprites(Check::Z2d);
    }

    #[test]
    fn full_height_cursor_sprites_respect_cursor_height_metric() {
        let mut atlas = Atlas::init(128, atlas::Format::Grayscale).unwrap();

        // face with a 8x16 cell dimension.
        let mut face = Face {
            metrics: Metrics::calc(FaceMetrics {
                // Fudged number, not used in anything we care about here.
                px_per_em: 16.0,

                cell_width: 8.0,
                ascent: 12.0,
                descent: -4.0,
                line_gap: 0.0,
                ..Default::default()
            }),
        };

        assert_eq!(16, face.metrics.cell_height);

        let check = |face: &Face, atlas: &mut Atlas, sprite: Sprite, height: u32, offset_y: i32| {
            let glyph = face
                .render_glyph(atlas, sprite.cp(), RenderOptions::new(face.metrics))
                .unwrap();
            assert_eq!(height, glyph.height, "{sprite:?}");
            assert_eq!(offset_y, glyph.offset_y, "{sprite:?}");
        };

        // --- smaller than cell height ---
        face.metrics.cursor_height = 12;
        // bar
        check(&face, &mut atlas, Sprite::CursorBar, 12, 14);
        // rect
        check(&face, &mut atlas, Sprite::CursorRect, 12, 14);
        // hollow rect
        check(&face, &mut atlas, Sprite::CursorHollowRect, 12, 14);

        // --- equal to the cell height ---
        face.metrics.cursor_height = 16;
        // bar
        check(&face, &mut atlas, Sprite::CursorBar, 16, 16);
        // rect
        check(&face, &mut atlas, Sprite::CursorRect, 16, 16);
        // hollow rect
        check(&face, &mut atlas, Sprite::CursorHollowRect, 16, 16);

        // --- greater than the cell height ---
        face.metrics.cursor_height = 20;
        // bar
        check(&face, &mut atlas, Sprite::CursorBar, 20, 18);
        // rect
        check(&face, &mut atlas, Sprite::CursorRect, 20, 18);
        // hollow rect
        check(&face, &mut atlas, Sprite::CursorHollowRect, 20, 18);
    }

    // test "sprite face print all sprites" {
    //     std.debug.print("\n\n", .{});
    //     inline for (ranges) |range| {
    //         for (range.min..range.max + 1) |cp| {
    //             std.debug.print("{u}", .{ @as(u21, @intCast(cp)) });
    //         }
    //     }
    //     std.debug.print("\n\n", .{});
    // }
}
