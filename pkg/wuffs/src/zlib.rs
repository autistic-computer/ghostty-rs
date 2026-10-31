//! Port of `src/wuffs/src/zlib.zig`.
//!
//! STATUS: ported
//!
//! Deviation: decompression uses `miniz_oxide`'s low-level inflate core
//! instead of Wuffs' zlib decoder; the output-growth policy, the output
//! limit and the error semantics (trailing input or a bad checksum is a
//! `WuffsError`, exceeding the limit is `Overflow`) are the same.

use miniz_oxide::inflate::core::{decompress, inflate_flags, DecompressorOxide};
use miniz_oxide::inflate::TINFLStatus;

use super::error::Error;

const LOG: &str = "wuffs_zlib";

/// Decode a complete RFC 1950 zlib stream. `size_hint` is the expected output
/// size when known; the output may grow up to `limit` bytes.
pub fn decode(data: &[u8], limit: usize, size_hint: Option<usize>) -> Result<Vec<u8>, Error> {
    if limit == 0 {
        return Err(Error::Overflow);
    }

    // The decoder state is large, so keep it off the stack.
    let mut decoder = Box::<DecompressorOxide>::default();
    let flags = inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER
        | inflate_flags::TINFL_FLAG_COMPUTE_ADLER32
        | inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;

    let initial_size =
        1.max(limit.min(size_hint.unwrap_or_else(|| data.len().checked_mul(2).unwrap_or(limit))));
    let mut destination = alloc_zeroed(initial_size)?;

    let mut in_pos = 0usize;
    let mut out_pos = 0usize;
    loop {
        let (status, in_consumed, out_written) = decompress(
            &mut decoder,
            &data[in_pos..],
            &mut destination,
            out_pos,
            flags,
        );
        in_pos += in_consumed;
        out_pos += out_written;

        match status {
            TINFLStatus::Done => {
                if in_pos != data.len() {
                    return Err(Error::WuffsError);
                }
                destination.truncate(out_pos);
                destination.shrink_to_fit();
                return Ok(destination);
            }

            // The equivalent of Wuffs' `suspension__short_write`.
            TINFLStatus::HasMoreOutput => {}

            status => {
                log::warn!(target: LOG, "decode err={status:?}");
                return Err(Error::WuffsError);
            }
        }

        if out_pos != destination.len() {
            return Err(Error::WuffsError);
        }
        if destination.len() == limit {
            return Err(Error::Overflow);
        }

        let next_size = destination.len() + destination.len().min(limit - destination.len());
        destination
            .try_reserve_exact(next_size - destination.len())
            .map_err(|_| Error::OutOfMemory)?;
        destination.resize(next_size, 0);
    }
}

fn alloc_zeroed(len: usize) -> Result<Vec<u8>, Error> {
    let mut v = Vec::new();
    v.try_reserve_exact(len).map_err(|_| Error::OutOfMemory)?;
    v.resize(len, 0);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPRESSED: [u8; 19] = [
        0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0x28, 0x2f, 0x4d, 0x4b, 0x2b, 0x06, 0x00,
        0x1a, 0x02, 0x04, 0x60,
    ];

    #[test]
    fn zlib_decode_grows_output() {
        let output = decode(&COMPRESSED, 64, Some(1)).unwrap();
        assert_eq!(b"hello wuffs", &output[..]);
    }

    #[test]
    fn zlib_decode_enforces_output_limit() {
        assert_eq!(Err(Error::Overflow), decode(&COMPRESSED, 10, None));
    }

    #[test]
    fn zlib_decode_accepts_exact_limit_and_rejects_truncated_input() {
        let output = decode(&COMPRESSED, 11, Some(11)).unwrap();
        assert_eq!(b"hello wuffs", &output[..]);
        assert_eq!(
            Err(Error::WuffsError),
            decode(&COMPRESSED[..COMPRESSED.len() - 1], 64, None)
        );
    }

    /// The Zig test checks that the decoder state is aligned even when the
    /// allocator hands out unaligned byte buffers. Rust allocations are
    /// always aligned for their type, so only the decode itself is checked.
    #[test]
    fn zlib_decode_aligns_decoder_in_an_unaligned_allocator_buffer() {
        let output = decode(&COMPRESSED, 64, Some(11)).unwrap();
        assert_eq!(b"hello wuffs", &output[..]);
    }

    #[test]
    fn zlib_decode_rejects_corrupted_checksum() {
        let mut compressed = COMPRESSED;
        // Change only the Adler-32 trailer, leaving the deflate data intact.
        let last = compressed.len() - 1;
        compressed[last] ^= 1;
        assert_eq!(Err(Error::WuffsError), decode(&compressed, 64, Some(11)));
    }
}
