//! Port of `src/font/opentype/sfnt.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::io::{self, Cursor, Read};

use crate::lib::compat::reader::{self as compat_reader, Endian, ReadEndian};

/// 8-bit unsigned integer.
pub type Uint8 = u8;

/// 8-bit signed integer.
pub type Int8 = i8;

/// 16-bit unsigned integer.
pub type Uint16 = u16;

/// 16-bit signed integer.
pub type Int16 = i16;

/// 24-bit unsigned integer.
///
/// Rust has no `u24`; only the low 24 bits are meaningful.
pub type Uint24 = u32;

/// 32-bit unsigned integer.
pub type Uint32 = u32;

/// 32-bit signed integer.
pub type Int32 = i32;

/// int16 that describes a quantity in font design units.
pub type Fword = i16;

/// uint16 that describes a quantity in font design units.
pub type Ufword = u16;

/// Date and time represented in number of seconds since 12:00 midnight, January 1, 1904, UTC. The value is represented as a signed 64-bit integer.
pub type LongDateTime = i64;

/// Array of four uint8s (length = 32 bits) used to identify a table,
/// design-variation axis, script, language system, feature, or baseline.
pub type Tag = [u8; 4];

/// 8-bit offset to a table, same as uint8, NULL offset = 0x00
pub type Offset8 = u8;

/// Short offset to a table, same as uint16, NULL offset = 0x0000
pub type Offset16 = u16;

/// 24-bit offset to a table, same as uint24, NULL offset = 0x000000
pub type Offset24 = u32;

/// Long offset to a table, same as uint32, NULL offset = 0x00000000
pub type Offset32 = u32;

/// Packed 32-bit value with major and minor version numbers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Version16Dot16 {
    pub minor: u16,
    pub major: u16,
}

impl Version16Dot16 {
    pub const fn from_bits(bits: u32) -> Self {
        Self {
            minor: bits as u16,
            major: (bits >> 16) as u16,
        }
    }

    pub const fn to_bits(self) -> u32 {
        ((self.major as u32) << 16) | self.minor as u32
    }
}

/// Port of the Zig `FixedPoint(T, int_bits, frac_bits)` type function.
///
/// Arguments: name, backing signed type `T`, its unsigned counterpart,
/// the Rust type holding the integer part, the Rust type holding the
/// fractional part, the number of integer bits and the number of
/// fraction bits.
macro_rules! fixed_point {
    (
        $(#[$m:meta])*
        $name:ident, $T:ty, $U:ty, $Int:ty, $Frac:ty, $int_bits:expr, $frac_bits:expr
    ) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        pub struct $name {
            pub frac: $Frac,
            pub int: $Int,
        }

        impl $name {
            const _ASSERT: () = assert!($int_bits + $frac_bits == <$T>::BITS);
            const FRAC_FACTOR: f64 = (1u64 << $frac_bits) as f64;
            const HALF: $T = 1 << ($frac_bits - 1);
            const FRAC_MASK: $U = ((1 as $U) << $frac_bits) - 1;

            /// Reinterpret the raw bits as a fixed point number.
            pub const fn from_bits(bits: $T) -> Self {
                #[allow(clippy::let_unit_value)]
                let () = Self::_ASSERT;
                Self {
                    frac: ((bits as $U) & Self::FRAC_MASK) as $Frac,
                    int: (bits >> $frac_bits) as $Int,
                }
            }

            /// The raw bits of this fixed point number.
            pub const fn to_bits(self) -> $T {
                ((self.int as $T) << $frac_bits) | (self.frac as $T)
            }

            pub fn to_f64(self) -> f64 {
                self.to_bits() as f64 / Self::FRAC_FACTOR
            }

            pub fn to_f32(self) -> f32 {
                self.to_bits() as f32 / Self::FRAC_FACTOR as f32
            }

            pub fn from(float: f64) -> Self {
                Self::from_bits((float * Self::FRAC_FACTOR).round() as $T)
            }

            /// Round to the nearest integer, .5 rounds away from 0.
            pub fn round(self) -> $T {
                if (self.frac as $T) & Self::HALF != 0 {
                    self.int as $T + 1
                } else {
                    self.int as $T
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.to_f64())
            }
        }

        impl ReadEndian for $name {
            fn read_endian<R: Read + ?Sized>(r: &mut R, endian: Endian) -> io::Result<Self> {
                Ok(Self::from_bits(<$T>::read_endian(r, endian)?))
            }
        }
    };
}

