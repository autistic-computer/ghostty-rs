//! Port of `src/terminal/style.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - `Style.Flags` (a `packed struct(u16)` in Zig) is a plain struct here;
//!   its packed form is available via [`Flags::to_bits`] and is what the
//!   style hash is computed from (through [`PackedStyle`]).
//! - The wasm-specific `default`/`eql` strategies are not needed on the
//!   native targets the port supports; the native strategy is used.
//! - Memory layout: the style set lives in page memory, so the size of
//!   `Style` decides the page layout (how many rows a page of a given
//!   width holds, and so the memory a terminal uses). Zig's `Style` is 28
//!   bytes with 4-byte alignment: each `Color` is 8 bytes (a `u8` tag and
//!   a `packed struct(u24)` RGB payload, which is 4-byte aligned) and
//!   `Flags` is a `u16`. Rust's `Flags` keeps one field per flag (9
//!   bytes), so `Color` uses a `u16` tag (6 bytes) and `Style` is
//!   `align(4)`, which gives `Style` exactly Zig's size and alignment
//!   (checked at compile time below).

use core::fmt;

use crate::fastprint;
use crate::terminal::color::{self, Palette, Rgb};
use crate::terminal::page;
use crate::terminal::ref_counted_set::{self, RefCountedSet, RefCountedSetContext};
use crate::terminal::sgr;
use crate::terminal::size;

/// The unique identifier for a style. This is at most the number of cells
/// that can fit into a terminal page.
pub type Id = size::StyleCountInt;

/// The Id to use for default styling.
pub const DEFAULT_ID: Id = 0;

/// The style attributes for a cell.
///
/// `align(4)` (with `Color`'s `u16` tag) gives this Zig's size and
/// alignment, which page layouts depend on; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(align(4))]
pub struct Style {
    /// Various colors, all self-explanatory.
    pub fg_color: Color,
    pub bg_color: Color,
    pub underline_color: Color,

    /// On/off attributes that don't require much bit width so we use
    /// a packed struct to make this take up significantly less space.
    pub flags: Flags,
}

/// (Zig: `Style.Flags`, a `packed struct(u16)`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags {
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: sgr::Underline,
}

impl Flags {
    /// The packed `u16` representation (field order as in Zig, LSB first).
    pub const fn to_bits(self) -> u16 {
        (self.bold as u16)
            | (self.italic as u16) << 1
            | (self.faint as u16) << 2
            | (self.blink as u16) << 3
            | (self.inverse as u16) << 4
            | (self.invisible as u16) << 5
            | (self.strikethrough as u16) << 6
            | (self.overline as u16) << 7
            | (self.underline as u16) << 8
    }
}

/// The tag of [`Color`] (Zig: `Color.Tag`, `enum(u8)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ColorTag {
    None = 0,
    Palette = 1,
    Rgb = 2,
}

/// The color for an SGR attribute. A color can come from multiple
/// sources so we use this to track the source plus color value so that
/// we can properly react to things like palette changes.
///
/// The `u16` tag makes this 6 bytes so that [`Style`] has Zig's size; see
/// the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u16)]
pub enum Color {
    #[default]
    None,
    Palette(u8),
    Rgb(Rgb),
}

impl Color {
    pub fn tag(self) -> ColorTag {
        match self {
            Color::None => ColorTag::None,
            Color::Palette(_) => ColorTag::Palette,
            Color::Rgb(_) => ColorTag::Rgb,
        }
    }

    /// True if the color is equal to another color.
    pub fn eql(self, other: Color) -> bool {
        match (self, other) {
            (Color::None, Color::None) => true,
            (Color::Palette(a), Color::Palette(b)) => a == b,
            (Color::Rgb(a), Color::Rgb(b)) => a == b,
            _ => false,
        }
    }
}

/// Formatting to make debug logs easier to read
/// by only including non-default attributes.
impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Color::None => f.write_str("Color.none"),
            Color::Palette(p) => write!(f, "Color.palette{{ {} }}", p),
            Color::Rgb(rgb) => write!(f, "Color.rgb{{ {}, {}, {} }}", rgb.r, rgb.g, rgb.b),
        }
    }
}

/// The color to use for bold text. This avoids a dependency on the
/// config module by using terminal-native color types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoldColor {
    Color(Rgb),
    Bright,
}

/// Options for [`Style::fg`] (Zig: `Style.Fg`).
#[derive(Debug, Clone, Copy)]
pub struct Fg<'a> {
    /// The default color to use if the style doesn't specify a
    /// foreground color and no configuration options override
    /// it.
    pub default: Rgb,

    /// The current color palette. Required to map palette indices to
    /// real color values.
    pub palette: &'a Palette,

    /// If specified, the color to use for bold text.
    pub bold: Option<BoldColor>,
}

