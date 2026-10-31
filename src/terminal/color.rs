//! Port of `src/terminal/color.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::io::Write as _;

use crate::lib::parse_int;
use crate::terminal::fraction;
use crate::terminal::x11_color;

/// The default palette.
pub static DEFAULT: Palette = default_palette();

const fn default_palette() -> Palette {
    let mut result: Palette = [Rgb { r: 0, g: 0, b: 0 }; 256];

    // Named values
    let mut i: usize = 0;
    while i < 16 {
        result[i] = match Name(i as u8).default_const() {
            Some(v) => v,
            None => unreachable!(),
        };
        i += 1;
    }

    // Cube
    assert!(i == 16);
    let mut r: u8 = 0;
    while r < 6 {
        let mut g: u8 = 0;
        while g < 6 {
            let mut b: u8 = 0;
            while b < 6 {
                result[i] = Rgb {
                    r: if r == 0 { 0 } else { r * 40 + 55 },
                    g: if g == 0 { 0 } else { g * 40 + 55 },
                    b: if b == 0 { 0 } else { b * 40 + 55 },
                };
                i += 1;
                b += 1;
            }
            g += 1;
        }
        r += 1;
    }

    // Gray ramp
    assert!(i == 232);
    while i < 256 {
        let value = (((i - 232) * 10) + 8) as u8;
        result[i] = Rgb { r: value, g: value, b: value };
        i += 1;
    }

    result
}

/// Palette is the 256 color palette.
pub type Palette = [Rgb; 256];

/// A parsed palette entry from Ghostty's config "N=COLOR" syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteEntry {
    pub index: u8,
    pub color: Rgb,
}

/// Errors for [`parse_palette_entry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteEntryError {
    InvalidFormat,
    Overflow,
}

impl fmt::Display for PaletteEntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat => f.write_str("InvalidFormat"),
            Self::Overflow => f.write_str("Overflow"),
        }
    }
}

impl std::error::Error for PaletteEntryError {}

impl From<RgbParseError> for PaletteEntryError {
    fn from(_: RgbParseError) -> Self {
        Self::InvalidFormat
    }
}

/// Parse a palette entry in Ghostty config syntax: "N=COLOR" where N is
/// a palette index 0-255 (decimal, or 0x/0o/0b-prefixed per Zig's
/// parseInt base-0 rules) and COLOR is anything Rgb::parse accepts.
/// Whitespace (spaces/tabs) around N and COLOR is ignored.
pub fn parse_palette_entry(value: &[u8]) -> Result<PaletteEntry, PaletteEntryError> {
    let eql_idx = value
        .iter()
        .position(|&c| c == b'=')
        .ok_or(PaletteEntryError::InvalidFormat)?;
    let index = parse_int::std_parse_int::<u8>(trim(&value[..eql_idx], b" \t"), 0).map_err(
        |err| match err {
            parse_int::ParseIntError::Overflow => PaletteEntryError::Overflow,
            parse_int::ParseIntError::InvalidCharacter => PaletteEntryError::InvalidFormat,
        },
    )?;
    let rgb = Rgb::parse(&value[eql_idx + 1..])?;
    Ok(PaletteEntry { index, color: rgb })
}

/// Trim bytes in `set` from both ends of `s` (Zig `std.mem.trim`).
pub(crate) fn trim<'a>(s: &'a [u8], set: &[u8]) -> &'a [u8] {
    let mut start = 0;
    let mut end = s.len();
    while start < end && set.contains(&s[start]) {
        start += 1;
    }
    while end > start && set.contains(&s[end - 1]) {
        end -= 1;
    }
    &s[start..end]
}

/// C-compatible palette type using the extern Rgb struct.
pub type PaletteC = [RgbC; 256];

/// Convert a Palette to a PaletteC.
pub fn palette_cval(palette: &Palette) -> PaletteC {
    let mut result: PaletteC = [RgbC { r: 0, g: 0, b: 0 }; 256];
    palette_cval_slice(&palette[..], &mut result[..]);
    result
}

/// Convert a slice of palette entries to their C representation.
/// Asserts that both slices are the same length.
///
/// The Zig version uses a vectorized 4-to-3 byte shuffle because its RGB is
/// a 4-byte packed struct. The Rust `Rgb` is already 3 bytes, so a plain
/// element-wise copy is used.
pub fn palette_cval_slice(src: &[Rgb], dst: &mut [RgbC]) {
    assert_eq!(src.len(), dst.len());
    for (entry, out) in src.iter().zip(dst.iter_mut()) {
        *out = entry.cval();
    }
}

/// Convert a PaletteC to a Palette.
pub fn palette_zval(palette: &PaletteC) -> Palette {
    let mut result: Palette = [Rgb::default(); 256];
    for (dst, src) in result.iter_mut().zip(palette.iter()) {
        *dst = Rgb::from_c(*src);
    }
    result
}

/// Mask that can be used to set which palette indexes were set
/// (Zig: `std.bit_set.Static(256)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PaletteMask {
    masks: [u64; 4],
}

impl PaletteMask {
    /// The bit length of the set.
    pub const BIT_LENGTH: usize = 256;

    /// A set with no bits set.
    pub const EMPTY: PaletteMask = PaletteMask { masks: [0; 4] };

    /// A set with every bit set.
    pub const FULL: PaletteMask = PaletteMask { masks: [u64::MAX; 4] };

    pub fn is_set(&self, idx: usize) -> bool {
        self.masks[idx / 64] & (1u64 << (idx % 64)) != 0
    }

    pub fn set(&mut self, idx: usize) {
        self.masks[idx / 64] |= 1u64 << (idx % 64);
    }

    pub fn unset(&mut self, idx: usize) {
        self.masks[idx / 64] &= !(1u64 << (idx % 64));
    }

    pub fn set_value(&mut self, idx: usize, value: bool) {
        if value {
            self.set(idx)
        } else {
            self.unset(idx)
        }
    }

    pub fn toggle(&mut self, idx: usize) {
        self.masks[idx / 64] ^= 1u64 << (idx % 64);
    }

    pub fn count(&self) -> usize {
        self.masks.iter().map(|m| m.count_ones() as usize).sum()
    }

    /// Iterate over the indexes of the set bits in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..Self::BIT_LENGTH).filter(move |&i| self.is_set(i))
    }
}

