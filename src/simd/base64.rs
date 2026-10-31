//! Port of `src/simd/base64.zig`.
//!
//! STATUS: ported
//!
//! Deviation: the Zig SIMD build calls simdutf (`base64.cpp`) for `maxLen`
//! and `decode`. This port always uses the Zig scalar path
//! (`base64_scalar.rs`), so unlike simdutf, ASCII whitespace inside the input
//! is rejected by [`decode`] instead of being skipped. [`decode_strict`] and
//! [`Streaming`] behave identically either way.

use std::fmt;

use super::base64_scalar;
use super::base64_scalar::SCALAR_DECODER;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Base64Invalid,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Base64Invalid")
    }
}

impl std::error::Error for DecodeError {}

pub fn max_len(input: &[u8]) -> usize {
    // The Ghostty application builds with `options.simd`, which uses
    // simdutf's `maximal_binary_length_from_base64`. Mirror that formula:
    // it ignores trailing padding and does not account for whitespace,
    // so it is an upper bound.
    let mut padding = 0;
    if input.last() == Some(&b'=') {
        padding += 1;
        if input.len() >= 2 && input[input.len() - 2] == b'=' {
            padding += 1;
        }
    }
    let actual = input.len() - padding;
    if actual % 4 <= 1 {
        actual / 4 * 3
    } else {
        actual / 4 * 3 + actual % 4 - 1
    }
}

fn max_len_scalar(input: &[u8]) -> usize {
    match SCALAR_DECODER.calc_size_for_slice(scalar_input(input)) {
        Ok(v) => v,
        Err(err) => {
            log::warn!(target: "simd_base64", "failed to calculate base64 size for payload: {}", err);
            0
        }
    }
}

pub fn decode<'a>(input: &[u8], output: &'a mut [u8]) -> Result<&'a [u8], DecodeError> {
    // The SIMD build uses simdutf's `base64_to_binary` (default options),
    // which skips ASCII whitespace and accepts a partial final chunk.
    // Emulate that on top of the scalar decoder.
    const fn is_ws(c: u8) -> bool {
        matches!(c, b' ' | b'\t' | b'\n' | b'\x0c' | b'\r')
    }
    if !input.iter().any(|&c| is_ws(c)) {
        return decode_scalar(input, output);
    }
    let filtered: Vec<u8> = input.iter().copied().filter(|&c| !is_ws(c)).collect();
    let len = decode_scalar(&filtered, output)?.len();
    Ok(&output[..len])
}

fn decode_scalar<'a>(input_raw: &[u8], output: &'a mut [u8]) -> Result<&'a [u8], DecodeError> {
    let input = scalar_input(input_raw);
    let size = max_len_scalar(input);
    if size == 0 {
        return Ok(&output[..0]);
    }
    debug_assert!(output.len() >= size);
    SCALAR_DECODER
        .decode(output, scalar_input(input))
        .map_err(|_| DecodeError::Base64Invalid)?;
    Ok(&output[..size])
}

/// For non-SIMD enabled builds, we trim the padding from the end of the
/// base64 input in order to get identical output with the SIMD version.
fn scalar_input(input: &[u8]) -> &[u8] {
    let mut end = input.len();
    while end > 0 && input[end - 1] == b'=' {
        end -= 1;
    }
    &input[..end]
}

/// Whether strict decoding requires the input to be padded to a
/// multiple of four bytes (RFC 4648 section 3.2). The Kitty clipboard
/// protocol requires padding; the legacy OSC 52 protocol tolerates a
/// missing-padding tail because it has no way to report errors to the
/// client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Padding {
    Required,
    Optional,
}