impl Style {
    /// True if the style is the default style.
    ///
    /// (Zig: `default`; renamed so it doesn't shadow `Default::default`.)
    #[inline]
    pub fn is_default(&self) -> bool {
        self.eql(&Style {
            fg_color: Color::None,
            bg_color: Color::None,
            underline_color: Color::None,
            flags: Flags {
                bold: false,
                italic: false,
                faint: false,
                blink: false,
                inverse: false,
                invisible: false,
                strikethrough: false,
                overline: false,
                underline: sgr::Underline::None,
            },
        })
    }

    /// True if the style is equal to another style.
    pub fn eql(&self, other: &Style) -> bool {
        // The branchy early-exit field compare measures faster on native
        // (unequal styles usually differ in the first field or two).
        self.flags == other.flags
            && self.fg_color.eql(other.fg_color)
            && self.bg_color.eql(other.bg_color)
            && self.underline_color.eql(other.underline_color)
    }

    /// Returns the bg color for a cell with this style given the cell
    /// that has this style and the palette to use.
    ///
    /// Note that generally if a cell is a color-only cell, it SHOULD
    /// only have the default style, but this is meant to work with the
    /// default style as well.
    pub fn bg(&self, cell: &page::Cell, palette: &Palette) -> Option<Rgb> {
        match cell.content_tag() {
            page::ContentTag::BgColorPalette => Some(palette[cell.content_color_palette() as usize]),
            page::ContentTag::BgColorRgb => {
                let rgb = cell.content_color_rgb();
                Some(Rgb {
                    r: rgb.r,
                    g: rgb.g,
                    b: rgb.b,
                })
            }

            _ => match self.bg_color {
                Color::None => None,
                Color::Palette(idx) => Some(palette[idx as usize]),
                Color::Rgb(rgb) => Some(rgb),
            },
        }
    }

    /// Returns the fg color for a cell with this style given the palette
    /// and various configuration options.
    pub fn fg(&self, opts: Fg<'_>) -> Rgb {
        // Note we don't pull the bold check to the top-level here because
        // we don't want to duplicate the conditional multiple times since
        // certain colors require more checks (e.g. `bold_is_bright`).

        match self.fg_color {
            Color::None => {
                if self.flags.bold {
                    if let Some(BoldColor::Color(v)) = opts.bold {
                        return v;
                    }
                }

                opts.default
            }

            Color::Palette(idx) => {
                if self.flags.bold && opts.bold.is_some() {
                    let bright_offset = color::Name::BRIGHT_BLACK.0;
                    if idx < bright_offset {
                        return opts.palette[(idx + bright_offset) as usize];
                    }
                }

                opts.palette[idx as usize]
            }

            Color::Rgb(rgb) => {
                if self.flags.bold && rgb.eql(opts.default) {
                    if let Some(BoldColor::Color(v)) = opts.bold {
                        return v;
                    }
                }

                rgb
            }
        }
    }

    /// Returns the underline color for this style.
    pub fn underline_color(&self, palette: &Palette) -> Option<Rgb> {
        match self.underline_color {
            Color::None => None,
            Color::Palette(idx) => Some(palette[idx as usize]),
            Color::Rgb(rgb) => Some(rgb),
        }
    }

    /// Returns a bg-color only cell from this style, if it exists.
    pub fn bg_cell(&self) -> Option<page::Cell> {
        match self.bg_color {
            Color::None => None,
            Color::Palette(idx) => {
                let mut cell = page::Cell::default();
                cell.set_content_tag(page::ContentTag::BgColorPalette);
                cell.set_content_color_palette(idx);
                Some(cell)
            }
            Color::Rgb(rgb) => {
                let mut cell = page::Cell::default();
                cell.set_content_tag(page::ContentTag::BgColorRgb);
                cell.set_content_color_rgb(page::Rgb {
                    r: rgb.r,
                    g: rgb.g,
                    b: rgb.b,
                });
                Some(cell)
            }
        }
    }

    /// Returns a formatter that renders this style as VT sequences,
    /// to be used with `{}`. This always resets the style first `\x1b[0m`
    /// since a style is meant to be fully self-contained.
    ///
    /// For individual styles, this always emits multiple SGR sequences
    /// (i.e. an individual `\x1b[<stuff>m` for each attribute) rather than
    /// trying to combine them into a single sequence. We do this because
    /// terminals have varying levels of support for combined sequences
    /// especially with mixed separators (e.g. `:` vs `;`).
    pub fn formatter_vt(&self) -> VtFormatter<'_> {
        VtFormatter {
            style: self,
            palette: None,
        }
    }

    /// Returns a formatter that renders this style as inline CSS properties,
    /// to be used with `{}`. The output is a valid CSS style string suitable
    /// for use in a `style` attribute (e.g., "color: rgb(255, 0, 0); font-weight: bold;").
    ///
    /// Palette colors are emitted as CSS variables like `var(--vt-palette-N)`.
    pub fn formatter_html(&self) -> HtmlFormatter<'_> {
        HtmlFormatter {
            style: self,
            palette: None,
        }
    }

    pub fn hash(&self) -> u64 {
        // We pack the style in to 128 bits, fold it to 64 bits,
        // then use std.hash.int to make it sufficiently uniform.
        let packed_style = PackedStyle::from_style(self);
        let wide: [u64; 2] = [packed_style.0 as u64, (packed_style.0 >> 64) as u64];
        hash_int_u64(wide[0] ^ wide[1])
    }
}