/// Generate the 256-color palette from the user's base16 theme colors,
/// terminal background, and terminal foreground.
///
/// Motivation: The default 256-color palette uses fixed, fully-saturated
/// colors that clash with custom base16 themes, have poor readability in
/// dark shades (the first non-black shade jumps to 37% intensity instead
/// of the expected 20%), and exhibit inconsistent perceived brightness
/// across hues of the same shade (e.g., blue appears darker than green).
/// By generating the extended palette from the user's chosen colors,
/// programs can use the richer 256-color range without requiring their
/// own theme configuration, and light/dark switching works automatically.
///
/// The 216-color cube (indices 16–231) is built via trilinear
/// interpolation in CIELAB space over the 8 base colors. The base16
/// palette maps to the 8 corners of a 6×6×6 RGB cube as follows:
///
///   R=0 edge: bg      → base[1] (red)
///   R=5 edge: base[6] → fg
///   G=0 edge: bg/base[6] (via R) → base[2]/base[4] (green/blue via R)
///   G=5 edge: base[1]/fg (via R) → base[3]/base[5] (yellow/magenta via R)
///
/// For each R slice, four corner colors (c0–c3) are interpolated along
/// the R axis, then for each G row two edge colors (c4–c5) are
/// interpolated along G, and finally each B cell is interpolated along B
/// to produce the final color. CIELAB interpolation ensures perceptually
/// uniform brightness transitions across different hues.
///
/// The 24-step grayscale ramp (indices 232–255) is a simple linear
/// interpolation in CIELAB from the background to the foreground,
/// excluding pure black and white (available in the cube at (0,0,0)
/// and (5,5,5)). The interpolation parameter runs from 1/25 to 24/25.
///
/// Fill `skip` with user-defined color indexes to avoid replacing them.
///
/// Reference: https://gist.github.com/jake-stewart/0a8ea46159a7da2c808e5be2177e1783
pub fn generate_256_color(
    base: Palette,
    skip: PaletteMask,
    bg: Rgb,
    fg: Rgb,
    harmonious: bool,
) -> Palette {
    // Convert the background, foreground, and 8 base theme colors into
    // CIELAB space so that all interpolation is perceptually uniform.
    let base8_lab: [Lab; 8] = {
        let mut base8: [Lab; 8] = [
            Lab::from_rgb(bg),
            Lab::from_rgb(base[1]),
            Lab::from_rgb(base[2]),
            Lab::from_rgb(base[3]),
            Lab::from_rgb(base[4]),
            Lab::from_rgb(base[5]),
            Lab::from_rgb(base[6]),
            Lab::from_rgb(fg),
        ];

        // For light themes (where the foreground is darker than the
        // background), the cube's dark-to-light orientation is inverted
        // relative to the base color mapping. When `harmonious` is false,
        // swap bg and fg so the cube still runs from black (16) to
        // white (231).
        let is_light_theme = base8[7].l < base8[0].l;
        let invert = is_light_theme && !harmonious;
        if invert {
            base8.swap(0, 7);
        }

        base8
    };

    // Start from the base palette so indices 0–15 are preserved as-is.
    let mut result = base;

    // Build the 216-color cube (indices 16–231) via trilinear interpolation
    // in CIELAB. The three nested loops correspond to the R, G, and B axes
    // of a 6×6×6 cube. For each R slice, four corner colors (c0–c3) are
    // interpolated along R from the 8 base colors, mapping the cube corners
    // to theme-aware anchors (see doc comment for the mapping). Then for
    // each G row, two edge colors (c4–c5) blend along G, and finally each
    // B cell interpolates along B to produce the final color.
    let mut idx: usize = 16;
    for ri in 0..6 {
        // R-axis corners: blend base colors along the red dimension.
        let tr = ri as f32 / 5.0;
        let c0 = Lab::lerp(tr, base8_lab[0], base8_lab[1]);
        let c1 = Lab::lerp(tr, base8_lab[2], base8_lab[3]);
        let c2 = Lab::lerp(tr, base8_lab[4], base8_lab[5]);
        let c3 = Lab::lerp(tr, base8_lab[6], base8_lab[7]);
        for gi in 0..6 {
            // G-axis edges: blend the R-interpolated corners along green.
            let tg = gi as f32 / 5.0;
            let c4 = Lab::lerp(tg, c0, c1);
            let c5 = Lab::lerp(tg, c2, c3);
            for bi in 0..6 {
                // B-axis: final interpolation along blue, then convert back to RGB.
                if !skip.is_set(idx) {
                    let c6 = Lab::lerp(bi as f32 / 5.0, c4, c5);
                    result[idx] = c6.to_rgb();
                }

                idx += 1;
            }
        }
    }

    // Build the 24-step grayscale ramp (indices 232–255) by linearly
    // interpolating in CIELAB from background to foreground. The parameter
    // runs from 1/25 to 24/25, excluding the endpoints which are already
    // available in the cube at (0,0,0) and (5,5,5).
    for i in 0..24 {
        let t = (i + 1) as f32 / 25.0;
        if !skip.is_set(idx) {
            let c = Lab::lerp(t, base8_lab[0], base8_lab[7]);
            result[idx] = c.to_rgb();
        }
        idx += 1;
    }

    result
}

/// A palette that can have its colors changed and reset. Purposely built
/// for terminal color operations.
#[derive(Debug, Clone)]
pub struct DynamicPalette {
    /// The current palette including any user modifications.
    pub current: Palette,

    /// The original/default palette values. `None` means the shared
    /// built-in default palette (see `changeDefault`); otherwise an owned
    /// copy. Sharing the built-in default saves a full palette copy for
    /// every terminal.
    original: Option<Box<Palette>>,

    /// A bitset where each bit represents whether the corresponding
    /// palette index has been modified from its default value.
    pub mask: PaletteMask,
}

impl Default for DynamicPalette {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl DynamicPalette {
    /// A dynamic palette using the built-in default palette. This owns
    /// no memory.
    pub const DEFAULT: DynamicPalette = DynamicPalette {
        current: default_palette(),
        original: None,
        mask: PaletteMask::EMPTY,
    };

    /// Initialize a dynamic palette with a default palette. No memory is
    /// allocated if `def` is the built-in default palette.
    pub fn init(def: Palette) -> DynamicPalette {
        let mut result = DynamicPalette::DEFAULT;
        result.change_default(def);
        result
    }

    /// The original/default palette values.
    pub fn original(&self) -> &Palette {
        match &self.original {
            Some(owned) => owned,
            None => &DEFAULT,
        }
    }

    /// Set a custom color at the given palette index.
    pub fn set(&mut self, idx: u8, color: Rgb) {
        self.current[idx as usize] = color;
        self.mask.set(idx as usize);
    }

    /// Reset the color at the given palette index to its original value.
    pub fn reset(&mut self, idx: u8) {
        self.current[idx as usize] = self.original()[idx as usize];
        self.mask.unset(idx as usize);
    }

    /// Reset all colors to their original values.
    pub fn reset_all(&mut self) {
        self.current = *self.original();
        self.mask = PaletteMask::EMPTY;
    }

    /// Change the default palette, but preserve the changed values.
    ///
    /// The built-in default palette is shared rather than copied, so
    /// changing back to it releases any owned copy (see `reset_default`).
    /// Any other default is copied into memory owned by this palette,
    /// allocated on the first change and reused after that.
    pub fn change_default(&mut self, def: Palette) {
        if def == DEFAULT {
            return self.reset_default();
        }

        match &mut self.original {
            Some(owned) => **owned = def,
            None => self.original = Some(Box::new(def)),
        }
        self.apply_default(def);
    }

    /// Change the default palette back to the built-in default palette,
    /// preserving the changed values. This releases any owned copy and
    /// never allocates.
    pub fn reset_default(&mut self) {
        self.original = None;
        self.apply_default(DEFAULT);
    }

    /// Rebuild the current palette from a new default, preserving the
    /// changed values.
    fn apply_default(&mut self, def: Palette) {
        // Fast path, the palette is usually not changed.
        if self.mask.count() == 0 {
            self.current = def;
            return;
        }

        // There are usually less set than unset, so iterate over the changed
        // values and override them.
        let mut current = def;
        for idx in self.mask.iter() {
            current[idx] = self.current[idx];
        }
        self.current = current;
    }
}

/// RGB value that can be changed and reset. This can also be totally unset
/// in every way, in which case the caller can determine their own ultimate
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DynamicRgb {
    pub r#override: Option<Rgb>,
    pub default: Option<Rgb>,
}

impl DynamicRgb {
    pub const UNSET: DynamicRgb = DynamicRgb {
        r#override: None,
        default: None,
    };

