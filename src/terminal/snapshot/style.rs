//! Port of `src/terminal/snapshot/style.zig`.
//!
//! STATUS: ported
//!
//! Snapshot style entry encoding.
//!
//! Each entry describes one terminal style. A record can use these entries to
//! build a style table and assign indexes according to that record's format.
//! Indexing, ordering, etc. are properties of the containing record.
//!
//! Styles are encoded field by field from the terminal's native `Style` type.
//! The native packed representation is deliberately not part of the snapshot
//! format. This gives flexibility for changing one side or the other.
//!
//! All integers are unsigned and little-endian.
//!
//! | Offset | Size | Field                     |
//! | -----: | ---: | :------------------------ |
//! |      0 |    4 | Foreground color          |
//! |      4 |    4 | Background color          |
//! |      8 |    4 | Underline color           |
//! |     12 |    2 | Style flags (`u16`)       |
//! |     14 |    2 | Reserved, must be zero    |
//!
//! The trailing reserved field is explicit wire padding that rounds each
//! style entry from 14 bytes to 16 bytes. This makes entry offsets and byte
//! counts straightforward.
//!
//! Each color begins with a one-byte kind followed by three data bytes:
//!
//! | Kind | Meaning | Data bytes                         |
//! | ---: | :------ | :--------------------------------- |
//! |    0 | None    | All zero                           |
//! |    1 | Palette | Palette index, then two zero bytes |
//! |    2 | RGB     | Red, green, blue                   |
//!
//! Style flag bits 0 through 7 are bold, italic, faint, blink, inverse,
//! invisible, strikethrough, and overline. Bits 8 through 10 contain the
//! underline kind. Bits 11 through 15 must be zero.
//!
//! | Underline | Meaning |
//! | --------: | :------ |
//! |         0 | None    |
//! |         1 | Single  |
//! |         2 | Double  |
//! |         3 | Curly   |
//! |         4 | Dotted  |
//! |         5 | Dashed  |
//!
//! Underline values 6 and 7 are invalid in snapshot version 1.

use super::io::{self, Error};
use crate::terminal::color::Rgb;
use crate::terminal::sgr;
use crate::terminal::style::{self as terminal_style, Style};

/// Number of bytes in one encoded style entry. This size is part of the
/// wire format: the codec reads and writes fixed offsets within an entry
/// of exactly this size, so if the layout changes, the snapshot version
/// and golden fixtures must also change.
pub const LEN: usize = 16;

/// The wire style flags (Zig: `packed struct(u16)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Flags {
    bold: bool,
    italic: bool,
    faint: bool,
    blink: bool,
    inverse: bool,
    invisible: bool,
    strikethrough: bool,
    overline: bool,
    /// u3
    underline: u8,
    /// u5
    reserved: u8,
}

impl Flags {
    fn to_bits(self) -> u16 {
        (self.bold as u16)
            | (self.italic as u16) << 1
            | (self.faint as u16) << 2
            | (self.blink as u16) << 3
            | (self.inverse as u16) << 4
            | (self.invisible as u16) << 5
            | (self.strikethrough as u16) << 6
            | (self.overline as u16) << 7
            | ((self.underline as u16) & 0x7) << 8
            | ((self.reserved as u16) & 0x1f) << 11
    }