/// Zig `std.hash.int` for `u64`.
#[inline(always)]
fn hash_int_u64(input: u64) -> u64 {
    let mut x = input;
    x ^= x >> 32;
    x = x.wrapping_mul(0xd6e8feb86659fd93);
    x ^= x >> 32;
    x = x.wrapping_mul(0xd6e8feb86659fd93);
    x ^= x >> 32;
    x
}

/// Formatting to make debug logs easier to read
/// by only including non-default attributes.
impl fmt::Display for Style {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dflt = Style::default_value();

        f.write_str("Style{ ")?;

        let mut started = false;

        let colors: [(&str, Color, Color); 3] = [
            ("fg_color", self.fg_color, dflt.fg_color),
            ("bg_color", self.bg_color, dflt.bg_color),
            ("underline_color", self.underline_color, dflt.underline_color),
        ];
        for (name, value, d_val) in colors {
            if value != d_val {
                if started {
                    f.write_str(", ")?;
                }
                write!(f, "{}={}", name, value)?;
                started = true;
            }
        }

        // flags
        {
            if started {
                f.write_str(", ")?;
            }

            f.write_str("flags={ ")?;

            started = false;

            let bools: [(&str, bool); 8] = [
                ("bold", self.flags.bold),
                ("italic", self.flags.italic),
                ("faint", self.flags.faint),
                ("blink", self.flags.blink),
                ("inverse", self.flags.inverse),
                ("invisible", self.flags.invisible),
                ("strikethrough", self.flags.strikethrough),
                ("overline", self.flags.overline),
            ];
            for (name, v) in bools {
                if v {
                    if started {
                        f.write_str(", ")?;
                    }
                    f.write_str(name)?;
                    started = true;
                }
            }
            if self.flags.underline != dflt.flags.underline {
                if started {
                    f.write_str(", ")?;
                }
                write!(f, "underline={:?}", self.flags.underline)?;
            }
            f.write_str(" }")?;
        }

        f.write_str(" }")
    }
}

impl Style {
    /// The default style value (Zig: `.{}`).
    #[inline]
    pub const fn default_value() -> Style {
        Style {
            fg_color: Color::None,
            bg_color: Color::None,
            underline_color: Color::None,
            flags: Flags {
                bold: false,
                italic: false,
                faint: false,
                blink: false,
                inverse: false,
                invisible: false,
                strikethrough: false,
                overline: false,
                underline: sgr::Underline::None,
            },
        }
    }
}

/// Renders a style as VT sequences (Zig: `Style.VTFormatter`).
#[derive(Debug, Clone, Copy)]
pub struct VtFormatter<'a> {
    pub style: &'a Style,

    /// If set, palette colors will be emitted as RGB values instead of
    /// palette indices. This is useful when you want to capture the
    /// exact colors at formatting time rather than relying on the
    /// terminal's palette.
    pub palette: Option<&'a Palette>,
}

impl fmt::Display for VtFormatter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Style emission is a hot path when formatting styled terminal
        // contents, so all of the sequences are assembled in a buffer
        // and written in one call rather than going through the
        // (slower) format string machinery with a write per sequence.
        //
        // Worst case: `\x1b[0m` (4) + 7 flags (28) + `\x1b[53m` (5) +
        // `\x1b[4:2m` (6) + 3 RGB colors (19 each = 57) = 100.
        let mut buf = [0u8; 128];

        // Always reset the style. Styles are fully self-contained.
        // Even if this style is empty, then that means we want to go
        // back to the default.
        buf[0..4].copy_from_slice(b"\x1b[0m");
        let mut len: usize = 4;

        let put = |buf: &mut [u8; 128], len: &mut usize, s: &[u8]| {
            buf[*len..*len + s.len()].copy_from_slice(s);
            *len += s.len();
        };

        let flags = &self.style.flags;

        // Our flags
        if flags.bold {
            put(&mut buf, &mut len, b"\x1b[1m");
        }
        if flags.faint {
            put(&mut buf, &mut len, b"\x1b[2m");
        }
        if flags.italic {
            put(&mut buf, &mut len, b"\x1b[3m");
        }
        if flags.blink {
            put(&mut buf, &mut len, b"\x1b[5m");
        }
        if flags.inverse {
            put(&mut buf, &mut len, b"\x1b[7m");
        }
        if flags.invisible {
            put(&mut buf, &mut len, b"\x1b[8m");
        }
        if flags.strikethrough {
            put(&mut buf, &mut len, b"\x1b[9m");
        }
        if flags.overline {
            put(&mut buf, &mut len, b"\x1b[53m");
        }
        match flags.underline {
            sgr::Underline::None => {}
            sgr::Underline::Single => put(&mut buf, &mut len, b"\x1b[4m"),
            sgr::Underline::Double => put(&mut buf, &mut len, b"\x1b[4:2m"),
            sgr::Underline::Curly => put(&mut buf, &mut len, b"\x1b[4:3m"),
            sgr::Underline::Dotted => put(&mut buf, &mut len, b"\x1b[4:4m"),
            sgr::Underline::Dashed => put(&mut buf, &mut len, b"\x1b[4:5m"),
        }

        // Various colors.
        len += self.append_color(&mut buf[len..], 38, self.style.fg_color);
        len += self.append_color(&mut buf[len..], 48, self.style.bg_color);
        len += self.append_color(&mut buf[len..], 58, self.style.underline_color);

        // SAFETY-free: all bytes written are ASCII.
        f.write_str(core::str::from_utf8(&buf[..len]).map_err(|_| fmt::Error)?)
    }
}

