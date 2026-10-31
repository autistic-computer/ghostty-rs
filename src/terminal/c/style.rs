//! Port of `src/terminal/c/style.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: Zig writes the eight flag bytes with a single
//! vectorized 8-byte store; the Rust port spreads the low eight flag bits
//! into a `u64` with the same layout and writes it unaligned, which keeps
//! the single store.

use std::ffi::c_int;

use crate::terminal::color::RgbC;
use crate::terminal::style;

crate::c_api_enum! {
    /// C: GhosttyStyleColorTag
    pub enum ColorTag {
        None = 0,
        Palette = 1,
        Rgb = 2,
    }
}

/// C: GhosttyStyleColorValue
#[repr(C)]
#[derive(Clone, Copy)]
pub union ColorValue {
    pub palette: u8,
    pub rgb: RgbC,
    pub _padding: u64,
}

/// C: GhosttyStyleColor
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Color {
    pub tag: ColorTag,
    pub value: ColorValue,
}

// from_color converts the tag as a plain integer, which requires the
// internal and C tag values to line up.
const _: () = {
    assert!(style::ColorTag::None as c_int == ColorTag::None as c_int);
    assert!(style::ColorTag::Palette as c_int == ColorTag::Palette as c_int);
    assert!(style::ColorTag::Rgb as c_int == ColorTag::Rgb as c_int);
};

impl Color {
    pub const NONE: Color = Color {
        tag: ColorTag::None,
        value: ColorValue { _padding: 0 },
    };

    pub fn from_color(c: style::Color) -> Color {
        // The value is built as a single integer rather than through
        // the per-variant union fields: the C union layout puts
        // .palette at byte 0 and .rgb at bytes 0-2, which is exactly
        // the internal payload representation, so each variant is the
        // payload zero-extended to the u64 union backing. This keeps
        // the conversion to two stores (tag + value) per color, which
        // matters for the per-cell style reads in the render state
        // API.
        let (tag, value): (ColorTag, u64) = match c {
            style::Color::None => (ColorTag::None, 0),
            style::Color::Palette(idx) => (ColorTag::Palette, idx as u64),
            style::Color::Rgb(rgb) => (
                ColorTag::Rgb,
                u64::from_le_bytes([rgb.r, rgb.g, rgb.b, 0, 0, 0, 0, 0]),
            ),
        };
        Color {
            tag,
            value: ColorValue { _padding: value },
        }
    }
}

/// C: GhosttyStyle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Style {
    pub size: usize,
    pub fg_color: Color,
    pub bg_color: Color,
    pub underline_color: Color,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: c_int,
}

// write() writes the eight bool fields as one 8-byte store, which
// requires them to be consecutive and to match the low eight flag bits
// in order.
const _: () = {
    use std::mem::offset_of;
    let base = offset_of!(Style, bold);
    assert!(offset_of!(Style, italic) == base + 1);
    assert!(offset_of!(Style, faint) == base + 2);
    assert!(offset_of!(Style, blink) == base + 3);
    assert!(offset_of!(Style, inverse) == base + 4);
    assert!(offset_of!(Style, invisible) == base + 5);
    assert!(offset_of!(Style, strikethrough) == base + 6);
    assert!(offset_of!(Style, overline) == base + 7);
};

impl Style {
    /// The default (empty) style in C representation.
    pub const DEFAULT: Style = Style {
        size: std::mem::size_of::<Style>(),
        fg_color: Color::NONE,
        bg_color: Color::NONE,
        underline_color: Color::NONE,
        bold: false,
        italic: false,
        faint: false,
        blink: false,
        inverse: false,
        invisible: false,
        strikethrough: false,
        overline: false,
        underline: 0,
    };

    /// Write the C representation of the given style directly through
    /// the out pointer. The hot per-cell style read in the render state
    /// API uses this rather than `*out = from_style(s)`.
    pub fn write(s: &style::Style, out: &mut Style) {
        out.size = std::mem::size_of::<Style>();
        out.fg_color = Color::from_color(s.fg_color);
        out.bg_color = Color::from_color(s.bg_color);
        out.underline_color = Color::from_color(s.underline_color);

        // Spread the low eight flag bits (the eight bool fields, in
        // order) into one byte each and write them with a single
        // 8-byte store.
        let flag_bits = s.flags.to_bits() as u8;
        let mut bytes = [0u8; 8];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (flag_bits >> i) & 1;
        }
        // SAFETY: the eight bool fields are consecutive (checked above)
        // and each byte is 0 or 1, a valid bool.
        unsafe {
            std::ptr::write_unaligned(std::ptr::addr_of_mut!(out.bold) as *mut [u8; 8], bytes);
        }

        out.underline = s.flags.underline as c_int;
    }

    pub fn from_style(s: &style::Style) -> Style {
        let mut result = Style::DEFAULT;
        Self::write(s, &mut result);
        result
    }
}

/// Returns the default style.
#[export_name = "ghostty_style_default"]
pub extern "C" fn default_style(result: &mut Style) {
    *result = Style::DEFAULT;
    debug_assert_eq!(result.size, std::mem::size_of::<Style>());
}

/// Returns true if the style is the default style.
#[export_name = "ghostty_style_is_default"]
pub extern "C" fn style_is_default(s: &Style) -> bool {
    debug_assert_eq!(s.size, std::mem::size_of::<Style>());
    s.fg_color.tag == ColorTag::None
        && s.bg_color.tag == ColorTag::None
        && s.underline_color.tag == ColorTag::None
        && !s.bold
        && !s.italic
        && !s.faint
        && !s.blink
        && !s.inverse
        && !s.invisible
        && !s.strikethrough
        && !s.overline
        && s.underline == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::color::Rgb;
    use crate::terminal::sgr;

    #[test]
    fn default_style_test() {
        let mut s = Style::from_style(&style::Style::default());
        s.bold = true;
        default_style(&mut s);
        assert!(style_is_default(&s));
        assert_eq!(ColorTag::None, s.fg_color.tag);
        assert_eq!(ColorTag::None, s.bg_color.tag);
        assert_eq!(ColorTag::None, s.underline_color.tag);
        assert!(!s.bold);
        assert!(!s.italic);
        assert_eq!(0, s.underline);
    }

    #[test]
    fn convert_style_with_colors() {
        let zig_style = style::Style {
            fg_color: style::Color::Palette(42),
            bg_color: style::Color::Rgb(Rgb { r: 255, g: 128, b: 64 }),
            underline_color: style::Color::None,
            flags: style::Flags {
                bold: true,
                underline: sgr::Underline::Curly,
                ..Default::default()
            },
        };

        let c_style = Style::from_style(&zig_style);
        assert_eq!(ColorTag::Palette, c_style.fg_color.tag);
        assert_eq!(42, unsafe { c_style.fg_color.value.palette });
        assert_eq!(ColorTag::Rgb, c_style.bg_color.tag);
        assert_eq!(255, unsafe { c_style.bg_color.value.rgb.r });
        assert_eq!(128, unsafe { c_style.bg_color.value.rgb.g });
        assert_eq!(64, unsafe { c_style.bg_color.value.rgb.b });
        assert_eq!(ColorTag::None, c_style.underline_color.tag);
        assert!(c_style.bold);
        assert_eq!(3, c_style.underline);
        assert!(!style_is_default(&c_style));
    }
}