    fn from_bits(bits: u16) -> Flags {
        Flags {
            bold: bits & (1 << 0) != 0,
            italic: bits & (1 << 1) != 0,
            faint: bits & (1 << 2) != 0,
            blink: bits & (1 << 3) != 0,
            inverse: bits & (1 << 4) != 0,
            invisible: bits & (1 << 5) != 0,
            strikethrough: bits & (1 << 6) != 0,
            overline: bits & (1 << 7) != 0,
            underline: ((bits >> 8) & 0x7) as u8,
            reserved: ((bits >> 11) & 0x1f) as u8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum ColorKind {
    None = 0,
    Palette = 1,
    Rgb = 2,
}

impl ColorKind {
    fn from_int(v: u8) -> Option<ColorKind> {
        match v {
            0 => Some(ColorKind::None),
            1 => Some(ColorKind::Palette),
            2 => Some(ColorKind::Rgb),
            _ => None,
        }
    }
}

/// Semantic validation errors for one complete style entry buffer:
/// `InvalidColorKind` (a color kind not defined by snapshot version 1),
/// `InvalidColor` (bytes unused by the selected color kind are not zero),
/// `InvalidUnderline` (undefined underline kind), `InvalidFlags` (reserved
/// style flag bits set), `InvalidReserved` (trailing reserved field not zero).
type ParseError = Error;

/// Errors possible while decoding one style entry.
pub type DecodeError = Error;

/// Encode one terminal style as a fixed-size snapshot style entry.
///
/// The entry is assembled in a fixed buffer and written once, so hot
/// encoders perform a single writer call per style.
pub fn encode(value: Style, writer: &mut dyn io::Writer) -> io::Result<()> {
    let mut encoded = [0u8; LEN];
    encode_color_buf(&mut encoded[0..4], value.fg_color);
    encode_color_buf(&mut encoded[4..8], value.bg_color);
    encode_color_buf(&mut encoded[8..12], value.underline_color);

    let flags = Flags {
        bold: value.flags.bold,
        italic: value.flags.italic,
        faint: value.flags.faint,
        blink: value.flags.blink,
        inverse: value.flags.inverse,
        invisible: value.flags.invisible,
        strikethrough: value.flags.strikethrough,
        overline: value.flags.overline,
        underline: value.flags.underline as u8,
        reserved: 0,
    };
    encoded[12..14].copy_from_slice(&flags.to_bits().to_le_bytes());
    writer.write_all(&encoded)
}

/// Decode and validate one fixed-size snapshot style entry.
pub fn decode(reader: &mut dyn io::Reader) -> Result<Style, DecodeError> {
    let mut encoded = [0u8; LEN];
    reader.read_slice_all(&mut encoded)?;
    parse(&encoded)
}

/// Decode and validate one complete fixed-size style entry buffer.
fn parse(encoded: &[u8; LEN]) -> Result<Style, ParseError> {
    let fg_color = parse_color(encoded[0..4].try_into().unwrap())?;
    let bg_color = parse_color(encoded[4..8].try_into().unwrap())?;
    let underline_color = parse_color(encoded[8..12].try_into().unwrap())?;

    let flags = Flags::from_bits(u16::from_le_bytes([encoded[12], encoded[13]]));
    if flags.reserved != 0 {
        return Err(Error::InvalidFlags);
    }

    let underline = sgr::Underline::from_int(flags.underline).ok_or(Error::InvalidUnderline)?;

    let reserved = u16::from_le_bytes([encoded[14], encoded[15]]);
    if reserved != 0 {
        return Err(Error::InvalidReserved);
    }

    Ok(Style {
        fg_color,
        bg_color,
        underline_color,
        flags: terminal_style::Flags {
            bold: flags.bold,
            italic: flags.italic,
            faint: flags.faint,
            blink: flags.blink,
            inverse: flags.inverse,
            invisible: flags.invisible,
            strikethrough: flags.strikethrough,
            overline: flags.overline,
            underline,
        },
    })
}

/// Decode strictly after consuming one complete fixed-size style entry.
///
/// Unlike `decode`, a semantic error leaves `reader` at the next entry. This
/// lets an enclosing codec catch the error and choose its own fallback without
/// losing the surrounding payload boundary.
pub fn decode_or_discard(reader: &mut dyn io::Reader) -> Result<Style, DecodeError> {
    let mut encoded = [0u8; LEN];
    reader.read_slice_all(&mut encoded)?;
    parse(&encoded)
}

/// Decode one complete entry, returning `None` for invalid semantic contents.
///
/// Reader errors remain structural and are always propagated. This is the
/// lenient entry point for enclosing codecs which can safely replace an
/// invalid fixed-size style without treating truncation as invalid styling.
pub fn decode_or_null(reader: &mut dyn io::Reader) -> io::Result<Option<Style>> {
    match decode_or_discard(reader) {
        Ok(style) => Ok(Some(style)),
        Err(Error::ReadFailed) => Err(Error::ReadFailed),
        Err(Error::EndOfStream) => Err(Error::EndOfStream),
        Err(
            Error::InvalidColorKind
            | Error::InvalidColor
            | Error::InvalidUnderline
            | Error::InvalidFlags
            | Error::InvalidReserved,
        ) => Ok(None),
        Err(other) => unreachable!("unexpected style decode error {other:?}"),
    }
}

/// `decode_or_null` over one already-buffered entry, for enclosing codecs
/// that parse many entries from a flat payload without reader calls.
pub fn parse_or_null(encoded: &[u8; LEN]) -> Option<Style> {
    parse(encoded).ok()
}

fn encode_color_buf(encoded: &mut [u8], value: terminal_style::Color) {
    match value {
        terminal_style::Color::None => encoded[0] = ColorKind::None as u8,
        terminal_style::Color::Palette(index) => {
            encoded[0] = ColorKind::Palette as u8;
            encoded[1] = index;
        }
        terminal_style::Color::Rgb(rgb) => {
            encoded[0] = ColorKind::Rgb as u8;
            encoded[1] = rgb.r;
            encoded[2] = rgb.g;
            encoded[3] = rgb.b;
        }
    }
}

fn parse_color(encoded: &[u8; 4]) -> Result<terminal_style::Color, ParseError> {
    // Kind must be something we know about.
    let kind = ColorKind::from_int(encoded[0]).ok_or(Error::InvalidColorKind)?;

    match kind {
        ColorKind::None => {
            if encoded[1] == 0 && encoded[2] == 0 && encoded[3] == 0 {
                Ok(terminal_style::Color::None)
            } else {
                Err(Error::InvalidColor)
            }
        }
        ColorKind::Palette => {
            if encoded[2] == 0 && encoded[3] == 0 {
                Ok(terminal_style::Color::Palette(encoded[1]))
            } else {
                Err(Error::InvalidColor)
            }
        }
        ColorKind::Rgb => Ok(terminal_style::Color::Rgb(Rgb {
            r: encoded[1],
            g: encoded[2],
            b: encoded[3],
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::snapshot::fixture;
    use crate::terminal::snapshot::io::{FixedReader, FixedWriter, Limited, Reader as _};

    #[test]
    fn golden_encoding_and_decoding() {
        let value = Style {
            fg_color: terminal_style::Color::None,
            bg_color: terminal_style::Color::Palette(0x7f),
            underline_color: terminal_style::Color::Rgb(Rgb {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            }),
            flags: terminal_style::Flags {
                bold: true,
                italic: true,
                faint: true,
                blink: true,
                inverse: true,
                invisible: true,
                strikethrough: true,
                overline: true,
                underline: sgr::Underline::Curly,
            },
        };
        let mut buf = [0u8; LEN];
        let mut writer = FixedWriter::new(&mut buf);
        encode(value, &mut writer).unwrap();
        let reference = fixture::parse(include_str!("testdata/style-v1.hex"));
        fixture::expect_equal(
            fixture::Kind::Bytes,
            "src/terminal/snapshot/testdata/style-v1.hex",
            "snapshot_fixture-style-v1.hex",
            &reference,
            writer.buffered(),
        );

        let mut limited = Limited::new(FixedReader::new(&reference), None, 1);
        assert!(value.eql(&decode(&mut limited).unwrap()));
    }

    #[test]
    fn flag_bit_layout() {
        let cases: [(Flags, u16); 14] = [
            (Flags { bold: true, ..Default::default() }, 1 << 0),
            (Flags { italic: true, ..Default::default() }, 1 << 1),
            (Flags { faint: true, ..Default::default() }, 1 << 2),
            (Flags { blink: true, ..Default::default() }, 1 << 3),
            (Flags { inverse: true, ..Default::default() }, 1 << 4),
            (Flags { invisible: true, ..Default::default() }, 1 << 5),
            (Flags { strikethrough: true, ..Default::default() }, 1 << 6),
            (Flags { overline: true, ..Default::default() }, 1 << 7),
            (
                Flags {
                    underline: sgr::Underline::Single as u8,
                    ..Default::default()
                },
                1 << 8,
            ),
            (
                Flags {
                    underline: sgr::Underline::Double as u8,
                    ..Default::default()
                },
                2 << 8,
            ),
            (
                Flags {
                    underline: sgr::Underline::Curly as u8,
                    ..Default::default()
                },
                3 << 8,
            ),
            (
                Flags {
                    underline: sgr::Underline::Dotted as u8,
                    ..Default::default()
                },
                4 << 8,
            ),
            (
                Flags {
                    underline: sgr::Underline::Dashed as u8,
                    ..Default::default()
                },
                5 << 8,
            ),
            (Flags { reserved: 1, ..Default::default() }, 1 << 11),
        ];

        for (value, expected) in cases {
            assert_eq!(expected, value.to_bits());
        }
    }

    #[test]
    fn reject_invalid_colors() {
        for offset in [0usize, 4, 8] {
            let mut invalid_kind = [0u8; LEN];
            invalid_kind[offset] = 3;
            let mut reader = FixedReader::new(&invalid_kind);
            assert_eq!(Err(Error::InvalidColorKind), decode(&mut reader));
        }

        let mut invalid_none = [0u8; LEN];
        invalid_none[1] = 1;
        let mut none_reader = FixedReader::new(&invalid_none);
        assert_eq!(Err(Error::InvalidColor), decode(&mut none_reader));

        let mut invalid_palette = [0u8; LEN];
        invalid_palette[0] = ColorKind::Palette as u8;
        invalid_palette[2] = 1;
        let mut palette_reader = FixedReader::new(&invalid_palette);
        assert_eq!(Err(Error::InvalidColor), decode(&mut palette_reader));
    }

    #[test]
    fn reject_invalid_flags_and_reserved_field() {
        for underline in [6u16, 7] {
            let mut invalid_underline = [0u8; LEN];
            invalid_underline[12..14].copy_from_slice(&(underline << 8).to_le_bytes());
            let mut reader = FixedReader::new(&invalid_underline);
            assert_eq!(Err(Error::InvalidUnderline), decode(&mut reader));
        }

        let mut invalid_flags = [0u8; LEN];
        invalid_flags[12..14].copy_from_slice(&(1u16 << 11).to_le_bytes());
        let mut flags_reader = FixedReader::new(&invalid_flags);
        assert_eq!(Err(Error::InvalidFlags), decode(&mut flags_reader));

        let mut invalid_reserved = [0u8; LEN];
        invalid_reserved[14..16].copy_from_slice(&1u16.to_le_bytes());
        let mut reserved_reader = FixedReader::new(&invalid_reserved);
        assert_eq!(Err(Error::InvalidReserved), decode(&mut reserved_reader));
    }

    #[test]
    fn decode_or_discard_preserves_the_next_entry_boundary() {
        let mut fixture = [0u8; LEN + 1];
        fixture[0] = 3;
        fixture[LEN] = 0xFF;

        let mut reader = FixedReader::new(&fixture);
        assert_eq!(Err(Error::InvalidColorKind), decode_or_discard(&mut reader));
        assert_eq!(0xFFu8, reader.take_byte().unwrap());
    }

    #[test]
    fn decode_or_null_distinguishes_semantic_errors_from_truncation() {
        let mut invalid = [0u8; LEN];
        invalid[0] = 3;
        let mut invalid_reader = FixedReader::new(&invalid);
        assert_eq!(None, decode_or_null(&mut invalid_reader).unwrap());

        let mut truncated = FixedReader::new(&invalid[..LEN - 1]);
        assert_eq!(Err(Error::EndOfStream), decode_or_null(&mut truncated));
    }

    #[test]
    fn reject_every_truncation() {
        let fixture = [0u8; LEN];
        for fixture_len in 0..LEN {
            let mut reader = FixedReader::new(&fixture[..fixture_len]);
            assert_eq!(Err(Error::EndOfStream), decode(&mut reader));
        }
    }
}