impl VtFormatter<'_> {
    /// Appends a standalone `\x1b[{prefix};5;{idx}m` or
    /// `\x1b[{prefix};2;{r};{g};{b}m` sequence to buf, returning the
    /// length written.
    fn append_color(&self, buf: &mut [u8], prefix: u8, value: Color) -> usize {
        match value {
            Color::None => 0,

            Color::Palette(idx) => {
                // Direct RGB: `\x1b[{prefix};2;{r};{g};{b}m`
                if let Some(p) = self.palette {
                    return Self::append_color_rgb(buf, prefix, p[idx as usize]);
                }

                // Palette reference: `\x1b[{prefix};5;{idx}m`
                buf[0..2].copy_from_slice(b"\x1b[");
                let mut len: usize = 2;
                len += fastprint::print_decimal(&mut buf[len..], prefix);
                buf[len..len + 3].copy_from_slice(b";5;");
                len += 3;
                len += fastprint::print_decimal(&mut buf[len..], idx);
                buf[len] = b'm';
                len += 1;
                len
            }

            Color::Rgb(rgb) => Self::append_color_rgb(buf, prefix, rgb),
        }
    }

    /// Appends `\x1b[{prefix};2;{r};{g};{b}m` to buf, returning the
    /// length written.
    fn append_color_rgb(buf: &mut [u8], prefix: u8, rgb: Rgb) -> usize {
        buf[0..2].copy_from_slice(b"\x1b[");
        let mut len: usize = 2;
        len += fastprint::print_decimal(&mut buf[len..], prefix);
        buf[len..len + 3].copy_from_slice(b";2;");
        len += 3;
        len += fastprint::print_decimal(&mut buf[len..], rgb.r);
        buf[len] = b';';
        len += 1;
        len += fastprint::print_decimal(&mut buf[len..], rgb.g);
        buf[len] = b';';
        len += 1;
        len += fastprint::print_decimal(&mut buf[len..], rgb.b);
        buf[len] = b'm';
        len += 1;
        len
    }
}

/// Renders a style as inline CSS (Zig: `Style.HtmlFormatter`).
#[derive(Debug, Clone, Copy)]
pub struct HtmlFormatter<'a> {
    pub style: &'a Style,

    /// If set, palette colors will be emitted as RGB values instead of
    /// CSS variables. This is useful when you want to capture the exact
    /// colors at formatting time rather than relying on CSS variables.
    pub palette: Option<&'a Palette>,
}

impl fmt::Display for HtmlFormatter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let flags = &self.style.flags;

        // Colors
        self.format_color(f, "color", self.style.fg_color)?;
        self.format_color(f, "background-color", self.style.bg_color)?;
        self.format_color(f, "text-decoration-color", self.style.underline_color)?;

        // Text decoration line
        let has_line =
            flags.underline != sgr::Underline::None || flags.strikethrough || flags.overline || flags.blink;
        if has_line {
            f.write_str("text-decoration-line:")?;
            if flags.underline != sgr::Underline::None {
                f.write_str(" underline")?;
            }
            if flags.strikethrough {
                f.write_str(" line-through")?;
            }
            if flags.overline {
                f.write_str(" overline")?;
            }
            if flags.blink {
                f.write_str(" blink")?;
            }
            f.write_str(";")?;
        }

        // Text decoration style
        match flags.underline {
            sgr::Underline::None => {}
            sgr::Underline::Single => f.write_str("text-decoration-style: solid;")?,
            sgr::Underline::Double => f.write_str("text-decoration-style: double;")?,
            sgr::Underline::Curly => f.write_str("text-decoration-style: wavy;")?,
            sgr::Underline::Dotted => f.write_str("text-decoration-style: dotted;")?,
            sgr::Underline::Dashed => f.write_str("text-decoration-style: dashed;")?,
        }

        if flags.bold {
            f.write_str("font-weight: bold;")?;
        }
        if flags.italic {
            f.write_str("font-style: italic;")?;
        }
        if flags.faint {
            f.write_str("opacity: 0.5;")?;
        }
        if flags.invisible {
            f.write_str("visibility: hidden;")?;
        }
        if flags.inverse {
            f.write_str("filter: invert(100%);")?;
        }
        Ok(())
    }
}

