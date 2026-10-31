//! Port of `src/terminal/sys.zig`.
//!
//! STATUS: ported
//!
//! System interface for the terminal package.
//!
//! This provides runtime-swappable function pointers for operations that
//! depend on external implementations (e.g. image decoding). Each function
//! pointer is initialized with a default implementation if available.
//!
//! This exists so that the terminal package doesn't have hard dependencies
//! on specific libraries and enables embedders of the terminal package to
//! swap out implementations as needed at startup to provide their own
//! implementations.
//!
//! Deviations: the default PNG decoder uses the `png` crate instead of
//! wuffs (same output: 8-bit non-premultiplied RGBA). Zig's `std.Io`
//! parameter of `randomSecure` is a function that fills a buffer
//! ([`IoRandomSecureFn`]); [`os_random_secure`] is the OS-backed default.
//! Zig passes the decoder an allocator, which callers wrap in a
//! `LimitedAllocator`; Rust decoders take no allocator, so a caller sets
//! the per-allocation limit for the current thread with [`DecodeLimit`]
//! and decoders allocate their output with [`decode_alloc`].

use std::fmt;
use std::sync::RwLock;

use super::build_options;

/// Decode PNG data into RGBA pixels. If null, PNG decoding is unsupported
/// and the exact semantics are up to callers. For example, the Kitty Graphics
/// Protocol will work but cannot accept PNG images.
pub static DECODE_PNG: RwLock<Option<DecodePngFn>> = RwLock::new(
    if matches!(build_options::ARTIFACT, build_options::Artifact::Lib) {
        None
    } else {
        Some(decode_png_default as DecodePngFn)
    },
);

/// The current PNG decoder (reads [`DECODE_PNG`]).
pub fn decode_png() -> Option<DecodePngFn> {
    *DECODE_PNG.read().unwrap_or_else(|e| e.into_inner())
}

/// Replace the PNG decoder (writes [`DECODE_PNG`]).
pub fn set_decode_png(f: Option<DecodePngFn>) {
    *DECODE_PNG.write().unwrap_or_else(|e| e.into_inner()) = f;
}

/// `Allocator.Error || error{InvalidData}`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    OutOfMemory,
    InvalidData,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DecodeError::OutOfMemory => "OutOfMemory",
            DecodeError::InvalidData => "InvalidData",
        })
    }
}

impl std::error::Error for DecodeError {}

pub type DecodePngFn = fn(&[u8]) -> Result<Image, DecodeError>;

/// The result of decoding an image. The caller owns the returned data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

thread_local! {
    static DECODE_LIMIT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static DECODE_LIMIT_EXCEEDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Limits every allocation made through [`decode_alloc`] on this thread to
/// `limit` bytes while it is alive (Zig: passing a `LimitedAllocator` to the
/// decode function). Dropping it restores the previous limit.
pub struct DecodeLimit {
    prev_limit: Option<usize>,
    prev_exceeded: bool,
}

impl DecodeLimit {
    pub fn init(limit: usize) -> DecodeLimit {
        DecodeLimit {
            prev_limit: DECODE_LIMIT.with(|l| l.replace(Some(limit))),
            prev_exceeded: DECODE_LIMIT_EXCEEDED.with(|e| e.replace(false)),
        }
    }

    /// Set after an allocation is rejected for exceeding the limit
    /// (`LimitedAllocator.limit_exceeded`).
    pub fn limit_exceeded(&self) -> bool {
        DECODE_LIMIT_EXCEEDED.with(|e| e.get())
    }
}

impl Drop for DecodeLimit {
    fn drop(&mut self) {
        DECODE_LIMIT.with(|l| l.set(self.prev_limit));
        DECODE_LIMIT_EXCEEDED.with(|e| e.set(self.prev_exceeded));
    }
}

/// Allocate a zeroed buffer of `len` bytes for decoded data, honoring the
/// current thread's [`DecodeLimit`] (Zig: `alloc.alloc` with the allocator
/// handed to the decoder).
pub fn decode_alloc(len: usize) -> Result<Vec<u8>, DecodeError> {
    if let Some(limit) = DECODE_LIMIT.with(|l| l.get()) {
        if len > limit {
            DECODE_LIMIT_EXCEEDED.with(|e| e.set(true));
            return Err(DecodeError::OutOfMemory);
        }
    }
    let mut buf = Vec::new();
    buf.try_reserve_exact(len)
        .map_err(|_| DecodeError::OutOfMemory)?;
    buf.resize(len, 0);
    Ok(buf)
}

/// Serializes tests that replace [`DECODE_PNG`] (write lock) against tests
/// that decode PNGs (read lock), since Rust runs tests in parallel.
#[cfg(test)]
pub(crate) static DECODE_PNG_TEST_LOCK: RwLock<()> = RwLock::new(());

/// The default PNG decoder (`decodePngWuffs` in Zig). Produces 8-bit
/// non-premultiplied RGBA.
fn decode_png_default(data: &[u8]) -> Result<Image, DecodeError> {
    use png::{ColorType, Transformations};

    let mut decoder = png::Decoder::new_with_limits(
        std::io::Cursor::new(data),
        // Wuffs has no allocation limit other than overflow checks.
        png::Limits { bytes: usize::MAX },
    );
    decoder.set_transformations(Transformations::normalize_to_color8() | Transformations::ALPHA);
    let mut reader = decoder.read_info().map_err(map_png_error)?;
    let size = reader.output_buffer_size().ok_or(DecodeError::InvalidData)?;
    let mut buf = decode_alloc(size)?;
    let info = reader.next_frame(&mut buf).map_err(map_png_error)?;
    buf.truncate(info.buffer_size());

    let pixels = (info.width as usize)
        .checked_mul(info.height as usize)
        .ok_or(DecodeError::InvalidData)?;
    let rgba_len = pixels.checked_mul(4).ok_or(DecodeError::InvalidData)?;

    let rgba = match info.color_type {
        ColorType::Rgba => buf,
        other => {
            let mut out = decode_alloc(rgba_len)?;
            out.clear();
            match other {
                ColorType::Rgb => {
                    for px in buf.chunks_exact(3) {
                        out.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
                    }
                }
                ColorType::GrayscaleAlpha => {
                    for px in buf.chunks_exact(2) {
                        out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
                    }
                }
                ColorType::Grayscale => {
                    for &g in &buf {
                        out.extend_from_slice(&[g, g, g, 0xFF]);
                    }
                }
                // Expanded by the EXPAND transformation.
                ColorType::Indexed | ColorType::Rgba => return Err(DecodeError::InvalidData),
            }
            out
        }
    };

    if rgba.len() != rgba_len {
        return Err(DecodeError::InvalidData);
    }

    Ok(Image {
        width: info.width,
        height: info.height,
        data: rgba,
    })
}

fn map_png_error(err: png::DecodingError) -> DecodeError {
    match err {
        png::DecodingError::LimitsExceeded => DecodeError::OutOfMemory,
        _ => DecodeError::InvalidData,
    }
}

/// Fill a buffer with cryptographically secure random bytes. If null,
/// the terminal's `io` (`random_secure`) is used. This is an override
/// for embedders whose Io has no entropy source (e.g. wasm32-freestanding,
/// where TinyIo degrades to `std.Io.failing`) or that want to control
/// the source; when set it is used on every target.
///
/// This is used for secrets, so it must be a real CSPRNG. An error
/// makes the operation that needed the entropy fail; nothing falls back
/// to weaker randomness.
pub static RANDOM_SECURE: RwLock<Option<RandomSecureFn>> = RwLock::new(None);

/// Serializes tests that set [`RANDOM_SECURE`], since it is process-global
/// and Rust runs tests in parallel (Zig runs them sequentially).
#[cfg(test)]
pub(crate) static RANDOM_SECURE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Replace the secure random override (writes [`RANDOM_SECURE`]).
pub fn set_random_secure(f: Option<RandomSecureFn>) {
    *RANDOM_SECURE.write().unwrap_or_else(|e| e.into_inner()) = f;
}

/// `error{EntropyUnavailable}`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RandomSecureError {
    EntropyUnavailable,
}

