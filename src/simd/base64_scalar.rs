//! Port of `src/simd/base64_scalar.zig`.
//!
//! STATUS: ported

use std::fmt;

/// The standard base64 alphabet (`std.base64.standard_alphabet_chars`).
const STANDARD_ALPHABET_CHARS: [u8; 64] =
    *b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub static SCALAR_DECODER: Base64Decoder = Base64Decoder::init(STANDARD_ALPHABET_CHARS, None);

/// Whether c is one of the 64 standard base64 alphabet characters
/// (padding is not part of the alphabet).
pub fn is_alphabet_char(c: u8) -> bool {
    SCALAR_DECODER.char_to_index[c as usize] != Base64Decoder::INVALID_CHAR
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base64DecoderError {
    InvalidCharacter,
    InvalidPadding,
    NoSpaceLeft,
}

impl fmt::Display for Base64DecoderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCharacter => f.write_str("InvalidCharacter"),
            Self::InvalidPadding => f.write_str("InvalidPadding"),
            Self::NoSpaceLeft => f.write_str("NoSpaceLeft"),
        }
    }
}

impl std::error::Error for Base64DecoderError {}

/// Copied from Zig 0.14.1 stdlib and commented out the invalid padding
/// scenarios, because Kitty Graphics requires a decoder that doesn't care
/// about invalid padding scenarios.
pub struct Base64Decoder {
    /// e.g. 'A' => 0.
    /// `INVALID_CHAR` for any value not in the 64 alphabet chars.
    char_to_index: [u8; 256],
    fast_char_to_index: [[u32; 256]; 4],
    pad_char: Option<u8>,
}

impl Base64Decoder {
    const INVALID_CHAR: u8 = 0xff;
    const INVALID_CHAR_TST: u32 = 0xff000000;

    pub const fn init(alphabet_chars: [u8; 64], pad_char: Option<u8>) -> Base64Decoder {
        let mut result = Base64Decoder {
            char_to_index: [Self::INVALID_CHAR; 256],
            fast_char_to_index: [[Self::INVALID_CHAR_TST; 256]; 4],
            pad_char,
        };

        let mut char_in_alphabet = [false; 256];
        let mut i = 0;
        while i < 64 {
            let c = alphabet_chars[i] as usize;
            assert!(!char_in_alphabet[c]);
            assert!(match pad_char {
                None => true,
                Some(p) => c != p as usize,
            });

            let ci = i as u32;
            result.fast_char_to_index[0][c] = ci << 2;
            result.fast_char_to_index[1][c] = (ci >> 4) | ((ci & 0x0f) << 12);
            result.fast_char_to_index[2][c] = ((ci & 0x3) << 22) | ((ci & 0x3c) << 6);
            result.fast_char_to_index[3][c] = ci << 16;

            result.char_to_index[c] = i as u8;
            char_in_alphabet[c] = true;
            i += 1;
        }
        result
    }

    /// Return the maximum possible decoded size for a given input length - The
    /// actual length may be less if the input includes padding.
    /// `InvalidPadding` is returned if the input length is not valid.
    pub fn calc_size_upper_bound(&self, source_len: usize) -> Result<usize, Base64DecoderError> {
        let mut result = source_len / 4 * 3;
        let leftover = source_len % 4;
        if self.pad_char.is_some() {
            if leftover % 4 != 0 {
                return Err(Base64DecoderError::InvalidPadding);
            }
        } else {
            if leftover % 4 == 1 {
                return Err(Base64DecoderError::InvalidPadding);
            }
            result += leftover * 3 / 4;
        }
        Ok(result)
    }