/// Decode strict RFC 4648 standard-alphabet base64: characters outside
/// the alphabet (including whitespace) and misplaced padding are
/// errors rather than being skipped, and padding is validated per the
/// given requirement. This is the decoding the Kitty clipboard
/// protocol specifies:
/// https://sw.kovidgoyal.net/kitty/clipboard/#encoding-of-payloads
///
/// The output must be at least max_len(input) bytes.
pub fn decode_strict<'a>(
    input: &[u8],
    output: &'a mut [u8],
    padding: Padding,
) -> Result<&'a [u8], DecodeError> {
    // Padding can only be a suffix of at most two bytes; any '='
    // elsewhere is rejected by the underlying decode.
    let mut pad: usize = 0;
    if !input.is_empty() && input[input.len() - 1] == b'=' {
        pad += 1;
    }
    if input.len() > 1 && input[input.len() - 2] == b'=' {
        pad += 1;
    }

    match padding {
        Padding::Required => {
            if input.len() % 4 != 0 {
                return Err(DecodeError::Base64Invalid);
            }
        }
        // Present padding must still complete a four byte group; only
        // fully absent padding is tolerated. A single leftover byte
        // can never carry a decodable value.
        Padding::Optional => {
            if pad > 0 {
                if input.len() % 4 != 0 {
                    return Err(DecodeError::Base64Invalid);
                }
            } else if input.len() % 4 == 1 {
                return Err(DecodeError::Base64Invalid);
            }
        }
    }

    // The permissive decode already rejects every invalid character
    // except whitespace, which simdutf silently skips. Skipped
    // characters make the decoded length fall short of the exact
    // length the input length implies, so comparing the two rejects
    // whitespace without a separate validation pass over the input.
    let decoded = decode(input, output).map_err(|_| DecodeError::Base64Invalid)?;
    let expected = match input.len() % 4 {
        0 => input.len() / 4 * 3 - pad,
        2 => input.len() / 4 * 3 + 1,
        3 => input.len() / 4 * 3 + 2,
        _ => unreachable!(), // rejected above
    };
    if decoded.len() != expected {
        return Err(DecodeError::Base64Invalid);
    }
    Ok(decoded)
}

/// A streaming strict base64 decoder for one logical payload split
/// across multiple chunks at arbitrary byte boundaries (e.g. the Kitty
/// clipboard protocol's wdata packets): the concatenation of the fed
/// chunks must be valid RFC 4648 standard-alphabet base64.
///
/// Padding is terminal within a feed: a padded group followed by more
/// data in the same feed is an error. A feed that ends exactly at
/// terminal padding resets the decoder so the next feed starts a
/// fresh stream, exactly like the kitty reference implementation
/// (which resets its aklomp streaming decoder on EOF); this keeps
/// clients that pad each chunk independently working.
#[derive(Debug, Clone, Copy, Default)]
pub struct Streaming {
    /// Partial group carried between feeds; a group only decodes once
    /// all four of its characters have arrived.
    carry: [u8; 4],
    /// Zig: u3
    carry_len: u8,
}

impl Streaming {
    /// Maximum decoded bytes one feed of input can produce.
    pub fn max_len(&self, input: &[u8]) -> usize {
        (self.carry_len as usize + input.len()) / 4 * 3
    }

    /// Decode the complete groups of input (with any carried bytes
    /// prepended) into output, which must be at least max_len(input)
    /// bytes, and carry the remainder for the next feed.
    pub fn feed<'a>(&mut self, input: &[u8], output: &'a mut [u8]) -> Result<&'a [u8], DecodeError> {
        debug_assert!(output.len() >= self.max_len(input));
        if input.is_empty() {
            return Ok(&output[..0]);
        }

        let mut rem = input;
        let mut written: usize = 0;

        // Complete a carried partial group first.
        if self.carry_len > 0 {
            let carry_len = self.carry_len as usize;
            let take = (4 - carry_len).min(rem.len());
            for (pos, &c) in (carry_len..).zip(rem[..take].iter()) {
                if !valid_partial_char(pos, c) {
                    return Err(DecodeError::Base64Invalid);
                }
            }
            self.carry[carry_len..carry_len + take].copy_from_slice(&rem[..take]);
            self.carry_len += take as u8;
            rem = &rem[take..];
            if self.carry_len < 4 {
                return Ok(&output[..0]);
            }
            self.carry_len = 0;
            let (decoded, padded) = group(self.carry, output)?;
            written += decoded;
            if padded {
                if !rem.is_empty() {
                    return Err(DecodeError::Base64Invalid);
                }
                return Ok(&output[..written]);
            }
        }

        // Bulk-decode all complete groups with the strict single-shot
        // decode, which also enforces that padding only appears as a
        // terminal suffix. Terminal padding must then end the feed.
        let bulk = &rem[..rem.len() - (rem.len() % 4)];
        written += decode_strict(bulk, &mut output[written..], Padding::Required)?.len();
        if !bulk.is_empty() && bulk[bulk.len() - 1] == b'=' {
            if bulk.len() != rem.len() {
                return Err(DecodeError::Base64Invalid);
            }
            return Ok(&output[..written]);
        }

        // Carry the trailing partial group, validated eagerly so
        // garbage is reported on the feed that contains it.
        let tail = &rem[bulk.len()..];
        for (pos, &c) in tail.iter().enumerate() {
            if !valid_partial_char(pos, c) {
                return Err(DecodeError::Base64Invalid);
            }
        }
        self.carry[..tail.len()].copy_from_slice(tail);
        self.carry_len = tail.len() as u8;
        Ok(&output[..written])
    }

    /// Finish the stream: the concatenation of the fed chunks must
    /// have formed complete groups, so a carried partial group is an
    /// error (the stream was not correctly padded). The decoder is
    /// ready for a fresh stream afterwards either way.
    pub fn finish(&mut self) -> Result<(), DecodeError> {
        let carry_len = self.carry_len;
        *self = Streaming::default();
        if carry_len != 0 {
            return Err(DecodeError::Base64Invalid);
        }
        Ok(())
    }
}