impl HtmlFormatter<'_> {
    fn format_color(&self, f: &mut fmt::Formatter<'_>, property: &str, c: Color) -> fmt::Result {
        // Style emission is a hot path when formatting styled terminal
        // contents, so the values are assembled in a buffer and written
        // in one call rather than going through the (slower) format
        // string machinery.
        let mut buf = [0u8; 32];
        let len: usize = match c {
            Color::None => return Ok(()),

            // `{property}: rgb({r}, {g}, {b});`
            Color::Palette(idx) => {
                if let Some(p) = self.palette {
                    Self::format_color_rgb(&mut buf, p[idx as usize])
                } else {
                    // `{property}: var(--vt-palette-{idx});`
                    let prefix = b": var(--vt-palette-";
                    buf[0..prefix.len()].copy_from_slice(prefix);
                    let mut len = prefix.len();
                    len += fastprint::print_decimal(&mut buf[len..], idx);
                    buf[len..len + 2].copy_from_slice(b");");
                    len + 2
                }
            }

            Color::Rgb(rgb) => Self::format_color_rgb(&mut buf, rgb),
        };

        f.write_str(property)?;
        f.write_str(core::str::from_utf8(&buf[..len]).map_err(|_| fmt::Error)?)
    }

    /// Writes `: rgb({r}, {g}, {b});` into buf, returning the length
    /// written.
    fn format_color_rgb(buf: &mut [u8; 32], rgb: Rgb) -> usize {
        buf[0..6].copy_from_slice(b": rgb(");
        let mut len: usize = 6;
        len += fastprint::print_decimal(&mut buf[len..], rgb.r);
        buf[len..len + 2].copy_from_slice(b", ");
        len += 2;
        len += fastprint::print_decimal(&mut buf[len..], rgb.g);
        buf[len..len + 2].copy_from_slice(b", ");
        len += 2;
        len += fastprint::print_decimal(&mut buf[len..], rgb.b);
        buf[len..len + 2].copy_from_slice(b");");
        len += 2;
        len
    }
}

/// `PackedStyle` represents the same data as `Style` but without padding,
/// which is necessary for hashing via re-interpretation of the underlying
/// bytes.
///
/// `Style` is still preferred for everything else as it has type-safety
/// when using the `Color` tagged union.
///
/// Empirical testing shows that storing all of the tags first and then the
/// data provides a better layout for serializing into and is faster on
/// benchmarks.
///
/// Zig `packed struct(u128)` layout (LSB first):
/// - bits 0..24: tags (fg, bg, underline; `Color.Tag` u8 each)
/// - bits 24..96: data (fg, bg, underline; 24 bits each)
/// - bits 96..112: flags
/// - bits 112..128: padding (zero)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PackedStyle(u128);

impl PackedStyle {
    /// `PackedStyle.Data.fromColor`: none is 0, palette is the index in
    /// the low 8 bits, rgb is r | g << 8 | b << 16.
    #[inline]
    fn data_from_color(c: Color) -> u128 {
        match c {
            Color::None => 0,
            Color::Palette(idx) => idx as u128,
            Color::Rgb(rgb) => rgb.to_bits() as u128,
        }
    }

    #[inline]
    fn from_style(style: &Style) -> PackedStyle {
        let tags = (style.fg_color.tag() as u128)
            | (style.bg_color.tag() as u128) << 8
            | (style.underline_color.tag() as u128) << 16;
        let data = Self::data_from_color(style.fg_color)
            | Self::data_from_color(style.bg_color) << 24
            | Self::data_from_color(style.underline_color) << 48;
        PackedStyle(tags | data << 24 | (style.flags.to_bits() as u128) << 96)
    }
}

/// The context for [`Set`] (Zig: the anonymous context struct).
#[derive(Debug, Clone, Copy, Default)]
pub struct SetContext;

impl RefCountedSetContext<Style> for SetContext {
    fn hash(&self, style: &Style) -> u64 {
        style.hash()
    }

    fn eql(&self, a: &Style, b: &Style) -> bool {
        a.eql(b)
    }
}

pub type Set = RefCountedSet<Style, Id, size::CellCountInt, SetContext>;

// Zig's `@sizeOf(Style)` / `@alignOf(Style)` and `@sizeOf(Set.Item)`: the
// style set's item size is part of every page's memory layout.
const _: () = assert!(core::mem::size_of::<Style>() == 28);
const _: () = assert!(core::mem::align_of::<Style>() == 4);
const _: () = assert!(core::mem::size_of::<SetItem>() == 36);

