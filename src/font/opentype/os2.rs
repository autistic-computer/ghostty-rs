//! Port of `src/font/opentype/os2.zig`.
//!
//! STATUS: ported

use std::fmt;

use std::io::{self, Cursor, Read};

use super::sfnt::{self, be_struct, Fword, Int16, ReadError, Tag, Ufword, Uint16, Uint32, Uint8};
use crate::lib::compat::reader::{self as compat_reader, Endian, ReadEndian};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FsSelection {
    /// Font contains italic or oblique glyphs, otherwise they are upright.
    pub italic: bool,

    /// Glyphs are underscored.
    pub underscore: bool,

    /// Glyphs have their foreground and background reversed.
    pub negative: bool,

    /// Outline (hollow) glyphs, otherwise they are solid.
    pub outlined: bool,

    /// Glyphs are overstruck.
    pub strikeout: bool,

    /// Glyphs are emboldened.
    pub bold: bool,

    /// Glyphs are in the standard weight/style for the font.
    pub regular: bool,

    /// If set, it is strongly recommended that applications use
    /// OS/2.sTypoAscender - OS/2.sTypoDescender + OS/2.sTypoLineGap
    /// as the default line spacing for this font.
    pub use_typo_metrics: bool,

    /// The font has 'name' table strings consistent with a weight/width/slope
    /// family without requiring use of name IDs 21 and 22.
    pub wws: bool,

    /// Font contains oblique glyphs.
    pub oblique: bool,

    /// Reserved bits 10-15 (a `u6` in Zig).
    pub _reserved: u8,
}

impl FsSelection {
    /// Decode from the packed `u16` representation (bit 0 = `italic`).
    pub const fn from_bits(bits: Uint16) -> Self {
        Self {
            italic: bits & (1 << 0) != 0,
            underscore: bits & (1 << 1) != 0,
            negative: bits & (1 << 2) != 0,
            outlined: bits & (1 << 3) != 0,
            strikeout: bits & (1 << 4) != 0,
            bold: bits & (1 << 5) != 0,
            regular: bits & (1 << 6) != 0,
            use_typo_metrics: bits & (1 << 7) != 0,
            wws: bits & (1 << 8) != 0,
            oblique: bits & (1 << 9) != 0,
            _reserved: (bits >> 10) as u8 & 0x3F,
        }
    }

    /// Encode to the packed `u16` representation.
    pub const fn to_bits(self) -> Uint16 {
        (self.italic as u16)
            | (self.underscore as u16) << 1
            | (self.negative as u16) << 2
            | (self.outlined as u16) << 3
            | (self.strikeout as u16) << 4
            | (self.bold as u16) << 5
            | (self.regular as u16) << 6
            | (self.use_typo_metrics as u16) << 7
            | (self.wws as u16) << 8
            | (self.oblique as u16) << 9
            | ((self._reserved as u16) & 0x3F) << 10
    }
}

impl ReadEndian for FsSelection {
    fn read_endian<R: Read + ?Sized>(r: &mut R, endian: Endian) -> io::Result<Self> {
        Ok(Self::from_bits(Uint16::read_endian(r, endian)?))
    }
}

be_struct! {
    /// OS/2 and Windows Metrics Table
    ///
    /// References:
    /// - https://learn.microsoft.com/en-us/typography/opentype/spec/os2
    ///
    /// Field names are in camelCase in the spec (and in the Zig source);
    /// they are converted to snake_case here.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Os2V5 {
        pub version: Uint16,
        pub x_avg_char_width: Fword,
        pub us_weight_class: Uint16,
        pub us_width_class: Uint16,
        pub fs_type: Uint16,
        pub y_subscript_x_size: Fword,
        pub y_subscript_y_size: Fword,
        pub y_subscript_x_offset: Fword,
        pub y_subscript_y_offset: Fword,
        pub y_superscript_x_size: Fword,
        pub y_superscript_y_size: Fword,
        pub y_superscript_x_offset: Fword,
        pub y_superscript_y_offset: Fword,
        pub y_strikeout_size: Fword,
        pub y_strikeout_position: Fword,
        pub s_family_class: Int16,
        pub panose: [Uint8; 10],
        pub ul_unicode_range1: Uint32,
        pub ul_unicode_range2: Uint32,
        pub ul_unicode_range3: Uint32,
        pub ul_unicode_range4: Uint32,
        pub ach_vend_id: Tag,
        pub fs_selection: FsSelection,
        pub us_first_char_index: Uint16,
        pub us_last_char_index: Uint16,
        pub s_typo_ascender: Fword,
        pub s_typo_descender: Fword,
        pub s_typo_line_gap: Fword,
        pub us_win_ascent: Ufword,
        pub us_win_descent: Ufword,
        pub ul_code_page_range1: Uint32,
        pub ul_code_page_range2: Uint32,
        pub sx_height: Fword,
        pub s_cap_height: Fword,
        pub us_default_char: Uint16,
        pub us_break_char: Uint16,
        pub us_max_context: Uint16,
        pub us_lower_optical_point_size: Uint16,
        pub us_upper_optical_point_size: Uint16,
    }
}

