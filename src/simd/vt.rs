//! Port of `src/simd/vt.zig`.
//!
//! STATUS: ported
//!
//! The Zig SIMD build calls `vt.cpp` (Highway + simdutf). This port replaces
//! it with a Rust implementation of the same structure: an ASCII fast path
//! that fuses the ESC scan with the UTF-8 -> UTF-32 widening 16 bytes at a
//! time (SSE2 on x86_64, NEON on aarch64, SWAR elsewhere), falling back on the first non-ASCII
//! byte to a `memchr` ESC search plus the scalar maximal-subpart decoder.
//! Results are identical to `utf8_decode_until_control_seq_scalar` (checked
//! by the differential test, as in Zig).

use super::index_of::index_of;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeResult {
    pub consumed: usize,
    pub decoded: usize,
}

/// Decode the UTF-8 text in input into output until an escape
/// character is found. Returns the number of bytes consumed from input
/// and the number of decoded codepoints written to output.
///
/// This may consume less than the full input even with no escape
/// character if the input ends with an incomplete (but valid so far) UTF-8
/// sequence; the caller should keep those bytes for the next call.
/// Ill-formed sequences are replaced with U+FFFD following the W3C/Unicode
/// "U+FFFD Substitution of Maximal Subparts" algorithm.
///
/// `output` must be at least as long as `input`.
#[inline]
pub fn utf8_decode_until_control_seq(input: &[u8], output: &mut [u32]) -> DecodeResult {
    utf8_decode_until_control_seq_with_ascii(input, output).0
}

/// The flag is true only when every decoded codepoint is ASCII. It is
/// derived from the decoder's existing checks, including invalid UTF-8.
#[inline]
pub(crate) fn utf8_decode_until_control_seq_with_ascii(
    input: &[u8],
    output: &mut [u32],
) -> (DecodeResult, bool) {
    debug_assert!(output.len() >= input.len());

    // ASCII fast path: terminal input is overwhelmingly ASCII, for which
    // UTF-8 decoding is a simple widening of each byte to 32 bits. We
    // fuse the ESC scan with the decode, one chunk at a time, and only
    // fall back to the full UTF-8 decoder when we encounter a non-ASCII
    // byte. This avoids a second pass over the input.
    let count = input.len();
    let mut i = 0;
    while i + CHUNK <= count {
        let chunk: &[u8; CHUNK] = input[i..i + CHUNK].try_into().unwrap();
        let out: &mut [u32; CHUNK] = (&mut output[i..i + CHUNK]).try_into().unwrap();

        // Widen the whole chunk unconditionally: output is guaranteed to
        // be at least as large as input, and if we stop mid-chunk only
        // the prefix is reported (the rest is scratch that the caller
        // never reads).
        let stop_mask = widen_ascii_chunk(chunk, out);
        if stop_mask == 0 {
            i += CHUNK;
            continue;
        }

        let stop_idx = i + (stop_mask.trailing_zeros() / STOP_BITS_PER_BYTE) as usize;
        if input[stop_idx] == 0x1B {
            // ESC: everything before it was ASCII, one codepoint per byte.
            return (
                DecodeResult {
                    consumed: stop_idx,
                    decoded: stop_idx,
                },
                true,
            );
        }

        // Non-ASCII: decode the rest (up to an ESC) with the full decoder.
        return (
            decode_non_ascii_until_control_seq(input, stop_idx, output),
            false,
        );
    }

    // Leftover input (< CHUNK bytes): process one byte at a time.
    while i < count {
        let b = input[i];
        if b == 0x1B {
            return (
                DecodeResult {
                    consumed: i,
                    decoded: i,
                },
                true,
            );
        }
        if b >= 0x80 {
            return (decode_non_ascii_until_control_seq(input, i, output), false);
        }
        output[i] = b as u32;
        i += 1;
    }

    // The entire input was ASCII (no ESC, no partial sequences possible).
    (
        DecodeResult {
            consumed: count,
            decoded: count,
        },
        true,
    )
}

const CHUNK: usize = 16;
const STOP_BITS_PER_BYTE: u32 = if cfg!(all(
    target_arch = "aarch64",
    target_feature = "neon",
    target_endian = "little"
)) { 4 } else { 1 };

/// Widen 16 bytes into `out` and mark ESC or any byte >= 0x80.
/// Each input byte occupies STOP_BITS_PER_BYTE consecutive flag bits.
/// Only zero/nonzero and the first flagged byte are used by the decoder.
#[inline(always)]
fn widen_ascii_chunk(chunk: &[u8; CHUNK], out: &mut [u32; CHUNK]) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::*;
        // SAFETY: SSE2 is part of the x86_64 baseline. All loads and stores
        // are unaligned and in bounds of the 16-byte input and the 16-element
        // (64-byte) output arrays.
        unsafe {
            let v = _mm_loadu_si128(chunk.as_ptr() as *const __m128i);
            let esc = _mm_cmpeq_epi8(v, _mm_set1_epi8(0x1B));
            let mask = (_mm_movemask_epi8(v) | _mm_movemask_epi8(esc)) as u32;

            let zero = _mm_setzero_si128();
            let lo = _mm_unpacklo_epi8(v, zero);
            let hi = _mm_unpackhi_epi8(v, zero);
            let p = out.as_mut_ptr() as *mut __m128i;
            _mm_storeu_si128(p, _mm_unpacklo_epi16(lo, zero));
            _mm_storeu_si128(p.add(1), _mm_unpackhi_epi16(lo, zero));
            _mm_storeu_si128(p.add(2), _mm_unpacklo_epi16(hi, zero));
            _mm_storeu_si128(p.add(3), _mm_unpackhi_epi16(hi, zero));
            u64::from(mask)
        }
    }

    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    {
        use std::arch::aarch64::*;
        // SAFETY: NEON is enabled. The input and output arrays contain
        // 16 bytes and 16 u32s respectively; every load and store fits.
        unsafe {
            let v = vld1q_u8(chunk.as_ptr());
            let stop = vorrq_u8(vceqq_u8(v, vdupq_n_u8(0x1B)), vcgeq_u8(v, vdupq_n_u8(0x80)));
            // Four flag bits per byte preserve the first stop position
            // without the two horizontal sums needed for a compact mask.
            #[cfg(target_endian = "little")]
            let mask = vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(
                vreinterpretq_u16_u8(stop),
            )));
            #[cfg(target_endian = "big")]
            let mask = {
                let weights = [1u8, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
                let bits = vandq_u8(stop, vld1q_u8(weights.as_ptr()));
                u64::from(vaddv_u8(vget_low_u8(bits)))
                    | (u64::from(vaddv_u8(vget_high_u8(bits))) << 8)
            };

            let lo = vmovl_u8(vget_low_u8(v));
            let hi = vmovl_u8(vget_high_u8(v));
            let p = out.as_mut_ptr();
            vst1q_u32(p, vmovl_u16(vget_low_u16(lo)));
            vst1q_u32(p.add(4), vmovl_u16(vget_high_u16(lo)));
            vst1q_u32(p.add(8), vmovl_u16(vget_low_u16(hi)));
            vst1q_u32(p.add(12), vmovl_u16(vget_high_u16(hi)));
            mask
        }
    }

    #[cfg(not(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_feature = "neon")
    )))]
    {
        u64::from(widen_ascii_chunk_portable(chunk, out))
    }
}

/// Portable (SWAR) version of [`widen_ascii_chunk`].
#[cfg(any(
    not(any(target_arch = "x86_64", all(target_arch = "aarch64", target_feature = "neon"))),
    test
))]
#[inline(always)]
fn widen_ascii_chunk_portable(chunk: &[u8; CHUNK], out: &mut [u32; CHUNK]) -> u32 {
    for (o, &b) in out.iter_mut().zip(chunk.iter()) {
        *o = b as u32;
    }
    let lo = u64::from_le_bytes(chunk[..8].try_into().unwrap());
    let hi = u64::from_le_bytes(chunk[8..].try_into().unwrap());
    (swar_stop_mask(lo) | (swar_stop_mask(hi) << 8)) as u32
}

/// For 8 little-endian bytes, a bitmask with bit k set for bytes that are
/// ESC or >= 0x80. Bits above the first set bit may be spurious (borrow
/// propagation), which is fine since only the lowest set bit is used.
#[cfg(any(
    not(any(target_arch = "x86_64", all(target_arch = "aarch64", target_feature = "neon"))),
    test
))]
#[inline(always)]
fn swar_stop_mask(w: u64) -> u64 {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    let x = w ^ (LO * 0x1B);
    let esc = x.wrapping_sub(LO) & !x & HI;
    let stop = (w & HI) | esc;
    // Compress the high bit of each byte into one bit per byte.
    let mut mask = 0u64;
    for k in 0..8 {
        mask |= ((stop >> (8 * k + 7)) & 1) << k;
    }
    mask
}