    pub fn init(def: Rgb) -> DynamicRgb {
        DynamicRgb {
            r#override: None,
            default: Some(def),
        }
    }

    pub fn get(&self) -> Option<Rgb> {
        self.r#override.or(self.default)
    }

    pub fn set(&mut self, color: Rgb) {
        self.r#override = Some(color);
    }

    pub fn reset(&mut self) {
        self.r#override = None;
    }
}

/// Color names in the standard 8 or 16 color palette.
///
/// This is a non-exhaustive enum in Zig: the remaining values are valid
/// unnamed values in the 256 color palette, so it is a newtype here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(pub u8);

/// Error for [`Name::default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    NoDefaultValue,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NoDefaultValue")
    }
}

impl std::error::Error for NameError {}

impl Name {
    pub const BLACK: Name = Name(0);
    pub const RED: Name = Name(1);
    pub const GREEN: Name = Name(2);
    pub const YELLOW: Name = Name(3);
    pub const BLUE: Name = Name(4);
    pub const MAGENTA: Name = Name(5);
    pub const CYAN: Name = Name(6);
    pub const WHITE: Name = Name(7);

    pub const BRIGHT_BLACK: Name = Name(8);
    pub const BRIGHT_RED: Name = Name(9);
    pub const BRIGHT_GREEN: Name = Name(10);
    pub const BRIGHT_YELLOW: Name = Name(11);
    pub const BRIGHT_BLUE: Name = Name(12);
    pub const BRIGHT_MAGENTA: Name = Name(13);
    pub const BRIGHT_CYAN: Name = Name(14);
    pub const BRIGHT_WHITE: Name = Name(15);

    pub fn cval(self) -> NameC {
        self.0
    }

    /// Default colors for tagged values.
    pub fn default(self) -> Result<Rgb, NameError> {
        self.default_const().ok_or(NameError::NoDefaultValue)
    }

    const fn default_const(self) -> Option<Rgb> {
        Some(match self.0 {
            0 => Rgb { r: 0x1D, g: 0x1F, b: 0x21 },
            1 => Rgb { r: 0xCC, g: 0x66, b: 0x66 },
            2 => Rgb { r: 0xB5, g: 0xBD, b: 0x68 },
            3 => Rgb { r: 0xF0, g: 0xC6, b: 0x74 },
            4 => Rgb { r: 0x81, g: 0xA2, b: 0xBE },
            5 => Rgb { r: 0xB2, g: 0x94, b: 0xBB },
            6 => Rgb { r: 0x8A, g: 0xBE, b: 0xB7 },
            7 => Rgb { r: 0xC5, g: 0xC8, b: 0xC6 },

            8 => Rgb { r: 0x66, g: 0x66, b: 0x66 },
            9 => Rgb { r: 0xD5, g: 0x4E, b: 0x53 },
            10 => Rgb { r: 0xB9, g: 0xCA, b: 0x4A },
            11 => Rgb { r: 0xE7, g: 0xC5, b: 0x47 },
            12 => Rgb { r: 0x7A, g: 0xA6, b: 0xDA },
            13 => Rgb { r: 0xC3, g: 0x97, b: 0xD8 },
            14 => Rgb { r: 0x70, g: 0xC0, b: 0xB1 },
            15 => Rgb { r: 0xEA, g: 0xEA, b: 0xEA },

            _ => return None,
        })
    }
}

/// C representation of [`Name`].
pub type NameC = u8;

/// The "special colors" as denoted by xterm. These can be set via
/// OSC 5 or via OSC 4 by adding the palette length to it.
///
/// https://invisible-island.net/xterm/ctlseqs/ctlseqs.html
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Special {
    Bold = 0,
    Underline = 1,
    Blink = 2,
    Reverse = 3,
    Italic = 4,
}

impl Special {
    /// Number of fields in the enum.
    pub const COUNT: usize = 5;

    /// Zig `std.enums.fromInt(Special, v)`.
    pub fn from_int(v: u64) -> Option<Special> {
        Some(match v {
            0 => Special::Bold,
            1 => Special::Underline,
            2 => Special::Blink,
            3 => Special::Reverse,
            4 => Special::Italic,
            _ => return None,
        })
    }

    pub fn osc4(self) -> u16 {
        // "The special colors can also be set by adding the maximum
        // number of colors (e.g., 88 or 256) to these codes in an
        // OSC 4  control" - xterm ctlseqs
        let max = 256u16;
        self as u16 + max
    }
}

/// The "dynamic colors" as denoted by xterm. These can be set via
/// OSC 10 through 19.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Dynamic {
    Foreground = 10,
    Background = 11,
    Cursor = 12,
    PointerForeground = 13,
    PointerBackground = 14,
    TektronixForeground = 15,
    TektronixBackground = 16,
    HighlightBackground = 17,
    TektronixCursor = 18,
    HighlightForeground = 19,
}

impl Dynamic {
    /// All values in declaration order.
    pub const ALL: [Dynamic; 10] = [
        Dynamic::Foreground,
        Dynamic::Background,
        Dynamic::Cursor,
        Dynamic::PointerForeground,
        Dynamic::PointerBackground,
        Dynamic::TektronixForeground,
        Dynamic::TektronixBackground,
        Dynamic::HighlightBackground,
        Dynamic::TektronixCursor,
        Dynamic::HighlightForeground,
    ];

    /// Zig `std.enums.fromInt(Dynamic, v)`.
    pub fn from_int(v: u64) -> Option<Dynamic> {
        if (10..=19).contains(&v) {
            Some(Self::ALL[(v - 10) as usize])
        } else {
            None
        }
    }

    /// The next dynamic color sequentially. This is required because
    /// specifying colors sequentially without their index will automatically
    /// use the next dynamic color.
    ///
    /// "Each successive parameter changes the next color in the list.  The
    /// value of Ps tells the starting point in the list."
    pub fn next(self) -> Option<Dynamic> {
        Self::from_int(self as u64 + 1)
    }
}

/// RGB
///
/// Zig: `packed struct(u24)` (4 bytes in memory). Here it is a plain
/// 3-byte struct; use [`Rgb::to_bits`]/[`Rgb::from_bits`] for the packed
/// `u24` representation (r in the low bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// C representation of [`Rgb`] (Zig: `RGB.C`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct RgbC {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Error for [`Rgb::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgbParseError {
    InvalidFormat,
}

impl fmt::Display for RgbParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InvalidFormat")
    }
}

