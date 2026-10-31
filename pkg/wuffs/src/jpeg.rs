//! Port of `src/wuffs/src/jpeg.zig`.
//!
//! STATUS: ported
//!
//! Deviation: decoding is done with the pure-Rust `zune-jpeg` crate instead
//! of the Wuffs C decoder. The output format is the same as Wuffs'
//! `RGBA_NONPREMUL` (8-bit RGBA, alpha always 255) and the same size limit is
//! enforced before the pixel data is decoded.

use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use super::error::{check, Error};
use super::{rgba_size, ImageData};

const LOG: &str = "wuffs_jpeg";

/// Decode a JPEG image.
pub fn decode(data: &[u8]) -> Result<ImageData, Error> {
    // We enforce our own size limit (below), like the Zig version, so the
    // decoder's own dimension limits are disabled.
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(usize::MAX)
        .set_max_height(usize::MAX);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);

    check(LOG, decoder.decode_headers())?;
    let (width, height) = decoder.dimensions().ok_or(Error::WuffsError)?;
    let width = u32::try_from(width).map_err(|_| Error::Overflow)?;
    let height = u32::try_from(height).map_err(|_| Error::Overflow)?;

    let size = rgba_size(LOG, width, height)?;

    let destination = check(LOG, decoder.decode())?;
    if destination.len() != size {
        log::warn!(target: LOG, "decode err=unexpected decoded size");
        return Err(Error::WuffsError);
    }

    Ok(ImageData {
        width,
        height,
        data: destination,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_decode_000000() {
        let data = decode(include_bytes!("1x1#000000.jpg")).unwrap();

        assert_eq!(1, data.width);
        assert_eq!(1, data.height);
        assert_eq!(&[0u8, 0, 0, 255][..], &data.data[..]);
    }

    #[test]
    fn jpeg_decode_ffffff() {
        let data = decode(include_bytes!("1x1#FFFFFF.jpg")).unwrap();

        assert_eq!(1, data.width);
        assert_eq!(1, data.height);
        assert_eq!(&[255u8, 255, 255, 255][..], &data.data[..]);
    }

    /// Note: `too_big.jpg` is the first 4 KiB of the Zig repo's 12 MiB test
    /// file. The size check happens on the header, before any scan data is
    /// decoded, so the rest of the file is not needed.
    #[test]
    fn jpeg_too_big() {
        let data = decode(include_bytes!("too_big.jpg"));
        assert_eq!(Err(Error::Overflow), data);
    }
}
