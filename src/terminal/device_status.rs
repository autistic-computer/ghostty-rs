//! Port of `src/terminal/device_status.zig`.
//!
//! STATUS: ported

use std::io::Write;

/// The color scheme reported in response to a CSI ? 996 n query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ColorScheme {
    Light = 0,
    Dark = 1,
}

impl ColorScheme {
    /// All values in declaration order.
    pub const ALL: [ColorScheme; 2] = [ColorScheme::Light, ColorScheme::Dark];
}

/// The visibility state reported in response to a CSI ? 998 n query or when
/// DEC mode 2033 is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Visibility {
    PotentiallyVisible = 0,
    NotVisible = 1,
}

/// Maximum number of bytes that `encode_color_scheme_report` will write.
pub const MAX_COLOR_SCHEME_REPORT_ENCODE_SIZE: usize = {
    let mut result = 0;
    let mut i = 0;
    while i < ColorScheme::ALL.len() {
        let len = color_scheme_report(ColorScheme::ALL[i]).len();
        if len > result {
            result = len;
        }
        i += 1;
    }
    result
};

const fn color_scheme_report(scheme: ColorScheme) -> &'static [u8] {
    match scheme {
        ColorScheme::Dark => b"\x1B[?997;1n",
        ColorScheme::Light => b"\x1B[?997;2n",
    }
}

/// Encode a color scheme report response for CSI ? 996 n queries.
pub fn encode_color_scheme_report<W: Write + ?Sized>(
    writer: &mut W,
    scheme: ColorScheme,
) -> std::io::Result<()> {
    writer.write_all(color_scheme_report(scheme))
}

/// Maximum number of bytes that `encode_visibility_report` will write.
pub const MAX_VISIBILITY_REPORT_ENCODE_SIZE: usize = b"\x1B[?999;2n".len();

/// Encode a visibility report response for CSI ? 998 n queries and DEC mode
/// 2033 notifications.
pub fn encode_visibility_report<W: Write + ?Sized>(
    writer: &mut W,
    visibility: Visibility,
) -> std::io::Result<()> {
    writer.write_all(match visibility {
        Visibility::PotentiallyVisible => b"\x1B[?999;1n",
        Visibility::NotVisible => b"\x1B[?999;2n",
    })
}

/// The tag type for our enum is a u16 but we use a packed struct
/// in order to pack the question bit into the tag. The "u16" size is
/// chosen somewhat arbitrarily to match the largest expected size
/// we see as a multiple of 8 bits.
///
/// Layout (`packed struct(u16)`): bits 0..15 are `value` (u15), bit 15 is
/// `question`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Tag {
    /// A u15 value.
    pub value: u16,
    pub question: bool,
}

/// `Tag.Backing`
pub type TagBacking = u16;

impl Tag {
    /// Pack into the backing integer (`@bitCast`).
    pub const fn to_bits(self) -> TagBacking {
        (self.value & 0x7FFF) | ((self.question as u16) << 15)
    }

    /// Unpack from the backing integer (`@bitCast`).
    pub const fn from_bits(bits: TagBacking) -> Tag {
        Tag {
            value: bits & 0x7FFF,
            question: bits & 0x8000 != 0,
        }
    }
}

const fn tag(value: u16, question: bool) -> u16 {
    Tag { value, question }.to_bits()
}

/// A single entry of a possible device status request we support. The
/// "question" field determines if it is valid with or without the "?"
/// prefix.
struct Entry {
    request: Request,
    value: u16,
    /// "?" request
    question: bool,
}

/// An enum(u16) of the available device status requests. See entries for
/// available values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Request {
    OperatingStatus = tag(5, false),
    CursorPosition = tag(6, false),
    // DECXCPR: the "?" form of value 6 reports the extended cursor position
    // (adds a page number) rather than the plain CPR.
    CursorPositionExtended = tag(6, true),
    ColorScheme = tag(996, true),
    Visibility = tag(998, true),
}

impl Request {
    /// The packed tag of this request.
    pub const fn tag(self) -> Tag {
        Tag::from_bits(self as u16)
    }
}

/// The full list of device status request entries.
const ENTRIES: &[Entry] = &[
    Entry { request: Request::OperatingStatus, value: 5, question: false },
    Entry { request: Request::CursorPosition, value: 6, question: false },
    Entry { request: Request::CursorPositionExtended, value: 6, question: true },
    Entry { request: Request::ColorScheme, value: 996, question: true },
    Entry { request: Request::Visibility, value: 998, question: true },
];

pub fn req_from_int(v: u16, question: bool) -> Option<Request> {
    for entry in ENTRIES {
        if entry.value == v && entry.question == question {
            return Some(entry.request);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixed<const N: usize>(
        f: impl FnOnce(&mut &mut [u8]) -> std::io::Result<()>,
    ) -> Vec<u8> {
        let mut buf = [0u8; N];
        let mut writer: &mut [u8] = &mut buf;
        f(&mut writer).unwrap();
        let n = N - writer.len();
        buf[..n].to_vec()
    }

    #[test]
    fn tag_order() {
        let t = Tag {
            value: 1,
            question: false,
        };
        let int: TagBacking = t.to_bits();
        assert_eq!(1u16, int);
    }

    #[test]
    fn entries_match_enum() {
        for entry in ENTRIES {
            assert_eq!(entry.request.tag(), Tag { value: entry.value, question: entry.question });
            assert_eq!(req_from_int(entry.value, entry.question), Some(entry.request));
        }
        assert_eq!(req_from_int(7, false), None);
        assert_eq!(req_from_int(5, true), None);
    }

    #[test]
    fn encode_color_scheme_report_dark() {
        assert_eq!(9usize, MAX_COLOR_SCHEME_REPORT_ENCODE_SIZE);

        let out = write_fixed::<MAX_COLOR_SCHEME_REPORT_ENCODE_SIZE>(|w| {
            encode_color_scheme_report(w, ColorScheme::Dark)
        });
        assert_eq!(b"\x1B[?997;1n".as_slice(), out.as_slice());
    }

    #[test]
    fn encode_color_scheme_report_light() {
        let out = write_fixed::<MAX_COLOR_SCHEME_REPORT_ENCODE_SIZE>(|w| {
            encode_color_scheme_report(w, ColorScheme::Light)
        });
        assert_eq!(b"\x1B[?997;2n".as_slice(), out.as_slice());
    }

    #[test]
    fn encode_visibility_report_test() {
        let out = write_fixed::<MAX_VISIBILITY_REPORT_ENCODE_SIZE>(|w| {
            encode_visibility_report(w, Visibility::PotentiallyVisible)
        });
        assert_eq!(b"\x1B[?999;1n".as_slice(), out.as_slice());

        let out = write_fixed::<MAX_VISIBILITY_REPORT_ENCODE_SIZE>(|w| {
            encode_visibility_report(w, Visibility::NotVisible)
        });
        assert_eq!(b"\x1B[?999;2n".as_slice(), out.as_slice());
    }
}
