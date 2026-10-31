//! Port of `src/config/string.zig`.
//!
//! STATUS: ported
//!
//! This file also contains a port of the parts of Zig's
//! `std.zig.string_literal` that Ghostty depends on
//! ([`parse_escape_sequence`] and [`parse_write`]), since Rust has no
//! equivalent in its standard library.

use std::fmt;

/// Errors returned by [`parse`] and [`CodepointIterator::next`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    InvalidString,
    /// From Zig's `std.unicode.utf8Encode` when a `\u{...}` escape encodes
    /// a UTF-16 surrogate half.
    Utf8CannotEncodeSurrogateHalf,
}

impl Error {
    /// The Zig error name.
    pub fn name(self) -> &'static str {
        match self {
            Error::OutOfMemory => "OutOfMemory",
            Error::InvalidString => "InvalidString",
            Error::Utf8CannotEncodeSurrogateHalf => "Utf8CannotEncodeSurrogateHalf",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error.{}", self.name())
    }
}

impl std::error::Error for Error {}

/// Port of Zig's `std.zig.string_literal.parseEscapeSequence`.
///
/// `bytes[*offset]` must be a backslash. On success `offset` is advanced
/// past the escape sequence and the escaped codepoint (or byte value for
/// `\xNN`) is returned. Zig asserts that the backslash isn't the last
/// byte; we return a failure instead.
pub fn parse_escape_sequence(bytes: &[u8], offset: &mut usize) -> Result<u32, ()> {
    debug_assert_eq!(bytes[*offset], b'\\');
    if bytes.len() <= *offset + 1 {
        return Err(());
    }

    *offset += 2;
    match bytes[*offset - 1] {
        b'n' => Ok(b'\n' as u32),
        b'r' => Ok(b'\r' as u32),
        b'\\' => Ok(b'\\' as u32),
        b't' => Ok(b'\t' as u32),
        b'\'' => Ok(b'\'' as u32),
        b'"' => Ok(b'"' as u32),
        b'x' => {
            let mut value: u32 = 0;
            let mut i = *offset;
            while i < *offset + 2 {
                if i == bytes.len() {
                    return Err(());
                }
                let c = bytes[i];
                value = value * 16 + hex_digit(c).ok_or(())?;
                i += 1;
            }
            *offset = i;
            Ok(value)
        }
        b'u' => {
            let mut i = *offset;
            if i >= bytes.len() || bytes[i] != b'{' {
                return Err(());
            }
            i += 1;
            if i >= bytes.len() {
                return Err(());
            }
            if bytes[i] == b'}' {
                return Err(());
            }

            let mut value: u32 = 0;
            loop {
                if i >= bytes.len() {
                    // expected_rbrace
                    return Err(());
                }
                let c = bytes[i];
                if c == b'}' {
                    i += 1;
                    break;
                }
                value = value * 16 + hex_digit(c).ok_or(())?;
                if value > 0x10ffff {
                    return Err(());
                }
                i += 1;
            }
            *offset = i;
            Ok(value)
        }
        _ => Err(()),
    }
}

fn hex_digit(c: u8) -> Option<u32> {
    match c {
        b'0'..=b'9' => Some((c - b'0') as u32),
        b'a'..=b'f' => Some((c - b'a' + 10) as u32),
        b'A'..=b'F' => Some((c - b'A' + 10) as u32),
        _ => None,
    }
}

/// Port of Zig's `std.unicode.utf8Encode`. Returns the number of bytes
/// written.
fn utf8_encode(cp: u32, out: &mut [u8]) -> Result<usize, Error> {
    if (0xd800..=0xdfff).contains(&cp) {
        return Err(Error::Utf8CannotEncodeSurrogateHalf);
    }
    let c = char::from_u32(cp).ok_or(Error::InvalidString)?;
    let len = c.len_utf8();
    if out.len() < len {
        // Zig asserts here; we treat it as running out of output space.
        return Err(Error::OutOfMemory);
    }
    c.encode_utf8(&mut out[..len]);
    Ok(len)
}

