//! Port of `src/terminal/c/color.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: the X11 entry table is built once on first use
//! (Zig: at comptime) and lives for the rest of the process, so the pointer
//! returned by `x11_names` is stable.

use std::ffi::c_char;
use std::sync::LazyLock;

use super::result::Result;
use crate::terminal::color::{self, Rgb, RgbC};
use crate::terminal::x11_color;

/// C: GhosttyColorPaletteMask
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaletteMask {
    pub bits: [u64; 4],
}

impl PaletteMask {
    /// Convert to the core PaletteMask (`std.bit_set.Static(256)`).
    pub fn to_zig(&self) -> color::PaletteMask {
        let mut result = color::PaletteMask::EMPTY;
        for i in 0..256 {
            if ((self.bits[i >> 6] >> (i & 63)) & 1) != 0 {
                result.set(i);
            }
        }
        result
    }
}

/// C: GhosttyColorX11Entry
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct X11Entry {
    /// Null-terminated color name; null marks the end of the table.
    pub name: *const c_char,
    pub color: RgbC,
}

/// The static table, terminated by a null-name entry.
struct X11Entries(Vec<X11Entry>);

// SAFETY: the names point at the immutable, process-lifetime strings of
// `x11_color::ENTRIES`.
unsafe impl Send for X11Entries {}
// SAFETY: see above.
unsafe impl Sync for X11Entries {}

static X11_ENTRIES: LazyLock<X11Entries> = LazyLock::new(|| {
    let entries = x11_color::entries();
    let mut result: Vec<X11Entry> = entries
        .iter()
        .map(|entry| X11Entry {
            name: entry.name.as_ptr(),
            color: entry.color.cval(),
        })
        .collect();
    result.push(X11Entry {
        name: std::ptr::null(),
        color: RgbC { r: 0, g: 0, b: 0 },
    });
    X11Entries(result)
});

/// C: ghostty_color_rgb_get
#[export_name = "ghostty_color_rgb_get"]
pub extern "C" fn rgb_get(c: &RgbC, r: &mut u8, g: &mut u8, b: &mut u8) {
    *r = c.r;
    *g = c.g;
    *b = c.b;
}

/// Trim spaces and tabs from both ends (Zig: `std.mem.trim(u8, s, " \t")`).
fn trim_space_tab(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&c| c != b' ' && c != b'\t').unwrap_or(s.len());
    let end = s.iter().rposition(|&c| c != b' ' && c != b'\t').map_or(start, |i| i + 1);
    &s[start..end]
}

/// C: ghostty_color_parse_x11
///
/// # Safety
/// A non-null `name_` must be valid for `len` reads.
#[export_name = "ghostty_color_parse_x11"]
pub unsafe extern "C" fn parse_x11(name_: *const u8, len: usize, out: &mut RgbC) -> Result {
    if name_.is_null() {
        return Result::InvalidValue;
    }
    let name = std::slice::from_raw_parts(name_, len);
    let trimmed = trim_space_tab(name);
    let Some(rgb) = x11_color::MAP.get(trimmed) else {
        return Result::InvalidValue;
    };
    *out = rgb.cval();
    Result::Success
}

/// C: ghostty_color_parse
///
/// # Safety
/// A non-null `value_` must be valid for `len` reads.
#[export_name = "ghostty_color_parse"]
pub unsafe extern "C" fn parse(value_: *const u8, len: usize, out: &mut RgbC) -> Result {
    if value_.is_null() {
        return Result::InvalidValue;
    }
    let value = std::slice::from_raw_parts(value_, len);
    let Ok(rgb) = Rgb::parse(value) else {
        return Result::InvalidValue;
    };
    *out = rgb.cval();
    Result::Success
}

/// C: ghostty_color_palette_default
#[export_name = "ghostty_color_palette_default"]
pub extern "C" fn palette_default(out: &mut color::PaletteC) {
    *out = color::palette_cval(&color::DEFAULT);
}