be_struct! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[allow(non_camel_case_types)]
    pub struct Os2V4_3_2 {
        pub version: Uint16,
        pub x_avg_char_width: Fword,
        pub us_weight_class: Uint16,
        pub us_width_class: Uint16,
        pub fs_type: Uint16,
        pub y_subscript_x_size: Fword,
        pub y_subscript_y_size: Fword,
        pub y_subscript_x_offset: Fword,
        pub y_subscript_y_offset: Fword,
        pub y_superscript_x_size: Fword,
        pub y_superscript_y_size: Fword,
        pub y_superscript_x_offset: Fword,
        pub y_superscript_y_offset: Fword,
        pub y_strikeout_size: Fword,
        pub y_strikeout_position: Fword,
        pub s_family_class: Int16,
        pub panose: [Uint8; 10],
        pub ul_unicode_range1: Uint32,
        pub ul_unicode_range2: Uint32,
        pub ul_unicode_range3: Uint32,
        pub ul_unicode_range4: Uint32,
        pub ach_vend_id: Tag,
        pub fs_selection: FsSelection,
        pub us_first_char_index: Uint16,
        pub us_last_char_index: Uint16,
        pub s_typo_ascender: Fword,
        pub s_typo_descender: Fword,
        pub s_typo_line_gap: Fword,
        pub us_win_ascent: Ufword,
        pub us_win_descent: Ufword,
        pub ul_code_page_range1: Uint32,
        pub ul_code_page_range2: Uint32,
        pub sx_height: Fword,
        pub s_cap_height: Fword,
        pub us_default_char: Uint16,
        pub us_break_char: Uint16,
        pub us_max_context: Uint16,
    }
}

be_struct! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Os2V1 {
        pub version: Uint16,
        pub x_avg_char_width: Fword,
        pub us_weight_class: Uint16,
        pub us_width_class: Uint16,
        pub fs_type: Uint16,
        pub y_subscript_x_size: Fword,
        pub y_subscript_y_size: Fword,
        pub y_subscript_x_offset: Fword,
        pub y_subscript_y_offset: Fword,
        pub y_superscript_x_size: Fword,
        pub y_superscript_y_size: Fword,
        pub y_superscript_x_offset: Fword,
        pub y_superscript_y_offset: Fword,
        pub y_strikeout_size: Fword,
        pub y_strikeout_position: Fword,
        pub s_family_class: Int16,
        pub panose: [Uint8; 10],
        pub ul_unicode_range1: Uint32,
        pub ul_unicode_range2: Uint32,
        pub ul_unicode_range3: Uint32,
        pub ul_unicode_range4: Uint32,
        pub ach_vend_id: Tag,
        pub fs_selection: FsSelection,
        pub us_first_char_index: Uint16,
        pub us_last_char_index: Uint16,
        pub s_typo_ascender: Fword,
        pub s_typo_descender: Fword,
        pub s_typo_line_gap: Fword,
        pub us_win_ascent: Ufword,
        pub us_win_descent: Ufword,
        pub ul_code_page_range1: Uint32,
        pub ul_code_page_range2: Uint32,
    }
}