/// Port of Zig's `std.zig.string_literal.parseWrite`. `bytes` must be a
/// double-quoted Zig string literal (including the quotes). Returns the
/// decoded bytes or `Err(())` if the literal is invalid.
pub fn parse_write(bytes: &[u8]) -> Result<Vec<u8>, ()> {
    debug_assert!(bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"');
    let mut out = Vec::with_capacity(bytes.len());
    let mut index: usize = 1;
    loop {
        // Zig would trip a bounds check here (e.g. `"abc\"`); report failure.
        let Some(&b) = bytes.get(index) else {
            return Err(());
        };
        match b {
            b'\\' => {
                let escape_char_index = index + 1;
                let codepoint = parse_escape_sequence(bytes, &mut index)?;
                if bytes[escape_char_index] == b'u' {
                    let mut buf = [0u8; 4];
                    let len = utf8_encode(codepoint, &mut buf).map_err(|_| ())?;
                    out.extend_from_slice(&buf[..len]);
                } else {
                    out.push(codepoint as u8);
                }
            }
            b'\n' => return Err(()),
            b'"' => return Ok(out),
            _ => {
                out.push(b);
                index += 1;
            }
        }
    }
}

/// Parse a string literal into a byte array. The string can contain
/// any valid Zig string literal escape sequences.
///
/// The output buffer never needs to be larger than the input buffer.
/// (The Zig buffers may alias; use [`parse_in_place`] for that.)
pub fn parse<'a>(out: &'a mut [u8], bytes: &[u8]) -> Result<&'a mut [u8], Error> {
    let mut dst_i: usize = 0;
    let mut src_i: usize = 0;
    while src_i < bytes.len() {
        if dst_i >= out.len() {
            return Err(Error::OutOfMemory);
        }

        // If this byte is not beginning an escape sequence we copy.
        let b = bytes[src_i];
        if b != b'\\' {
            out[dst_i] = b;
            dst_i += 1;
            src_i += 1;
            continue;
        }

        // Zig's \xNN syntax represents a single byte, while the other
        // escape sequences represent Unicode codepoints.
        let is_byte_escape = src_i + 1 < bytes.len() && bytes[src_i + 1] == b'x';

        // Parse the escape sequence
        match parse_escape_sequence(bytes, &mut src_i) {
            Err(()) => return Err(Error::InvalidString),
            Ok(cp) => {
                if is_byte_escape {
                    out[dst_i] = cp as u8;
                    dst_i += 1;
                } else {
                    dst_i += utf8_encode(cp, &mut out[dst_i..])?;
                }
            }
        }
    }

    Ok(&mut out[..dst_i])
}

/// Like [`parse`] but with the output aliasing the input (the Zig function
/// allows the buffers to alias). Returns the length of the parsed value.
pub fn parse_in_place(buf: &mut [u8]) -> Result<usize, Error> {
    let src = buf.to_vec();
    Ok(parse(buf, &src)?.len())
}

/// Creates an iterator that requires no allocation to extract codepoints
/// from the string literal, parsing escape sequences as it goes.
pub fn codepoint_iterator(bytes: &[u8]) -> CodepointIterator<'_> {
    CodepointIterator { bytes, i: 0 }
}

#[derive(Debug, Clone)]
pub struct CodepointIterator<'a> {
    pub bytes: &'a [u8],
    pub i: usize,
}

impl CodepointIterator<'_> {
    /// Returns the next codepoint (Zig `u21`).
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<u32>, Error> {
        if self.i >= self.bytes.len() {
            return Ok(None);
        }
        match self.bytes[self.i] {
            // An escape sequence
            b'\\' => match parse_escape_sequence(self.bytes, &mut self.i) {
                Err(()) => Err(Error::InvalidString),
                Ok(cp) => Ok(Some(cp)),
            },

            // Not an escape, parse as UTF-8
            start => {
                let cp_len = utf8_byte_sequence_length(start).ok_or(Error::InvalidString)?;
                if cp_len > self.bytes.len() - self.i {
                    return Err(Error::InvalidString);
                }

                let seq = &self.bytes[self.i..self.i + cp_len];
                self.i += cp_len;
                let s = std::str::from_utf8(seq).map_err(|_| Error::InvalidString)?;
                Ok(Some(s.chars().next().ok_or(Error::InvalidString)? as u32))
            }
        }
    }
}