/// Generate a 256-color palette. The output may alias the base input.
///
/// # Safety
/// `base_`/`skip_` must be NULL or valid; `out` must be valid for writes
/// (it may alias `base_`).
#[export_name = "ghostty_color_palette_generate"]
pub unsafe extern "C" fn palette_generate(
    base_: *const color::PaletteC,
    skip_: *const PaletteMask,
    bg: &RgbC,
    fg: &RgbC,
    harmonious: bool,
    out: *mut color::PaletteC,
) {
    let base: color::Palette = match base_.as_ref() {
        Some(base) => color::palette_zval(base),
        None => color::DEFAULT,
    };
    let skip: color::PaletteMask = match skip_.as_ref() {
        Some(skip) => skip.to_zig(),
        None => color::PaletteMask::EMPTY,
    };
    let result = color::generate_256_color(base, skip, Rgb::from_c(*bg), Rgb::from_c(*fg), harmonious);
    *out = color::palette_cval(&result);
}

/// C: ghostty_color_x11_names
#[export_name = "ghostty_color_x11_names"]
pub extern "C" fn x11_names() -> *const X11Entry {
    X11_ENTRIES.0.as_ptr()
}

/// C: ghostty_color_x11_name_count
#[export_name = "ghostty_color_x11_name_count"]
pub extern "C" fn x11_name_count() -> usize {
    x11_color::entries().len()
}

/// C: ghostty_color_parse_palette_entry
///
/// # Safety
/// A non-null `value_` must be valid for `len` reads.
#[export_name = "ghostty_color_parse_palette_entry"]
pub unsafe extern "C" fn parse_palette_entry(
    value_: *const u8,
    len: usize,
    out_index: &mut u8,
    out_rgb: &mut RgbC,
) -> Result {
    if value_.is_null() {
        return Result::InvalidValue;
    }
    let value = std::slice::from_raw_parts(value_, len);
    let Ok(entry) = color::parse_palette_entry(value) else {
        return Result::InvalidValue;
    };
    *out_index = entry.index;
    *out_rgb = entry.color.cval();
    Result::Success
}

/// C: ghostty_color_luminance
#[export_name = "ghostty_color_luminance"]
pub extern "C" fn luminance(c: &RgbC) -> f64 {
    Rgb::from_c(*c).luminance()
}

/// C: ghostty_color_perceived_luminance
#[export_name = "ghostty_color_perceived_luminance"]
pub extern "C" fn perceived_luminance(c: &RgbC) -> f64 {
    Rgb::from_c(*c).perceived_luminance()
}