/// The general (non-ASCII) portion of `utf8_decode_until_control_seq`.
/// Scans for ESC starting at byte offset `base` and decodes
/// input[base..stop). The caller must have already decoded input[0..base)
/// as ASCII into output[0..base) (one codepoint per byte).
#[inline(never)]
fn decode_non_ascii_until_control_seq(input: &[u8], base: usize, output: &mut [u32]) -> DecodeResult {
    let rest = &input[base..];
    let idx = index_of(rest, 0x1B).unwrap_or(rest.len());
    let r = decode_region(&rest[..idx], idx == rest.len(), &mut output[base..]);
    DecodeResult {
        consumed: base + r.consumed,
        decoded: base + r.decoded,
    }
}

#[allow(dead_code)]
fn utf8_decode_until_control_seq_scalar(input: &[u8], output: &mut [u32]) -> DecodeResult {
    // Find our escape
    let idx = index_of(input, 0x1B).unwrap_or(input.len());
    let decode = &input[..idx];
    decode_scalar(decode, decode.len() == input.len(), output, 0, 0, decode.len())
}

/// Decodes `decode` into `output`: well-formed blocks with the validated
/// SIMD decoder, everything else with the scalar loop.
/// `at_input_end` is true when `decode` ends at the true end of the input
/// (`decode.len == input.len` in Zig), i.e. it was not bounded by an ESC.
fn decode_region(decode: &[u8], at_input_end: bool, output: &mut [u32]) -> DecodeResult {
    let (offset, count) = validated::decode_blocks(decode, output);
    decode_scalar(decode, at_input_end, output, offset, count, decode.len())
}

/// The decode loop of the Zig `utf8DecodeUntilControlSeqScalar`, resumable:
/// it starts at byte `decode_offset` (a code point boundary) with
/// `decode_count` codepoints already in `output`, and returns at the first
/// boundary at or after `limit`.
fn decode_scalar(
    decode: &[u8],
    at_input_end: bool,
    output: &mut [u32],
    mut decode_offset: usize,
    mut decode_count: usize,
    limit: usize,
) -> DecodeResult {
    // Go through and decode one item at a time, following the W3C/Unicode
    // "U+FFFD Substitution of Maximal Subparts" algorithm for ill-formed
    // subsequences.
    while decode_offset < limit {
        let b0 = decode[decode_offset];

        // ASCII fast path.
        if b0 < 0x80 {
            while decode_offset + 8 <= decode.len() {
                let w = u64::from_le_bytes(decode[decode_offset..decode_offset + 8].try_into().unwrap());
                if w & 0x8080_8080_8080_8080 != 0 {
                    break;
                }
                for k in 0..8 {
                    output[decode_count + k] = decode[decode_offset + k] as u32;
                }
                decode_count += 8;
                decode_offset += 8;
            }
            while decode_offset < decode.len() {
                let b = decode[decode_offset];
                if b >= 0x80 {
                    break;
                }
                output[decode_count] = b as u32;
                decode_count += 1;
                decode_offset += 1;
            }
            continue;
        }

        // Continuation byte (80-BF) or invalid byte (C0-C1, F5-FF)
        // as lead: each is its own maximal subpart → one FFFD per byte.
        if !(0xC2..=0xF4).contains(&b0) {
            output[decode_count] = 0xFFFD;
            decode_count += 1;
            decode_offset += 1;
            continue;
        }

        // Multi-byte sequence. Only the first continuation byte has a
        // lead-dependent valid range per Unicode Table 3-7; later
        // continuation bytes are always 80-BF. Range validity per
        // Table 3-7 excludes overlong, surrogate, and out-of-range
        // encodings, so a fully valid sequence can be decoded by
        // direct bit assembly with no further checks.
        let seq_len: usize = if b0 < 0xE0 {
            2
        } else if b0 < 0xF0 {
            3
        } else {
            4
        };
        let (cb1_lo, cb1_hi): (u8, u8) = match b0 {
            0xE0 => (0xA0, 0xBF),
            0xED => (0x80, 0x9F),
            0xF0 => (0x90, 0xBF),
            0xF4 => (0x80, 0x8F),
            _ => (0x80, 0xBF),
        };

        // Check how many continuation bytes form a valid prefix (the
        // maximal subpart), accumulating codepoint bits as we go. The
        // lead byte contributes its low 7-len bits.
        let mut cp: u32 = (b0 as u32) & (0x7F >> seq_len);
        let mut valid: usize = 1; // lead byte is valid
        while valid < seq_len {
            if decode_offset + valid >= decode.len() {
                // The sequence is cut off by the end of the decode
                // region. If the region ends at the true end of the
                // input then it may be completed by future input, so
                // stop without consuming these bytes. If the region
                // was bounded by an ESC then the sequence can never
                // be completed; the valid-so-far prefix is a maximal
                // subpart which maps to a single U+FFFD below.
                if at_input_end {
                    return DecodeResult {
                        consumed: decode_offset,
                        decoded: decode_count,
                    };
                }
                break;
            }
            let cb = decode[decode_offset + valid];
            let lo: u8 = if valid == 1 { cb1_lo } else { 0x80 };
            let hi: u8 = if valid == 1 { cb1_hi } else { 0xBF };
            if cb < lo || cb > hi {
                break;
            }
            cp = (cp << 6) | (cb as u32 & 0x3F);
            valid += 1;
        }

        if valid == seq_len {
            output[decode_count] = cp;
            decode_count += 1;
            decode_offset += seq_len;
        } else {
            // Incomplete/ill-formed: the maximal subpart (valid bytes)
            // maps to a single FFFD.
            output[decode_count] = 0xFFFD;
            decode_count += 1;
            decode_offset += valid;
        }
    }

    DecodeResult {
        consumed: decode_offset,
        decoded: decode_count,
    }
}

/// Bulk decoding of well-formed UTF-8.
///
/// Differs from Zig, which calls simdutf's `convert_utf8_to_utf32_with_errors`
/// from vt.cpp: this is a Rust version of the same two steps, using 128-bit
/// vectors on x86_64 (SSSE3, detected at run time) and aarch64 (NEON).
/// The scalar loop was the largest cost of printing non-ASCII text.
///
/// 1. Validate a 64-byte block with the lookup algorithm from Keiser and
///    Lemire, "Validating UTF-8 In Less Than One Instruction Per Byte".
/// 2. Convert the block's complete code points 12 bytes at a time, picking a
///    byte shuffle from the positions where code points end.
///
/// A block that fails validation goes through `decode_scalar` instead, so
/// ill-formed input keeps its exact maximal-subpart replacement. Blocks start
/// at code point boundaries, so each is validated on its own.
mod validated {
    use super::decode_scalar;

    const BLOCK: usize = 64;

    /// Input a block needs past its start: the block itself plus the 16-byte
    /// load and 16-codepoint store of a 12-byte window starting near its end.
    const MARGIN: usize = BLOCK + 16;

    /// Decodes whole blocks from the start of `input` while at least MARGIN
    /// bytes remain. Returns the bytes consumed and the codepoints written,
    /// both ending at a code point boundary. `output` must be at least as
    /// long as `input`.
    #[inline]
    pub(super) fn decode_blocks(input: &[u8], output: &mut [u32]) -> (usize, usize) {
        assert!(output.len() >= input.len());
        if input.len() < MARGIN {
            return (0, 0);
        }
        decode_blocks_native(input, output)
    }

    #[cfg(target_arch = "x86_64")]
    #[inline]
    fn decode_blocks_native(input: &[u8], output: &mut [u32]) -> (usize, usize) {
        if std::is_x86_feature_detected!("ssse3") {
            // SAFETY: SSSE3 is available, and output is long enough.
            unsafe { x86::decode_blocks(input, output) }
        } else {
            (0, 0)
        }
    }

    #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
    #[inline]
    fn decode_blocks_native(input: &[u8], output: &mut [u32]) -> (usize, usize) {
        // SAFETY: NEON is enabled, and output is long enough.
        unsafe { decode_blocks_with::<neon::V>(input, output) }
    }