impl std::error::Error for RgbParseError {}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// The packed `u24` representation (Zig `@bitCast`).
    pub const fn to_bits(self) -> u32 {
        self.r as u32 | (self.g as u32) << 8 | (self.b as u32) << 16
    }

    /// From the packed `u24` representation.
    pub const fn from_bits(bits: u32) -> Rgb {
        Rgb {
            r: bits as u8,
            g: (bits >> 8) as u8,
            b: (bits >> 16) as u8,
        }
    }

    pub fn from_c(c: RgbC) -> Rgb {
        Rgb { r: c.r, g: c.g, b: c.b }
    }

    pub fn cval(self) -> RgbC {
        RgbC {
            r: self.r,
            g: self.g,
            b: self.b,
        }
    }

    pub fn eql(self, other: Rgb) -> bool {
        self.r == other.r && self.g == other.g && self.b == other.b
    }

    pub fn encode_rgb8<W: std::io::Write + ?Sized>(self, writer: &mut W) -> std::io::Result<()> {
        write!(writer, "rgb:{:02x}/{:02x}/{:02x}", self.r, self.g, self.b)
    }

    pub fn encode_rgb16<W: std::io::Write + ?Sized>(self, writer: &mut W) -> std::io::Result<()> {
        write!(
            writer,
            "rgb:{:04x}/{:04x}/{:04x}",
            self.r as u16 * 257,
            self.g as u16 * 257,
            self.b as u16 * 257,
        )
    }

    /// Calculates the contrast ratio between two colors. The contrast
    /// ration is a value between 1 and 21 where 1 is the lowest contrast
    /// and 21 is the highest contrast.
    ///
    /// https://www.w3.org/TR/WCAG20/#contrast-ratiodef
    pub fn contrast(self, other: Rgb) -> f64 {
        // pair[0] = lighter, pair[1] = darker
        let pair: [f64; 2] = {
            let self_lum = self.luminance();
            let other_lum = other.luminance();
            if self_lum > other_lum {
                [self_lum, other_lum]
            } else {
                [other_lum, self_lum]
            }
        };

        (pair[0] + 0.05) / (pair[1] + 0.05)
    }

    /// Calculates luminance based on the W3C formula. This returns a
    /// normalized value between 0 and 1 where 0 is black and 1 is white.
    ///
    /// https://www.w3.org/TR/WCAG20/#relativeluminancedef
    pub fn luminance(self) -> f64 {
        let r_lum = Self::component_luminance(self.r);
        let g_lum = Self::component_luminance(self.g);
        let b_lum = Self::component_luminance(self.b);
        0.2126 * r_lum + 0.7152 * g_lum + 0.0722 * b_lum
    }

    /// Calculates single-component luminance based on the W3C formula.
    ///
    /// Expects sRGB color space which at the time of writing we don't
    /// generally use but it's a good enough approximation until we fix that.
    /// https://www.w3.org/TR/WCAG20/#relativeluminancedef
    fn component_luminance(c: u8) -> f64 {
        let c_f64 = c as f64;
        let normalized = c_f64 / 255.0;
        if normalized <= 0.03928 {
            return normalized / 12.92;
        }
        ((normalized + 0.055) / 1.055).powf(2.4)
    }

    /// Calculates "perceived luminance" which is better for determining
    /// light vs dark.
    ///
    /// Source: https://www.w3.org/TR/AERT/#color-contrast
    pub fn perceived_luminance(self) -> f64 {
        let r_f64 = self.r as f64;
        let g_f64 = self.g as f64;
        let b_f64 = self.b as f64;
        0.299 * (r_f64 / 255.0) + 0.587 * (g_f64 / 255.0) + 0.114 * (b_f64 / 255.0)
    }

    /// Parse a color from a floating point intensity value.
    ///
    /// The value should be between 0.0 and 1.0, inclusive.
    fn from_intensity(value: &[u8]) -> Result<u8, RgbParseError> {
        let i = fraction::parse(value).ok_or(RgbParseError::InvalidFormat)?;
        Ok((i * u8::MAX as f64) as u8)
    }

    /// Parse a color from a string of hexadecimal digits
    ///
    /// The string can contain 1, 2, 3, or 4 characters and represents the color
    /// value scaled in 4, 8, 12, or 16 bits, respectively.
    fn from_hex(value: &[u8]) -> Result<u8, RgbParseError> {
        if value.is_empty() || value.len() > 4 {
            return Err(RgbParseError::InvalidFormat);
        }

        let color: u16 =
            parse_int::parse_int::<u16>(value, 16).map_err(|_| RgbParseError::InvalidFormat)?;

        let divisor: usize = match value.len() {
            1 => 0xF,
            2 => 0xFF,
            3 => 0xFFF,
            4 => 0xFFFF,
            _ => unreachable!(),
        };

        Ok((color as usize * u8::MAX as usize / divisor) as u8)
    }

    /// Parse a color specification.
    ///
    /// Leading and trailing spaces and tabs are ignored.
    ///
    /// Any of the following forms are accepted:
    ///
    /// 1. rgb:<red>/<green>/<blue>
    ///
    ///    <red>, <green>, <blue> := h | hh | hhh | hhhh
    ///
    ///    where `h` is a single hexadecimal digit.
    ///
    /// 2. rgbi:<red>/<green>/<blue>
    ///
    ///    where <red>, <green>, and <blue> are floating point values between
    ///    0.0 and 1.0 (inclusive).
    ///
    /// 3. #rgb, #rrggbb, rgb, rrggbb, #rrrgggbbb, #rrrrggggbbbb
    ///
    ///    where `r`, `g`, and `b` are hexadecimal digits. The forms with
    ///    a leading # specify a color with 4, 8, 12, and 16 bits of
    ///    precision per color channel. The forms without a leading # are
    ///    accepted for compatibility with Ghostty config/theme color values.
    ///
    /// 4. X11 color names
    pub fn parse(value: &[u8]) -> Result<Rgb, RgbParseError> {
        let input = trim(value, b" \t");
        if input.is_empty() {
            return Err(RgbParseError::InvalidFormat);
        }

        if input[0] == b'#' {
            return match input.len() {
                4 => Ok(Rgb {
                    r: Rgb::from_hex(&input[1..2])?,
                    g: Rgb::from_hex(&input[2..3])?,
                    b: Rgb::from_hex(&input[3..4])?,
                }),
                7 => Ok(Rgb {
                    r: Rgb::from_hex(&input[1..3])?,
                    g: Rgb::from_hex(&input[3..5])?,
                    b: Rgb::from_hex(&input[5..7])?,
                }),
                10 => Ok(Rgb {
                    r: Rgb::from_hex(&input[1..4])?,
                    g: Rgb::from_hex(&input[4..7])?,
                    b: Rgb::from_hex(&input[7..10])?,
                }),
                13 => Ok(Rgb {
                    r: Rgb::from_hex(&input[1..5])?,
                    g: Rgb::from_hex(&input[5..9])?,
                    b: Rgb::from_hex(&input[9..13])?,
                }),
                _ => Err(RgbParseError::InvalidFormat),
            };
        }

        // Check for X11 named colors. We allow whitespace around the edges.
        if let Some(rgb) = x11_color::MAP.get(input) {
            return Ok(rgb);
        }

        match input.len() {
            3 => {
                return Ok(Rgb {
                    r: Rgb::from_hex(&input[0..1])?,
                    g: Rgb::from_hex(&input[1..2])?,
                    b: Rgb::from_hex(&input[2..3])?,
                })
            }
            6 => {
                return Ok(Rgb {
                    r: Rgb::from_hex(&input[0..2])?,
                    g: Rgb::from_hex(&input[2..4])?,
                    b: Rgb::from_hex(&input[4..6])?,
                })
            }
            _ => {}
        }

        if input.len() < b"rgb:a/a/a".len() || &input[0..3] != b"rgb" {
            return Err(RgbParseError::InvalidFormat);
        }

        let mut i: usize = 3;

        let use_intensity = if input[i] == b'i' {
            i += 1;
            true
        } else {
            false
        };

        if input[i] != b':' {
            return Err(RgbParseError::InvalidFormat);
        }

        i += 1;

        let r = {
            let end = index_of_scalar_pos(input, i, b'/').ok_or(RgbParseError::InvalidFormat)?;
            let slice = &input[i..end];
            i += slice.len() + 1;
            if use_intensity {
                Rgb::from_intensity(slice)?
            } else {
                Rgb::from_hex(slice)?
            }
        };

        let g = {
            let end = index_of_scalar_pos(input, i, b'/').ok_or(RgbParseError::InvalidFormat)?;
            let slice = &input[i..end];
            i += slice.len() + 1;
            if use_intensity {
                Rgb::from_intensity(slice)?
            } else {
                Rgb::from_hex(slice)?
            }
        };

        let b = if use_intensity {
            Rgb::from_intensity(&input[i..])?
        } else {
            Rgb::from_hex(&input[i..])?
        };

        Ok(Rgb { r, g, b })
    }
}