    /// Return the exact decoded size for a slice.
    /// `InvalidPadding` is returned if the input length is not valid.
    pub fn calc_size_for_slice(&self, source: &[u8]) -> Result<usize, Base64DecoderError> {
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

    /// dest.len must be what you get from ::calc_size.
    /// Invalid characters result in `InvalidCharacter`.
    /// Invalid padding results in `InvalidPadding`.
    pub fn decode(&self, dest: &mut [u8], source: &[u8]) -> Result<(), Base64DecoderError> {
        if self.pad_char.is_some() && source.len() % 4 != 0 {
            return Err(Base64DecoderError::InvalidPadding);
        }
        let mut dest_idx: usize = 0;
        let mut fast_src_idx: usize = 0;
        // Zig: u12
        let mut acc: u16 = 0;
        // Zig: u4
        let mut acc_len: u8 = 0;
        let mut leftover_idx: Option<usize> = None;
        while fast_src_idx + 16 < source.len() && dest_idx + 15 < dest.len() {
            let mut bits: u128 = 0;
            for i in 0..4 {
                let mut new_bits: u128 =
                    self.fast_char_to_index[0][source[fast_src_idx + i * 4] as usize] as u128;
                new_bits |= self.fast_char_to_index[1][source[fast_src_idx + 1 + i * 4] as usize] as u128;
                new_bits |= self.fast_char_to_index[2][source[fast_src_idx + 2 + i * 4] as usize] as u128;
                new_bits |= self.fast_char_to_index[3][source[fast_src_idx + 3 + i * 4] as usize] as u128;
                if (new_bits & Self::INVALID_CHAR_TST as u128) != 0 {
                    return Err(Base64DecoderError::InvalidCharacter);
                }
                bits |= new_bits << (24 * i);
            }
            dest[dest_idx..dest_idx + 16].copy_from_slice(&bits.to_le_bytes());
            fast_src_idx += 16;
            dest_idx += 12;
        }
        while fast_src_idx + 4 < source.len() && dest_idx + 3 < dest.len() {
            let mut bits = self.fast_char_to_index[0][source[fast_src_idx] as usize];
            bits |= self.fast_char_to_index[1][source[fast_src_idx + 1] as usize];
            bits |= self.fast_char_to_index[2][source[fast_src_idx + 2] as usize];
            bits |= self.fast_char_to_index[3][source[fast_src_idx + 3] as usize];
            if (bits & Self::INVALID_CHAR_TST) != 0 {
                return Err(Base64DecoderError::InvalidCharacter);
            }
            dest[dest_idx..dest_idx + 4].copy_from_slice(&bits.to_le_bytes());
            fast_src_idx += 4;
            dest_idx += 3;
        }
        let remaining = &source[fast_src_idx..];
        for (src_idx, &c) in (fast_src_idx..).zip(remaining.iter()) {
            let d = self.char_to_index[c as usize];
            if d == Self::INVALID_CHAR {
                if self.pad_char != Some(c) {
                    return Err(Base64DecoderError::InvalidCharacter);
                }
                leftover_idx = Some(src_idx);
                break;
            }
            acc = ((acc << 6) & 0xFFF) + d as u16;
            acc_len += 6;
            if acc_len >= 8 {
                acc_len -= 8;
                dest[dest_idx] = (acc >> acc_len) as u8;
                dest_idx += 1;
            }
        }
        // if (acc_len > 4 or (acc & (@as(u12, 1) << acc_len) - 1) != 0) {
        //     return error.InvalidPadding;
        // }
        let Some(leftover_idx) = leftover_idx else {
            return Ok(());
        };
        let leftover = &source[leftover_idx..];
        if let Some(pad_char) = self.pad_char {
            let padding_len = (acc_len / 2) as usize;
            let mut padding_chars: usize = 0;
            for &c in leftover {
                if c != pad_char {
                    return if c == Self::INVALID_CHAR {
                        Err(Base64DecoderError::InvalidCharacter)
                    } else {
                        Err(Base64DecoderError::InvalidPadding)
                    };
                }
                padding_chars += 1;
            }
            if padding_chars != padding_len {
                return Err(Base64DecoderError::InvalidPadding);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alphabet() {
        for &c in STANDARD_ALPHABET_CHARS.iter() {
            assert!(is_alphabet_char(c));
        }
        assert!(!is_alphabet_char(b'='));
        assert!(!is_alphabet_char(b' '));
        assert!(!is_alphabet_char(b'-'));
    }

    #[test]
    fn decode_long() {
        let input = b"VGhlIHF1aWNrIGJyb3duIGZveCBqdW1wcyBvdmVyIHRoZSBsYXp5IGRvZw";
        let len = SCALAR_DECODER.calc_size_for_slice(input).unwrap();
        let mut out = vec![0u8; len];
        SCALAR_DECODER.decode(&mut out, input).unwrap();
        assert_eq!(b"The quick brown fox jumps over the lazy dog", &out[..]);
    }
}
