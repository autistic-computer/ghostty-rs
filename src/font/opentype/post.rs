//! Port of `src/font/opentype/post.zig`.
//!
//! STATUS: ported

use std::io::Cursor;

use super::sfnt::{self, be_struct, Fixed, Fword, ReadError, Uint32, Version16Dot16};
use crate::lib::compat::reader::{self as compat_reader, Endian};

be_struct! {
    /// PostScript Table
    ///
    /// This implementation doesn't parse the
    /// extra fields in versions 2.0 and 2.5.
    ///
    /// References:
    /// - https://learn.microsoft.com/en-us/typography/opentype/spec/post
    ///
    /// Field names are in camelCase in the spec (and in the Zig source);
    /// they are converted to snake_case here.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Post {
        pub version: Version16Dot16,

        /// Italic angle in counter-clockwise degrees from the vertical.
        /// Zero for upright text, negative for text that leans to the
        /// right (forward).
        pub italic_angle: Fixed,

        /// Suggested y-coordinate of the top of the underline.
        pub underline_position: Fword,

        /// Suggested values for the underline thickness.
        /// In general, the underline thickness should match the thickness of
        /// the underscore character (U+005F LOW LINE), and should also match
        /// the strikeout thickness, which is specified in the OS/2 table.
        pub underline_thickness: Fword,

        /// Set to 0 if the font is proportionally spaced, non-zero if
        /// the font is not proportionally spaced (i.e. monospaced).
        pub is_fixed_pitch: Uint32,

        /// Minimum memory usage when an OpenType font is downloaded.
        pub min_mem_type42: Uint32,

        /// Maximum memory usage when an OpenType font is downloaded.
        pub max_mem_type42: Uint32,

        /// Minimum memory usage when an OpenType
        /// font is downloaded as a Type 1 font.
        pub min_mem_type1: Uint32,

        /// Maximum memory usage when an OpenType
        /// font is downloaded as a Type 1 font.
        pub max_mem_type1: Uint32,
    }
}

impl Post {
    /// Parse the table from raw data.
    pub fn init(data: &[u8]) -> Result<Post, ReadError> {
        let mut reader = Cursor::new(data);
        Ok(compat_reader::read_struct_endian(&mut reader, Endian::Big)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post() {
        let test_font = crate::font::embedded::JULIA_MONO;

        let font = sfnt::Sfnt::init(test_font).unwrap();

        let table = font.get_table(b"post").unwrap();

        let post = Post::init(table).unwrap();

        assert_eq!(
            Post {
                version: Version16Dot16 { minor: 0, major: 2 },
                italic_angle: Fixed::from(0.0),
                underline_position: -200,
                underline_thickness: 100,
                is_fixed_pitch: 1,
                min_mem_type42: 0,
                max_mem_type42: 0,
                min_mem_type1: 0,
                max_mem_type1: 0,
            },
            post,
        );
    }
}
