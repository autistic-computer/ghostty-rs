//! Port of `src/terminal/sgr.zig`.
//!
//! STATUS: ported
//!
//! SGR (Select Graphic Rendition) attrinvbute parsing and types.
//!
//! Notes on the port:
//! - Zig tag names that start with a digit (`8_bg`, `256_fg`, ...) use the
//!   C ABI field renames as Rust variant names (`Bg8`, `Fg256`, ...);
//!   [`Tag::as_str`] returns the original spelling.
//! - The C ABI tagged union (`lib.TaggedUnion`) is empty for the Ghostty
//!   application build (`lib.target == .zig`), so [`AttributeC`] and
//!   [`AttributeCValue`] are empty structs, as in Zig. The comptime-only
//!   `Value(tag)` type function has no Rust counterpart.

use crate::terminal::color::{self, Rgb};
use crate::terminal::parser::SepList;

/// Attribute type for SGR
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribute<'a> {
    /// Unset all attributes
    Unset,

    /// Unknown attribute, the raw CSI command parameters are here.
    Unknown(Unknown<'a>),

    /// Bold the text.
    Bold,
    ResetBold,

    /// Italic text.
    Italic,
    ResetItalic,

    /// Faint/dim text.
    /// Note: reset faint is the same SGR code as reset bold
    Faint,

    /// Underline the text
    Underline(Underline),
    UnderlineColor(Rgb),
    UnderlineColor256(u8),
    ResetUnderlineColor,

    // Overline the text
    Overline,
    ResetOverline,

    /// Blink the text
    Blink,
    ResetBlink,

    /// Invert fg/bg colors.
    Inverse,
    ResetInverse,

    /// Invisible
    Invisible,
    ResetInvisible,

    /// Strikethrough the text.
    Strikethrough,
    ResetStrikethrough,

    /// Set foreground color as RGB values.
    DirectColorFg(Rgb),

    /// Set background color as RGB values.
    DirectColorBg(Rgb),

    /// Set the background/foreground as a named color attribute.
    Bg8(color::Name),
    Fg8(color::Name),

    /// Reset the fg/bg to their default values.
    ResetFg,
    ResetBg,

    /// Set the background/foreground as a named bright color attribute.
    BrightBg8(color::Name),
    BrightFg8(color::Name),

    /// Set background color as 256-color palette.
    Bg256(u8),

    /// Set foreground color as 256-color palette.
    Fg256(u8),
}

/// The tag of [`Attribute`] (Zig: `Attribute.Tag`, a `lib.Enum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Tag {
    Unset = 0,
    Unknown = 1,
    Bold = 2,
    ResetBold = 3,
    Italic = 4,
    ResetItalic = 5,
    Faint = 6,
    Underline = 7,
    UnderlineColor = 8,
    UnderlineColor256 = 9,
    ResetUnderlineColor = 10,
    Overline = 11,
    ResetOverline = 12,
    Blink = 13,
    ResetBlink = 14,
    Inverse = 15,
    ResetInverse = 16,
    Invisible = 17,
    ResetInvisible = 18,
    Strikethrough = 19,
    ResetStrikethrough = 20,
    DirectColorFg = 21,
    DirectColorBg = 22,
    Bg8 = 23,
    Fg8 = 24,
    ResetFg = 25,
    ResetBg = 26,
    BrightBg8 = 27,
    BrightFg8 = 28,
    Bg256 = 29,
    Fg256 = 30,
}

impl Tag {
    /// The original Zig tag name.
    pub fn as_str(self) -> &'static str {
        match self {
            Tag::Unset => "unset",
            Tag::Unknown => "unknown",
            Tag::Bold => "bold",
            Tag::ResetBold => "reset_bold",
            Tag::Italic => "italic",
            Tag::ResetItalic => "reset_italic",
            Tag::Faint => "faint",
            Tag::Underline => "underline",
            Tag::UnderlineColor => "underline_color",
            Tag::UnderlineColor256 => "256_underline_color",
            Tag::ResetUnderlineColor => "reset_underline_color",
            Tag::Overline => "overline",
            Tag::ResetOverline => "reset_overline",
            Tag::Blink => "blink",
            Tag::ResetBlink => "reset_blink",
            Tag::Inverse => "inverse",
            Tag::ResetInverse => "reset_inverse",
            Tag::Invisible => "invisible",
            Tag::ResetInvisible => "reset_invisible",
            Tag::Strikethrough => "strikethrough",
            Tag::ResetStrikethrough => "reset_strikethrough",
            Tag::DirectColorFg => "direct_color_fg",
            Tag::DirectColorBg => "direct_color_bg",
            Tag::Bg8 => "8_bg",
            Tag::Fg8 => "8_fg",
            Tag::ResetFg => "reset_fg",
            Tag::ResetBg => "reset_bg",
            Tag::BrightBg8 => "8_bright_bg",
            Tag::BrightFg8 => "8_bright_fg",
            Tag::Bg256 => "256_bg",
            Tag::Fg256 => "256_fg",
        }
    }
}

