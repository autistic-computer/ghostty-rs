//! Port of `src/font/opentype/hhea.zig`.
//!
//! STATUS: ported

use std::io::Cursor;

use super::sfnt::{self, be_struct, Fword, Int16, ReadError, Ufword, Uint16};
use crate::lib::compat::reader::{self as compat_reader, Endian};

be_struct! {
    /// Horizontal Header Table
    ///
    /// References:
    /// - https://learn.microsoft.com/en-us/typography/opentype/spec/hhea
    ///
    /// Field names are in camelCase in the spec (and in the Zig source);
    /// they are converted to snake_case here.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Hhea {
        /// Major version number of the horizontal header table — set to 1.
        pub major_version: Uint16,

        /// Minor version number of the horizontal header table — set to 0.
        pub minor_version: Uint16,

        /// Typographic ascent—see remarks below.
        pub ascender: Fword,

        /// Typographic descent—see remarks below.
        pub descender: Fword,

        /// Typographic line gap.
        ///
        /// Negative lineGap values are treated as zero
        /// in some legacy platform implementations.
        pub line_gap: Fword,

        /// Maximum advance width value in 'hmtx' table.
        pub advance_width_max: Ufword,

        /// Minimum left sidebearing value in 'hmtx' table for
        /// glyphs with contours (empty glyphs should be ignored).
        pub min_left_side_bearing: Fword,

        /// Minimum right sidebearing value; calculated as
        /// min(aw - (lsb + xMax - xMin)) for glyphs with
        /// contours (empty glyphs should be ignored).
        pub min_right_side_bearing: Fword,

        /// Max(lsb + (xMax - xMin)).
        pub x_max_extent: Fword,

        /// Used to calculate the slope of the cursor (rise/run); 1 for vertical.
        pub caret_slope_rise: Int16,

        /// 0 for vertical.
        pub caret_slope_run: Int16,

        /// The amount by which a slanted highlight on a glyph needs to be shifted
        /// to produce the best appearance. Set to 0 for non-slanted fonts
        pub caret_offset: Int16,

        /// set to 0
        pub _reserved0: Int16,

        /// set to 0
        pub _reserved1: Int16,

        /// set to 0
        pub _reserved2: Int16,

        /// set to 0
        pub _reserved3: Int16,

        /// 0 for current format.
        pub metric_data_format: Int16,

        /// Number of hMetric entries in 'hmtx' table
        pub number_of_h_metrics: Uint16,
    }
}

impl Hhea {
    /// Parse the table from raw data.
    pub fn init(data: &[u8]) -> Result<Hhea, ReadError> {
        let mut reader = Cursor::new(data);
        Ok(compat_reader::read_struct_endian(&mut reader, Endian::Big)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hhea() {
        let test_font = crate::font::embedded::JULIA_MONO;

        let font = sfnt::Sfnt::init(test_font).unwrap();

        let table = font.get_table(b"hhea").unwrap();

        let hhea = Hhea::init(table).unwrap();

        assert_eq!(
            Hhea {
                major_version: 1,
                minor_version: 0,
                ascender: 1900,
                descender: -450,
                line_gap: 0,
                advance_width_max: 1200,
                min_left_side_bearing: -1000,
                min_right_side_bearing: -1889,
                x_max_extent: 3089,
                caret_slope_rise: 1,
                caret_slope_run: 0,
                caret_offset: 0,
                _reserved0: 0,
                _reserved1: 0,
                _reserved2: 0,
                _reserved3: 0,
                metric_data_format: 0,
                number_of_h_metrics: 2,
            },
            hhea,
        );
    }
}