    #[cfg(not(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_feature = "neon", target_endian = "little")
    )))]
    #[inline]
    fn decode_blocks_native(_input: &[u8], _output: &mut [u32]) -> (usize, usize) {
        (0, 0)
    }

    // Error bits of the lookup algorithm. Each flags an ill-formed pair of a
    // byte and the byte before it.
    const TOO_SHORT: u8 = 1 << 0; // 11______ 0_______, 11______ 11______
    const TOO_LONG: u8 = 1 << 1; // 0_______ 10______
    const OVERLONG_3: u8 = 1 << 2; // 11100000 100_____
    const TOO_LARGE: u8 = 1 << 3; // 11110100 1001____ and above
    const SURROGATE: u8 = 1 << 4; // 11101101 101_____
    const OVERLONG_2: u8 = 1 << 5; // 1100000_ 10______
    const TOO_LARGE_1000: u8 = 1 << 6; // 11110101 1000____ and above
    const OVERLONG_4: u8 = 1 << 6; // 11110000 1000____
    const TWO_CONTS: u8 = 1 << 7; // 10______ 10______
    const CARRY: u8 = TOO_SHORT | TOO_LONG | TWO_CONTS;

    /// Indexed by the high nibble of the previous byte.
    const BYTE_1_HIGH: [u8; 16] = [
        // 0_______: ASCII
        TOO_LONG, TOO_LONG, TOO_LONG, TOO_LONG, TOO_LONG, TOO_LONG, TOO_LONG, TOO_LONG,
        // 10______: continuation
        TWO_CONTS, TWO_CONTS, TWO_CONTS, TWO_CONTS,
        // 1100____, 1101____: two-byte lead
        TOO_SHORT | OVERLONG_2,
        TOO_SHORT,
        // 1110____: three-byte lead
        TOO_SHORT | OVERLONG_3 | SURROGATE,
        // 1111____: four-byte lead
        TOO_SHORT | TOO_LARGE | TOO_LARGE_1000 | OVERLONG_4,
    ];

    /// Indexed by the low nibble of the previous byte.
    const BYTE_1_LOW: [u8; 16] = [
        CARRY | OVERLONG_3 | OVERLONG_2 | OVERLONG_4,
        CARRY | OVERLONG_2,
        CARRY,
        CARRY,
        CARRY | TOO_LARGE,
        CARRY | TOO_LARGE | TOO_LARGE_1000,
        CARRY | TOO_LARGE | TOO_LARGE_1000,
        CARRY | TOO_LARGE | TOO_LARGE_1000,
        CARRY | TOO_LARGE | TOO_LARGE_1000,
        CARRY | TOO_LARGE | TOO_LARGE_1000,
        CARRY | TOO_LARGE | TOO_LARGE_1000,
        CARRY | TOO_LARGE | TOO_LARGE_1000,
        CARRY | TOO_LARGE | TOO_LARGE_1000,
        CARRY | TOO_LARGE | TOO_LARGE_1000 | SURROGATE,
        CARRY | TOO_LARGE | TOO_LARGE_1000,
        CARRY | TOO_LARGE | TOO_LARGE_1000,
    ];

    /// Indexed by the high nibble of the byte itself.
    const BYTE_2_HIGH: [u8; 16] = [
        // 0_______
        TOO_SHORT, TOO_SHORT, TOO_SHORT, TOO_SHORT, TOO_SHORT, TOO_SHORT, TOO_SHORT, TOO_SHORT,
        // 1000____
        TOO_LONG | OVERLONG_2 | TWO_CONTS | OVERLONG_3 | TOO_LARGE_1000 | OVERLONG_4,
        // 1001____
        TOO_LONG | OVERLONG_2 | TWO_CONTS | OVERLONG_3 | TOO_LARGE,
        // 101_____
        TOO_LONG | OVERLONG_2 | TWO_CONTS | SURROGATE | TOO_LARGE,
        TOO_LONG | OVERLONG_2 | TWO_CONTS | SURROGATE | TOO_LARGE,
        // 11______
        TOO_SHORT, TOO_SHORT, TOO_SHORT, TOO_SHORT,
    ];

    // A 12-byte window starting at a code point boundary holds at least three
    // complete code points. Its conversion writes one of three lane layouts,
    // each selected by a shuffle that puts every code point's bytes in one
    // lane, last byte lowest:
    //   index 0..64:    six code points of 1-2 bytes in 16-bit lanes,
    //   index 64..145:  four code points of 1-3 bytes in 32-bit lanes,
    //   index 145..209: three code points of 1-4 bytes in 32-bit lanes.
    // Within a layout the index encodes the code point lengths in base 2, 3
    // or 4, first code point in the lowest digit.
    const LAYOUT_3: usize = 64;
    const LAYOUT_4: usize = 145;
    const LAYOUTS_END: usize = 209;

    /// The shuffle for each layout index; entries past LAYOUTS_END are unused
    /// and make any u8 index in bounds.
    static SHUFFLES: [[u8; 16]; 256] = shuffles();

    /// For each 12-bit mask of code point ends (bit i set when byte i is the
    /// last byte of a code point), the layout index and the bytes consumed.
    /// Masks no well-formed window produces map to LAYOUTS_END and 0.
    pub(super) static WINDOWS: [[u8; 2]; 4096] = windows();

    const fn shuffles() -> [[u8; 16]; 256] {
        let mut table = [[0xFF; 16]; 256];
        let mut idx = 0;
        while idx < LAYOUTS_END {
            let (cps, lane_bytes, radix, mut code) = if idx < LAYOUT_3 {
                (6, 2, 2, idx)
            } else if idx < LAYOUT_4 {
                (4, 4, 3, idx - LAYOUT_3)
            } else {
                (3, 4, 4, idx - LAYOUT_4)
            };
            let mut start = 0;
            let mut k = 0;
            while k < cps {
                let len = code % radix + 1;
                code /= radix;
                let mut j = 0;
                while j < len {
                    // The j-th byte from the end of code point k.
                    table[idx][k * lane_bytes + j] = (start + len - 1 - j) as u8;
                    j += 1;
                }
                start += len;
                k += 1;
            }
            idx += 1;
        }
        table
    }

    const fn windows() -> [[u8; 2]; 4096] {
        let mut table = [[LAYOUTS_END as u8, 0]; 4096];
        let mut mask = 0;
        while mask < 4096 {
            let mut lens = [0usize; 12];
            let mut n = 0;
            let mut start = 0;
            let mut i = 0;
            while i < 12 {
                if mask >> i & 1 == 1 {
                    lens[n] = i + 1 - start;
                    n += 1;
                    start = i + 1;
                }
                i += 1;
            }
            // (code points, max length, radix, first index) for each layout.
            let layouts = [(6, 2, 2, 0), (4, 3, 3, LAYOUT_3), (3, 4, 4, LAYOUT_4)];
            let mut l = 0;
            while l < layouts.len() {
                let (cps, max_len, radix, first) = layouts[l];
                let mut fits = n >= cps;
                let mut code = 0;
                let mut consumed = 0;
                let mut weight = 1;
                let mut k = 0;
                while fits && k < cps {
                    fits = lens[k] <= max_len;
                    code += (lens[k] - 1) * weight;
                    consumed += lens[k];
                    weight *= radix;
                    k += 1;
                }
                if fits {
                    table[mask] = [(first + code) as u8, consumed as u8];
                    break;
                }
                l += 1;
            }
            mask += 1;
        }
        table
    }

    /// The vector operations the block decoder needs. Every method is
    /// unsafe: callers must have the CPU feature the implementation uses.
    trait Simd: Copy {
        unsafe fn load(p: *const u8) -> Self;
        unsafe fn splat(b: u8) -> Self;
        unsafe fn and(self, o: Self) -> Self;
        unsafe fn or(self, o: Self) -> Self;
        unsafe fn xor(self, o: Self) -> Self;
        /// Each byte shifted right by four.
        unsafe fn shr4(self) -> Self;
        /// Looks each byte of `idx` (0..16) up in the 16-byte table `self`.
        unsafe fn lookup(self, idx: Self) -> Self;
        /// The input shifted by one, two or three bytes, the first bytes
        /// taken from the end of `prev`.
        unsafe fn prev1(self, prev: Self) -> Self;
        unsafe fn prev2(self, prev: Self) -> Self;
        unsafe fn prev3(self, prev: Self) -> Self;
        unsafe fn saturating_sub(self, b: u8) -> Self;
        unsafe fn any_nonzero(self) -> bool;
        unsafe fn any_high_bit(self) -> bool;
        /// Bit i set when byte i of the 64 is not a continuation byte.
        unsafe fn lead_mask(block: &[Self; 4]) -> u64;
        /// Widens 16 ASCII bytes to 16 codepoints.
        unsafe fn store_ascii(self, out: *mut u32);
        /// Converts the well-formed code points that end in the 12 bytes at
        /// `p`. `ends` has bit i set when byte i ends a code point. Reads 16
        /// bytes and writes up to 16 codepoints. Returns the bytes consumed
        /// and the codepoints decoded; (0, 0) for a window no well-formed
        /// input produces.
        unsafe fn convert(p: *const u8, ends: u64, out: *mut u32) -> (usize, usize);
    }

    /// The lookup algorithm's error bits for one 16-byte chunk, given the
    /// chunk before it.
    #[inline(always)]
    unsafe fn check<S: Simd>(input: S, prev_input: S, tables: &[S; 3]) -> S {
        let prev1 = input.prev1(prev_input);
        let special = tables[0]
            .lookup(prev1.shr4())
            .and(tables[1].lookup(prev1.and(S::splat(0x0F))))
            .and(tables[2].lookup(input.shr4()));
        // Third and fourth bytes of a sequence must be continuations: only
        // 111_____ two bytes back or 1111____ three back saturate to >= 0x80.
        let must23 = input
            .prev2(prev_input)
            .saturating_sub(0xE0 - 0x80)
            .or(input.prev3(prev_input).saturating_sub(0xF0 - 0x80));
        must23.and(S::splat(0x80)).xor(special)
    }

    #[inline(always)]
    unsafe fn decode_blocks_with<S: Simd>(input: &[u8], output: &mut [u32]) -> (usize, usize) {
        let src = input.as_ptr();
        let dst = output.as_mut_ptr();
        let tables = [
            S::load(BYTE_1_HIGH.as_ptr()),
            S::load(BYTE_1_LOW.as_ptr()),
            S::load(BYTE_2_HIGH.as_ptr()),
        ];
        let zero = S::splat(0);

        // Every codepoint takes at least one byte, so count <= pos and the
        // stores below stay within output. All loads stay within input:
        // pos + MARGIN <= input.len().
        let mut pos = 0;
        let mut count = 0;
        while pos + MARGIN <= input.len() {
            let p = src.add(pos);
            let block = [S::load(p), S::load(p.add(16)), S::load(p.add(32)), S::load(p.add(48))];

            if !block[0].or(block[1]).or(block[2]).or(block[3]).any_high_bit() {
                for (k, chunk) in block.iter().enumerate() {
                    chunk.store_ascii(dst.add(count + 16 * k));
                }
                pos += BLOCK;
                count += BLOCK;
                continue;
            }

            // The block starts at a code point boundary, so the chunk before
            // it can be taken as ASCII. A sequence cut off by the end of the
            // block is not an error here: it is never converted and starts
            // the next block.
            let errors = check(block[0], zero, &tables)
                .or(check(block[1], block[0], &tables))
                .or(check(block[2], block[1], &tables))
                .or(check(block[3], block[2], &tables));
            if errors.any_nonzero() {
                let r = decode_scalar(input, false, output, pos, count, pos + BLOCK);
                pos = r.consumed;
                count = r.decoded;
                continue;
            }

            // Byte i ends a code point when byte i + 1 starts one. The last
            // byte's successor is outside the block, so no window converts it.
            let mut ends = S::lead_mask(&block) >> 1;
            let stop = pos + BLOCK - 12;
            while pos < stop {
                let (consumed, decoded) = S::convert(src.add(pos), ends, dst.add(count));
                if consumed == 0 {
                    // Not reachable for a validated block; stay correct anyway.
                    debug_assert!(false, "validated block has an invalid window");
                    let r = decode_scalar(input, false, output, pos, count, pos + BLOCK);
                    pos = r.consumed;
                    count = r.decoded;
                    break;
                }
                pos += consumed;
                count += decoded;
                ends >>= consumed;
            }
        }
        (pos, count)
    }

    #[cfg(target_arch = "x86_64")]
    mod x86 {
        use super::{Simd, LAYOUTS_END, LAYOUT_3, LAYOUT_4, SHUFFLES, WINDOWS};
        use std::arch::x86_64::*;

        #[derive(Clone, Copy)]
        pub(super) struct V(__m128i);

        /// SAFETY: the CPU must support SSSE3 and `output` must be at least
        /// as long as `input`.
        #[target_feature(enable = "ssse3")]
        pub(super) unsafe fn decode_blocks(input: &[u8], output: &mut [u32]) -> (usize, usize) {
            super::decode_blocks_with::<V>(input, output)
        }

        /// Composes four code points of 1-3 bytes from 32-bit lanes.
        #[inline(always)]
        unsafe fn compose3(perm: __m128i) -> __m128i {
            let ascii = _mm_and_si128(perm, _mm_set1_epi32(0x7F));
            let middle = _mm_srli_epi32::<2>(_mm_and_si128(perm, _mm_set1_epi32(0x3F00)));
            let high = _mm_srli_epi32::<4>(_mm_and_si128(perm, _mm_set1_epi32(0x0F_0000)));
            _mm_or_si128(_mm_or_si128(ascii, middle), high)
        }

        impl Simd for V {
            #[inline(always)]
            unsafe fn load(p: *const u8) -> Self {
                V(_mm_loadu_si128(p as *const __m128i))
            }
            #[inline(always)]
            unsafe fn splat(b: u8) -> Self {
                V(_mm_set1_epi8(b as i8))
            }
            #[inline(always)]
            unsafe fn and(self, o: Self) -> Self {
                V(_mm_and_si128(self.0, o.0))
            }
            #[inline(always)]
            unsafe fn or(self, o: Self) -> Self {
                V(_mm_or_si128(self.0, o.0))
            }
            #[inline(always)]
            unsafe fn xor(self, o: Self) -> Self {
                V(_mm_xor_si128(self.0, o.0))
            }
            #[inline(always)]
            unsafe fn shr4(self) -> Self {
                V(_mm_and_si128(_mm_srli_epi16::<4>(self.0), _mm_set1_epi8(0x0F)))
            }
            #[inline(always)]
            unsafe fn lookup(self, idx: Self) -> Self {
                V(_mm_shuffle_epi8(self.0, idx.0))
            }
            #[inline(always)]
            unsafe fn prev1(self, prev: Self) -> Self {
                V(_mm_alignr_epi8::<15>(self.0, prev.0))
            }
            #[inline(always)]
            unsafe fn prev2(self, prev: Self) -> Self {
                V(_mm_alignr_epi8::<14>(self.0, prev.0))
            }
            #[inline(always)]
            unsafe fn prev3(self, prev: Self) -> Self {
                V(_mm_alignr_epi8::<13>(self.0, prev.0))
            }
            #[inline(always)]
            unsafe fn saturating_sub(self, b: u8) -> Self {
                V(_mm_subs_epu8(self.0, _mm_set1_epi8(b as i8)))
            }
            #[inline(always)]
            unsafe fn any_nonzero(self) -> bool {
                _mm_movemask_epi8(_mm_cmpeq_epi8(self.0, _mm_setzero_si128())) != 0xFFFF
            }
            #[inline(always)]
            unsafe fn any_high_bit(self) -> bool {
                _mm_movemask_epi8(self.0) != 0
            }
            #[inline(always)]
            unsafe fn lead_mask(block: &[Self; 4]) -> u64 {
                // Continuation bytes are -128..=-65 as i8.
                let lead = _mm_set1_epi8(-65);
                let m0 = _mm_movemask_epi8(_mm_cmpgt_epi8(block[0].0, lead)) as u16 as u64;
                let m1 = _mm_movemask_epi8(_mm_cmpgt_epi8(block[1].0, lead)) as u16 as u64;
                let m2 = _mm_movemask_epi8(_mm_cmpgt_epi8(block[2].0, lead)) as u16 as u64;
                let m3 = _mm_movemask_epi8(_mm_cmpgt_epi8(block[3].0, lead)) as u16 as u64;
                m0 | m1 << 16 | m2 << 32 | m3 << 48
            }
            #[inline(always)]
            unsafe fn store_ascii(self, out: *mut u32) {
                let zero = _mm_setzero_si128();
                let lo = _mm_unpacklo_epi8(self.0, zero);
                let hi = _mm_unpackhi_epi8(self.0, zero);
                let p = out as *mut __m128i;
                _mm_storeu_si128(p, _mm_unpacklo_epi16(lo, zero));
                _mm_storeu_si128(p.add(1), _mm_unpackhi_epi16(lo, zero));
                _mm_storeu_si128(p.add(2), _mm_unpacklo_epi16(hi, zero));
                _mm_storeu_si128(p.add(3), _mm_unpackhi_epi16(hi, zero));
            }
            #[inline(always)]
            unsafe fn convert(p: *const u8, ends: u64, out: *mut u32) -> (usize, usize) {
                let input = _mm_loadu_si128(p as *const __m128i);
                let mask = (ends & 0xFFF) as usize;
                let o = out as *mut __m128i;
                if mask == 0xFFF {
                    // Twelve ASCII bytes.
                    V(input).store_ascii(out);
                    return (12, 12);
                }
                if mask == 0x924 {
                    // Four three-byte code points, as in CJK text.
                    let sh = _mm_setr_epi8(2, 1, 0, -1, 5, 4, 3, -1, 8, 7, 6, -1, 11, 10, 9, -1);
                    _mm_storeu_si128(o, compose3(_mm_shuffle_epi8(input, sh)));
                    return (12, 4);
                }
                let [idx, consumed] = WINDOWS[mask];
                let idx = idx as usize;
                let sh = _mm_loadu_si128(SHUFFLES[idx].as_ptr() as *const __m128i);
                let perm = _mm_shuffle_epi8(input, sh);
                if idx < LAYOUT_3 {
                    let ascii = _mm_and_si128(perm, _mm_set1_epi16(0x7F));
                    let high = _mm_srli_epi16::<2>(_mm_and_si128(perm, _mm_set1_epi16(0x1F00)));
                    let composed = _mm_or_si128(ascii, high);
                    let zero = _mm_setzero_si128();
                    _mm_storeu_si128(o, _mm_unpacklo_epi16(composed, zero));
                    _mm_storeu_si128(o.add(1), _mm_unpackhi_epi16(composed, zero));
                    (consumed as usize, 6)
                } else if idx < LAYOUT_4 {
                    _mm_storeu_si128(o, compose3(perm));
                    (consumed as usize, 4)
                } else if idx < LAYOUTS_END {
                    let ascii = _mm_and_si128(perm, _mm_set1_epi32(0x7F));
                    let middle = _mm_srli_epi32::<2>(_mm_and_si128(perm, _mm_set1_epi32(0x3F00)));
                    // The third byte from the end is a continuation or a
                    // three-byte lead 1110____, whose bit 5 must be cleared:
                    // only leads have bit 6 set.
                    let third = _mm_and_si128(perm, _mm_set1_epi32(0x3F_0000));
                    let lead_bit = _mm_srli_epi32::<1>(_mm_and_si128(perm, _mm_set1_epi32(0x40_0000)));
                    let third = _mm_srli_epi32::<4>(_mm_xor_si128(third, lead_bit));
                    let fourth = _mm_srli_epi32::<6>(_mm_and_si128(perm, _mm_set1_epi32(0x0700_0000)));
                    let composed = _mm_or_si128(_mm_or_si128(ascii, middle), _mm_or_si128(third, fourth));
                    _mm_storeu_si128(o, composed);
                    (consumed as usize, 3)
                } else {
                    (0, 0)
                }
            }
        }
    }

    #[cfg(all(target_arch = "aarch64", target_feature = "neon", target_endian = "little"))]
    mod neon {
        use super::{Simd, LAYOUTS_END, LAYOUT_3, LAYOUT_4, SHUFFLES, WINDOWS};
        use std::arch::aarch64::*;

        #[derive(Clone, Copy)]
        pub(super) struct V(uint8x16_t);

        /// Composes four code points of 1-3 bytes from 32-bit lanes.
        #[inline(always)]
        unsafe fn compose3(perm: uint32x4_t) -> uint32x4_t {
            let ascii = vandq_u32(perm, vdupq_n_u32(0x7F));
            let middle = vshrq_n_u32::<2>(vandq_u32(perm, vdupq_n_u32(0x3F00)));
            let high = vshrq_n_u32::<4>(vandq_u32(perm, vdupq_n_u32(0x0F_0000)));
            vorrq_u32(vorrq_u32(ascii, middle), high)
        }

        impl Simd for V {
            #[inline(always)]
            unsafe fn load(p: *const u8) -> Self {
                V(vld1q_u8(p))
            }
            #[inline(always)]
            unsafe fn splat(b: u8) -> Self {
                V(vdupq_n_u8(b))
            }
            #[inline(always)]
            unsafe fn and(self, o: Self) -> Self {
                V(vandq_u8(self.0, o.0))
            }
            #[inline(always)]
            unsafe fn or(self, o: Self) -> Self {
                V(vorrq_u8(self.0, o.0))
            }
            #[inline(always)]
            unsafe fn xor(self, o: Self) -> Self {
                V(veorq_u8(self.0, o.0))
            }
            #[inline(always)]
            unsafe fn shr4(self) -> Self {
                V(vshrq_n_u8::<4>(self.0))
            }
            #[inline(always)]
            unsafe fn lookup(self, idx: Self) -> Self {
                V(vqtbl1q_u8(self.0, idx.0))
            }
            #[inline(always)]
            unsafe fn prev1(self, prev: Self) -> Self {
                V(vextq_u8::<15>(prev.0, self.0))
            }
            #[inline(always)]
            unsafe fn prev2(self, prev: Self) -> Self {
                V(vextq_u8::<14>(prev.0, self.0))
            }
            #[inline(always)]
            unsafe fn prev3(self, prev: Self) -> Self {
                V(vextq_u8::<13>(prev.0, self.0))
            }
            #[inline(always)]
            unsafe fn saturating_sub(self, b: u8) -> Self {
                V(vqsubq_u8(self.0, vdupq_n_u8(b)))
            }
            #[inline(always)]
            unsafe fn any_nonzero(self) -> bool {
                vmaxvq_u8(self.0) != 0
            }
            #[inline(always)]
            unsafe fn any_high_bit(self) -> bool {
                vmaxvq_u8(self.0) >= 0x80
            }
            #[inline(always)]
            unsafe fn lead_mask(block: &[Self; 4]) -> u64 {
                // Continuation bytes are -128..=-65 as i8. Weight each lane's
                // flag by its bit and fold pairs of bytes until one byte holds
                // the flags of eight.
                let weights: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
                let w = vld1q_u8(weights.as_ptr());
                let lead = vdupq_n_s8(-65);
                let flag = |v: V| vandq_u8(vcgtq_s8(vreinterpretq_s8_u8(v.0), lead), w);
                let sum0 = vpaddq_u8(flag(block[0]), flag(block[1]));
                let sum1 = vpaddq_u8(flag(block[2]), flag(block[3]));
                let sum = vpaddq_u8(sum0, sum1);
                let sum = vpaddq_u8(sum, sum);
                vgetq_lane_u64::<0>(vreinterpretq_u64_u8(sum))
            }
            #[inline(always)]
            unsafe fn store_ascii(self, out: *mut u32) {
                let lo = vmovl_u8(vget_low_u8(self.0));
                let hi = vmovl_u8(vget_high_u8(self.0));
                vst1q_u32(out, vmovl_u16(vget_low_u16(lo)));
                vst1q_u32(out.add(4), vmovl_u16(vget_high_u16(lo)));
                vst1q_u32(out.add(8), vmovl_u16(vget_low_u16(hi)));
                vst1q_u32(out.add(12), vmovl_u16(vget_high_u16(hi)));
            }
            #[inline(always)]
            unsafe fn convert(p: *const u8, ends: u64, out: *mut u32) -> (usize, usize) {
                let input = vld1q_u8(p);
                let mask = (ends & 0xFFF) as usize;
                if mask == 0xFFF {
                    // Twelve ASCII bytes.
                    V(input).store_ascii(out);
                    return (12, 12);
                }
                if mask == 0x924 {
                    // Four three-byte code points, as in CJK text.
                    let sh: [u8; 16] = [2, 1, 0, 0xFF, 5, 4, 3, 0xFF, 8, 7, 6, 0xFF, 11, 10, 9, 0xFF];
                    let perm = vreinterpretq_u32_u8(vqtbl1q_u8(input, vld1q_u8(sh.as_ptr())));
                    vst1q_u32(out, compose3(perm));
                    return (12, 4);
                }
                let [idx, consumed] = WINDOWS[mask];
                let idx = idx as usize;
                let perm = vqtbl1q_u8(input, vld1q_u8(SHUFFLES[idx].as_ptr()));
                if idx < LAYOUT_3 {
                    let perm = vreinterpretq_u16_u8(perm);
                    let ascii = vandq_u16(perm, vdupq_n_u16(0x7F));
                    let high = vshrq_n_u16::<2>(vandq_u16(perm, vdupq_n_u16(0x1F00)));
                    let composed = vorrq_u16(ascii, high);
                    vst1q_u32(out, vmovl_u16(vget_low_u16(composed)));
                    vst1q_u32(out.add(4), vmovl_u16(vget_high_u16(composed)));
                    (consumed as usize, 6)
                } else if idx < LAYOUT_4 {
                    vst1q_u32(out, compose3(vreinterpretq_u32_u8(perm)));
                    (consumed as usize, 4)
                } else if idx < LAYOUTS_END {
                    let perm = vreinterpretq_u32_u8(perm);
                    let ascii = vandq_u32(perm, vdupq_n_u32(0x7F));
                    let middle = vshrq_n_u32::<2>(vandq_u32(perm, vdupq_n_u32(0x3F00)));
                    // The third byte from the end is a continuation or a
                    // three-byte lead 1110____, whose bit 5 must be cleared:
                    // only leads have bit 6 set.
                    let third = vandq_u32(perm, vdupq_n_u32(0x3F_0000));
                    let lead_bit = vshrq_n_u32::<1>(vandq_u32(perm, vdupq_n_u32(0x40_0000)));
                    let third = vshrq_n_u32::<4>(veorq_u32(third, lead_bit));
                    let fourth = vshrq_n_u32::<6>(vandq_u32(perm, vdupq_n_u32(0x0700_0000)));
                    vst1q_u32(out, vorrq_u32(vorrq_u32(ascii, middle), vorrq_u32(third, fourth)));
                    (consumed as usize, 3)
                } else {
                    (0, 0)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small deterministic PRNG for the differential test (Zig uses
    /// `std.Random.DefaultPrng`; the exact sequence doesn't matter).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            // splitmix64
            self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            z ^ (z >> 31)
        }

        fn range(&mut self, lo: u64, hi_inclusive: u64) -> u64 {
            lo + self.next() % (hi_inclusive - lo + 1)
        }

        fn byte(&mut self) -> u8 {
            self.next() as u8
        }
    }

    // Differential test: the SIMD implementation must agree with the
    // scalar implementation on any input. Exercises random mixtures of
    // ASCII, escapes, controls, valid and invalid UTF-8, at various
    // lengths (including chunk-boundary straddling cases).
    #[test]
    fn decode_simd_matches_scalar() {
        let mut rand = Rng(0xf00dface);

        let mut input = [0u8; 257];
        let mut out_simd = [0u32; 257];
        let mut out_scalar = [0u32; 257];

        for _ in 0..10_000 {
            let len = rand.range(0, input.len() as u64) as usize;
            let style = rand.range(0, 2);
            for b in input[..len].iter_mut() {
                *b = match style {
                    // Mostly ASCII with occasional specials.
                    0 => match rand.range(0, 20) {
                        0 => 0x1B,
                        1 => rand.range(0, 0x1F) as u8,
                        2 => rand.byte(),
                        _ => rand.range(0x20, 0x7E) as u8,
                    },
                    // Heavy multi-byte/invalid UTF-8.
                    1 => match rand.range(0, 3) {
                        0 => rand.range(0x80, 0xBF) as u8,
                        1 => rand.range(0xC0, 0xFF) as u8,
                        2 => 0x1B,
                        _ => rand.range(0x20, 0x7E) as u8,
                    },
                    // Fully random bytes.
                    _ => rand.byte(),
                };
            }

            let (res_simd, ascii) =
                utf8_decode_until_control_seq_with_ascii(&input[..len], &mut out_simd);
            if ascii {
                assert!(out_simd[..res_simd.decoded].iter().all(|&cp| cp < 0x80));
            }
            let res_scalar = utf8_decode_until_control_seq_scalar(&input[..len], &mut out_scalar);
            assert_eq!(res_scalar.consumed, res_simd.consumed, "input={:x?}", &input[..len]);
            assert_eq!(res_scalar.decoded, res_simd.decoded, "input={:x?}", &input[..len]);
            assert_eq!(
                &out_scalar[..res_scalar.decoded],
                &out_simd[..res_simd.decoded],
                "input={:x?}",
                &input[..len]
            );
        }
    }

    /// Appends one code point of `len` UTF-8 bytes, spread over the ranges
    /// with special second bytes (E0, ED, F0, F4) and their edges.
    fn push_cp(rand: &mut Rng, out: &mut Vec<u8>, len: u64) {
        let cp = match len {
            1 => rand.range(0x20, 0x7E) as u32,
            2 => rand.range(0x80, 0x7FF) as u32,
            3 => match rand.range(0, 4) {
                0 => rand.range(0x800, 0xFFF) as u32,
                1 => rand.range(0xD000, 0xD7FF) as u32,
                2 => rand.range(0xE000, 0xFFFF) as u32,
                _ => rand.range(0x4E00, 0x9FFF) as u32,
            },
            _ => match rand.range(0, 2) {
                0 => rand.range(0x10000, 0x3FFFF) as u32,
                _ => rand.range(0x100000, 0x10FFFF) as u32,
            },
        };
        let mut buf = [0u8; 4];
        out.extend_from_slice(char::from_u32(cp).unwrap().encode_utf8(&mut buf).as_bytes());
    }

    fn assert_region_matches_scalar(input: &[u8]) {
        let mut out_blocks = vec![0u32; input.len()];
        let mut out_scalar = vec![0u32; input.len()];
        for at_input_end in [false, true] {
            let a = decode_region(input, at_input_end, &mut out_blocks);
            let b = decode_scalar(input, at_input_end, &mut out_scalar, 0, 0, input.len());
            assert_eq!(b, a, "input={:x?}", input);
            assert_eq!(&out_scalar[..b.decoded], &out_blocks[..a.decoded], "input={:x?}", input);
        }
    }

    // The validated block decoder must agree with the scalar decoder on
    // well-formed text of every length mix, and on text with corruption at
    // any position relative to block and window boundaries.
    #[test]
    fn validated_blocks_match_scalar() {
        let mut rand = Rng(0x5eed_cafe);
        let mut input = Vec::new();
        for iter in 0..4000 {
            input.clear();
            let target = rand.range(0, 700) as usize;
            // Per-iteration length mix, so some inputs are all one width.
            let weights = [rand.range(0, 4), rand.range(0, 4), rand.range(0, 4), rand.range(0, 2)];
            let total: u64 = weights.iter().sum::<u64>().max(1);
            while input.len() < target {
                let mut pick = rand.range(0, total - 1);
                let mut len = 1;
                for (i, &w) in weights.iter().enumerate() {
                    if pick < w {
                        len = i as u64 + 1;
                        break;
                    }
                    pick -= w;
                }
                push_cp(&mut rand, &mut input, len);
            }
            let s = std::str::from_utf8(&input).unwrap();
            let mut out = vec![0u32; input.len()];
            let r = decode_region(&input, true, &mut out);
            let expected: Vec<u32> = s.chars().map(|c| c as u32).collect();
            assert_eq!(input.len(), r.consumed);
            assert_eq!(&expected[..], &out[..r.decoded]);
            if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) && input.len() >= 80 {
                // Well-formed input this long takes the block path.
                assert!(validated::decode_blocks(&input, &mut out).0 > 0);
            }

            // Corrupt a few bytes: random bytes, truncations, stray
            // continuations and leads.
            if iter % 2 == 1 && !input.is_empty() {
                for _ in 0..rand.range(1, 3) {
                    let at = rand.range(0, input.len() as u64 - 1) as usize;
                    match rand.range(0, 3) {
                        0 => input[at] = rand.byte(),
                        1 => {
                            input.remove(at);
                        }
                        2 => input.insert(at, rand.range(0x80, 0xBF) as u8),
                        _ => input.insert(at, rand.range(0xC0, 0xFF) as u8),
                    }
                }
            }
            assert_region_matches_scalar(&input);
        }
    }

    // Every 1-4 byte sequence class (valid, overlong, surrogate, too large,
    // truncated, stray) at offsets around block and window boundaries.
    #[test]
    fn validated_blocks_match_scalar_on_sequence_edges() {
        let edges: &[u8] = &[
            0x00, 0x41, 0x7F, 0x80, 0x8F, 0x90, 0x9F, 0xA0, 0xBF, 0xC0, 0xC1, 0xC2, 0xDF, 0xE0,
            0xE1, 0xEC, 0xED, 0xEE, 0xEF, 0xF0, 0xF1, 0xF3, 0xF4, 0xF5, 0xFF,
        ];
        let conts: &[u8] = &[0x41, 0x80, 0x8F, 0x90, 0x9F, 0xA0, 0xBF, 0xC2];
        // A well-formed CJK filler keeps every block on the validated path.
        let filler = "日本語のテキスト".repeat(8);
        for &b0 in edges {
            for &b1 in edges {
                for &b2 in conts {
                    for &b3 in &conts[..4] {
                        for offset in [0usize, 12, 13, 47, 51, 60, 61, 63] {
                            let mut input = filler.as_bytes()[..offset].to_vec();
                            input.extend_from_slice(&[b0, b1, b2, b3]);
                            input.extend_from_slice(filler.as_bytes());
                            assert_region_matches_scalar(&input);
                        }
                    }
                }
            }
        }
    }

    // Every window a well-formed block can produce has a layout, and the
    // layout consumes exactly its code points.
    #[test]
    fn validated_window_table_covers_well_formed_masks() {
        for mask in 0..4096usize {
            let mut lens = Vec::new();
            let mut start = 0;
            for i in 0..12 {
                if mask >> i & 1 == 1 {
                    lens.push(i + 1 - start);
                    start = i + 1;
                }
            }
            let well_formed = lens.len() >= 3 && lens[..3].iter().all(|&l| l <= 4);
            let [idx, consumed] = validated::WINDOWS[mask];
            if !well_formed {
                continue;
            }
            let (cps, max_len) = if (idx as usize) < 64 {
                (6, 2)
            } else if (idx as usize) < 145 {
                (4, 3)
            } else {
                assert!((idx as usize) < 209, "mask={mask:#x}");
                (3, 4)
            };
            assert!(lens[..cps].iter().all(|&l| l <= max_len), "mask={mask:#x}");
            assert_eq!(lens[..cps].iter().sum::<usize>(), consumed as usize, "mask={mask:#x}");
        }
    }

    #[test]
    fn ascii_classification_rejects_replacement_codepoints() {
        let mut out = [0u32; 64];
        for input in [b"ASCII\x1b".as_slice(), b"\x00\x1f\x7f".as_slice()] {
            let (_, ascii) = utf8_decode_until_control_seq_with_ascii(input, &mut out);
            assert!(ascii);
        }
        for input in [b"bad\xff".as_slice(), b"bad\xc0\xaf".as_slice()] {
            let (result, ascii) = utf8_decode_until_control_seq_with_ascii(input, &mut out);
            assert_eq!(result.consumed, result.decoded);
            assert!(!ascii);
        }
    }

    #[test]
    fn decode_matches_std_for_valid_utf8() {
        let s = "héllo wörld ✓ 日本語 🎉 abcdefghijklmnopqrstuvwxyz 0123456789 ünïcödé";
        let mut out = vec![0u32; s.len()];
        let r = utf8_decode_until_control_seq(s.as_bytes(), &mut out);
        let expected: Vec<u32> = s.chars().map(|c| c as u32).collect();
        assert_eq!(s.len(), r.consumed);
        assert_eq!(&expected[..], &out[..r.decoded]);
    }

    #[test]
    fn swar_stop_mask_test() {
        for pos in 0..8 {
            for &b in &[0x1Bu8, 0x80, 0xFF, 0xC3] {
                let mut bytes = [b'a'; 8];
                bytes[pos] = b;
                let m = swar_stop_mask(u64::from_le_bytes(bytes));
                assert_eq!(pos as u32, m.trailing_zeros());
            }
        }
        assert_eq!(0, swar_stop_mask(u64::from_le_bytes(*b"abcdefgh")));
    }

    #[test]
    fn widen_portable_matches_native() {
        let mut rand = Rng(1234);
        for _ in 0..10_000 {
            let mut chunk = [0u8; CHUNK];
            for b in chunk.iter_mut() {
                *b = match rand.range(0, 10) {
                    0 => 0x1B,
                    1 => rand.byte(),
                    _ => rand.range(0x20, 0x7E) as u8,
                };
            }
            let mut a = [0u32; CHUNK];
            let mut b = [0u32; CHUNK];
            let ma = widen_ascii_chunk(&chunk, &mut a);
            let mb = widen_ascii_chunk_portable(&chunk, &mut b);
            assert_eq!(a, b);
            assert_eq!(ma == 0, mb == 0);
            if ma != 0 {
                assert_eq!(ma.trailing_zeros() / STOP_BITS_PER_BYTE, mb.trailing_zeros());
            }
        }
    }

    #[test]
    fn decode_no_escape() {
        let mut output = [0u32; 1024];

        // TODO: many more test cases
        {
            let s = "hello".repeat(128);
            let str = s.as_bytes();
            assert_eq!(
                DecodeResult {
                    consumed: str.len(),
                    decoded: str.len(),
                },
                utf8_decode_until_control_seq(str, &mut output)
            );
        }
    }

    #[test]
    fn decode_ascii_to_escape() {
        let mut output = [0u32; 1024];

        // TODO: many more test cases
        {
            let prefix = "hello".repeat(64);
            let payload = "world".repeat(64);
            let s = format!("{prefix}\x1b{payload}");
            assert_eq!(
                DecodeResult {
                    consumed: prefix.len(),
                    decoded: prefix.len(),
                },
                utf8_decode_until_control_seq(s.as_bytes(), &mut output)
            );
        }
    }

    #[test]
    fn decode_immediate_esc_sequence() {
        let mut output = [0u32; 64];
        let str = b"\x1b[?5s";
        assert_eq!(
            DecodeResult {
                consumed: 0,
                decoded: 0,
            },
            utf8_decode_until_control_seq(str, &mut output)
        );
    }

    #[test]
    fn decode_incomplete_utf8() {
        let mut output = [0u32; 64];

        // 2-byte truncated at end of buffer
        {
            let str = b"hello\xc2";
            assert_eq!(
                DecodeResult {
                    consumed: 5,
                    decoded: 5,
                },
                utf8_decode_until_control_seq(str, &mut output)
            );
        }

        // 3-byte: \xe0 expects A0-BF next, but \x00 is not in range.
        // \xe0 is a maximal subpart of length 1 → FFFD, then \x00 is ASCII NUL.
        {
            let str = b"hello\xe0\x00";
            let result = utf8_decode_until_control_seq(str, &mut output);
            assert_eq!(7usize, result.consumed);
            assert_eq!(7usize, result.decoded);
            assert_eq!(0xFFFDu32, output[5]);
            assert_eq!(0x00u32, output[6]);
        }

        // 4-byte truncated at end of buffer (F0 90 is valid so far)
        {
            let str = b"hello\xf0\x90";
            assert_eq!(
                DecodeResult {
                    consumed: 5,
                    decoded: 5,
                },
                utf8_decode_until_control_seq(str, &mut output)
            );
        }
    }

    #[test]
    fn decode_invalid_utf8() {
        let mut output = [0u32; 64];

        // Invalid leading 2-byte sequence
        {
            let str = b"hello\xc2\x01";
            assert_eq!(
                DecodeResult {
                    consumed: 7,
                    decoded: 7,
                },
                utf8_decode_until_control_seq(str, &mut output)
            );
        }

        // Replacement will only replace the invalid leading byte.
        assert_eq!(0xFFFDu32, output[5]);
        assert_eq!(0x01u32, output[6]);
    }

    // Per the maximal subpart spec, bytes F5-FF are each replaced with FFFD.
    #[test]
    fn decode_invalid_leading_byte_is_replaced() {
        let mut output = [0u32; 64];
        {
            let str = b"hello\xFF";
            let result = utf8_decode_until_control_seq(str, &mut output);
            assert_eq!(6usize, result.consumed);
            assert_eq!(6usize, result.decoded);
            assert_eq!(0xFFFDu32, output[5]);
        }
    }

    #[test]
    fn decode_invalid_continuation_in_3_byte_sequence() {
        let mut output = [0u32; 64];

        // \xe2 expects two continuation bytes, \x28 is not one
        {
            let str = b"hello\xe2\x28world";
            let result = utf8_decode_until_control_seq(str, &mut output);
            // "hello" + replacement + "(" + "world" = 12 codepoints
            assert_eq!(12usize, result.decoded);
            assert_eq!(0xFFFDu32, output[5]);
            assert_eq!('(' as u32, output[6]);
            assert_eq!('w' as u32, output[7]);
        }
    }

    #[test]
    fn decode_invalid_continuation_in_4_byte_sequence() {
        let mut output = [0u32; 64];

        // \xf0\x90 is a valid prefix of a 4-byte sequence, but \x28 breaks it.
        // Maximal subpart is F0 90 (length 2) → single FFFD, then '(' proceeds.
        {
            let str = b"hello\xf0\x90\x28world";
            let result = utf8_decode_until_control_seq(str, &mut output);
            // "hello" + FFFD + "(" + "world" = 12 codepoints
            assert_eq!(12usize, result.decoded);
            assert_eq!(0xFFFDu32, output[5]);
            assert_eq!('(' as u32, output[6]);
            assert_eq!('w' as u32, output[7]);
        }
    }

    #[test]
    fn decode_multiple_consecutive_invalid_bytes() {
        let mut output = [0u32; 64];

        // Each lone continuation byte is its own maximal subpart → one FFFD each.
        {
            let str = b"a\x80\x80b";
            let result = utf8_decode_until_control_seq(str, &mut output);
            // "a" + FFFD + FFFD + "b" = 4 codepoints
            assert_eq!(4usize, result.decoded);
            assert_eq!('a' as u32, output[0]);
            assert_eq!(0xFFFDu32, output[1]);
            assert_eq!(0xFFFDu32, output[2]);
            assert_eq!('b' as u32, output[3]);
        }

        // C0 is an invalid lead byte (< C2), each byte gets its own FFFD.
        {
            let str = b"a\xc0\xc0b";
            let result = utf8_decode_until_control_seq(str, &mut output);
            // "a" + FFFD + FFFD + "b" = 4 codepoints
            assert_eq!(4usize, result.decoded);
            assert_eq!('a' as u32, output[0]);
            assert_eq!(0xFFFDu32, output[1]);
            assert_eq!(0xFFFDu32, output[2]);
            assert_eq!('b' as u32, output[3]);
        }
    }

    #[test]
    fn decode_unexpected_continuation_byte_as_lead() {
        let mut output = [0u32; 64];

        // 0x80 is a continuation byte appearing as a lead byte
        {
            let str = b"a\x80b";
            let result = utf8_decode_until_control_seq(str, &mut output);
            // "a" + replacement + "b" = 3 codepoints
            assert_eq!(3usize, result.decoded);
            assert_eq!('a' as u32, output[0]);
            assert_eq!(0xFFFDu32, output[1]);
            assert_eq!('b' as u32, output[2]);
        }
    }

    #[test]
    fn decode_overlong_2_byte_encoding() {
        let mut output = [0u32; 64];

        // \xc0\xaf: C0 is invalid lead (< C2) → FFFD, AF is lone continuation → FFFD
        // Per Table 3-8: C0 AF → FFFD FFFD
        {
            let str = b"a\xc0\xafb";
            let result = utf8_decode_until_control_seq(str, &mut output);
            // "a" + FFFD + FFFD + "b" = 4 codepoints
            assert_eq!(4usize, result.decoded);
            assert_eq!('a' as u32, output[0]);
            assert_eq!(0xFFFDu32, output[1]);
            assert_eq!(0xFFFDu32, output[2]);
            assert_eq!('b' as u32, output[3]);
        }
    }

    #[test]
    fn decode_surrogate_half() {
        let mut output = [0u32; 64];

        // \xed\xa0\x80 encodes U+D800 (a surrogate). Per Table 3-7, after ED
        // the next byte must be 80-9F. A0 is out of range, so ED is a maximal
        // subpart of length 1 → FFFD. Then A0 and 80 are lone continuations
        // → FFFD each. Per Table 3-9: ED A0 80 → FFFD FFFD FFFD
        {
            let str = b"a\xed\xa0\x80b";
            let result = utf8_decode_until_control_seq(str, &mut output);
            // "a" + FFFD + FFFD + FFFD + "b" = 5 codepoints
            assert_eq!(5usize, result.decoded);
            assert_eq!('a' as u32, output[0]);
            assert_eq!(0xFFFDu32, output[1]);
            assert_eq!(0xFFFDu32, output[2]);
            assert_eq!(0xFFFDu32, output[3]);
            assert_eq!('b' as u32, output[4]);
        }
    }

    #[test]
    fn decode_valid_multibyte_surrounded_by_invalid() {
        let mut output = [0u32; 64];

        // \xc3\xa9 = é (U+00E9), surrounded by invalid continuation bytes
        {
            let str = b"\x80\xc3\xa9\x80";
            let result = utf8_decode_until_control_seq(str, &mut output);
            // replacement + é + replacement = 3 codepoints
            assert_eq!(3usize, result.decoded);
            assert_eq!(0xFFFDu32, output[0]);
            assert_eq!(0x00E9u32, output[1]);
            assert_eq!(0xFFFDu32, output[2]);
        }
    }

    #[test]
    fn decode_partial_utf8_before_escape() {
        // A valid-so-far but incomplete sequence cut off by an ESC can
        // never be completed, so it is consumed and replaced by a single
        // U+FFFD (maximal subpart) rather than left pending. Only
        // sequences cut off by the true end of input are left pending.
        let mut output = [0u32; 64];

        // 2-byte lead cut off by ESC.
        {
            let str = b"hi\xc2\x1b[0m";
            let result = utf8_decode_until_control_seq(str, &mut output);
            assert_eq!(3usize, result.consumed);
            assert_eq!(3usize, result.decoded);
            assert_eq!(0xFFFDu32, output[2]);
        }

        // 3-byte lead plus one valid continuation cut off by ESC:
        // the whole prefix is one maximal subpart, one U+FFFD.
        {
            let str = b"\xe0\xa0\x1bX";
            let result = utf8_decode_until_control_seq(str, &mut output);
            assert_eq!(2usize, result.consumed);
            assert_eq!(1usize, result.decoded);
            assert_eq!(0xFFFDu32, output[0]);
        }
    }

    #[test]
    fn decode_invalid_byte_before_escape() {
        let mut output = [0u32; 64];

        // Invalid byte followed by ESC - should replace then stop
        {
            let str = b"hi\x80\x1b[0m";
            let result = utf8_decode_until_control_seq(str, &mut output);
            assert_eq!(3usize, result.consumed);
            assert_eq!(3usize, result.decoded);
            assert_eq!('h' as u32, output[0]);
            assert_eq!('i' as u32, output[1]);
            assert_eq!(0xFFFDu32, output[2]);
        }
    }

    // Unicode Table 3-8: U+FFFD for Non-Shortest Form Sequences
    // Bytes:  C0  AF  E0  80  BF  F0  81  82  41
    // Output: FFFD FFFD FFFD FFFD FFFD FFFD FFFD FFFD 0041
    #[test]
    fn table_3_8_non_shortest_form_sequences() {
        let mut output = [0u32; 64];

        let str = b"\xC0\xAF\xE0\x80\xBF\xF0\x81\x82\x41";
        let result = utf8_decode_until_control_seq(str, &mut output);
        assert_eq!(9usize, result.consumed);
        assert_eq!(9usize, result.decoded);
        for o in &output[..8] {
            assert_eq!(0xFFFDu32, *o);
        }
        assert_eq!(0x41u32, output[8]);
    }

    // Unicode Table 3-9: U+FFFD for Ill-Formed Sequences for Surrogates
    // Bytes:  ED  A0  80  ED  BF  BF  ED  AF  41
    // Output: FFFD FFFD FFFD FFFD FFFD FFFD FFFD FFFD 0041
    #[test]
    fn table_3_9_surrogate_sequences() {
        let mut output = [0u32; 64];

        let str = b"\xED\xA0\x80\xED\xBF\xBF\xED\xAF\x41";
        let result = utf8_decode_until_control_seq(str, &mut output);
        assert_eq!(9usize, result.consumed);
        assert_eq!(9usize, result.decoded);
        for o in &output[..8] {
            assert_eq!(0xFFFDu32, *o);
        }
        assert_eq!(0x41u32, output[8]);
    }

    // Unicode Table 3-10: U+FFFD for Other Ill-Formed Sequences
    // Bytes:  F4  91  92  93  FF  41  80  BF  42
    // Output: FFFD FFFD FFFD FFFD FFFD 0041 FFFD FFFD 0042
    #[test]
    fn table_3_10_other_ill_formed_sequences() {
        let mut output = [0u32; 64];

        let str = b"\xF4\x91\x92\x93\xFF\x41\x80\xBF\x42";
        let result = utf8_decode_until_control_seq(str, &mut output);
        assert_eq!(9usize, result.consumed);
        assert_eq!(9usize, result.decoded);
        assert_eq!(0xFFFDu32, output[0]); // F4
        assert_eq!(0xFFFDu32, output[1]); // 91
        assert_eq!(0xFFFDu32, output[2]); // 92
        assert_eq!(0xFFFDu32, output[3]); // 93
        assert_eq!(0xFFFDu32, output[4]); // FF
        assert_eq!(0x0041u32, output[5]); // 41
        assert_eq!(0xFFFDu32, output[6]); // 80
        assert_eq!(0xFFFDu32, output[7]); // BF
        assert_eq!(0x0042u32, output[8]); // 42
    }

    // Unicode Table 3-11: U+FFFD for Truncated Sequences
    // Bytes:  E1  80  E2  F0  91  92  F1  BF  41
    // Output: FFFD     FFFD    FFFD         FFFD     0041
    #[test]
    fn table_3_11_truncated_sequences() {
        let mut output = [0u32; 64];

        let str = b"\xE1\x80\xE2\xF0\x91\x92\xF1\xBF\x41";
        let result = utf8_decode_until_control_seq(str, &mut output);
        assert_eq!(9usize, result.consumed);
        assert_eq!(5usize, result.decoded);
        assert_eq!(0xFFFDu32, output[0]); // E1 80 (truncated 3-byte)
        assert_eq!(0xFFFDu32, output[1]); // E2 (truncated 3-byte, next byte F0 not continuation)
        assert_eq!(0xFFFDu32, output[2]); // F0 91 92 (truncated 4-byte)
        assert_eq!(0xFFFDu32, output[3]); // F1 BF (truncated 4-byte, next byte 41 not continuation)
        assert_eq!(0x0041u32, output[4]); // 41
    }
}