impl fmt::Display for RandomSecureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EntropyUnavailable")
    }
}

impl std::error::Error for RandomSecureError {}

pub type RandomSecureFn = fn(&mut [u8]) -> Result<(), RandomSecureError>;

/// The entropy source of an Io (`std.Io.randomSecure`).
pub type IoRandomSecureFn = fn(&mut [u8]) -> Result<(), RandomSecureError>;

/// The OS entropy source (the default `std.Io` behavior).
pub fn os_random_secure(buffer: &mut [u8]) -> Result<(), RandomSecureError> {
    getrandom::fill(buffer).map_err(|_| RandomSecureError::EntropyUnavailable)
}

/// Fill `buffer` with secure random bytes from `RANDOM_SECURE` if set,
/// otherwise from `io`. Every use of secure entropy in the terminal
/// package goes through this so the override applies uniformly.
pub fn random_secure(io: IoRandomSecureFn, buffer: &mut [u8]) -> Result<(), RandomSecureError> {
    let override_fn = *RANDOM_SECURE.read().unwrap_or_else(|e| e.into_inner());
    if let Some(func) = override_fn {
        return func(buffer);
    }
    io(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failing_io(_: &mut [u8]) -> Result<(), RandomSecureError> {
        Err(RandomSecureError::EntropyUnavailable)
    }

    #[test]
    fn random_secure_override_is_preferred_over_the_io() {
        fn fill(buffer: &mut [u8]) -> Result<(), RandomSecureError> {
            buffer.fill(0xAB);
            Ok(())
        }
        fn fail(_: &mut [u8]) -> Result<(), RandomSecureError> {
            Err(RandomSecureError::EntropyUnavailable)
        }

        let _guard = RANDOM_SECURE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Without the override a failing Io fails.
        let mut buf = [0u8; 8];
        assert_eq!(
            Err(RandomSecureError::EntropyUnavailable),
            random_secure(failing_io, &mut buf)
        );

        // With it, the Io is never consulted.
        set_random_secure(Some(fill));
        let res = random_secure(failing_io, &mut buf);
        let all_ab = buf.iter().all(|&b| b == 0xAB);

        // An override failure surfaces as the Io's error.
        set_random_secure(Some(fail));
        let res2 = random_secure(os_random_secure, &mut buf);
        set_random_secure(None);

        assert_eq!(Ok(()), res);
        assert!(all_ab);
        assert_eq!(Err(RandomSecureError::EntropyUnavailable), res2);
    }

    #[test]
    fn decode_png_rgba() {
        // Encode a 2x1 RGB image and decode it back as RGBA.
        let mut encoded = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut encoded, 2, 1);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[1, 2, 3, 4, 5, 6]).unwrap();
        }

        let decode = decode_png().unwrap();
        let img = decode(&encoded).unwrap();
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 1);
        assert_eq!(img.data, vec![1, 2, 3, 255, 4, 5, 6, 255]);

        assert_eq!(Err(DecodeError::InvalidData), decode(b"not a png"));
    }
}