/// Port of Zig's `std.unicode.utf8ByteSequenceLength`.
fn utf8_byte_sequence_length(first_byte: u8) -> Option<usize> {
    match first_byte {
        0b0000_0000..=0b0111_1111 => Some(1),
        0b1100_0000..=0b1101_1111 => Some(2),
        0b1110_0000..=0b1110_1111 => Some(3),
        0b1111_0000..=0b1111_0111 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty() {
        let mut buf = [0u8; 128];
        let result = parse(&mut buf, b"").unwrap();
        assert_eq!(b"", &result[..]);
    }

    #[test]
    fn parse_no_escapes() {
        let mut buf = [0u8; 128];
        let result = parse(&mut buf, b"hello world").unwrap();
        assert_eq!(b"hello world", &result[..]);
    }

    #[test]
    fn parse_escapes() {
        let mut buf = [0u8; 128];
        {
            let result = parse(&mut buf, b"hello\\nworld").unwrap();
            assert_eq!(b"hello\nworld", &result[..]);
        }
        {
            let result = parse(&mut buf, b"hello\\u{1F601}world").unwrap();
            assert_eq!("hello\u{1F601}world".as_bytes(), &result[..]);
        }
    }

    #[test]
    fn parse_hex_escapes_are_bytes() {
        let mut buf = [0u8; 128];
        let result = parse(&mut buf, b"\\xe6\\x97\\xa5\\xe6\\x9c\\xac\\xe8\\xaa\\x9e").unwrap();
        assert_eq!("日本語".as_bytes(), &result[..]);
    }

    #[test]
    fn codepoint_iterator_empty() {
        let mut it = codepoint_iterator(b"");
        assert_eq!(None, it.next().unwrap());
    }

    #[test]
    fn codepoint_iterator_ascii_no_escapes() {
        let mut it = codepoint_iterator(b"abc");
        assert_eq!('a' as u32, it.next().unwrap().unwrap());
        assert_eq!('b' as u32, it.next().unwrap().unwrap());
        assert_eq!('c' as u32, it.next().unwrap().unwrap());
        assert_eq!(None, it.next().unwrap());
    }

    #[test]
    fn codepoint_iterator_multibyte_utf8() {
        // │ is U+2502 (3 bytes in UTF-8)
        let mut it = codepoint_iterator("a│b".as_bytes());
        assert_eq!('a' as u32, it.next().unwrap().unwrap());
        assert_eq!('│' as u32, it.next().unwrap().unwrap());
        assert_eq!('b' as u32, it.next().unwrap().unwrap());
        assert_eq!(None, it.next().unwrap());
    }

    #[test]
    fn codepoint_iterator_escape_sequences() {
        let mut it = codepoint_iterator(b"a\\tb\\n\\\\");
        assert_eq!('a' as u32, it.next().unwrap().unwrap());
        assert_eq!('\t' as u32, it.next().unwrap().unwrap());
        assert_eq!('b' as u32, it.next().unwrap().unwrap());
        assert_eq!('\n' as u32, it.next().unwrap().unwrap());
        assert_eq!('\\' as u32, it.next().unwrap().unwrap());
        assert_eq!(None, it.next().unwrap());
    }

    #[test]
    fn codepoint_iterator_unicode_escape() {
        let mut it = codepoint_iterator(b"\\u{2502}x");
        assert_eq!('│' as u32, it.next().unwrap().unwrap());
        assert_eq!('x' as u32, it.next().unwrap().unwrap());
        assert_eq!(None, it.next().unwrap());
    }

    #[test]
    fn codepoint_iterator_emoji_unicode_escape() {
        let mut it = codepoint_iterator(b"\\u{1F601}");
        assert_eq!(0x1F601, it.next().unwrap().unwrap());
        assert_eq!(None, it.next().unwrap());
    }

    #[test]
    fn codepoint_iterator_truncated_utf_8() {
        for bytes in [
            &b"\xc2"[..],
            &b"\xe2"[..],
            &b"\xe2\x94"[..],
            &b"\xf0"[..],
            &b"\xf0\x9f"[..],
            &b"\xf0\x9f\x98"[..],
        ] {
            let mut it = codepoint_iterator(bytes);
            assert_eq!(Err(Error::InvalidString), it.next());
        }
    }

    #[test]
    fn codepoint_iterator_truncated_utf_8_after_valid_prefix() {
        let mut it = codepoint_iterator(b"a\xc2");
        assert_eq!('a' as u32, it.next().unwrap().unwrap());
        assert_eq!(Err(Error::InvalidString), it.next());
    }

    #[test]
    fn codepoint_iterator_valid_two_byte_utf_8_at_end() {
        let mut it = codepoint_iterator(b"\xc2\x80");
        assert_eq!(0x80, it.next().unwrap().unwrap());
        assert_eq!(None, it.next().unwrap());
    }
}