be_struct! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Os2V0 {
        pub version: Uint16,
        pub x_avg_char_width: Fword,
        pub us_weight_class: Uint16,
        pub us_width_class: Uint16,
        pub fs_type: Uint16,
        pub y_subscript_x_size: Fword,
        pub y_subscript_y_size: Fword,
        pub y_subscript_x_offset: Fword,
        pub y_subscript_y_offset: Fword,
        pub y_superscript_x_size: Fword,
        pub y_superscript_y_size: Fword,
        pub y_superscript_x_offset: Fword,
        pub y_superscript_y_offset: Fword,
        pub y_strikeout_size: Fword,
        pub y_strikeout_position: Fword,
        pub s_family_class: Int16,
        pub panose: [Uint8; 10],
        pub ul_unicode_range1: Uint32,
        pub ul_unicode_range2: Uint32,
        pub ul_unicode_range3: Uint32,
        pub ul_unicode_range4: Uint32,
        pub ach_vend_id: Tag,
        pub fs_selection: FsSelection,
        pub us_first_char_index: Uint16,
        pub us_last_char_index: Uint16,
        pub s_typo_ascender: Fword,
        pub s_typo_descender: Fword,
        pub s_typo_line_gap: Fword,
        pub us_win_ascent: Ufword,
        pub us_win_descent: Ufword,
    }
}

/// Errors that can be returned from [`Os2::init`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os2Error {
    Os2VersionNotSupported,
    EndOfStream,
}

impl From<ReadError> for Os2Error {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::EndOfStream => Os2Error::EndOfStream,
        }
    }
}

impl From<std::io::Error> for Os2Error {
    fn from(e: std::io::Error) -> Self {
        ReadError::from(e).into()
    }
}

impl fmt::Display for Os2Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Os2Error::Os2VersionNotSupported => f.write_str("OS2VersionNotSupported"),
            Os2Error::EndOfStream => f.write_str("EndOfStream"),
        }
    }
}

impl std::error::Error for Os2Error {}

/// Generic OS/2 table with optional fields
/// for those that don't exist in all versions.
///
/// References:
/// - https://learn.microsoft.com/en-us/typography/opentype/spec/os2
///
/// Field names are in camelCase in the spec (and in the Zig source);
/// they are converted to snake_case here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Os2 {
    /// The version number for the OS/2 table: 0x0000 to 0x0005.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2#version
    pub version: u16,
    /// The Average Character Width field specifies the arithmetic average of the escapement (width) of all non-zero width glyphs in the font.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#xavgcharwidth
    pub x_avg_char_width: i16,
    /// Indicates the visual weight (degree of blackness or thickness of strokes) of the characters in the font. Values from 1 to 1000 are valid.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#usweightclass
    pub us_weight_class: u16,
    /// Indicates a relative change from the normal aspect ratio (width to height ratio) as specified by a font designer for the glyphs in a font.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#uswidthclass
    pub us_width_class: u16,
    /// Indicates font embedding licensing rights for the font.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#fstype
    pub fs_type: u16,
    /// The recommended horizontal size in font design units for subscripts for this font.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ysubscriptxsize
    pub y_subscript_x_size: i16,
    /// The recommended vertical size in font design units for subscripts for this font.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ysubscriptysize
    pub y_subscript_y_size: i16,
    /// The recommended horizontal offset in font design units for subscripts for this font.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ysubscriptxoffset
    pub y_subscript_x_offset: i16,
    /// The recommended vertical offset in font design units from the baseline for subscripts for this font.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ysubscriptyoffset
    pub y_subscript_y_offset: i16,
    /// The recommended horizontal size in font design units for superscripts for this font.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ysuperscriptxsize
    pub y_superscript_x_size: i16,
    /// The recommended vertical size in font design units for superscripts for this font.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ysuperscriptysize
    pub y_superscript_y_size: i16,
    /// The recommended horizontal offset in font design units for superscripts for this font.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ysuperscriptxoffset
    pub y_superscript_x_offset: i16,
    /// The recommended vertical offset in font design units from the baseline for superscripts for this font.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ysuperscriptyoffset
    pub y_superscript_y_offset: i16,
    /// Thickness of the strikeout stroke in font design units. Should be > 0.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ystrikeoutsize
    pub y_strikeout_size: i16,
    /// The position of the top of the strikeout stroke relative to the baseline in font design units.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ystrikeoutposition
    pub y_strikeout_position: i16,
    /// This field provides a classification of font-family design.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#sfamilyclass
    pub s_family_class: i16,
    /// This 10-byte array of numbers is used to describe the visual characteristics of a given typeface.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#panose
    pub panose: [u8; 10],
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ulunicoderange
    pub ul_unicode_range1: u32,
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ulunicoderange
    pub ul_unicode_range2: u32,
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ulunicoderange
    pub ul_unicode_range3: u32,
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ulunicoderange
    pub ul_unicode_range4: u32,
    /// The four character identifier for the vendor of the given type face.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#achvendid
    pub ach_vend_id: [u8; 4],
    /// Contains information concerning the nature of the font patterns.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#fsselection
    pub fs_selection: FsSelection,
    /// The minimum Unicode index (character code) in this font, according to the 'cmap' subtable for platform ID 3 and platform-specific encoding ID 0 or 1.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#usfirstcharindex
    pub us_first_char_index: u16,
    /// The maximum Unicode index (character code) in this font, according to the 'cmap' subtable for platform ID 3 and encoding ID 0 or 1.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#uslastcharindex
    pub us_last_char_index: u16,
    /// The typographic ascender for this font. This field should be combined with the sTypoDescender and sTypoLineGap values to determine default line spacing.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#stypoascender
    pub s_typo_ascender: i16,
    /// The typographic descender for this font. This field should be combined with the sTypoAscender and sTypoLineGap values to determine default line spacing.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#stypodescender
    pub s_typo_descender: i16,
    /// The typographic line gap for this font. This field should be combined with the sTypoAscender and sTypoDescender values to determine default line spacing.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#stypolinegap
    pub s_typo_line_gap: i16,
    /// The “Windows ascender” metric. This should be used to specify the height above the baseline for a clipping region.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#uswinascent
    pub us_win_ascent: u16,
    /// The “Windows descender” metric. This should be used to specify the vertical extent below the baseline for a clipping region.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#uswindescent
    pub us_win_descent: u16,
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ulcodepagerange
    pub ul_code_page_range1: Option<u32>,
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#ulcodepagerange
    pub ul_code_page_range2: Option<u32>,
    /// This metric specifies the distance between the baseline and the approximate height of non-ascending lowercase letters measured in font design units.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#sxheight
    pub sx_height: Option<i16>,
    /// This metric specifies the distance between the baseline and the approximate height of uppercase letters measured in font design units.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#scapheight
    pub s_cap_height: Option<i16>,
    /// This is the Unicode code point, in UTF-16 encoding, of a character that can be used for a default glyph if a requested character is not supported in the font.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#usdefaultchar
    pub us_default_char: Option<u16>,
    /// This is the Unicode code point, in UTF-16 encoding, of a character that can be used as a default break character.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#usbreakchar
    pub us_break_char: Option<u16>,
    /// The maximum length of a target glyph context for any feature in this font. For example, a font which has only a pair kerning feature should set this field to 2.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#usmaxcontext
    pub us_max_context: Option<u16>,
    /// This field is used for fonts with multiple optical styles.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#usloweropticalpointsize
    pub us_lower_optical_point_size: Option<u16>,
    /// This field is used for fonts with multiple optical styles.
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/os2/#usupperopticalpointsize
    pub us_upper_optical_point_size: Option<u16>,
}

