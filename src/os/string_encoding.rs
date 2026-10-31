//! Port of `src/os/string_encoding.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::io::{self, Write};

#[derive(Debug)]
pub enum DecodeError {
    /// The input was not validly encoded.
    DecodeError,
    /// Writing to the output failed.
    Write(io::Error),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::DecodeError => f.write_str("DecodeError"),
            DecodeError::Write(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<io::Error> for DecodeError {
    fn from(e: io::Error) -> Self {
        DecodeError::Write(e)
    }
}

impl DecodeError {
    pub fn is_decode_error(&self) -> bool {
        matches!(self, DecodeError::DecodeError)
    }
}

/// Decode data from the buffer that has been encoded in the same way that
/// `bash`'s `printf %q` encodes a string and write it to the writer. If an
/// error is returned garbage may have been written to the writer.
pub fn printf_q_decode<W: Write + ?Sized>(writer: &mut W, buf: &[u8]) -> Result<(), DecodeError> {
    let data: &[u8] = if buf.starts_with(b"$'") {
        // Strip off `$''` quoting.
        if buf.len() < 3 || !buf.ends_with(b"'") {
            return Err(DecodeError::DecodeError);
        }
        &buf[2..buf.len() - 1]
    } else if buf.starts_with(b"'") {
        // Strip off `''` quoting.
        if buf.len() < 2 || !buf.ends_with(b"'") {
            return Err(DecodeError::DecodeError);
        }
        &buf[1..buf.len() - 1]
    } else {
        buf
    };

    let mut src = 0usize;

    while src < data.len() {
        match data[src] {
            b'\\' => {
                if src + 1 >= data.len() {
                    return Err(DecodeError::DecodeError);
                }
                match data[src + 1] {
                    c @ (b' ' | b'\\' | b'"' | b'\'' | b'$') => {
                        writer.write_all(&[c])?;
                        src += 2;
                    }
                    b'e' => {
                        writer.write_all(&[0x1b])?;
                        src += 2;
                    }
                    b'n' => {
                        writer.write_all(b"\n")?;
                        src += 2;
                    }
                    b'r' => {
                        writer.write_all(b"\r")?;
                        src += 2;
                    }
                    b't' => {
                        writer.write_all(b"\t")?;
                        src += 2;
                    }
                    b'v' => {
                        writer.write_all(&[0x0b])?;
                        src += 2;
                    }
                    _ => return Err(DecodeError::DecodeError),
                }
            }
            c => {
                writer.write_all(&[c])?;
                src += 1;
            }
        }
    }

    Ok(())
}

/// Decode data from the buffer that has been URL percent encoded and write
/// it to the given writer. If an error is returned the garbage may have been
/// written to the writer.
pub fn url_percent_decode<W: Write + ?Sized>(
    writer: &mut W,
    buf: &[u8],
) -> Result<(), DecodeError> {
    let mut src = 0usize;
    while src < buf.len() {
        match buf[src] {
            b'%' => {
                if src + 2 >= buf.len() {
                    return Err(DecodeError::DecodeError);
                }
                if !buf[src + 1].is_ascii_hexdigit() || !buf[src + 2].is_ascii_hexdigit() {
                    return Err(DecodeError::DecodeError);
                }
                writer.write_all(&[(hex(buf[src + 1]) << 4) | hex(buf[src + 2])])?;
                src += 3;
            }
            c => {
                writer.write_all(&[c])?;
                src += 1;
            }
        }
    }
    Ok(())
}

#[inline]
fn hex(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => unreachable!(),
    }
}

/// Is the given character valid in URI percent encoding?
fn is_valid_char(c: u8) -> bool {
    match c {
        b' ' | b';' | b'=' => false,
        _ => (0x20..=0x7e).contains(&c),
    }
}

/// Write data to the writer after URI percent encoding.
/// Zig: `std.Uri.Component.percentEncode(writer, data, isValidChar)`.
pub fn url_percent_encode<W: Write + ?Sized>(writer: &mut W, data: &[u8]) -> io::Result<()> {
    super::uri::percent_encode(writer, data, is_valid_char)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(s: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut w = Vec::new();
        printf_q_decode(&mut w, s)?;
        Ok(w)
    }

    #[test]
    fn printf_q_1() {
        assert_eq!(b"bobr kurwa", &decode(b"bobr\\ kurwa").unwrap()[..]);
    }

    #[test]
    fn printf_q_2() {
        assert_eq!(b"bobr\nkurwa", &decode(b"bobr\\nkurwa").unwrap()[..]);
    }

    #[test]
    fn printf_q_3() {
        assert!(decode(b"bobr\\dkurwa").unwrap_err().is_decode_error());
    }

    #[test]
    fn printf_q_4() {
        assert!(decode(b"bobr kurwa\\").unwrap_err().is_decode_error());
    }

    #[test]
    fn printf_q_5() {
        assert_eq!(b"bobr kurwa", &decode(b"$'bobr kurwa'").unwrap()[..]);
    }

    #[test]
    fn printf_q_6() {
        assert_eq!(b"bobr kurwa", &decode(b"'bobr kurwa'").unwrap()[..]);
    }

    #[test]
    fn printf_q_7() {
        assert!(decode(b"$'bobr kurwa").unwrap_err().is_decode_error());
    }

    #[test]
    fn printf_q_8() {
        assert!(decode(b"$'").unwrap_err().is_decode_error());
    }

    #[test]
    fn printf_q_9() {
        assert!(decode(b"'bobr kurwa").unwrap_err().is_decode_error());
    }

    #[test]
    fn printf_q_10() {
        assert!(decode(b"'").unwrap_err().is_decode_error());
    }

    fn pdecode(s: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut w = Vec::new();
        url_percent_decode(&mut w, s)?;
        Ok(w)
    }

    #[test]
    fn singles_percent() {
        for c in 0..255u32 {
            let buf = format!("%{c:02x}");
            let decoded = pdecode(buf.as_bytes()).unwrap();
            assert_eq!(1, decoded.len());
            assert_eq!(c as u8, decoded[0]);
        }
        for c in 0..255u32 {
            let buf = format!("%{c:02X}");
            let decoded = pdecode(buf.as_bytes()).unwrap();
            assert_eq!(1, decoded.len());
            assert_eq!(c as u8, decoded[0]);
        }
    }

    #[test]
    fn percent_1() {
        assert_eq!(b"bobr kurwa", &pdecode(b"bobr%20kurwa").unwrap()[..]);
    }

    #[test]
    fn percent_2() {
        assert!(pdecode(b"bobr%2kurwa").unwrap_err().is_decode_error());
    }

    #[test]
    fn percent_3() {
        assert!(pdecode(b"bobr%kurwa").unwrap_err().is_decode_error());
    }

    #[test]
    fn percent_4() {
        assert!(pdecode(b"bobr%%kurwa").unwrap_err().is_decode_error());
    }

    #[test]
    fn percent_5() {
        assert_eq!(b"bobr kurwa ", &pdecode(b"bobr%20kurwa%20").unwrap()[..]);
    }

    #[test]
    fn percent_6() {
        assert!(pdecode(b"bobr%20kurwa%2").unwrap_err().is_decode_error());
    }

    #[test]
    fn percent_7() {
        assert!(pdecode(b"bobr%20kurwa%").unwrap_err().is_decode_error());
    }

    #[test]
    fn percent_encode_roundtrip() {
        let mut w = Vec::new();
        url_percent_encode(&mut w, b"a b;c=d\n/e").unwrap();
        assert_eq!(b"a%20b%3Bc%3Dd%0A/e", &w[..]);
        assert_eq!(b"a b;c=d\n/e", &pdecode(&w).unwrap()[..]);
    }
}