/// Zig `std.mem.indexOfScalarPos`.
pub(crate) fn index_of_scalar_pos(s: &[u8], start: usize, c: u8) -> Option<usize> {
    if start >= s.len() {
        return None;
    }
    s[start..].iter().position(|&x| x == c).map(|p| p + start)
}

/// LAB color space
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lab {
    l: f32,
    a: f32,
    b: f32,
}

impl Lab {
    /// RGB to LAB
    fn from_rgb(rgb: Rgb) -> Lab {
        // Step 1: Normalize sRGB channels from [0, 255] to [0.0, 1.0].
        let mut r: f32 = rgb.r as f32 / 255.0;
        let mut g: f32 = rgb.g as f32 / 255.0;
        let mut b: f32 = rgb.b as f32 / 255.0;

        // Step 2: Apply the inverse sRGB companding (gamma correction) to
        // convert from sRGB to linear RGB. The sRGB transfer function has
        // two segments: a linear portion for small values and a power curve
        // for the rest.
        r = if r > 0.04045 { ((r + 0.055) / 1.055).powf(2.4) } else { r / 12.92 };
        g = if g > 0.04045 { ((g + 0.055) / 1.055).powf(2.4) } else { g / 12.92 };
        b = if b > 0.04045 { ((b + 0.055) / 1.055).powf(2.4) } else { b / 12.92 };

        // Step 3: Convert linear RGB to CIE XYZ using the sRGB to XYZ
        // transformation matrix (D65 illuminant). The X and Z values are
        // normalized by the D65 white point reference values (Xn=0.95047,
        // Zn=1.08883; Yn=1.0 is implicit).
        let mut x = (r * 0.4124564 + g * 0.3575761 + b * 0.1804375) / 0.95047;
        let mut y = r * 0.2126729 + g * 0.7151522 + b * 0.0721750;
        let mut z = (r * 0.0193339 + g * 0.1191920 + b * 0.9503041) / 1.08883;

        // Step 4: Apply the CIE f(t) nonlinear transform to each XYZ
        // component. Above the threshold (epsilon ≈ 0.008856) the cube
        // root is used; below it, a linear approximation avoids numerical
        // instability near zero.
        x = if x > 0.008856 { x.cbrt() } else { 7.787 * x + 16.0 / 116.0 };
        y = if y > 0.008856 { y.cbrt() } else { 7.787 * y + 16.0 / 116.0 };
        z = if z > 0.008856 { z.cbrt() } else { 7.787 * z + 16.0 / 116.0 };

        // Step 5: Compute the final CIELAB values from the transformed XYZ.
        // L* is lightness (0–100), a* is green–red, b* is blue–yellow.
        Lab {
            l: 116.0 * y - 16.0,
            a: 500.0 * (x - y),
            b: 200.0 * (y - z),
        }
    }

    /// LAB to RGB
    fn to_rgb(self) -> Rgb {
        // Step 1: Recover the intermediate f(Y), f(X), f(Z) values from
        // L*a*b* by inverting the CIELAB formulas.
        let y = (self.l + 16.0) / 116.0;
        let x = self.a / 500.0 + y;
        let z = y - self.b / 200.0;

        // Step 2: Apply the inverse CIE f(t) transform to get back to
        // XYZ. Above epsilon (≈0.008856) the cube is used; below it the
        // linear segment is inverted. Results are then scaled by the D65
        // white point reference values (Xn=0.95047, Zn=1.08883; Yn=1.0).
        let x3 = x * x * x;
        let y3 = y * y * y;
        let z3 = z * z * z;
        let xf = (if x3 > 0.008856 { x3 } else { (x - 16.0 / 116.0) / 7.787 }) * 0.95047;
        let yf = if y3 > 0.008856 { y3 } else { (y - 16.0 / 116.0) / 7.787 };
        let zf = (if z3 > 0.008856 { z3 } else { (z - 16.0 / 116.0) / 7.787 }) * 1.08883;

        // Step 3: Convert CIE XYZ back to linear RGB using the XYZ to sRGB
        // matrix (inverse of the sRGB to XYZ matrix, D65 illuminant).
        let mut r = xf * 3.2404542 - yf * 1.5371385 - zf * 0.4985314;
        let mut g = -xf * 0.9692660 + yf * 1.8760108 + zf * 0.0415560;
        let mut b = xf * 0.0556434 - yf * 0.2040259 + zf * 1.0572252;

        // Step 4: Apply sRGB companding (gamma correction) to convert from
        // linear RGB back to sRGB. This is the forward sRGB transfer
        // function with the same two-segment split as the inverse.
        r = if r > 0.0031308 { 1.055 * r.powf(1.0 / 2.4) - 0.055 } else { 12.92 * r };
        g = if g > 0.0031308 { 1.055 * g.powf(1.0 / 2.4) - 0.055 } else { 12.92 * g };
        b = if b > 0.0031308 { 1.055 * b.powf(1.0 / 2.4) - 0.055 } else { 12.92 * b };

        // Step 5: Clamp to [0.0, 1.0], scale to [0, 255], and round to
        // the nearest integer to produce the final 8-bit sRGB values.
        Rgb {
            r: (r.max(0.0).min(1.0) * 255.0 + 0.5) as u8,
            g: (g.max(0.0).min(1.0) * 255.0 + 0.5) as u8,
            b: (b.max(0.0).min(1.0) * 255.0 + 0.5) as u8,
        }
    }