/// (Zig: `Attribute.Unknown`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unknown<'a> {
    /// Full is the full SGR input.
    pub full: &'a [u16],

    /// Partial is the remaining, where we got hung up.
    pub partial: &'a [u16],
}

/// (Zig: `Attribute.Unknown.C`.)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct UnknownC {
    pub full_ptr: *const u16,
    pub full_len: usize,
    pub partial_ptr: *const u16,
    pub partial_len: usize,
}

impl Unknown<'_> {
    pub fn cval(&self) -> UnknownC {
        UnknownC {
            full_ptr: self.full.as_ptr(),
            full_len: self.full.len(),
            partial_ptr: self.partial.as_ptr(),
            partial_len: self.partial.len(),
        }
    }
}

/// (Zig: `Attribute.Underline`, `enum(u3)`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Underline {
    #[default]
    None = 0,
    Single = 1,
    Double = 2,
    Curly = 3,
    Dotted = 4,
    Dashed = 5,
}

/// (Zig: `Attribute.Underline.C`.)
pub type UnderlineC = core::ffi::c_int;

impl Underline {
    pub fn cval(self) -> UnderlineC {
        self as UnderlineC
    }

    /// From the backing integer (Zig `@enumFromInt`); values above 5 are
    /// invalid.
    pub fn from_int(v: u8) -> Option<Underline> {
        Some(match v {
            0 => Underline::None,
            1 => Underline::Single,
            2 => Underline::Double,
            3 => Underline::Curly,
            4 => Underline::Dotted,
            5 => Underline::Dashed,
            _ => return None,
        })
    }
}

/// C ABI tagged union of [`Attribute`] (Zig: `Attribute.C`). Empty for the
/// application build (`lib.target == .zig`).
#[derive(Debug, Clone, Copy, Default)]
pub struct AttributeC {}

/// C ABI value union of [`Attribute`] (Zig: `Attribute.CValue`). Empty for
/// the application build (`lib.target == .zig`).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct AttributeCValue {}

impl Attribute<'_> {
    /// The active tag.
    pub fn tag(&self) -> Tag {
        match self {
            Attribute::Unset => Tag::Unset,
            Attribute::Unknown(_) => Tag::Unknown,
            Attribute::Bold => Tag::Bold,
            Attribute::ResetBold => Tag::ResetBold,
            Attribute::Italic => Tag::Italic,
            Attribute::ResetItalic => Tag::ResetItalic,
            Attribute::Faint => Tag::Faint,
            Attribute::Underline(_) => Tag::Underline,
            Attribute::UnderlineColor(_) => Tag::UnderlineColor,
            Attribute::UnderlineColor256(_) => Tag::UnderlineColor256,
            Attribute::ResetUnderlineColor => Tag::ResetUnderlineColor,
            Attribute::Overline => Tag::Overline,
            Attribute::ResetOverline => Tag::ResetOverline,
            Attribute::Blink => Tag::Blink,
            Attribute::ResetBlink => Tag::ResetBlink,
            Attribute::Inverse => Tag::Inverse,
            Attribute::ResetInverse => Tag::ResetInverse,
            Attribute::Invisible => Tag::Invisible,
            Attribute::ResetInvisible => Tag::ResetInvisible,
            Attribute::Strikethrough => Tag::Strikethrough,
            Attribute::ResetStrikethrough => Tag::ResetStrikethrough,
            Attribute::DirectColorFg(_) => Tag::DirectColorFg,
            Attribute::DirectColorBg(_) => Tag::DirectColorBg,
            Attribute::Bg8(_) => Tag::Bg8,
            Attribute::Fg8(_) => Tag::Fg8,
            Attribute::ResetFg => Tag::ResetFg,
            Attribute::ResetBg => Tag::ResetBg,
            Attribute::BrightBg8(_) => Tag::BrightBg8,
            Attribute::BrightFg8(_) => Tag::BrightFg8,
            Attribute::Bg256(_) => Tag::Bg256,
            Attribute::Fg256(_) => Tag::Fg256,
        }
    }

    /// Convert to the C union (empty for the application build).
    pub fn cval(&self) -> AttributeC {
        AttributeC {}
    }
}

