//! Port of `src/wuffs/src/png.zig`.
//!
//! STATUS: ported
//!
//! Deviation: decoding is done with the pure-Rust `png` crate instead of the
//! Wuffs C decoder. The output format is the same as Wuffs'
//! `RGBA_NONPREMUL` (8-bit straight-alpha RGBA) and the same size limit is
//! enforced before the pixel data is decoded.

use std::io::Cursor;

use super::error::{check, Error};
use super::{rgba_size, ImageData};

const LOG: &str = "wuffs_png";

/// Decode a PNG image.
pub fn decode(data: &[u8]) -> Result<ImageData, Error> {
    // We do our own size limit check (below) so that the behavior matches
    // the Zig version: the decoder's own allocation limit is disabled.
    let mut decoder =
        png::Decoder::new_with_limits(Cursor::new(data), png::Limits { bytes: usize::MAX });

    // Expand palettes, low bit depths and tRNS chunks, add an alpha channel
    // and strip 16-bit samples so the output is always 8 bits per sample.
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::ALPHA | png::Transformations::STRIP_16,
    );

    let (width, height) = {
        let info = check(LOG, decoder.read_header_info())?;
        (info.width, info.height)
    };

    let size = rgba_size(LOG, width, height)?;

    let mut reader = check(LOG, decoder.read_info())?;
    let buf_size = reader.output_buffer_size().ok_or(Error::OutOfMemory)?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(buf_size)
        .map_err(|_| Error::OutOfMemory)?;
    buf.resize(buf_size, 0);
    let output = check(LOG, reader.next_frame(&mut buf))?;
    buf.truncate(output.buffer_size());

    let destination = to_rgba(&buf, output.color_type, size)?;

    Ok(ImageData {
        width,
        height,
        data: destination,
    })
}

/// Convert 8-bit decoder output into straight-alpha RGBA.
fn to_rgba(buf: &[u8], color_type: png::ColorType, size: usize) -> Result<Vec<u8>, Error> {
    let mut destination = Vec::new();
    destination
        .try_reserve_exact(size)
        .map_err(|_| Error::OutOfMemory)?;

    match color_type {
        png::ColorType::Rgba => destination.extend_from_slice(buf),
        png::ColorType::Rgb => {
            for px in buf.chunks_exact(3) {
                destination.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for px in buf.chunks_exact(2) {
                destination.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
        }
        png::ColorType::Grayscale => {
            for &y in buf {
                destination.extend_from_slice(&[y, y, y, 0xFF]);
            }
        }
        png::ColorType::Indexed => {
            // EXPAND always expands palettes.
            log::warn!(target: LOG, "decode err=unexpected indexed output");
            return Err(Error::WuffsError);
        }
    }

    if destination.len() != size {
        log::warn!(target: LOG, "decode err=unexpected decoded size");
        return Err(Error::WuffsError);
    }

    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_decode_000000() {
        let data = decode(include_bytes!("1x1#000000.png")).unwrap();

        assert_eq!(1, data.width);
        assert_eq!(1, data.height);
        assert_eq!(&[0u8, 0, 0, 255][..], &data.data[..]);
    }

    #[test]
    fn png_decode_ffffff() {
        let data = decode(include_bytes!("1x1#FFFFFF.png")).unwrap();

        assert_eq!(1, data.width);
        assert_eq!(1, data.height);
        assert_eq!(&[255u8, 255, 255, 255][..], &data.data[..]);
    }

    #[test]
    fn png_too_big() {
        let data = decode(include_bytes!("too_big.png"));
        assert_eq!(Err(Error::Overflow), data);
    }
}