fixed_point!(
    /// 32-bit signed fixed-point number (16.16)
    Fixed, i32, u32, i16, u16, 16, 16
);

fixed_point!(
    /// 16-bit signed fixed number with the low 14 bits of fraction (2.14).
    F2Dot14, i16, u16, i8, u16, 2, 14
);

fixed_point!(
    /// 32-bit signed 26.6 fixed point numbers.
    F26Dot6, i32, u32, i32, u8, 26, 6
);

// ---------------------------------------------------------------------------
// Reading helpers.
//
// The Zig tables read through `std.Io.Reader.fixed(data)` and the helpers in
// `lib/compat/reader.zig`. In Rust a fixed reader is a
// `std::io::Cursor<&[u8]>` (its `position()` is `reader.seek`) and the
// helpers are `crate::lib::compat::reader`.
// ---------------------------------------------------------------------------

/// `std.Io.Reader.Error`. A fixed reader can only fail with `EndOfStream`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    EndOfStream,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::EndOfStream => f.write_str("EndOfStream"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<io::Error> for ReadError {
    /// Reading from a fixed buffer (`std::io::Cursor<&[u8]>`) only fails
    /// when it runs out of data.
    fn from(_: io::Error) -> Self {
        ReadError::EndOfStream
    }
}

impl ReadEndian for Version16Dot16 {
    fn read_endian<R: Read + ?Sized>(r: &mut R, endian: Endian) -> io::Result<Self> {
        Ok(Self::from_bits(u32::read_endian(r, endian)?))
    }
}

/// Define a struct whose binary layout is a sequence of big-endian fields
/// (a Zig `extern struct` with `align(1)` fields read via
/// `readStructEndian(.., .big)`), and implement [`ReadEndian`] for it.
macro_rules! be_struct {
    (
        $(#[$m:meta])*
        $vis:vis struct $name:ident {
            $(
                $(#[$fm:meta])*
                $fvis:vis $field:ident : $ty:ty
            ),* $(,)?
        }
    ) => {
        $(#[$m])*
        $vis struct $name {
            $(
                $(#[$fm])*
                $fvis $field: $ty,
            )*
        }

        impl $crate::lib::compat::reader::ReadEndian for $name {
            fn read_endian<R: ::std::io::Read + ?Sized>(
                r: &mut R,
                endian: $crate::lib::compat::reader::Endian,
            ) -> ::std::io::Result<Self> {
                Ok(Self {
                    $(
                        $field: <$ty as $crate::lib::compat::reader::ReadEndian>::read_endian(
                            r, endian,
                        )?,
                    )*
                })
            }
        }
    };
}
pub(crate) use be_struct;

be_struct! {
    /// The static (fixed-sized) portion of the table directory
    ///
    /// This struct matches the memory layout of the TrueType/OpenType
    /// TableDirectory, but does not include the TableRecord array, since
    /// that is dynamically sized, so we parse it separately.
    ///
    /// In the TrueType reference manual this
    /// is referred to as the "offset subtable".
    ///
    /// https://learn.microsoft.com/en-us/typography/opentype/spec/otff#table-directory
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct OffsetSubtable {
        /// Indicates the type of font file we're reading.
        ///   - 0x00_01_00_00   ----   TrueType
        ///   - 0x74_72_75_65  'true'  TrueType
        ///   - 0x4F_54_54_4F  'OTTO'  OpenType
        ///   - 0x74_79_70_31  'typ1'  PostScript
        pub sfnt_version: Uint32,
        /// Number of tables.
        pub num_tables: Uint16,
        /// Maximum power of 2 less than or equal to numTables, times 16 ((2**floor(log2(numTables))) * 16, where “**” is an exponentiation operator).
        pub search_range: Uint16,
        /// Log2 of the maximum power of 2 less than or equal to numTables (log2(searchRange/16), which is equal to floor(log2(numTables))).
        pub entry_selector: Uint16,
        /// numTables times 16, minus searchRange ((numTables * 16) - searchRange).
        pub range_shift: Uint16,
    }
}

impl fmt::Display for OffsetSubtable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let version = if self.sfnt_version == 0x00_01_00_00 {
            "0x00010000".to_string()
        } else {
            String::from_utf8_lossy(&self.sfnt_version.to_be_bytes()).into_owned()
        };
        write!(
            f,
            "OffsetSubtable('{}'){{ .num_tables = {} }}",
            version, self.num_tables
        )
    }
}

be_struct! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct TableRecord {
        /// Table identifier.
        pub tag: Tag,
        /// Checksum for this table.
        pub checksum: Uint32,
        /// Offset from beginning of font file.
        pub offset: Offset32,
        /// Length of this table.
        pub length: Uint32,
    }
}

impl fmt::Display for TableRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TableRecord(\"{}\"){{ .checksum = {}, .offset = {}, .length = {} }}",
            String::from_utf8_lossy(&self.tag),
            self.checksum,
            self.offset,
            self.length
        )
    }
}

