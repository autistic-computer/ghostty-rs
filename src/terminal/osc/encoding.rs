//! Port of `src/terminal/osc/encoding.zig`.
//!
//! STATUS: ported
//!
//! Specialized encodings used in some OSC protocols.
//!
//! This file additionally hosts [`base64`], a port of the subset of Zig's
//! `std.base64` decoder used by the OSC parsers (Zig's standard library has
//! no Rust counterpart with identical strictness rules).

/// Kitty defines "Escape code safe UTF-8" as valid UTF-8 with the
/// additional requirement of not containing any C0 escape codes
/// (0x00-0x1f), DEL (0x7f) and C1 escape codes (0x80-0x9f).
///
/// Used by OSC 66 (text sizing) and OSC 99 (Kitty notifications).
///
/// See: https://sw.kovidgoyal.net/kitty/desktop-notifications/#safe-utf8
pub fn is_safe_utf8(s: &[u8]) -> bool {
    let Ok(utf8) = std::str::from_utf8(s) else {
        return false;
    };

    for cp in utf8.chars() {
        match cp as u32 {
            0x00..=0x1f | 0x7f | 0x80..=0x9f => return false,
            _ => {}
        }
    }

    true
}

/// Port of Zig's `std.base64.standard` / `standard_no_pad` decoders.
pub(crate) mod base64 {
    /// Zig `std.base64.Error`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Error {
        InvalidCharacter,
        InvalidPadding,
        NoSpaceLeft,
    }

    const INVALID: u8 = 0xff;

    const fn char_to_index() -> [u8; 256] {
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut result = [INVALID; 256];
        let mut i = 0;
        while i < alphabet.len() {
            result[alphabet[i] as usize] = i as u8;
            i += 1;
        }
        result
    }

    const CHAR_TO_INDEX: [u8; 256] = char_to_index();

    /// A base64 decoder using the standard alphabet.
    #[derive(Debug, Clone, Copy)]
    pub struct Decoder {
        pub pad_char: Option<u8>,
    }

    /// `std.base64.standard.Decoder`
    pub const STANDARD: Decoder = Decoder { pad_char: Some(b'=') };

    /// `std.base64.standard_no_pad.Decoder`
    pub const STANDARD_NO_PAD: Decoder = Decoder { pad_char: None };

    impl Decoder {
        /// Return the maximum possible decoded size for a given input length.
        pub fn calc_size_upper_bound(&self, source_len: usize) -> Result<usize, Error> {
            let mut result = source_len / 4 * 3;
            let leftover = source_len % 4;
            if self.pad_char.is_some() {
                if leftover % 4 != 0 {
                    return Err(Error::InvalidPadding);
                }
            } else {
                if leftover % 4 == 1 {
                    return Err(Error::InvalidPadding);
                }
                result += leftover * 3 / 4;
            }
            Ok(result)
        }

        /// Return the exact decoded size for a slice.
        pub fn calc_size_for_slice(&self, source: &[u8]) -> Result<usize, Error> {
            let source_len = source.len();
            let mut result = self.calc_size_upper_bound(source_len)?;
            if let Some(pad_char) = self.pad_char {
                if source_len >= 1 && source[source_len - 1] == pad_char {
                    result -= 1;
                }
                if source_len >= 2 && source[source_len - 2] == pad_char {
                    result -= 1;
                }
            }
            Ok(result)
        }

        /// Decode `source` into `dest`. `dest.len()` must be what
        /// `calc_size_for_slice` returned.
        pub fn decode(&self, dest: &mut [u8], source: &[u8]) -> Result<(), Error> {
            if self.pad_char.is_some() && source.len() % 4 != 0 {
                return Err(Error::InvalidPadding);
            }
            let mut dest_idx: usize = 0;
            let mut acc: u32 = 0;
            let mut acc_len: u32 = 0;
            let mut leftover_idx: Option<usize> = None;
            for (src_idx, &c) in source.iter().enumerate() {
                let d = CHAR_TO_INDEX[c as usize];
                if d == INVALID {
                    if self.pad_char.is_none() || Some(c) != self.pad_char {
                        return Err(Error::InvalidCharacter);
                    }
                    leftover_idx = Some(src_idx);
                    break;
                }
                acc = ((acc << 6) + d as u32) & 0xfff;
                acc_len += 6;
                if acc_len >= 8 {
                    acc_len -= 8;
                    if dest_idx >= dest.len() {
                        return Err(Error::NoSpaceLeft);
                    }
                    dest[dest_idx] = (acc >> acc_len) as u8;
                    dest_idx += 1;
                }
            }
            if acc_len > 4 || (acc & ((1u32 << acc_len) - 1)) != 0 {
                return Err(Error::InvalidPadding);
            }
            let Some(leftover_idx) = leftover_idx else {
                return Ok(());
            };
            let leftover = &source[leftover_idx..];
            if let Some(pad_char) = self.pad_char {
                let padding_len = acc_len / 2;
                let mut padding_chars: u32 = 0;
                for &c in leftover {
                    if c != pad_char {
                        return Err(if CHAR_TO_INDEX[c as usize] == INVALID {
                            Error::InvalidCharacter
                        } else {
                            Error::InvalidPadding
                        });
                    }
                    padding_chars += 1;
                }
                if padding_chars != padding_len {
                    return Err(Error::InvalidPadding);
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_utf8() {
        assert!(is_safe_utf8(b"Hello world!"));
        assert!(is_safe_utf8("安全的ユニコード☀️".as_bytes()));
        assert!(!is_safe_utf8(b"No linebreaks\nallowed"));
        assert!(!is_safe_utf8(b"\x07no bells"));
        assert!(!is_safe_utf8(b"\x1b]9;no OSCs\x1b\\\x1b[m"));
        assert!(!is_safe_utf8(b"\x9f8-bit escapes are clever, but no"));
    }

    #[test]
    fn base64_decode() {
        let d = base64::STANDARD;
        let mut buf = [0u8; 16];
        let n = d.calc_size_for_slice(b"UGxhbg==").unwrap();
        assert_eq!(4, n);
        d.decode(&mut buf[..n], b"UGxhbg==").unwrap();
        assert_eq!(b"Plan", &buf[..n]);

        let d = base64::STANDARD_NO_PAD;
        let n = d.calc_size_for_slice(b"UGxhbg").unwrap();
        assert_eq!(4, n);
        d.decode(&mut buf[..n], b"UGxhbg").unwrap();
        assert_eq!(b"Plan", &buf[..n]);

        assert!(d.calc_size_for_slice(b"a").is_err());
        assert!(base64::STANDARD.calc_size_for_slice(b"QQ=").is_err());
    }
}