/// Decode one complete four character group, the only place
/// padding is legal: the last two characters may be '=' ('=' in
/// the second-to-last position requires it in the last). Returns
/// the decoded length and whether the group was padded.
fn group(g: [u8; 4], output: &mut [u8]) -> Result<(usize, bool), DecodeError> {
    let padded = g[3] == b'=';
    let decoded = decode_strict(&g, output, Padding::Required)?;
    Ok((decoded.len(), padded))
}

/// Whether c is valid at the given position of a partial group
/// still waiting for its remaining characters: the first two
/// positions must be alphabet characters and only the last two
/// may open the padding suffix.
fn valid_partial_char(pos: usize, c: u8) -> bool {
    base64_scalar::is_alphabet_char(c) || (pos >= 2 && c == b'=')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_skips_whitespace_like_simdutf() {
        let input = b"aGVs\nbG8g\r\nd29y bGQ=";
        let mut out = vec![0u8; max_len(input)];
        assert_eq!(decode(input, &mut out).unwrap(), b"hello world");
        assert_eq!(max_len(b"aGVsbG8="), 5);
        assert_eq!(max_len(b"aGVsbG8"), 5);
    }

    #[test]
    fn base64_max_len() {
        let len = max_len(b"aGVsbG8gd29ybGQ=");
        assert_eq!(11, len);
    }

    #[test]
    fn base64_empty_input() {
        let mut output: [u8; 0] = [];
        assert_eq!(0, max_len(b""));
        assert_eq!(b"", decode(b"", &mut output).unwrap());
    }

    #[test]
    fn base64_decode() {
        let input = b"aGVsbG8gd29ybGQ=";
        let len = max_len(input);
        let mut output = vec![0u8; len];
        let s = decode(input, &mut output).unwrap();
        assert_eq!(b"hello world", s);
    }

    #[test]
    fn base64_strict_decode_valid() {
        let mut output = [0u8; 128];
        let cases: &[(&[u8], &[u8])] = &[
            (b"", b""),
            (b"aGVsbG8gd29ybGQ=", b"hello world"),
            (b"bGlnaHQgdw==", b"light w"),
            (b"Zm9vYmFy", b"foobar"),
            // Every alphabet character in one input.
            (
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/",
                b"\x00\x10\x83\x10\x51\x87\x20\x92\x8b\x30\xd3\x8f\x41\x14\x93\x51\x55\x97\x61\x96\x9b\x71\xd7\x9f\x82\x18\xa3\x92\x59\xa7\xa2\x9a\xab\xb2\xdb\xaf\xc3\x1c\xb3\xd3\x5d\xb7\xe3\x9e\xbb\xf3\xdf\xbf",
            ),
        ];
        for (input, expect) in cases {
            assert_eq!(*expect, decode_strict(input, &mut output, Padding::Required).unwrap());
            assert_eq!(*expect, decode_strict(input, &mut output, Padding::Optional).unwrap());
        }
    }

    #[test]
    fn base64_strict_decode_invalid() {
        let mut output = [0u8; 128];

        // The invalid inputs from kitty's own strict decoding tests plus
        // some extra padding-placement cases. All of these are invalid for
        // both padding requirements.
        let cases: &[&[u8]] = &[
            b"bGlnaHQgdw=",    // missing one padding byte
            b"bGln!!Qgdw==",   // invalid characters
            b"bGlnaHQgdw==\n", // trailing whitespace
            b"\nbGlnaHQgdw==", // leading whitespace
            b"bGlnaHQg dw==",  // interior whitespace
            b"!!!!",
            b"=",
            b"==",
            b"A===",
            b"AB=C", // padding must be the suffix
            b"Zm9v YmFy",
        ];
        for case in cases {
            assert_eq!(
                Err(DecodeError::Base64Invalid),
                decode_strict(case, &mut output, Padding::Required)
            );
            assert_eq!(
                Err(DecodeError::Base64Invalid),
                decode_strict(case, &mut output, Padding::Optional)
            );
        }

        // Unpadded input is only tolerated when padding is optional. A
        // single leftover character can never be decoded.
        assert_eq!(
            Err(DecodeError::Base64Invalid),
            decode_strict(b"bGlnaHQgdw", &mut output, Padding::Required)
        );
        assert_eq!(
            b"light w",
            decode_strict(b"bGlnaHQgdw", &mut output, Padding::Optional).unwrap()
        );
        assert_eq!(
            Err(DecodeError::Base64Invalid),
            decode_strict(b"bGl", &mut output, Padding::Required)
        );
        assert_eq!(b"li", decode_strict(b"bGl", &mut output, Padding::Optional).unwrap());
        assert_eq!(
            Err(DecodeError::Base64Invalid),
            decode_strict(b"bGlna", &mut output, Padding::Optional)
        );
    }

    #[test]
    fn base64_streaming_decode_chunk_boundaries() {
        // Decoding a stream split at every possible boundary, including
        // one byte at a time, matches the single-shot decode.
        let input = b"c29tZSBsb25nZXIgZGF0YSB3aXRoIHBhZGRpbmc+Pz8=";
        let expect = b"some longer data with padding>??";
        for split in 0..input.len() + 1 {
            let mut s = Streaming::default();
            let mut result: Vec<u8> = Vec::new();
            let mut output = [0u8; 64];
            result.extend_from_slice(s.feed(&input[..split], &mut output).unwrap());
            result.extend_from_slice(s.feed(&input[split..], &mut output).unwrap());
            s.finish().unwrap();
            assert_eq!(&expect[..], &result[..]);
        }
        {
            let mut s = Streaming::default();
            let mut result: Vec<u8> = Vec::new();
            let mut output = [0u8; 4];
            for i in 0..input.len() {
                result.extend_from_slice(s.feed(&input[i..i + 1], &mut output).unwrap());
            }
            s.finish().unwrap();
            assert_eq!(&expect[..], &result[..]);
        }
    }

    #[test]
    fn base64_streaming_decode_invalid() {
        let mut output = [0u8; 64];
        let invalid = Err(DecodeError::Base64Invalid);

        // Invalid characters are rejected wherever they appear.
        {
            let mut s = Streaming::default();
            assert_eq!(invalid, s.feed(b"!!!!", &mut output).map(|v| v.to_vec()));
        }
        {
            let mut s = Streaming::default();
            assert_eq!(invalid, s.feed(b"SGVs!!!bG8=", &mut output).map(|v| v.to_vec()));
        }
        {
            let mut s = Streaming::default();
            assert_eq!(invalid, s.feed(b"\nc29tZSBkYXRh", &mut output).map(|v| v.to_vec()));
        }

        // Data after terminal padding within one feed is rejected, even
        // when the padded group only completes in that feed.
        {
            let mut s = Streaming::default();
            assert_eq!(invalid, s.feed(b"Z29vZA==SGVsbG8=", &mut output).map(|v| v.to_vec()));
        }
        {
            let mut s = Streaming::default();
            s.feed(b"Z29vZA=", &mut output).unwrap();
            assert_eq!(invalid, s.feed(b"=SGVs", &mut output).map(|v| v.to_vec()));
        }

        // A feed that ends exactly at terminal padding resets the stream:
        // the next feed starts fresh, so clients that pad every chunk
        // independently keep working (matching the kitty implementation,
        // which resets its streaming decoder on EOF).
        {
            let mut s = Streaming::default();
            assert_eq!(b"good", s.feed(b"Z29vZA==", &mut output).unwrap());
            assert_eq!(b"Hello", s.feed(b"SGVsbG8=", &mut output).unwrap());
            s.finish().unwrap();
        }
        {
            // Padding split across feeds resets too.
            let mut s = Streaming::default();
            assert_eq!(b"goo", s.feed(b"Z29vZA=", &mut output).unwrap());
            assert_eq!(b"d", s.feed(b"=", &mut output).unwrap());
            assert_eq!(b"more", s.feed(b"bW9yZQ==", &mut output).unwrap());
            s.finish().unwrap();
        }

        // Misplaced padding within a group.
        {
            let mut s = Streaming::default();
            assert_eq!(invalid, s.feed(b"YQ=X", &mut output).map(|v| v.to_vec()));
        }
        {
            let mut s = Streaming::default();
            assert_eq!(invalid, s.feed(b"=AAA", &mut output).map(|v| v.to_vec()));
        }

        // A stream that ends in a partial group is missing its padding.
        // The failed finish resets the decoder for the next stream.
        {
            let mut s = Streaming::default();
            assert_eq!(b"Hel", s.feed(b"SGVsbG8", &mut output).unwrap());
            assert_eq!(Err(DecodeError::Base64Invalid), s.finish());
            assert_eq!(b"Hel", s.feed(b"SGVsbG8", &mut output).unwrap());
            assert_eq!(b"lo", s.feed(b"=", &mut output).unwrap());
            s.finish().unwrap();
        }
    }
}