/// The table directory of an SFNT font.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    pub offset: OffsetSubtable,
    pub records: Vec<TableRecord>,
}

/// Wrapper for parsing a SFNT font and accessing its tables.
///
/// References:
/// - https://learn.microsoft.com/en-us/typography/opentype/spec/otff
/// - https://developer.apple.com/fonts/TrueType-Reference-Manual/RM06/Chap6.html
#[derive(Debug, Clone)]
pub struct Sfnt<'a> {
    pub directory: Directory,
    pub data: &'a [u8],
}

impl<'a> Sfnt<'a> {
    /// Parse a font from raw data. The struct will keep a
    /// reference to `data` and use it for future operations.
    pub fn init(data: &'a [u8]) -> Result<Sfnt<'a>, ReadError> {
        // SFNT files use big endian; the reader converts to native.
        let mut reader = Cursor::new(data);

        let offset: OffsetSubtable = compat_reader::read_struct_endian(&mut reader, Endian::Big)?;

        let mut records = Vec::with_capacity(offset.num_tables as usize);
        for _ in 0..offset.num_tables {
            records.push(compat_reader::read_struct_endian::<_, TableRecord>(
                &mut reader,
                Endian::Big,
            )?);
        }

        Ok(Sfnt {
            directory: Directory { offset, records },
            data,
        })
    }

    /// Returns the bytes of the table with the provided tag if present.
    ///
    /// Port note: returns `None` (instead of a safety-checked panic) if the
    /// table record points outside of the font data.
    pub fn get_table(&self, tag: &[u8; 4]) -> Option<&'a [u8]> {
        for record in &self.directory.records {
            if tag == &record.tag {
                let start = record.offset as usize;
                let end = start.checked_add(record.length as usize)?;
                return self.data.get(start..end);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::terminal::build_options::{self, Artifact};

    /// `@import("terminal_options").artifact == .lib`
    const ARTIFACT_IS_LIB: bool = matches!(build_options::ARTIFACT, Artifact::Lib);

    #[test]
    fn fixed_point() {
        let p26d6 = F26Dot6::from(26.6);
        assert_eq!(F26Dot6 { int: 26, frac: 38 }, p26d6);
        assert_eq!(26.59375, p26d6.to_f64());
        assert_eq!(27, p26d6.round());

        let n26d6 = F26Dot6::from(-26.6);
        assert_eq!(F26Dot6 { int: -27, frac: 26 }, n26d6);
        assert_eq!(-26.59375, n26d6.to_f64());
        assert_eq!(-27, n26d6.round());
    }

    #[test]
    fn parse_font() {
        // lib-vt source archives intentionally exclude full Ghostty font fixtures.
        if ARTIFACT_IS_LIB {
            return;
        }

        let test_font = crate::font::embedded::JULIA_MONO;

        let sfnt = Sfnt::init(test_font).unwrap();

        assert_eq!(19, sfnt.directory.offset.num_tables);
        assert_eq!(b"prep", &sfnt.directory.records[18].tag);
    }

    #[test]
    fn get_table() {
        // lib-vt source archives intentionally exclude full Ghostty font fixtures.
        if ARTIFACT_IS_LIB {
            return;
        }

        let test_font = crate::font::embedded::JULIA_MONO;

        let sfnt = Sfnt::init(test_font).unwrap();

        let svg = sfnt.get_table(b"SVG ").unwrap();

        assert_eq!(430, svg.len());
    }
}