impl Os2 {
    /// Parse the table from raw data.
    pub fn init(data: &[u8]) -> Result<Os2, Os2Error> {
        let mut reader = Cursor::new(data);

        let version: Uint16 = compat_reader::reader_int(&mut reader, Endian::Big)?;

        // Return to the start, cause the version is part of the struct.
        reader.set_position(0);

        match version {
            5 => {
                let table: Os2V5 = compat_reader::read_struct_endian(&mut reader, Endian::Big)?;
                Ok(Os2 {
                    version: table.version,
                    x_avg_char_width: table.x_avg_char_width,
                    us_weight_class: table.us_weight_class,
                    us_width_class: table.us_width_class,
                    fs_type: table.fs_type,
                    y_subscript_x_size: table.y_subscript_x_size,
                    y_subscript_y_size: table.y_subscript_y_size,
                    y_subscript_x_offset: table.y_subscript_x_offset,
                    y_subscript_y_offset: table.y_subscript_y_offset,
                    y_superscript_x_size: table.y_superscript_x_size,
                    y_superscript_y_size: table.y_superscript_y_size,
                    y_superscript_x_offset: table.y_superscript_x_offset,
                    y_superscript_y_offset: table.y_superscript_y_offset,
                    y_strikeout_size: table.y_strikeout_size,
                    y_strikeout_position: table.y_strikeout_position,
                    s_family_class: table.s_family_class,
                    panose: table.panose,
                    ul_unicode_range1: table.ul_unicode_range1,
                    ul_unicode_range2: table.ul_unicode_range2,
                    ul_unicode_range3: table.ul_unicode_range3,
                    ul_unicode_range4: table.ul_unicode_range4,
                    ach_vend_id: table.ach_vend_id,
                    fs_selection: table.fs_selection,
                    us_first_char_index: table.us_first_char_index,
                    us_last_char_index: table.us_last_char_index,
                    s_typo_ascender: table.s_typo_ascender,
                    s_typo_descender: table.s_typo_descender,
                    s_typo_line_gap: table.s_typo_line_gap,
                    us_win_ascent: table.us_win_ascent,
                    us_win_descent: table.us_win_descent,
                    ul_code_page_range1: Some(table.ul_code_page_range1),
                    ul_code_page_range2: Some(table.ul_code_page_range2),
                    sx_height: Some(table.sx_height),
                    s_cap_height: Some(table.s_cap_height),
                    us_default_char: Some(table.us_default_char),
                    us_break_char: Some(table.us_break_char),
                    us_max_context: Some(table.us_max_context),
                    us_lower_optical_point_size: Some(table.us_lower_optical_point_size),
                    us_upper_optical_point_size: Some(table.us_upper_optical_point_size),
                })
            }
            4 | 3 | 2 => {
                let table: Os2V4_3_2 = compat_reader::read_struct_endian(&mut reader, Endian::Big)?;
                Ok(Os2 {
                    version: table.version,
                    x_avg_char_width: table.x_avg_char_width,
                    us_weight_class: table.us_weight_class,
                    us_width_class: table.us_width_class,
                    fs_type: table.fs_type,
                    y_subscript_x_size: table.y_subscript_x_size,
                    y_subscript_y_size: table.y_subscript_y_size,
                    y_subscript_x_offset: table.y_subscript_x_offset,
                    y_subscript_y_offset: table.y_subscript_y_offset,
                    y_superscript_x_size: table.y_superscript_x_size,
                    y_superscript_y_size: table.y_superscript_y_size,
                    y_superscript_x_offset: table.y_superscript_x_offset,
                    y_superscript_y_offset: table.y_superscript_y_offset,
                    y_strikeout_size: table.y_strikeout_size,
                    y_strikeout_position: table.y_strikeout_position,
                    s_family_class: table.s_family_class,
                    panose: table.panose,
                    ul_unicode_range1: table.ul_unicode_range1,
                    ul_unicode_range2: table.ul_unicode_range2,
                    ul_unicode_range3: table.ul_unicode_range3,
                    ul_unicode_range4: table.ul_unicode_range4,
                    ach_vend_id: table.ach_vend_id,
                    fs_selection: table.fs_selection,
                    us_first_char_index: table.us_first_char_index,
                    us_last_char_index: table.us_last_char_index,
                    s_typo_ascender: table.s_typo_ascender,
                    s_typo_descender: table.s_typo_descender,
                    s_typo_line_gap: table.s_typo_line_gap,
                    us_win_ascent: table.us_win_ascent,
                    us_win_descent: table.us_win_descent,
                    ul_code_page_range1: Some(table.ul_code_page_range1),
                    ul_code_page_range2: Some(table.ul_code_page_range2),
                    sx_height: Some(table.sx_height),
                    s_cap_height: Some(table.s_cap_height),
                    us_default_char: Some(table.us_default_char),
                    us_break_char: Some(table.us_break_char),
                    us_max_context: Some(table.us_max_context),
                    ..Default::default()
                })
            }
            1 => {
                let table: Os2V1 = compat_reader::read_struct_endian(&mut reader, Endian::Big)?;
                Ok(Os2 {
                    version: table.version,
                    x_avg_char_width: table.x_avg_char_width,
                    us_weight_class: table.us_weight_class,
                    us_width_class: table.us_width_class,
                    fs_type: table.fs_type,
                    y_subscript_x_size: table.y_subscript_x_size,
                    y_subscript_y_size: table.y_subscript_y_size,
                    y_subscript_x_offset: table.y_subscript_x_offset,
                    y_subscript_y_offset: table.y_subscript_y_offset,
                    y_superscript_x_size: table.y_superscript_x_size,
                    y_superscript_y_size: table.y_superscript_y_size,
                    y_superscript_x_offset: table.y_superscript_x_offset,
                    y_superscript_y_offset: table.y_superscript_y_offset,
                    y_strikeout_size: table.y_strikeout_size,
                    y_strikeout_position: table.y_strikeout_position,
                    s_family_class: table.s_family_class,
                    panose: table.panose,
                    ul_unicode_range1: table.ul_unicode_range1,
                    ul_unicode_range2: table.ul_unicode_range2,
                    ul_unicode_range3: table.ul_unicode_range3,
                    ul_unicode_range4: table.ul_unicode_range4,
                    ach_vend_id: table.ach_vend_id,
                    fs_selection: table.fs_selection,
                    us_first_char_index: table.us_first_char_index,
                    us_last_char_index: table.us_last_char_index,
                    s_typo_ascender: table.s_typo_ascender,
                    s_typo_descender: table.s_typo_descender,
                    s_typo_line_gap: table.s_typo_line_gap,
                    us_win_ascent: table.us_win_ascent,
                    us_win_descent: table.us_win_descent,
                    ul_code_page_range1: Some(table.ul_code_page_range1),
                    ul_code_page_range2: Some(table.ul_code_page_range2),
                    ..Default::default()
                })
            }
            0 => {
                let table: Os2V0 = compat_reader::read_struct_endian(&mut reader, Endian::Big)?;
                Ok(Os2 {
                    version: table.version,
                    x_avg_char_width: table.x_avg_char_width,
                    us_weight_class: table.us_weight_class,
                    us_width_class: table.us_width_class,
                    fs_type: table.fs_type,
                    y_subscript_x_size: table.y_subscript_x_size,
                    y_subscript_y_size: table.y_subscript_y_size,
                    y_subscript_x_offset: table.y_subscript_x_offset,
                    y_subscript_y_offset: table.y_subscript_y_offset,
                    y_superscript_x_size: table.y_superscript_x_size,
                    y_superscript_y_size: table.y_superscript_y_size,
                    y_superscript_x_offset: table.y_superscript_x_offset,
                    y_superscript_y_offset: table.y_superscript_y_offset,
                    y_strikeout_size: table.y_strikeout_size,
                    y_strikeout_position: table.y_strikeout_position,
                    s_family_class: table.s_family_class,
                    panose: table.panose,
                    ul_unicode_range1: table.ul_unicode_range1,
                    ul_unicode_range2: table.ul_unicode_range2,
                    ul_unicode_range3: table.ul_unicode_range3,
                    ul_unicode_range4: table.ul_unicode_range4,
                    ach_vend_id: table.ach_vend_id,
                    fs_selection: table.fs_selection,
                    us_first_char_index: table.us_first_char_index,
                    us_last_char_index: table.us_last_char_index,
                    s_typo_ascender: table.s_typo_ascender,
                    s_typo_descender: table.s_typo_descender,
                    s_typo_line_gap: table.s_typo_line_gap,
                    us_win_ascent: table.us_win_ascent,
                    us_win_descent: table.us_win_descent,
                    ..Default::default()
                })
            }
            _ => Err(Os2Error::Os2VersionNotSupported),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_2() {
        let test_font = crate::font::embedded::JULIA_MONO;

        let font = sfnt::Sfnt::init(test_font).unwrap();

        let table = font.get_table(b"OS/2").unwrap();

        let os2 = Os2::init(table).unwrap();

        assert_eq!(
            Os2 {
                version: 4,
                x_avg_char_width: 1200,
                us_weight_class: 400,
                us_width_class: 5,
                fs_type: 0,
                y_subscript_x_size: 1300,
                y_subscript_y_size: 1200,
                y_subscript_x_offset: 0,
                y_subscript_y_offset: 150,
                y_superscript_x_size: 1300,
                y_superscript_y_size: 1200,
                y_superscript_x_offset: 0,
                y_superscript_y_offset: 700,
                y_strikeout_size: 100,
                y_strikeout_position: 550,
                s_family_class: 0,
                panose: [2, 11, 6, 9, 6, 3, 0, 2, 0, 4],
                ul_unicode_range1: 3843162111,
                ul_unicode_range2: 3603300351,
                ul_unicode_range3: 117760229,
                ul_unicode_range4: 96510060,
                ach_vend_id: *b"corm",
                fs_selection: FsSelection {
                    regular: true,
                    use_typo_metrics: true,
                    ..Default::default()
                },
                us_first_char_index: 13,
                us_last_char_index: 65535,
                s_typo_ascender: 1900,
                s_typo_descender: -450,
                s_typo_line_gap: 0,
                us_win_ascent: 2400,
                us_win_descent: 450,
                ul_code_page_range1: Some(1613234687),
                ul_code_page_range2: Some(0),
                sx_height: Some(1100),
                s_cap_height: Some(1450),
                us_default_char: Some(0),
                us_break_char: Some(32),
                us_max_context: Some(126),
                us_lower_optical_point_size: None,
                us_upper_optical_point_size: None,
            },
            os2,
        );
    }
}
