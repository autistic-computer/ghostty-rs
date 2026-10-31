//! Port of `src/font/opentype/head.zig`.
//!
//! STATUS: ported

use std::io::Cursor;

use super::sfnt::{self, be_struct, Fixed, Int16, LongDateTime, ReadError, Uint16, Uint32};
use crate::lib::compat::reader::{self as compat_reader, Endian};

be_struct! {
    /// Font Header Table
    ///
    /// References:
    /// - https://learn.microsoft.com/en-us/typography/opentype/spec/head
    ///
    /// Field names are in camelCase in the spec (and in the Zig source);
    /// they are converted to snake_case here.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Head {
        /// Major version number of the font header table — set to 1.
        pub major_version: Uint16,

        /// Minor version number of the font header table — set to 0.
        pub minor_version: Uint16,

        /// Set by font manufacturer.
        pub font_revision: Fixed,

        /// To compute: set it to 0, sum the entire font as uint32, then store
        /// 0xB1B0AFBA - sum. If the font is used as a component in a font
        /// collection file, the value of this field will be invalidated by
        /// changes to the file structure and font table directory, and must
        /// be ignored.
        pub checksum_adjustment: Uint32,

        /// Set to 0x5F0F3CF5.
        pub magic_number: Uint32,

        /// Bit 0: Baseline for font at y=0.
        ///
        /// Bit 1: Left sidebearing point at x=0
        ///        (relevant only for TrueType rasterizers)
        ///
        /// Bit 2: Instructions may depend on point size.
        ///
        /// Bit 3: Force ppem to integer values for all internal scaler math; may
        ///        use fractional ppem sizes if this bit is clear. It is strongly
        ///        recommended that this be set in hinted fonts.
        ///
        /// Bit 4: Instructions may alter advance width
        ///        (the advance widths might not scale linearly).
        ///
        /// Bit 5: This bit is not used in OpenType, and should not be set in order
        ///        to ensure compatible behavior on all platforms. If set, it may
        ///        result in different behavior for vertical layout in some
        ///        platforms.
        ///
        ///        (See Apple’s specification for details
        ///        regarding behavior in Apple platforms.)
        ///
        /// Bits 6 – 10: These bits are not used in OpenType and should always be
        ///              cleared.
        ///
        ///              (See Apple’s specification for details
        ///              regarding legacy use in Apple platforms.)
        ///
        /// Bit 11: Font data is “lossless” as a result of having been
        ///         subjected to optimizing transformation and/or compression
        ///         (such as compression mechanisms defined by ISO/IEC 14496-18,
        ///         MicroType® Express, WOFF 2.0, or similar) where the original
        ///         font functionality and features are retained but the binary
        ///         compatibility between input and output font files is not
        ///         guaranteed. As a result of the applied transform, the DSIG
        ///         table may also be invalidated.
        ///
        /// Bit 12: Font converted (produce compatible metrics).
        ///
        /// Bit 13: Font optimized for ClearType®. Note, fonts that rely on embedded
        ///         bitmaps (EBDT) for rendering should not be considered optimized
        ///         for ClearType, and therefore should keep this bit cleared.
        ///
        /// Bit 14: Last Resort font. If set, indicates that the glyphs encoded in
        ///         the 'cmap' subtables are simply generic symbolic representations
        ///         of code point ranges and do not truly represent support for
        ///         those code points. If unset, indicates that the glyphs encoded
        ///         in the 'cmap' subtables represent proper support for those code
        ///         points.
        ///
        /// Bit 15: Reserved, set to 0.
        pub flags: Uint16,

        /// Set to a value from 16 to 16384. Any value in this range is valid.
        ///
        /// In fonts that have TrueType outlines, a power of 2 is recommended
        /// as this allows performance optimization in some rasterizers.
        pub units_per_em: Uint16,

        /// Number of seconds since 12:00 midnight that started
        /// January 1st, 1904, in GMT/UTC time zone.
        pub created: LongDateTime,

        /// Number of seconds since 12:00 midnight that started
        /// January 1st, 1904, in GMT/UTC time zone.
        pub modified: LongDateTime,

        /// Minimum x coordinate across all glyph bounding boxes.
        pub x_min: Int16,

        /// Minimum y coordinate across all glyph bounding boxes.
        pub y_min: Int16,

        /// Maximum x coordinate across all glyph bounding boxes.
        pub x_max: Int16,

        /// Maximum y coordinate across all glyph bounding boxes.
        pub y_max: Int16,

        /// Bit 0: Bold (if set to 1);
        /// Bit 1: Italic (if set to 1)
        /// Bit 2: Underline (if set to 1)
        /// Bit 3: Outline (if set to 1)
        /// Bit 4: Shadow (if set to 1)
        /// Bit 5: Condensed (if set to 1)
        /// Bit 6: Extended (if set to 1)
        /// Bits 7 – 15: Reserved (set to 0).
        pub mac_style: Uint16,

        /// Smallest readable size in pixels.
        pub lowest_rec_ppem: Uint16,

        /// Deprecated (Set to 2).
        /// 0: Fully mixed directional glyphs;
        /// 1: Only strongly left to right;
        /// 2: Like 1 but also contains neutrals;
        /// -1: Only strongly right to left;
        /// -2: Like -1 but also contains neutrals.
        pub font_direction_hint: Int16,

        /// 0 for short offsets (Offset16), 1 for long (Offset32).
        pub index_to_loc_format: Int16,

        /// 0 for current format.
        pub glyph_data_format: Int16,
    }
}

impl Head {
    /// Parse the table from raw data.
    pub fn init(data: &[u8]) -> Result<Head, ReadError> {
        let mut reader = Cursor::new(data);
        Ok(compat_reader::read_struct_endian(&mut reader, Endian::Big)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head() {
        let test_font = crate::font::embedded::JULIA_MONO;

        let font = sfnt::Sfnt::init(test_font).unwrap();

        let table = font.get_table(b"head").unwrap();

        let head = Head::init(table).unwrap();

        assert_eq!(
            Head {
                major_version: 1,
                minor_version: 0,
                font_revision: Fixed::from(0.05499267578125),
                checksum_adjustment: 1007668681,
                magic_number: 1594834165,
                flags: 7,
                units_per_em: 2000,
                created: 3797757830,
                modified: 3797760444,
                x_min: -1000,
                y_min: -1058,
                x_max: 3089,
                y_max: 2400,
                mac_style: 0,
                lowest_rec_ppem: 7,
                font_direction_hint: 2,
                index_to_loc_format: 1,
                glyph_data_format: 0,
            },
            head,
        );
    }
}