/// C: ghostty_color_contrast
#[export_name = "ghostty_color_contrast"]
pub extern "C" fn contrast(a: &RgbC, b: &RgbC) -> f64 {
    Rgb::from_c(*a).contrast(Rgb::from_c(*b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_rgb(expected: Rgb, actual: RgbC) {
        assert_eq!(expected, Rgb::from_c(actual));
    }

    fn expect_palette_c(expected: &color::PaletteC, actual: &color::PaletteC) {
        for (e, a) in expected.iter().zip(actual.iter()) {
            expect_rgb(Rgb::from_c(*e), *a);
        }
    }

    fn generate_palette(
        base_: Option<&color::PaletteC>,
        skip_: Option<&PaletteMask>,
        bg: Rgb,
        fg: Rgb,
        harmonious: bool,
    ) -> color::PaletteC {
        let mut out: color::PaletteC = [RgbC::default(); 256];
        let bg_c = bg.cval();
        let fg_c = fg.cval();
        unsafe {
            palette_generate(
                base_.map_or(std::ptr::null(), |b| b as *const _),
                skip_.map_or(std::ptr::null(), |s| s as *const _),
                &bg_c,
                &fg_c,
                harmonious,
                &mut out,
            )
        };
        out
    }

    fn set_mask_bit(mask: &mut PaletteMask, idx: usize) {
        mask.bits[idx >> 6] |= 1u64 << (idx & 63);
    }

    const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    #[test]
    fn color_parse_x11_valid() {
        let cases: [(&[u8], Rgb); 6] = [
            (b"white", rgb(255, 255, 255)),
            (b"medium spring green", rgb(0, 250, 154)),
            (b"ForestGreen", rgb(34, 139, 34)),
            (b"FoReStGReen", rgb(34, 139, 34)),
            (b" Forest Green ", rgb(34, 139, 34)),
            (b"\tblack\t", rgb(0, 0, 0)),
        ];

        for (input, expected) in cases {
            let mut out = RgbC::default();
            assert_eq!(Result::Success, unsafe { parse_x11(input.as_ptr(), input.len(), &mut out) });
            expect_rgb(expected, out);
        }
    }

    #[test]
    fn color_parse_x11_invalid() {
        let cases: [&[u8]; 4] = [b"nosuchcolor", b"", b"   ", b"#ffffff"];

        for case in cases {
            let mut out = RgbC::default();
            assert_eq!(Result::InvalidValue, unsafe { parse_x11(case.as_ptr(), case.len(), &mut out) });
        }

        let mut out = RgbC::default();
        assert_eq!(Result::InvalidValue, unsafe { parse_x11(std::ptr::null(), 0, &mut out) });
    }

    #[test]
    fn color_parse_valid() {
        let cases: [(&[u8], Rgb); 8] = [
            (b"black", rgb(0, 0, 0)),
            (b"#AABBCC", rgb(170, 187, 204)),
            (b"0A0B0C", rgb(10, 11, 12)),
            (b"FFF", rgb(255, 255, 255)),
            (b"#345", rgb(51, 68, 85)),
            (b"rgb:1/2/3", rgb(17, 34, 51)),
            (b"  black ", rgb(0, 0, 0)),
            (b" #AABBCC   ", rgb(170, 187, 204)),
        ];

        for (input, expected) in cases {
            let mut out = RgbC::default();
            assert_eq!(Result::Success, unsafe { parse(input.as_ptr(), input.len(), &mut out) });
            expect_rgb(expected, out);
        }
    }

    #[test]
    fn color_parse_invalid() {
        let cases: [&[u8]; 3] = [b"", b"notacolor", b"#12345"];

        for case in cases {
            let mut out = RgbC::default();
            assert_eq!(Result::InvalidValue, unsafe { parse(case.as_ptr(), case.len(), &mut out) });
        }

        let mut out = RgbC::default();
        assert_eq!(Result::InvalidValue, unsafe { parse(std::ptr::null(), 0, &mut out) });
    }

    #[test]
    fn color_parse_palette_entry_valid() {
        let cases: [(&[u8], u8, Rgb); 6] = [
            (b"0=#AABBCC", 0, rgb(170, 187, 204)),
            (b"0xF=#ABCDEF", 15, rgb(171, 205, 239)),
            (b"0b1=#014589", 1, rgb(1, 69, 137)),
            (b"0o7=#234567", 7, rgb(35, 69, 103)),
            (b" 1= #DDEEFF    ", 1, rgb(221, 238, 255)),
            (b"1=black", 1, rgb(0, 0, 0)),
        ];

        for (input, idx, expected) in cases {
            let mut index: u8 = 0;
            let mut out = RgbC::default();
            assert_eq!(Result::Success, unsafe {
                parse_palette_entry(input.as_ptr(), input.len(), &mut index, &mut out)
            });
            assert_eq!(idx, index);
            expect_rgb(expected, out);
        }
    }

    #[test]
    fn color_parse_palette_entry_invalid() {
        let cases: [&[u8]; 4] = [b"256=#AABBCC", b"a", b"", b"1=notacolor"];

        for case in cases {
            let mut index: u8 = 0;
            let mut out = RgbC::default();
            assert_eq!(Result::InvalidValue, unsafe {
                parse_palette_entry(case.as_ptr(), case.len(), &mut index, &mut out)
            });
        }

        let mut index: u8 = 0;
        let mut out = RgbC::default();
        assert_eq!(Result::InvalidValue, unsafe {
            parse_palette_entry(std::ptr::null(), 0, &mut index, &mut out)
        });
    }

    #[test]
    fn color_palette_default() {
        let mut out: color::PaletteC = [RgbC::default(); 256];
        palette_default(&mut out);

        let expected = color::palette_cval(&color::DEFAULT);
        expect_palette_c(&expected, &out);

        expect_rgb(rgb(0x1D, 0x1F, 0x21), out[0]);
        expect_rgb(rgb(0, 0, 0), out[16]);
        expect_rgb(rgb(255, 255, 255), out[231]);
        expect_rgb(rgb(8, 8, 8), out[232]);
        expect_rgb(rgb(238, 238, 238), out[255]);
    }

    #[test]
    fn color_palette_generate_base16_preserved() {
        let base = color::palette_cval(&color::DEFAULT);
        let palette = generate_palette(Some(&base), None, rgb(0, 0, 0), rgb(255, 255, 255), false);

        for i in 0..16 {
            assert_eq!(Rgb::from_c(base[i]), Rgb::from_c(palette[i]));
        }
    }

    #[test]
    fn color_palette_generate_cube_corners() {
        let bg = rgb(0, 0, 0);
        let fg = rgb(255, 255, 255);
        let palette = generate_palette(None, None, bg, fg, false);

        expect_rgb(bg, palette[16]);
        expect_rgb(fg, palette[231]);
    }

    #[test]
    fn color_palette_generate_light_theme_harmonious_false() {
        let white = rgb(255, 255, 255);
        let black = rgb(0, 0, 0);

        let normal = generate_palette(None, None, white, black, false);
        expect_rgb(black, normal[16]);
        expect_rgb(white, normal[231]);

        let harmonious = generate_palette(None, None, white, black, true);
        expect_rgb(white, harmonious[16]);
        expect_rgb(black, harmonious[231]);
    }

    #[test]
    fn color_palette_generate_grayscale_monotonic() {
        let palette = generate_palette(None, None, rgb(0, 0, 0), rgb(255, 255, 255), false);

        let mut prev_lum: f64 = 0.0;
        for c in &palette[232..256] {
            let lum = Rgb::from_c(*c).luminance();
            assert!(lum >= prev_lum);
            prev_lum = lum;
        }
    }

    #[test]
    fn color_palette_generate_skip_mask() {
        let base = color::palette_cval(&color::DEFAULT);
        let mut skip = PaletteMask { bits: [0; 4] };
        set_mask_bit(&mut skip, 20);
        set_mask_bit(&mut skip, 100);
        set_mask_bit(&mut skip, 240);

        let palette = generate_palette(Some(&base), Some(&skip), rgb(0, 0, 0), rgb(255, 255, 255), false);

        assert_eq!(Rgb::from_c(base[20]), Rgb::from_c(palette[20]));
        assert_eq!(Rgb::from_c(base[100]), Rgb::from_c(palette[100]));
        assert_eq!(Rgb::from_c(base[240]), Rgb::from_c(palette[240]));
        assert_ne!(Rgb::from_c(palette[21]), Rgb::from_c(base[21]));
    }

    #[test]
    fn color_palette_generate_dark_harmonious_no_op() {
        let bg = rgb(0, 0, 0);
        let fg = rgb(255, 255, 255);
        let normal = generate_palette(None, None, bg, fg, false);
        let harmonious = generate_palette(None, None, bg, fg, true);

        for i in 16..256 {
            assert_eq!(Rgb::from_c(normal[i]), Rgb::from_c(harmonious[i]));
        }
    }

    #[test]
    fn color_palette_generate_light_harmonious_ramp() {
        let bg = rgb(255, 255, 255);
        let fg = rgb(0, 0, 0);

        {
            let palette = generate_palette(None, None, bg, fg, false);
            let mut prev_lum: f64 = 0.0;
            for c in &palette[232..256] {
                let lum = Rgb::from_c(*c).luminance();
                assert!(lum >= prev_lum);
                prev_lum = lum;
            }
        }

        {
            let palette = generate_palette(None, None, bg, fg, true);
            let mut prev_lum: f64 = 1.0;
            for c in &palette[232..256] {
                let lum = Rgb::from_c(*c).luminance();
                assert!(lum <= prev_lum);
                prev_lum = lum;
            }
        }
    }

    #[test]
    fn color_palette_generate_null_base_uses_default() {
        let base = color::palette_cval(&color::DEFAULT);
        let bg = rgb(5, 10, 15);
        let fg = rgb(245, 250, 255);

        let null_base = generate_palette(None, None, bg, fg, false);
        let explicit = generate_palette(Some(&base), None, bg, fg, false);
        expect_palette_c(&explicit, &null_base);
    }

    #[test]
    fn color_palette_generate_null_skip() {
        let base = color::palette_cval(&color::DEFAULT);
        let bg = rgb(5, 10, 15);
        let fg = rgb(245, 250, 255);
        let skip = PaletteMask { bits: [0; 4] };

        let null_skip = generate_palette(Some(&base), None, bg, fg, false);
        let explicit = generate_palette(Some(&base), Some(&skip), bg, fg, false);
        expect_palette_c(&explicit, &null_skip);
    }

    #[test]
    fn color_palette_generate_matches_generate256_color() {
        let base_c = color::palette_cval(&color::DEFAULT);
        let bg = rgb(16, 32, 48);
        let fg = rgb(240, 224, 208);
        let mut skip_c = PaletteMask { bits: [0; 4] };
        set_mask_bit(&mut skip_c, 20);
        set_mask_bit(&mut skip_c, 100);
        set_mask_bit(&mut skip_c, 240);

        let actual = generate_palette(Some(&base_c), Some(&skip_c), bg, fg, true);
        let expected_z = color::generate_256_color(color::palette_zval(&base_c), skip_c.to_zig(), bg, fg, true);
        let expected = color::palette_cval(&expected_z);
        expect_palette_c(&expected, &actual);
    }

    #[test]
    fn color_palette_generate_in_place() {
        let mut base = color::palette_cval(&color::DEFAULT);
        let bg = rgb(0, 0, 0);
        let fg = rgb(255, 255, 255);
        let expected = generate_palette(Some(&base), None, bg, fg, false);

        let bg_c = bg.cval();
        let fg_c = fg.cval();
        let base_ptr: *mut color::PaletteC = &mut base;
        unsafe { palette_generate(base_ptr, std::ptr::null(), &bg_c, &fg_c, false, base_ptr) };
        expect_palette_c(&expected, &base);
    }

    #[test]
    fn color_luminance() {
        let black_c = rgb(0, 0, 0).cval();
        let white_c = rgb(255, 255, 255).cval();
        assert!((luminance(&black_c) - 0.0).abs() <= 0.000001);
        assert!((luminance(&white_c) - 1.0).abs() <= 0.000001);

        let samples = [rgb(40, 44, 52), rgb(171, 205, 239), rgb(12, 34, 56)];
        for sample in samples {
            let sample_c = sample.cval();
            assert!((sample.luminance() - luminance(&sample_c)).abs() <= 0.000001);
        }
    }

    #[test]
    fn color_perceived_luminance() {
        let black_c = rgb(0, 0, 0).cval();
        let white_c = rgb(255, 255, 255).cval();
        let dark = rgb(0x28, 0x2C, 0x34);
        let dark_c = dark.cval();
        assert!((perceived_luminance(&black_c) - 0.0).abs() <= 0.000001);
        assert!((perceived_luminance(&white_c) - 1.0).abs() <= 0.000001);
        assert!(perceived_luminance(&dark_c) < 0.5);
        assert!(perceived_luminance(&white_c) > 0.5);

        let samples = [dark, rgb(171, 205, 239), rgb(12, 34, 56)];
        for sample in samples {
            let sample_c = sample.cval();
            assert!((sample.perceived_luminance() - perceived_luminance(&sample_c)).abs() <= 0.000001);
        }
    }

    #[test]
    fn color_contrast() {
        let black_c = rgb(0, 0, 0).cval();
        let white_c = rgb(255, 255, 255).cval();
        let gray_c = rgb(128, 128, 128).cval();

        assert!((contrast(&white_c, &black_c) - 21.0).abs() <= 0.000001);
        assert!((contrast(&black_c, &white_c) - contrast(&white_c, &black_c)).abs() <= 0.000001);
        assert!((contrast(&gray_c, &gray_c) - 1.0).abs() <= 0.000001);
    }

    #[test]
    fn color_x11_names() {
        let names = x11_names();
        let count = x11_name_count();
        assert_eq!(x11_color::entries().len(), count);
        assert!(count > 700);

        let mut walked: usize = 0;
        unsafe {
            while !(*names.add(walked)).name.is_null() {
                let name = std::ffi::CStr::from_ptr((*names.add(walked)).name);
                let expected = x11_color::MAP.get(name.to_bytes()).unwrap();
                assert_eq!(expected, Rgb::from_c((*names.add(walked)).color));
                walked += 1;
            }
            assert_eq!(count, walked);
            assert!((*names.add(count)).name.is_null());
        }
    }

    #[test]
    fn color_x11_names_static() {
        assert_eq!(x11_names(), x11_names());
    }
}