/// Which direct color attribute `parse_direct_color` produces.
#[derive(Clone, Copy)]
enum DirectColorTag {
    DirectColorFg,
    DirectColorBg,
    UnderlineColor,
}

impl DirectColorTag {
    fn make(self, rgb: Rgb) -> Attribute<'static> {
        match self {
            DirectColorTag::DirectColorFg => Attribute::DirectColorFg(rgb),
            DirectColorTag::DirectColorBg => Attribute::DirectColorBg(rgb),
            DirectColorTag::UnderlineColor => Attribute::UnderlineColor(rgb),
        }
    }
}

/// Parser parses the attributes from a list of SGR parameters.
#[derive(Debug, Clone, Copy, Default)]
pub struct Parser<'a> {
    pub params: &'a [u16],
    pub params_sep: SepList,
    pub idx: usize,
}

impl<'a> Parser<'a> {
    /// Empty state parser.
    pub const EMPTY: Parser<'static> = Parser {
        params: &[],
        params_sep: SepList::EMPTY,
        idx: 0,
    };

    /// A parser over the given params with no colon separators.
    pub fn new(params: &'a [u16]) -> Self {
        Parser {
            params,
            params_sep: SepList::EMPTY,
            idx: 0,
        }
    }

    /// Next returns the next attribute or null if there are no more attributes.
    pub fn next(&mut self) -> Option<Attribute<'a>> {
        if self.idx >= self.params.len() {
            // Add one to ensure we don't loop on unset
            let idx = self.idx;
            self.idx += 1;

            // If we're at index zero it means we must have an empty list
            // and an empty list implicitly means unset, otherwise we're
            // done and return null.
            return if idx == 0 { Some(Attribute::Unset) } else { None };
        }

        let params: &'a [u16] = self.params;
        let slice: &'a [u16] = &params[self.idx..];
        let colon = self.params_sep.is_set(self.idx);
        self.idx += 1;

        // Our last one will have an idx be the last value.
        if slice.is_empty() {
            return None;
        }

        // If we have a colon separator then we need to ensure we're
        // parsing a value that allows it.
        if colon {
            match slice[0] {
                4 | 38 | 48 | 58 => {}

                _ => {
                    // Consume all the colon separated
                    // values and return them as unknown.
                    let start = self.idx;
                    while self.params_sep.is_set(self.idx) {
                        self.idx += 1;
                    }
                    self.idx += 1;
                    return Some(Attribute::Unknown(Unknown {
                        full: params,
                        partial: &slice[..(self.idx - start + 1).min(slice.len())],
                    }));
                }
            }
        }

        match slice[0] {
            0 => return Some(Attribute::Unset),

            1 => return Some(Attribute::Bold),

            2 => return Some(Attribute::Faint),

            3 => return Some(Attribute::Italic),

            4 => 'underline: {
                if colon {
                    // A trailing colon with no following sub-param
                    // (e.g. "ESC[58:4:m") leaves the colon separator
                    // bit set on the last param without adding another
                    // entry, so we can see param 4 with a colon but
                    // nothing after it.
                    if slice.len() < 2 {
                        break 'underline;
                    }

                    if self.is_colon() {
                        self.consume_unknown_colon();
                        break 'underline;
                    }

                    self.idx += 1;
                    return Some(Attribute::Underline(match slice[1] {
                        0 => Underline::None,
                        1 => Underline::Single,
                        2 => Underline::Double,
                        3 => Underline::Curly,
                        4 => Underline::Dotted,
                        5 => Underline::Dashed,

                        // For unknown underline styles,
                        // just render a single underline.
                        _ => Underline::Single,
                    }));
                }

                return Some(Attribute::Underline(Underline::Single));
            }

            5 => return Some(Attribute::Blink),

            6 => return Some(Attribute::Blink),

            7 => return Some(Attribute::Inverse),

            8 => return Some(Attribute::Invisible),

            9 => return Some(Attribute::Strikethrough),

            21 => return Some(Attribute::Underline(Underline::Double)),

            22 => return Some(Attribute::ResetBold),

            23 => return Some(Attribute::ResetItalic),

            24 => return Some(Attribute::Underline(Underline::None)),

            25 => return Some(Attribute::ResetBlink),

            27 => return Some(Attribute::ResetInverse),

            28 => return Some(Attribute::ResetInvisible),

            29 => return Some(Attribute::ResetStrikethrough),

            30..=37 => return Some(Attribute::Fg8(color::Name((slice[0] - 30) as u8))),

            38 => {
                if slice.len() >= 2 {
                    match slice[1] {
                        // `2` indicates direct-color (r, g, b).
                        // We need at least 3 more params for this to make sense.
                        2 => {
                            if let Some(v) = self.parse_direct_color(DirectColorTag::DirectColorFg, slice, colon) {
                                return Some(v);
                            }
                        }

                        // `5` indicates indexed color.
                        5 => {
                            if slice.len() >= 3 {
                                self.idx += 2;
                                return Some(Attribute::Fg256(slice[2] as u8));
                            }
                        }

                        _ => {}
                    }
                }
            }

            39 => return Some(Attribute::ResetFg),

            40..=47 => return Some(Attribute::Bg8(color::Name((slice[0] - 40) as u8))),

            48 => {
                if slice.len() >= 2 {
                    match slice[1] {
                        // `2` indicates direct-color (r, g, b).
                        // We need at least 3 more params for this to make sense.
                        2 => {
                            if let Some(v) = self.parse_direct_color(DirectColorTag::DirectColorBg, slice, colon) {
                                return Some(v);
                            }
                        }

                        // `5` indicates indexed color.
                        5 => {
                            if slice.len() >= 3 {
                                self.idx += 2;
                                return Some(Attribute::Bg256(slice[2] as u8));
                            }
                        }

                        _ => {}
                    }
                }
            }

            49 => return Some(Attribute::ResetBg),

            53 => return Some(Attribute::Overline),
            55 => return Some(Attribute::ResetOverline),

            58 => {
                if slice.len() >= 2 {
                    match slice[1] {
                        // `2` indicates direct-color (r, g, b).
                        // We need at least 3 more params for this to make sense.
                        2 => {
                            if let Some(v) = self.parse_direct_color(DirectColorTag::UnderlineColor, slice, colon) {
                                return Some(v);
                            }
                        }

                        // `5` indicates indexed color.
                        5 => {
                            if slice.len() >= 3 {
                                self.idx += 2;
                                return Some(Attribute::UnderlineColor256(slice[2] as u8));
                            }
                        }
                        _ => {}
                    }
                }
            }

            59 => return Some(Attribute::ResetUnderlineColor),

            // 82 instead of 90 to offset to "bright" colors
            90..=97 => return Some(Attribute::BrightFg8(color::Name((slice[0] - 82) as u8))),

            100..=107 => return Some(Attribute::BrightBg8(color::Name((slice[0] - 92) as u8))),

            _ => {}
        }

        Some(Attribute::Unknown(Unknown {
            full: params,
            partial: slice,
        }))
    }

    fn parse_direct_color(&mut self, tag: DirectColorTag, slice: &[u16], colon: bool) -> Option<Attribute<'a>> {
        // Any direct color style must have at least 5 values.
        if slice.len() < 5 {
            return None;
        }

        // Only used for direct color sets (38, 48, 58) and subparam 2.
        debug_assert!(slice[1] == 2);

        // Note: We truncate because the value should be 0 to 255. If
        // it isn't, the behavior is undefined so we just... truncate it.

        // If we don't have a colon, then we expect exactly 3 semicolon
        // separated values.
        if !colon {
            self.idx += 4;
            return Some(tag.make(Rgb {
                r: slice[2] as u8,
                g: slice[3] as u8,
                b: slice[4] as u8,
            }));
        }

        // We have a colon, we might have either 5 or 6 values depending
        // on if the colorspace is present.
        let count = self.count_colon();
        match count {
            3 => {
                self.idx += 4;
                Some(tag.make(Rgb {
                    r: slice[2] as u8,
                    g: slice[3] as u8,
                    b: slice[4] as u8,
                }))
            }

            4 => {
                self.idx += 5;
                Some(tag.make(Rgb {
                    r: slice[3] as u8,
                    g: slice[4] as u8,
                    b: slice[5] as u8,
                }))
            }

            _ => {
                self.consume_unknown_colon();
                None
            }
        }
    }

    /// Returns true if the present position has a colon separator.
    /// This always returns false for the last value since it has no
    /// separator.
    #[inline(always)]
    fn is_colon(&self) -> bool {
        self.params_sep.is_set(self.idx)
    }

    fn count_colon(&self) -> usize {
        let mut count: usize = 0;
        let mut idx = self.idx;
        while idx < self.params.len() - 1 && self.params_sep.is_set(idx) {
            count += 1;
            idx += 1;
        }
        count
    }

    /// Consumes all the remaining parameters separated by a colon and
    /// returns an unknown attribute.
    fn consume_unknown_colon(&mut self) {
        let count = self.count_colon();
        self.idx += count + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_parse(params: &[u16]) -> Attribute<'_> {
        let mut p = Parser::new(params);
        p.next().unwrap()
    }

    fn test_parse_colon(params: &[u16]) -> Attribute<'_> {
        let mut p = Parser::new(params);
        // Mark all parameters except the last as having a colon after.
        for i in 0..params.len() - 1 {
            p.params_sep.set(i);
        }
        p.next().unwrap()
    }

    fn sep(idxs: &[usize]) -> SepList {
        let mut list = SepList::EMPTY;
        for &i in idxs {
            list.set(i);
        }
        list
    }

    fn rgb_of(v: Attribute<'_>) -> Rgb {
        match v {
            Attribute::DirectColorFg(c) | Attribute::DirectColorBg(c) | Attribute::UnderlineColor(c) => c,
            _ => panic!("not a direct color: {:?}", v),
        }
    }

    #[test]
    fn sgr_attribute_c_compat() {
        let _ = AttributeC::default();
        let _ = Attribute::Bold.cval();
    }

    #[test]
    fn sgr_parser() {
        assert!(test_parse(&[]) == Attribute::Unset);
        assert!(test_parse(&[0]) == Attribute::Unset);

        {
            let v = test_parse(&[38, 2, 40, 44, 52]);
            assert_eq!(v.tag(), Tag::DirectColorFg);
            assert_eq!(rgb_of(v), Rgb::new(40, 44, 52));
        }

        assert_eq!(test_parse(&[38, 2, 44, 52]).tag(), Tag::Unknown);

        {
            let v = test_parse(&[48, 2, 40, 44, 52]);
            assert_eq!(v.tag(), Tag::DirectColorBg);
            assert_eq!(rgb_of(v), Rgb::new(40, 44, 52));
        }

        assert_eq!(test_parse(&[48, 2, 44, 52]).tag(), Tag::Unknown);
    }

    #[test]
    fn sgr_parser_multiple() {
        let mut p = Parser::new(&[0, 38, 2, 40, 44, 52]);
        assert_eq!(p.next().unwrap().tag(), Tag::Unset);
        assert_eq!(p.next().unwrap().tag(), Tag::DirectColorFg);
        assert!(p.next().is_none());
        assert!(p.next().is_none());
    }

    #[test]
    fn sgr_unsupported_with_colon() {
        let mut p = Parser {
            params: &[0, 4, 1],
            params_sep: sep(&[0]),
            idx: 0,
        };
        assert_eq!(p.next().unwrap().tag(), Tag::Unknown);
        assert_eq!(p.next().unwrap().tag(), Tag::Bold);
        assert!(p.next().is_none());
    }

    #[test]
    fn sgr_unsupported_with_multiple_colon() {
        let mut p = Parser {
            params: &[0, 4, 2, 1],
            params_sep: sep(&[0, 1]),
            idx: 0,
        };
        assert_eq!(p.next().unwrap().tag(), Tag::Unknown);
        assert_eq!(p.next().unwrap().tag(), Tag::Bold);
        assert!(p.next().is_none());
    }

    #[test]
    fn sgr_bold() {
        assert_eq!(test_parse(&[1]).tag(), Tag::Bold);
        assert_eq!(test_parse(&[22]).tag(), Tag::ResetBold);
    }

    #[test]
    fn sgr_italic() {
        assert_eq!(test_parse(&[3]).tag(), Tag::Italic);
        assert_eq!(test_parse(&[23]).tag(), Tag::ResetItalic);
    }

    #[test]
    fn sgr_underline() {
        assert_eq!(test_parse(&[4]).tag(), Tag::Underline);
        assert_eq!(test_parse(&[24]), Attribute::Underline(Underline::None));
    }

    #[test]
    fn sgr_underline_styles() {
        assert_eq!(test_parse_colon(&[4, 2]), Attribute::Underline(Underline::Double));
        assert_eq!(test_parse_colon(&[4, 0]), Attribute::Underline(Underline::None));
        assert_eq!(test_parse_colon(&[4, 1]), Attribute::Underline(Underline::Single));
        assert_eq!(test_parse_colon(&[4, 3]), Attribute::Underline(Underline::Curly));
        assert_eq!(test_parse_colon(&[4, 4]), Attribute::Underline(Underline::Dotted));
        assert_eq!(test_parse_colon(&[4, 5]), Attribute::Underline(Underline::Dashed));
    }

    #[test]
    fn sgr_underline_style_with_more() {
        let mut p = Parser {
            params: &[4, 2, 1],
            params_sep: sep(&[0]),
            idx: 0,
        };

        assert_eq!(p.next().unwrap().tag(), Tag::Underline);
        assert_eq!(p.next().unwrap().tag(), Tag::Bold);
        assert!(p.next().is_none());
    }

    #[test]
    fn sgr_underline_style_with_too_many_colons() {
        let mut p = Parser {
            params: &[4, 2, 3, 1],
            params_sep: sep(&[0, 1]),
            idx: 0,
        };

        assert_eq!(p.next().unwrap().tag(), Tag::Unknown);
        assert_eq!(p.next().unwrap().tag(), Tag::Bold);
        assert!(p.next().is_none());
    }

    #[test]
    fn sgr_blink() {
        assert_eq!(test_parse(&[5]).tag(), Tag::Blink);
        assert_eq!(test_parse(&[6]).tag(), Tag::Blink);
        assert_eq!(test_parse(&[25]).tag(), Tag::ResetBlink);
    }

    #[test]
    fn sgr_inverse() {
        assert_eq!(test_parse(&[7]).tag(), Tag::Inverse);
        assert_eq!(test_parse(&[27]).tag(), Tag::ResetInverse);
    }

    #[test]
    fn sgr_strikethrough() {
        assert_eq!(test_parse(&[9]).tag(), Tag::Strikethrough);
        assert_eq!(test_parse(&[29]).tag(), Tag::ResetStrikethrough);
    }

    #[test]
    fn sgr_8_color() {
        let mut p = Parser::new(&[31, 43, 90, 103]);

        assert_eq!(p.next().unwrap(), Attribute::Fg8(color::Name::RED));
        assert_eq!(p.next().unwrap(), Attribute::Bg8(color::Name::YELLOW));
        assert_eq!(p.next().unwrap(), Attribute::BrightFg8(color::Name::BRIGHT_BLACK));
        assert_eq!(p.next().unwrap(), Attribute::BrightBg8(color::Name::BRIGHT_YELLOW));
    }

    #[test]
    fn sgr_256_color() {
        let mut p = Parser::new(&[38, 5, 161, 48, 5, 236]);
        assert_eq!(p.next().unwrap().tag(), Tag::Fg256);
        assert_eq!(p.next().unwrap().tag(), Tag::Bg256);
        assert!(p.next().is_none());
    }

    #[test]
    fn sgr_256_color_underline() {
        let mut p = Parser::new(&[58, 5, 9]);
        assert_eq!(p.next().unwrap().tag(), Tag::UnderlineColor256);
        assert!(p.next().is_none());
    }

    #[test]
    fn sgr_24_bit_bg_color() {
        let v = test_parse_colon(&[48, 2, 1, 2, 3]);
        assert_eq!(v.tag(), Tag::DirectColorBg);
        assert_eq!(rgb_of(v), Rgb::new(1, 2, 3));
    }

    #[test]
    fn sgr_underline_color() {
        {
            let v = test_parse_colon(&[58, 2, 1, 2, 3]);
            assert_eq!(v.tag(), Tag::UnderlineColor);
            assert_eq!(rgb_of(v), Rgb::new(1, 2, 3));
        }

        {
            let v = test_parse_colon(&[58, 2, 0, 1, 2, 3]);
            assert_eq!(v.tag(), Tag::UnderlineColor);
            assert_eq!(rgb_of(v), Rgb::new(1, 2, 3));
        }
    }

    #[test]
    fn sgr_reset_underline_color() {
        let mut p = Parser::new(&[59]);
        assert_eq!(p.next().unwrap().tag(), Tag::ResetUnderlineColor);
    }

    #[test]
    fn sgr_invisible() {
        let mut p = Parser::new(&[8, 28]);
        assert_eq!(p.next().unwrap().tag(), Tag::Invisible);
        assert_eq!(p.next().unwrap().tag(), Tag::ResetInvisible);
    }

    #[test]
    fn sgr_underline_bg_and_fg() {
        let mut p = Parser::new(&[4, 38, 2, 255, 247, 219, 48, 2, 242, 93, 147, 4]);
        assert_eq!(p.next().unwrap(), Attribute::Underline(Underline::Single));
        {
            let v = p.next().unwrap();
            assert_eq!(v.tag(), Tag::DirectColorFg);
            assert_eq!(rgb_of(v), Rgb::new(255, 247, 219));
        }
        {
            let v = p.next().unwrap();
            assert_eq!(v.tag(), Tag::DirectColorBg);
            assert_eq!(rgb_of(v), Rgb::new(242, 93, 147));
        }
        assert_eq!(p.next().unwrap(), Attribute::Underline(Underline::Single));
    }

    #[test]
    fn sgr_direct_color_fg_missing_color() {
        // This used to crash
        let mut p = Parser::new(&[38, 5]);
        while p.next().is_some() {}
    }

    #[test]
    fn sgr_direct_color_bg_missing_color() {
        // This used to crash
        let mut p = Parser::new(&[48, 5]);
        while p.next().is_some() {}
    }

    #[test]
    fn sgr_direct_fg_bg_underline_ignore_optional_color_space() {
        // These behaviors have been verified against xterm.

        // Colon version should skip the optional color space identifier
        {
            // 3 8 : 2 : Pi : Pr : Pg : Pb
            let v = test_parse_colon(&[38, 2, 0, 1, 2, 3]);
            assert_eq!(v.tag(), Tag::DirectColorFg);
            assert_eq!(rgb_of(v), Rgb::new(1, 2, 3));
        }
        {
            // 4 8 : 2 : Pi : Pr : Pg : Pb
            let v = test_parse_colon(&[48, 2, 0, 1, 2, 3]);
            assert_eq!(v.tag(), Tag::DirectColorBg);
            assert_eq!(rgb_of(v), Rgb::new(1, 2, 3));
        }
        {
            // 5 8 : 2 : Pi : Pr : Pg : Pb
            let v = test_parse_colon(&[58, 2, 0, 1, 2, 3]);
            assert_eq!(v.tag(), Tag::UnderlineColor);
            assert_eq!(rgb_of(v), Rgb::new(1, 2, 3));
        }

        // Semicolon version should not parse optional color space identifier
        {
            // 3 8 ; 2 ; Pr ; Pg ; Pb
            let v = test_parse(&[38, 2, 0, 1, 2, 3]);
            assert_eq!(v.tag(), Tag::DirectColorFg);
            assert_eq!(rgb_of(v), Rgb::new(0, 1, 2));
        }
        {
            // 4 8 ; 2 ; Pr ; Pg ; Pb
            let v = test_parse(&[48, 2, 0, 1, 2, 3]);
            assert_eq!(v.tag(), Tag::DirectColorBg);
            assert_eq!(rgb_of(v), Rgb::new(0, 1, 2));
        }
        {
            // 5 8 ; 2 ; Pr ; Pg ; Pb
            let v = test_parse(&[58, 2, 0, 1, 2, 3]);
            assert_eq!(v.tag(), Tag::UnderlineColor);
            assert_eq!(rgb_of(v), Rgb::new(0, 1, 2));
        }
    }

    #[test]
    fn sgr_direct_fg_colon_with_too_many_colons() {
        let mut p = Parser {
            params: &[38, 2, 0, 1, 2, 3, 4, 1],
            params_sep: sep(&[0, 1, 2, 3, 4, 5]),
            idx: 0,
        };

        assert_eq!(p.next().unwrap().tag(), Tag::Unknown);
        assert_eq!(p.next().unwrap().tag(), Tag::Bold);
        assert!(p.next().is_none());
    }

    #[test]
    fn sgr_direct_fg_colon_with_colorspace_and_extra_param() {
        let mut p = Parser {
            params: &[38, 2, 0, 1, 2, 3, 1],
            params_sep: sep(&[0, 1, 2, 3, 4]),
            idx: 0,
        };

        {
            let v = p.next().unwrap();
            assert_eq!(v.tag(), Tag::DirectColorFg);
            assert_eq!(rgb_of(v), Rgb::new(1, 2, 3));
        }

        assert_eq!(p.next().unwrap().tag(), Tag::Bold);
        assert!(p.next().is_none());
    }

    #[test]
    fn sgr_direct_fg_colon_no_colorspace_and_extra_param() {
        let mut p = Parser {
            params: &[38, 2, 1, 2, 3, 1],
            params_sep: sep(&[0, 1, 2, 3]),
            idx: 0,
        };

        {
            let v = p.next().unwrap();
            assert_eq!(v.tag(), Tag::DirectColorFg);
            assert_eq!(rgb_of(v), Rgb::new(1, 2, 3));
        }

        assert_eq!(p.next().unwrap().tag(), Tag::Bold);
        assert!(p.next().is_none());
    }

    // Kakoune sent this complex SGR sequence that caused invalid behavior.
    #[test]
    fn sgr_kakoune_input() {
        // This used to crash
        let mut p = Parser {
            params: &[0, 4, 3, 38, 2, 175, 175, 215, 58, 2, 0, 190, 80, 70],
            params_sep: sep(&[1, 8, 9, 10, 11, 12]),
            idx: 0,
        };

        assert_eq!(p.next().unwrap().tag(), Tag::Unset);
        assert_eq!(p.next().unwrap(), Attribute::Underline(Underline::Curly));
        {
            let v = p.next().unwrap();
            assert_eq!(v.tag(), Tag::DirectColorFg);
            assert_eq!(rgb_of(v), Rgb::new(175, 175, 215));
        }
        {
            let v = p.next().unwrap();
            assert_eq!(v.tag(), Tag::UnderlineColor);
            assert_eq!(rgb_of(v), Rgb::new(190, 80, 70));
        }

        //assert!(p.next().is_none());
    }

    // Discussion #5930, another input sent by kakoune
    #[test]
    fn sgr_kakoune_input_issue_underline_fg_and_bg() {
        // echo -e "\033[4:3;38;2;51;51;51;48;2;170;170;170;58;2;255;97;136mset everything in one sequence, broken\033[m"

        // This used to crash
        let mut p = Parser {
            params: &[4, 3, 38, 2, 51, 51, 51, 48, 2, 170, 170, 170, 58, 2, 255, 97, 136],
            params_sep: sep(&[0]),
            idx: 0,
        };

        assert_eq!(p.next().unwrap(), Attribute::Underline(Underline::Curly));

        {
            let v = p.next().unwrap();
            assert_eq!(v.tag(), Tag::DirectColorFg);
            assert_eq!(rgb_of(v), Rgb::new(51, 51, 51));
        }

        {
            let v = p.next().unwrap();
            assert_eq!(v.tag(), Tag::DirectColorBg);
            assert_eq!(rgb_of(v), Rgb::new(170, 170, 170));
        }

        {
            let v = p.next().unwrap();
            assert_eq!(v.tag(), Tag::UnderlineColor);
            assert_eq!(rgb_of(v), Rgb::new(255, 97, 136));
        }

        assert!(p.next().is_none());
    }

    // Fuzz crash: afl-out/stream/default/crashes/id:000021
    // Input "ESC [ 5 8 : 4 : m" produces params [58, 4] with colon
    // separator bits set at indices 0 and 1. The trailing colon causes
    // the second iteration to see param 4 (underline) with a colon,
    // triggering assert(slice.len >= 2) with slice.len == 1.
    #[test]
    fn sgr_underline_colon_with_trailing_separator_and_short_slice() {
        let mut p = Parser {
            params: &[58, 4],
            params_sep: sep(&[0, 1]),
            idx: 0,
        };

        // 58:4 is not a valid underline color (sub-param 4 is not 2 or 5),
        // so it falls through as unknown.
        assert_eq!(p.next().unwrap().tag(), Tag::Unknown);

        // Param 4 with a trailing colon but no sub-param is malformed,
        // so it also falls through as unknown rather than panicking.
        assert_eq!(p.next().unwrap().tag(), Tag::Unknown);

        assert!(p.next().is_none());
    }
}