    /// Linearly interpolate between two LAB colors component-wise.
    /// `t` is the interpolation factor in [0, 1]: t=0 returns `a`,
    /// t=1 returns `b`, and values in between blend proportionally.
    fn lerp(t: f32, a: Lab, b: Lab) -> Lab {
        Lab {
            l: a.l + t * (b.l - a.l),
            a: a.a + t * (b.a - a.a),
            b: a.b + t * (b.b - a.b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    fn approx(expected: f32, actual: f32, eps: f32) {
        assert!(
            (expected - actual).abs() <= eps,
            "expected {expected} got {actual}"
        );
    }

    #[test]
    fn test_parse_palette_entry() {
        {
            let entry = parse_palette_entry(b"0=#AABBCC").unwrap();
            assert_eq!(0u8, entry.index);
            assert_eq!(rgb(170, 187, 204), entry.color);
        }
        {
            let entry = parse_palette_entry(b"0b1=#014589").unwrap();
            assert_eq!(1u8, entry.index);
            assert_eq!(rgb(1, 69, 137), entry.color);
        }
        {
            let entry = parse_palette_entry(b"0o7=#234567").unwrap();
            assert_eq!(7u8, entry.index);
            assert_eq!(rgb(35, 69, 103), entry.color);
        }
        {
            let entry = parse_palette_entry(b"0xF=#ABCDEF").unwrap();
            assert_eq!(15u8, entry.index);
            assert_eq!(rgb(171, 205, 239), entry.color);
        }
        {
            let entry = parse_palette_entry(b"0 =  #AABBCC").unwrap();
            assert_eq!(0u8, entry.index);
            assert_eq!(rgb(170, 187, 204), entry.color);
        }
        {
            let entry = parse_palette_entry(b" 1= #DDEEFF    ").unwrap();
            assert_eq!(1u8, entry.index);
            assert_eq!(rgb(221, 238, 255), entry.color);
        }
        {
            let entry = parse_palette_entry(b"  2  =  #123456 ").unwrap();
            assert_eq!(2u8, entry.index);
            assert_eq!(rgb(18, 52, 86), entry.color);
        }
        {
            let entry = parse_palette_entry(b"1=black").unwrap();
            assert_eq!(1u8, entry.index);
            assert_eq!(rgb(0, 0, 0), entry.color);
        }

        assert_eq!(Err(PaletteEntryError::InvalidFormat), parse_palette_entry(b" "));
        assert_eq!(Err(PaletteEntryError::InvalidFormat), parse_palette_entry(b"a"));
        assert_eq!(Err(PaletteEntryError::Overflow), parse_palette_entry(b"256=#AABBCC"));
        assert_eq!(Err(PaletteEntryError::InvalidFormat), parse_palette_entry(b"1=notacolor"));
    }

    #[test]
    fn special_osc4() {
        assert_eq!(256, Special::Bold.osc4());
        assert_eq!(257, Special::Underline.osc4());
        assert_eq!(258, Special::Blink.osc4());
        assert_eq!(259, Special::Reverse.osc4());
        assert_eq!(260, Special::Italic.osc4());
    }

    #[test]
    fn dynamic_next() {
        assert_eq!(Some(Dynamic::Background), Dynamic::Foreground.next());
        assert_eq!(Some(Dynamic::Cursor), Dynamic::Background.next());
        assert_eq!(Some(Dynamic::PointerForeground), Dynamic::Cursor.next());
        assert_eq!(Some(Dynamic::PointerBackground), Dynamic::PointerForeground.next());
        assert_eq!(Some(Dynamic::TektronixForeground), Dynamic::PointerBackground.next());
        assert_eq!(Some(Dynamic::TektronixBackground), Dynamic::TektronixForeground.next());
        assert_eq!(Some(Dynamic::HighlightBackground), Dynamic::TektronixBackground.next());
        assert_eq!(Some(Dynamic::TektronixCursor), Dynamic::HighlightBackground.next());
        assert_eq!(Some(Dynamic::HighlightForeground), Dynamic::TektronixCursor.next());
        assert_eq!(None, Dynamic::HighlightForeground.next());
    }

    #[test]
    fn palette_default() {
        // Safety check
        for i in 0..16u8 {
            assert_eq!(Name(i).default().unwrap(), DEFAULT[i as usize]);
        }
    }

    #[test]
    fn rgb_parse() {
        assert_eq!(rgb(255, 0, 0), Rgb::parse(b"rgbi:1.0/0/0").unwrap());
        assert_eq!(rgb(127, 160, 0), Rgb::parse(b"rgb:7f/a0a0/0").unwrap());
        assert_eq!(rgb(255, 255, 255), Rgb::parse(b"rgb:f/ff/fff").unwrap());
        assert_eq!(rgb(255, 255, 255), Rgb::parse(b"#ffffff").unwrap());
        assert_eq!(rgb(255, 255, 255), Rgb::parse(b"#fff").unwrap());
        assert_eq!(rgb(255, 255, 255), Rgb::parse(b"#fffffffff").unwrap());
        assert_eq!(rgb(255, 255, 255), Rgb::parse(b"#ffffffffffff").unwrap());
        assert_eq!(rgb(255, 0, 16), Rgb::parse(b"#ff0010").unwrap());
        assert_eq!(rgb(10, 11, 12), Rgb::parse(b"0A0B0C").unwrap());
        assert_eq!(rgb(255, 255, 255), Rgb::parse(b"FFFFFF").unwrap());
        assert_eq!(rgb(255, 255, 255), Rgb::parse(b"FFF").unwrap());
        assert_eq!(rgb(51, 68, 85), Rgb::parse(b"#345").unwrap());
        assert_eq!(rgb(170, 187, 204), Rgb::parse(b" #AABBCC   ").unwrap());

        assert_eq!(rgb(0, 0, 0), Rgb::parse(b"black").unwrap());
        assert_eq!(rgb(255, 0, 0), Rgb::parse(b"red").unwrap());
        assert_eq!(rgb(0, 255, 0), Rgb::parse(b"green").unwrap());
        assert_eq!(rgb(0, 0, 255), Rgb::parse(b"blue").unwrap());
        assert_eq!(rgb(255, 255, 255), Rgb::parse(b"white").unwrap());

        assert_eq!(rgb(124, 252, 0), Rgb::parse(b"LawnGreen").unwrap());
        assert_eq!(rgb(0, 250, 154), Rgb::parse(b"medium spring green").unwrap());
        assert_eq!(rgb(34, 139, 34), Rgb::parse(b" Forest Green ").unwrap());
        assert_eq!(rgb(34, 139, 34), Rgb::parse(b"\tForestGreen\t").unwrap());

        // Invalid format
        let invalid: &[&[u8]] = &[
            b"",
            b"  ",
            b"rgb;",
            b"rgb:",
            b":a/a/a",
            b"a/a/a",
            b"rgb:a/a/a/",
            b"rgb:00000///",
            b"rgb:000/",
            b"rgbi:a/a/a",
            b"rgb:0.5/0.0/1.0",
            b"rgb:not/hex/zz",
            b"#",
            b"#ff",
            b"#ffff",
            b"#fffff",
            b"#gggggg",
            b"#12345",
            b"12345",
            b"nosuchcolor",
        ];
        for v in invalid {
            assert_eq!(Err(RgbParseError::InvalidFormat), Rgb::parse(v), "{:?}", v);
        }
    }

    #[test]
    fn rgb_parse_rejects_digit_separators() {
        assert_eq!(Err(RgbParseError::InvalidFormat), Rgb::parse(b"rgb:f_f/0/0"));
        assert_eq!(Err(RgbParseError::InvalidFormat), Rgb::parse(b"#f_f000000"));
    }

    #[test]
    fn rgb_encode() {
        let c = rgb(0x01, 0x23, 0xff);

        let mut buf: Vec<u8> = Vec::new();
        c.encode_rgb8(&mut buf).unwrap();
        assert_eq!(b"rgb:01/23/ff", &buf[..]);

        buf.clear();
        c.encode_rgb16(&mut buf).unwrap();
        assert_eq!(b"rgb:0101/2323/ffff", &buf[..]);
    }

    #[test]
    fn dynamic_palette_init() {
        let p = DynamicPalette::DEFAULT;
        assert_eq!(DEFAULT, p.current);
        assert_eq!(DEFAULT, *p.original());
        assert!(std::ptr::eq(&DEFAULT, p.original()));
        assert_eq!(0usize, p.mask.count());
    }

    #[test]
    fn dynamic_palette_set() {
        let mut p = DynamicPalette::DEFAULT;
        let new_color = rgb(255, 0, 0);

        p.set(0, new_color);
        assert_eq!(new_color, p.current[0]);
        assert!(p.mask.is_set(0));
        assert_eq!(1usize, p.mask.count());

        assert_eq!(DEFAULT[0], p.original()[0]);
    }

    #[test]
    fn dynamic_palette_reset() {
        let mut p = DynamicPalette::DEFAULT;
        let new_color = rgb(255, 0, 0);

        p.set(0, new_color);
        assert!(p.mask.is_set(0));

        p.reset(0);
        assert_eq!(DEFAULT[0], p.current[0]);
        assert!(!p.mask.is_set(0));
        assert_eq!(0usize, p.mask.count());
    }

    #[test]
    fn dynamic_palette_reset_all() {
        let mut p = DynamicPalette::DEFAULT;
        let new_color = rgb(255, 0, 0);

        p.set(0, new_color);
        p.set(5, new_color);
        p.set(10, new_color);
        assert_eq!(3usize, p.mask.count());

        p.reset_all();
        assert_eq!(DEFAULT, p.current);
        assert_eq!(DEFAULT, *p.original());
        assert_eq!(0usize, p.mask.count());
    }

    #[test]
    fn dynamic_palette_change_default_with_no_changes() {
        let mut p = DynamicPalette::DEFAULT;
        let mut new_palette = DEFAULT;
        new_palette[0] = rgb(100, 100, 100);

        p.change_default(new_palette);
        assert_eq!(new_palette, *p.original());
        assert_eq!(new_palette, p.current);
        assert_eq!(0usize, p.mask.count());
    }

    #[test]
    fn dynamic_palette_change_default_shares_the_built_in_default() {
        let mut new_palette = DEFAULT;
        new_palette[0] = rgb(100, 100, 100);

        // A custom default is an owned copy.
        let mut p = DynamicPalette::init(new_palette);
        assert!(!std::ptr::eq(p.original(), &DEFAULT));
        assert_eq!(new_palette, *p.original());

        // Changing the custom default reuses the copy.
        let owned = p.original() as *const Palette;
        new_palette[1] = rgb(101, 101, 101);
        p.change_default(new_palette);
        assert!(std::ptr::eq(owned, p.original()));
        assert_eq!(new_palette, *p.original());

        // Changing back to the built-in default releases the copy and
        // shares it again.
        p.change_default(DEFAULT);
        assert!(std::ptr::eq(&DEFAULT, p.original()));
        assert_eq!(DEFAULT, p.current);

        // reset_default does the same directly, preserving changed values,
        // and never allocates.
        p.change_default(new_palette);
        p.set(2, rgb(1, 2, 3));
        p.reset_default();
        assert!(std::ptr::eq(&DEFAULT, p.original()));
        assert_eq!(DEFAULT[0], p.current[0]);
        assert_eq!(rgb(1, 2, 3), p.current[2]);
        assert!(p.mask.is_set(2));

        // The built-in default never allocates.
        let q = DynamicPalette::init(DEFAULT);
        assert!(std::ptr::eq(&DEFAULT, q.original()));
        assert!(q.original.is_none());
    }

    #[test]
    fn dynamic_palette_change_default_preserves_changes() {
        let mut p = DynamicPalette::DEFAULT;
        let custom_color = rgb(255, 0, 0);

        p.set(5, custom_color);
        assert!(p.mask.is_set(5));

        let mut new_palette = DEFAULT;
        new_palette[0] = rgb(100, 100, 100);
        new_palette[5] = rgb(50, 50, 50);

        p.change_default(new_palette);

        assert_eq!(new_palette, *p.original());
        assert_eq!(new_palette[0], p.current[0]);
        assert_eq!(custom_color, p.current[5]);
        assert!(p.mask.is_set(5));
        assert_eq!(1usize, p.mask.count());
    }

    #[test]
    fn dynamic_palette_change_default_with_multiple_changes() {
        let mut p = DynamicPalette::DEFAULT;
        let red = rgb(255, 0, 0);
        let green = rgb(0, 255, 0);
        let blue = rgb(0, 0, 255);

        p.set(1, red);
        p.set(2, green);
        p.set(3, blue);

        let mut new_palette = DEFAULT;
        new_palette[0] = rgb(50, 50, 50);
        new_palette[1] = rgb(60, 60, 60);

        p.change_default(new_palette);

        assert_eq!(new_palette[0], p.current[0]);
        assert_eq!(red, p.current[1]);
        assert_eq!(green, p.current[2]);
        assert_eq!(blue, p.current[3]);
        assert_eq!(3usize, p.mask.count());
    }

    #[test]
    fn lab_from_rgb() {
        let epsilon = 0.5;

        // White (255, 255, 255) -> L*=100, a*=0, b*=0
        let white = Lab::from_rgb(rgb(255, 255, 255));
        approx(100.0, white.l, epsilon);
        approx(0.0, white.a, epsilon);
        approx(0.0, white.b, epsilon);

        // Black (0, 0, 0) -> L*=0, a*=0, b*=0
        let black = Lab::from_rgb(rgb(0, 0, 0));
        approx(0.0, black.l, epsilon);
        approx(0.0, black.a, epsilon);
        approx(0.0, black.b, epsilon);

        // Pure red (255, 0, 0) -> L*≈53.23, a*≈80.11, b*≈67.22
        let red = Lab::from_rgb(rgb(255, 0, 0));
        approx(53.23, red.l, epsilon);
        approx(80.11, red.a, epsilon);
        approx(67.22, red.b, epsilon);

        // Pure green (0, 128, 0) -> L*≈46.23, a*≈-51.70, b*≈49.90
        let green = Lab::from_rgb(rgb(0, 128, 0));
        approx(46.23, green.l, epsilon);
        approx(-51.70, green.a, epsilon);
        approx(49.90, green.b, epsilon);

        // Pure blue (0, 0, 255) -> L*≈32.30, a*≈79.20, b*≈-107.86
        let blue = Lab::from_rgb(rgb(0, 0, 255));
        approx(32.30, blue.l, epsilon);
        approx(79.20, blue.a, epsilon);
        approx(-107.86, blue.b, epsilon);
    }

    #[test]
    fn generate_256_color_base16_preserved() {
        let bg = rgb(0, 0, 0);
        let fg = rgb(255, 255, 255);
        let palette = generate_256_color(DEFAULT, PaletteMask::EMPTY, bg, fg, false);

        // The first 16 colors (base16) must remain unchanged.
        for i in 0..16 {
            assert_eq!(DEFAULT[i], palette[i]);
        }
    }

    #[test]
    fn generate_256_color_cube_corners_match_base_colors() {
        let bg = rgb(0, 0, 0);
        let fg = rgb(255, 255, 255);
        let palette = generate_256_color(DEFAULT, PaletteMask::EMPTY, bg, fg, false);

        // Index 16 is cube (0,0,0) which should equal bg.
        assert_eq!(bg, palette[16]);

        // Index 231 is cube (5,5,5) which should equal fg.
        assert_eq!(fg, palette[231]);
    }

    #[test]
    fn generate_256_color_cube_corners_black_white_with_harmonious_false() {
        let black = rgb(0, 0, 0);
        let white = rgb(255, 255, 255);

        // Dark theme: bg=black, fg=white.
        let dark = generate_256_color(DEFAULT, PaletteMask::EMPTY, black, white, false);
        assert_eq!(black, dark[16]);
        assert_eq!(white, dark[231]);

        // Light theme: bg=white, fg=black. The bg/red swap ensures
        // the cube still runs from black (16) to white (231).
        let light = generate_256_color(DEFAULT, PaletteMask::EMPTY, white, black, false);
        assert_eq!(black, light[16]);
        assert_eq!(white, light[231]);
    }

    #[test]
    fn generate_256_color_light_theme_cube_corners_with_harmonious_true() {
        let white = rgb(255, 255, 255);
        let black = rgb(0, 0, 0);

        // harmonious=true skips the bg/fg swap, so the cube preserves the
        // original orientation: (0,0,0)=bg=white, (5,5,5)=fg=black.
        let palette = generate_256_color(DEFAULT, PaletteMask::EMPTY, white, black, true);
        assert_eq!(white, palette[16]);
        assert_eq!(black, palette[231]);
    }

    #[test]
    fn generate_256_color_grayscale_ramp_monotonic_luminance() {
        let bg = rgb(0, 0, 0);
        let fg = rgb(255, 255, 255);
        let palette = generate_256_color(DEFAULT, PaletteMask::EMPTY, bg, fg, false);

        // The grayscale ramp (232–255) should have monotonically increasing
        // luminance from near-black to near-white.
        let mut prev_lum: f64 = 0.0;
        for i in 232..256 {
            let lum = palette[i].luminance();
            assert!(lum >= prev_lum);
            prev_lum = lum;
        }
    }

    #[test]
    fn generate_256_color_skip_mask_preserves_original_colors() {
        let bg = rgb(0, 0, 0);
        let fg = rgb(255, 255, 255);

        // Mark a few indices as skipped; they should keep their base value.
        let mut skip = PaletteMask::EMPTY;
        skip.set(20);
        skip.set(100);
        skip.set(240);

        let palette = generate_256_color(DEFAULT, skip, bg, fg, false);
        assert_eq!(DEFAULT[20], palette[20]);
        assert_eq!(DEFAULT[100], palette[100]);
        assert_eq!(DEFAULT[240], palette[240]);

        // A non-skipped index in the cube should differ from the default.
        assert!(!palette[21].eql(DEFAULT[21]));
    }

    #[test]
    fn generate_256_color_dark_theme_harmonious_has_no_effect() {
        // For a dark theme (fg lighter than bg), harmonious should not change
        // the output because the inversion is only relevant for light themes.
        let bg = rgb(0, 0, 0);
        let fg = rgb(255, 255, 255);
        let normal = generate_256_color(DEFAULT, PaletteMask::EMPTY, bg, fg, false);
        let harmonious = generate_256_color(DEFAULT, PaletteMask::EMPTY, bg, fg, true);

        for i in 16..256 {
            assert_eq!(normal[i], harmonious[i]);
        }
    }

    #[test]
    fn generate_256_color_light_theme_harmonious_skips_inversion() {
        // For a light theme (fg darker than bg), harmonious=true skips the
        // bg/red swap, producing different cube colors than harmonious=false.
        let bg = rgb(255, 255, 255);
        let fg = rgb(0, 0, 0);
        let inverted = generate_256_color(DEFAULT, PaletteMask::EMPTY, bg, fg, false);
        let harmonious = generate_256_color(DEFAULT, PaletteMask::EMPTY, bg, fg, true);

        // Cube origin (0,0,0) at index 16: without harmonious, bg and red are
        // swapped so it becomes the red base; with harmonious it stays as bg.
        assert_eq!(bg, harmonious[16]);
        assert!(!inverted[16].eql(bg));

        // At least some cube colors should differ between the two modes.
        let mut differ: usize = 0;
        for i in 16..232 {
            if !inverted[i].eql(harmonious[i]) {
                differ += 1;
            }
        }
        assert!(differ > 0);
    }

    #[test]
    fn generate_256_color_light_theme_harmonious_grayscale_ramp() {
        let bg = rgb(255, 255, 255);
        let fg = rgb(0, 0, 0);

        // harmonious=false swaps bg/fg, so the ramp runs black→white (increasing).
        {
            let palette = generate_256_color(DEFAULT, PaletteMask::EMPTY, bg, fg, false);
            let mut prev_lum: f64 = 0.0;
            for i in 232..256 {
                let lum = palette[i].luminance();
                assert!(lum >= prev_lum);
                prev_lum = lum;
            }
        }

        // harmonious=true keeps original order, so the ramp runs white→black (decreasing).
        {
            let palette = generate_256_color(DEFAULT, PaletteMask::EMPTY, bg, fg, true);
            let mut prev_lum: f64 = 1.0;
            for i in 232..256 {
                let lum = palette[i].luminance();
                assert!(lum <= prev_lum);
                prev_lum = lum;
            }
        }
    }

    #[test]
    fn lab_to_rgb() {
        // Round-trip: RGB -> LAB -> RGB should recover the original values.
        let cases = [
            rgb(255, 255, 255),
            rgb(0, 0, 0),
            rgb(255, 0, 0),
            rgb(0, 128, 0),
            rgb(0, 0, 255),
            rgb(128, 128, 128),
            rgb(64, 224, 208),
        ];

        for expected in cases {
            let lab = Lab::from_rgb(expected);
            let actual = lab.to_rgb();
            assert_eq!(expected.r, actual.r);
            assert_eq!(expected.g, actual.g);
            assert_eq!(expected.b, actual.b);
        }
    }

    #[test]
    fn test_palette_cval_slice() {
        // Every length from empty through a full palette, so that the
        // edges are all exercised.
        let mut src: Palette = [Rgb::default(); 256];
        for (i, c) in src.iter_mut().enumerate() {
            *c = Rgb {
                r: (i % 256) as u8,
                g: ((i * 7 + 1) % 256) as u8,
                b: ((i * 13 + 2) % 256) as u8,
            };
        }

        let poison = RgbC { r: 0xAA, g: 0xAA, b: 0xAA };
        let mut dst: PaletteC;
        for len in 0..src.len() + 1 {
            // Poison the destination so unwritten bytes are detected.
            dst = [poison; 256];

            palette_cval_slice(&src[..len], &mut dst[..len]);
            for (c, out) in src[..len].iter().zip(dst[..len].iter()) {
                assert_eq!(c.r, out.r);
                assert_eq!(c.g, out.g);
                assert_eq!(c.b, out.b);
            }

            // Nothing beyond the requested length may be written.
            for out in &dst[len..] {
                assert_eq!(poison, *out);
            }
        }

        // Round trip through the C palette.
        assert_eq!(src, palette_zval(&palette_cval(&src)));
    }

    #[test]
    fn rgb_bits_roundtrip() {
        let c = rgb(1, 2, 3);
        assert_eq!(0x030201, c.to_bits());
        assert_eq!(c, Rgb::from_bits(c.to_bits()));
    }
}
