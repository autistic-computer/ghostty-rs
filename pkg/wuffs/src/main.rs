//! Port of `src/wuffs/src/main.zig`.
//!
//! STATUS: ported
//!
//! Deviation: the Zig package links the Wuffs C library and exports weak
//! `calloc`/`free` stubs for libc-less builds. The Rust port decodes with
//! pure-Rust crates, so there is no C code to satisfy and no stubs.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "error.rs"]
pub mod error;
#[path = "jpeg.rs"]
pub mod jpeg;
#[path = "png.rs"]
pub mod png;
#[path = "swizzle.rs"]
pub mod swizzle;
#[path = "zlib.rs"]
pub mod zlib;
// @generated-mods-end

pub use error::Error;

/// The maximum image size, based on the 4G limit of Ghostty's
/// `image-storage-limit` config.
pub const MAXIMUM_IMAGE_SIZE: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Compute the size of a decoded RGBA image (4 bytes per pixel), returning
/// [`Error::Overflow`] when it overflows or exceeds [`MAXIMUM_IMAGE_SIZE`].
pub(crate) fn rgba_size(log: &str, width: u32, height: u32) -> Result<usize, Error> {
    let size = usize::try_from(width)
        .ok()
        .and_then(|w| w.checked_mul(usize::try_from(height).ok()?))
        .and_then(|wh| wh.checked_mul(4))
        .ok_or(Error::Overflow)?;

    if size as u64 > MAXIMUM_IMAGE_SIZE {
        log::warn!(
            target: log,
            "image size {size} is larger than the maximum allowed ({MAXIMUM_IMAGE_SIZE})"
        );
        return Err(Error::Overflow);
    }

    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_size_limits() {
        assert_eq!(rgba_size("wuffs_test", 2, 3), Ok(24));
        assert_eq!(rgba_size("wuffs_test", 32768, 32769), Err(Error::Overflow));
        #[cfg(target_pointer_width = "64")]
        assert_eq!(rgba_size("wuffs_test", 32768, 32768), Ok(1 << 32));
    }
}