/// The item type of [`Set`] (Zig: `Set.Item`).
pub type SetItem = ref_counted_set::Item<Style, Id, size::CellCountInt>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::ref_counted_set::AddError;
    use crate::terminal::size::{AlignedBuf, OffsetBuf};

    fn vt(style: &Style) -> String {
        format!("{}", style.formatter_vt())
    }

    fn html(style: &Style) -> String {
        format!("{}", style.formatter_html())
    }

    fn flags(f: impl FnOnce(&mut Flags)) -> Style {
        let mut s = Style::default();
        f(&mut s.flags);
        s
    }

    #[test]
    fn style_vt_formatting_empty() {
        assert_eq!(vt(&Style::default()), "\x1b[0m");
    }

    #[test]
    fn style_vt_formatting_bold() {
        assert_eq!(vt(&flags(|f| f.bold = true)), "\x1b[0m\x1b[1m");
    }

    #[test]
    fn style_vt_formatting_faint() {
        assert_eq!(vt(&flags(|f| f.faint = true)), "\x1b[0m\x1b[2m");
    }

    #[test]
    fn style_vt_formatting_italic() {
        assert_eq!(vt(&flags(|f| f.italic = true)), "\x1b[0m\x1b[3m");
    }

    #[test]
    fn style_vt_formatting_blink() {
        assert_eq!(vt(&flags(|f| f.blink = true)), "\x1b[0m\x1b[5m");
    }

    #[test]
    fn style_vt_formatting_inverse() {
        assert_eq!(vt(&flags(|f| f.inverse = true)), "\x1b[0m\x1b[7m");
    }

    #[test]
    fn style_vt_formatting_invisible() {
        assert_eq!(vt(&flags(|f| f.invisible = true)), "\x1b[0m\x1b[8m");
    }

    #[test]
    fn style_vt_formatting_strikethrough() {
        assert_eq!(vt(&flags(|f| f.strikethrough = true)), "\x1b[0m\x1b[9m");
    }

    #[test]
    fn style_vt_formatting_overline() {
        assert_eq!(vt(&flags(|f| f.overline = true)), "\x1b[0m\x1b[53m");
    }

    #[test]
    fn style_vt_formatting_underline_single() {
        assert_eq!(vt(&flags(|f| f.underline = sgr::Underline::Single)), "\x1b[0m\x1b[4m");
    }

    #[test]
    fn style_vt_formatting_underline_double() {
        assert_eq!(vt(&flags(|f| f.underline = sgr::Underline::Double)), "\x1b[0m\x1b[4:2m");
    }

    #[test]
    fn style_vt_formatting_underline_curly() {
        assert_eq!(vt(&flags(|f| f.underline = sgr::Underline::Curly)), "\x1b[0m\x1b[4:3m");
    }

    #[test]
    fn style_vt_formatting_underline_dotted() {
        assert_eq!(vt(&flags(|f| f.underline = sgr::Underline::Dotted)), "\x1b[0m\x1b[4:4m");
    }

    #[test]
    fn style_vt_formatting_underline_dashed() {
        assert_eq!(vt(&flags(|f| f.underline = sgr::Underline::Dashed)), "\x1b[0m\x1b[4:5m");
    }

    #[test]
    fn style_vt_formatting_fg_palette() {
        let style = Style {
            fg_color: Color::Palette(42),
            ..Default::default()
        };
        assert_eq!(vt(&style), "\x1b[0m\x1b[38;5;42m");
    }

    #[test]
    fn style_vt_formatting_fg_rgb() {
        let style = Style {
            fg_color: Color::Rgb(Rgb::new(255, 128, 64)),
            ..Default::default()
        };
        assert_eq!(vt(&style), "\x1b[0m\x1b[38;2;255;128;64m");
    }

    #[test]
    fn style_vt_formatting_bg_palette() {
        let style = Style {
            bg_color: Color::Palette(7),
            ..Default::default()
        };
        assert_eq!(vt(&style), "\x1b[0m\x1b[48;5;7m");
    }

    #[test]
    fn style_vt_formatting_bg_rgb() {
        let style = Style {
            bg_color: Color::Rgb(Rgb::new(32, 64, 96)),
            ..Default::default()
        };
        assert_eq!(vt(&style), "\x1b[0m\x1b[48;2;32;64;96m");
    }

    #[test]
    fn style_vt_formatting_underline_color_palette() {
        let style = Style {
            underline_color: Color::Palette(15),
            ..Default::default()
        };
        assert_eq!(vt(&style), "\x1b[0m\x1b[58;5;15m");
    }

    #[test]
    fn style_vt_formatting_underline_color_rgb() {
        let style = Style {
            underline_color: Color::Rgb(Rgb::new(200, 100, 50)),
            ..Default::default()
        };
        assert_eq!(vt(&style), "\x1b[0m\x1b[58;2;200;100;50m");
    }

    #[test]
    fn style_vt_formatting_multiple_flags() {
        let style = flags(|f| {
            f.bold = true;
            f.italic = true;
            f.underline = sgr::Underline::Single;
        });
        assert_eq!(vt(&style), "\x1b[0m\x1b[1m\x1b[3m\x1b[4m");
    }

    #[test]
    fn style_vt_formatting_all_flags() {
        let style = flags(|f| {
            f.bold = true;
            f.faint = true;
            f.italic = true;
            f.blink = true;
            f.inverse = true;
            f.invisible = true;
            f.strikethrough = true;
            f.overline = true;
            f.underline = sgr::Underline::Curly;
        });
        assert_eq!(
            vt(&style),
            "\x1b[0m\x1b[1m\x1b[2m\x1b[3m\x1b[5m\x1b[7m\x1b[8m\x1b[9m\x1b[53m\x1b[4:3m"
        );
    }

    #[test]
    fn style_vt_formatting_combined_colors_and_flags() {
        let style = Style {
            fg_color: Color::Rgb(Rgb::new(255, 0, 0)),
            bg_color: Color::Palette(8),
            underline_color: Color::Rgb(Rgb::new(0, 255, 0)),
            flags: Flags {
                bold: true,
                italic: true,
                underline: sgr::Underline::Double,
                ..Default::default()
            },
        };
        assert_eq!(
            vt(&style),
            "\x1b[0m\x1b[1m\x1b[3m\x1b[4:2m\x1b[38;2;255;0;0m\x1b[48;5;8m\x1b[58;2;0;255;0m"
        );
    }

    #[test]
    fn style_vt_formatting_all_colors_rgb() {
        let style = Style {
            fg_color: Color::Rgb(Rgb::new(10, 20, 30)),
            bg_color: Color::Rgb(Rgb::new(40, 50, 60)),
            underline_color: Color::Rgb(Rgb::new(70, 80, 90)),
            ..Default::default()
        };
        assert_eq!(
            vt(&style),
            "\x1b[0m\x1b[38;2;10;20;30m\x1b[48;2;40;50;60m\x1b[58;2;70;80;90m"
        );
    }

    #[test]
    fn style_vt_formatting_all_colors_palette() {
        let style = Style {
            fg_color: Color::Palette(1),
            bg_color: Color::Palette(2),
            underline_color: Color::Palette(3),
            ..Default::default()
        };
        assert_eq!(vt(&style), "\x1b[0m\x1b[38;5;1m\x1b[48;5;2m\x1b[58;5;3m");
    }

    #[test]
    fn style_vt_formatting_palette_with_palette_set_emits_rgb() {
        let style = Style {
            fg_color: Color::Palette(1),
            ..Default::default()
        };
        let mut formatter = style.formatter_vt();
        formatter.palette = Some(&color::DEFAULT);
        assert_eq!(format!("{}", formatter), "\x1b[0m\x1b[38;2;204;102;102m");
    }

    #[test]
    fn style_vt_formatting_all_palette_colors_with_palette_set() {
        let style = Style {
            fg_color: Color::Palette(1),
            bg_color: Color::Palette(2),
            underline_color: Color::Palette(3),
            ..Default::default()
        };
        let mut formatter = style.formatter_vt();
        formatter.palette = Some(&color::DEFAULT);
        assert_eq!(
            format!("{}", formatter),
            "\x1b[0m\x1b[38;2;204;102;102m\x1b[48;2;181;189;104m\x1b[58;2;240;198;116m"
        );
    }

    #[test]
    fn set_basic_usage() {
        let layout = Set::layout(16);
        let buf = AlignedBuf::new(layout.total_size, Set::BASE_ALIGN);
        let base = buf.ptr();

        let style = flags(|f| f.bold = true);
        let style2 = flags(|f| f.italic = true);

        unsafe {
            let mut set = Set::init(OffsetBuf::init(base), layout, SetContext);

            // Add style
            let id = set.add(base, style).unwrap();
            assert!(id > 0);

            // Second add should return the same metadata.
            {
                let id2 = set.add(base, style).unwrap();
                assert_eq!(id, id2);
            }

            // Look it up
            {
                let v = *set.get(base, id);
                assert!(v.flags.bold);

                let v2 = *set.get(base, id);
                assert_eq!(v, v2);
            }

            // Add a second style
            let id2 = set.add(base, style2).unwrap();

            // Look it up
            {
                let v = *set.get(base, id2);
                assert!(v.flags.italic);
            }

            // Ref count
            assert!(set.ref_count(base, id) == 2);
            assert!(set.ref_count(base, id2) == 1);

            // Release
            set.release(base, id);
            assert!(set.ref_count(base, id) == 1);
            set.release(base, id2);
            assert!(set.ref_count(base, id2) == 0);

            // We added the first one twice, so
            set.release(base, id);
            assert!(set.ref_count(base, id) == 0);
        }
    }

    #[test]
    fn set_capacities() {
        // We want to support at least this many styles without overflowing.
        let _ = Set::layout(16384);
    }

    #[test]
    fn set_zero_capacity() {
        // A zero-capacity set is a valid special case (see Layout.init).
        // It occurs in practice for pages with exact capacities where no
        // cell is styled (see Page.exactRowCapacity).
        let layout = Set::layout(0);
        assert_eq!(0, layout.total_size);

        // We allocate a nonzero buffer filled with 0xFF to simulate the
        // set being embedded in a larger structure (e.g. a Page) where
        // other data follows it. Lookups must not probe the zero-size
        // table: table[0] would read this adjacent memory and treat it
        // as an item ID, leading to far out-of-bounds item reads.
        let buf = AlignedBuf::new(64, Set::BASE_ALIGN);
        let base = buf.ptr();
        unsafe { core::ptr::write_bytes(base, 0xFF, 64) };

        unsafe {
            let mut set = Set::init(OffsetBuf::init(base), layout, SetContext);

            let style = flags(|f| f.bold = true);
            assert_eq!(None, set.lookup(base, style));
            assert_eq!(Err(AddError::OutOfMemory), set.add(base, style));
            assert_eq!(0, set.count());

            // The adjacent memory must be untouched.
            for i in 0..buf.len() {
                assert_eq!(0xFF, *base.add(i));
            }
        }
    }

    #[test]
    fn style_html_formatting_basic_bold() {
        assert_eq!(html(&flags(|f| f.bold = true)), "font-weight: bold;");
    }

    #[test]
    fn style_html_formatting_fg_color_rgb() {
        let style = Style {
            fg_color: Color::Rgb(Rgb::new(255, 128, 64)),
            ..Default::default()
        };
        assert_eq!(html(&style), "color: rgb(255, 128, 64);");
    }

    #[test]
    fn style_html_formatting_bg_color_palette() {
        let style = Style {
            bg_color: Color::Palette(7),
            ..Default::default()
        };
        assert_eq!(html(&style), "background-color: var(--vt-palette-7);");
    }

    #[test]
    fn style_html_formatting_combined_colors_and_flags() {
        let style = Style {
            fg_color: Color::Rgb(Rgb::new(255, 0, 0)),
            bg_color: Color::Rgb(Rgb::new(0, 0, 255)),
            flags: Flags {
                bold: true,
                italic: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let result = html(&style);
        assert!(result.contains("color: rgb(255, 0, 0);"));
        assert!(result.contains("background-color: rgb(0, 0, 255);"));
        assert!(result.contains("font-weight: bold;"));
        assert!(result.contains("font-style: italic;"));
    }

    #[test]
    fn style_html_formatting_single_decoration_line() {
        let result = html(&flags(|f| f.underline = sgr::Underline::Single));
        assert!(result.contains("text-decoration-line: underline;"));
        assert!(result.contains("text-decoration-style: solid;"));
    }

    #[test]
    fn style_html_formatting_multiple_decoration_lines() {
        let result = html(&flags(|f| {
            f.underline = sgr::Underline::Curly;
            f.strikethrough = true;
            f.overline = true;
        }));
        assert!(result.contains("text-decoration-line: underline line-through overline;"));
        assert!(result.contains("text-decoration-style: wavy;"));
    }

    #[test]
    fn style_html_formatting_palette_with_palette_set_emits_rgb() {
        let style = Style {
            bg_color: Color::Palette(7),
            ..Default::default()
        };
        let mut formatter = style.formatter_html();
        formatter.palette = Some(&color::DEFAULT);
        assert_eq!(format!("{}", formatter), "background-color: rgb(197, 200, 198);");
    }

    #[test]
    fn style_html_formatting_all_palette_colors_with_palette_set() {
        let style = Style {
            fg_color: Color::Palette(1),
            bg_color: Color::Palette(2),
            underline_color: Color::Palette(3),
            ..Default::default()
        };
        let mut formatter = style.formatter_html();
        formatter.palette = Some(&color::DEFAULT);
        assert_eq!(
            format!("{}", formatter),
            "color: rgb(204, 102, 102);background-color: rgb(181, 189, 104);text-decoration-color: rgb(240, 198, 116);"
        );
    }

    // comptime assertions on PackedStyle (Zig: `comptime { ... }`).
    #[test]
    fn packed_style_layout() {
        assert_eq!(core::mem::size_of::<PackedStyle>(), 16);
        // Distinct styles pack differently.
        let a = PackedStyle::from_style(&flags(|f| f.bold = true));
        let b = PackedStyle::from_style(&flags(|f| f.italic = true));
        assert_ne!(a, b);
        assert_eq!(PackedStyle::from_style(&Style::default()).0, 0);
    }
}
