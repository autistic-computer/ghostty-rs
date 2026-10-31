//! Port of `src/terminal/snapshot/io.zig`.
//!
//! STATUS: ported
//!
//! Wire-format I/O helpers.
//!
//! These are sometimes very thin layers over standard `std.Io.Writer`
//! but its to help ensure consistency in our behavior and avoid some
//! pitfalls.
//!
//! Notes on the port:
//! - Zig's `std.Io.Reader`/`std.Io.Writer` interfaces have no direct Rust
//!   counterpart with the semantics the snapshot codecs rely on (a peekable
//!   buffer that can be borrowed in place, `toss`, exact `EndOfStream`
//!   errors). This module therefore also hosts the minimal port of those
//!   interfaces: the [`Reader`] and [`Writer`] traits and the adapters the
//!   snapshot codecs and their tests use ([`FixedReader`] = `Reader.fixed`,
//!   [`Limited`] = `Reader.limited`, [`Hashed`] = `Reader.Hashed`,
//!   [`FixedWriter`] = `Writer.fixed`, `Vec<u8>` = `Writer.Allocating`,
//!   [`Discarding`] = `Writer.Discarding`).
//! - Zig merges every error set of the snapshot package into one global
//!   error namespace. The port uses a single [`Error`] enum carrying every
//!   error name used by the snapshot codecs; each module keeps its Zig error
//!   set names as type aliases of it.

use std::fmt;

use crate::crc32c::Crc32c;
use crate::terminal::page::IntegrityError;
use crate::terminal::stream_continuation::ValidateError;

/// Every error the snapshot codecs can return (the union of the Zig error
/// sets of the snapshot package).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    // std.Io.Reader.Error / std.Io.Writer.Error
    ReadFailed,
    EndOfStream,
    WriteFailed,
    // Allocator.Error
    OutOfMemory,

    // envelope
    InvalidMagic,
    UnsupportedVersion,

    // record
    InvalidChecksum,
    PayloadNotExhausted,
    PayloadTooLarge,
    UnexpectedRecordTag,

    // continuation (stream_continuation.ValidateError and own)
    NoPendingState,
    NonCanonicalContinuation,
    ReplayWouldCommit,
    Overflow,
    ContinuationLimitExceeded,

    // style
    InvalidColorKind,
    InvalidColor,
    InvalidUnderline,
    InvalidFlags,
    InvalidReserved,

    // hyperlink
    InvalidKind,
    InvalidUri,
    InvalidExplicitId,
    StringTooLong,

    // grid
    InvalidWideCell,
    TooManyGraphemes,
    InvalidRowCellCount,

    // page
    InvalidDimensions,
    PageTooLarge,
    InvalidDestinationCapacity,
    /// `Page.IntegrityError` (Zig merges its members into the error set).
    Integrity(IntegrityError),

    // screen
    InvalidCursorFlags,
    InvalidCharsetState,
    InvalidKittyKeyboardIndex,
    InvalidKittyKeyboardFlags,
    InvalidSavedCursorFlags,
    PageCountOverflow,
    InvalidPageCount,
    InvalidKey,
    // PageList.Builder.FinishError
    InvalidPageDimensions,
    NoPages,
    InsufficientRows,
    // PageList.IncreaseCapacityError
    OutOfSpace,

    // history
    UnexpectedScreenKey,
    ExistingHistory,
    // PageList.PageAllocation.FinalizeError
    RowCountOverflow,
    PageSizeOverflow,
    MaxSizeExceeded,
    MaxLinesExceeded,

    // terminal
    InvalidScrollingRegion,
    InvalidScreenCount,
    InvalidActiveScreenKey,
    InvalidPreviousCodepoint,
    InvalidScrollbackLimit,
    ScrollbackLimitOverflow,
    InvalidTabStops,
    InvalidPalette,

    // kitty
    UnexpectedKittyKey,

    // snapshot
    DuplicateScreen,
    DecoderNotReady,
    DecoderFailed,
    UnexpectedHistoryKey,
    DuplicateHistory,
    TrailingData,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Integrity(e) => write!(f, "{e}"),
            other => write!(f, "{other:?}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<ValidateError> for Error {
    fn from(e: ValidateError) -> Self {
        match e {
            ValidateError::NoPendingState => Error::NoPendingState,
            ValidateError::NonCanonicalContinuation => Error::NonCanonicalContinuation,
            ValidateError::ReplayWouldCommit => Error::ReplayWouldCommit,
        }
    }
}

impl From<IntegrityError> for Error {
    fn from(e: IntegrityError) -> Self {
        Error::Integrity(e)
    }
}

impl From<crate::terminal::size::AllocError> for Error {
    fn from(_: crate::terminal::size::AllocError) -> Self {
        Error::OutOfMemory
    }
}

impl From<crate::terminal::page_list::FinishError> for Error {
    fn from(e: crate::terminal::page_list::FinishError) -> Self {
        use crate::terminal::page_list::FinishError as E;
        match e {
            E::OutOfMemory => Error::OutOfMemory,
            E::InvalidDimensions => Error::InvalidDimensions,
            E::InvalidPageDimensions => Error::InvalidPageDimensions,
            E::NoPages => Error::NoPages,
            E::InsufficientRows => Error::InsufficientRows,
        }
    }
}

impl From<crate::terminal::page_list::IncreaseCapacityError> for Error {
    fn from(e: crate::terminal::page_list::IncreaseCapacityError) -> Self {
        use crate::terminal::page_list::IncreaseCapacityError as E;
        match e {
            E::OutOfMemory => Error::OutOfMemory,
            E::OutOfSpace => Error::OutOfSpace,
        }
    }
}

impl From<crate::terminal::page_list::FinalizeError> for Error {
    fn from(e: crate::terminal::page_list::FinalizeError) -> Self {
        use crate::terminal::page_list::FinalizeError as E;
        match e {
            E::InvalidPageDimensions => Error::InvalidPageDimensions,
            E::RowCountOverflow => Error::RowCountOverflow,
            E::PageSizeOverflow => Error::PageSizeOverflow,
            E::MaxSizeExceeded => Error::MaxSizeExceeded,
            E::MaxLinesExceeded => Error::MaxLinesExceeded,
        }
    }
}

/// `std.Io.Reader.Error || std.Io.Writer.Error`; kept as an alias so
/// signatures read like the Zig ones.
pub type IoError = Error;

/// Shorthand result type.
pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Integers

/// Fixed-size unsigned integers transported little-endian.
pub trait Int: Copy {
    const SIZE: usize;
    fn write_le(self, out: &mut [u8]);
    fn read_le(bytes: &[u8]) -> Self;
}

macro_rules! impl_int {
    ($($t:ty),*) => {$(
        impl Int for $t {
            const SIZE: usize = std::mem::size_of::<$t>();
            #[inline(always)]
            fn write_le(self, out: &mut [u8]) {
                out[..Self::SIZE].copy_from_slice(&self.to_le_bytes());
            }
            #[inline(always)]
            fn read_le(bytes: &[u8]) -> Self {
                let mut buf = [0u8; std::mem::size_of::<$t>()];
                buf.copy_from_slice(&bytes[..Self::SIZE]);
                <$t>::from_le_bytes(buf)
            }
        }
    )*};
}
impl_int!(u8, u16, u32, u64);

/// writeInt ensures we write in little-endian since that is our
/// standard form.
pub fn write_int<T: Int>(writer: &mut dyn Writer, value: T) -> Result<()> {
    let mut buf = [0u8; 8];
    value.write_le(&mut buf);
    writer.write_all(&buf[..T::SIZE])
}

/// Read a little-endian integer.
///
/// This avoids `takeInt` because at the time of writing this the underlying
/// implementation uses `takeArray` which asserts that the reader buffer
/// is as large as T. Our approach works with caller-selected buffers that
/// may be smaller.
pub fn read_int<T: Int>(reader: &mut dyn Reader) -> Result<T> {
    let mut buf = [0u8; 8];
    reader.read_slice_all(&mut buf[..T::SIZE])?;
    Ok(T::read_le(&buf))
}

// ---------------------------------------------------------------------------
// Writer

/// Port of the subset of `std.Io.Writer` used by the snapshot codecs.
pub trait Writer {
    /// Write every byte or fail with `WriteFailed`.
    fn write_all(&mut self, bytes: &[u8]) -> Result<()>;

    /// Reserve exactly `n` bytes at the end of the written data and return
    /// them for the caller to fill completely, or `None` when the writer
    /// cannot provide contiguous space (Zig: `unusedCapacitySlice` followed
    /// by `advance`).
    fn reserve_slice(&mut self, _n: usize) -> Option<&mut [u8]> {
        None
    }

    fn write_byte(&mut self, byte: u8) -> Result<()> {
        self.write_all(&[byte])
    }

    /// Write `byte` `n` times (Zig: `splatByteAll`).
    fn splat_byte_all(&mut self, byte: u8, n: usize) -> Result<()> {
        let chunk = [byte; 64];
        let mut remaining = n;
        while remaining > 0 {
            let len = remaining.min(chunk.len());
            self.write_all(&chunk[..len])?;
            remaining -= len;
        }
        Ok(())
    }
}

impl<W: Writer + ?Sized> Writer for &mut W {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        (**self).write_all(bytes)
    }
    fn reserve_slice(&mut self, n: usize) -> Option<&mut [u8]> {
        (**self).reserve_slice(n)
    }
}

/// `std.Io.Writer.Allocating`.
impl Writer for Vec<u8> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        if self.try_reserve(bytes.len()).is_err() {
            return Err(Error::WriteFailed);
        }
        self.extend_from_slice(bytes);
        Ok(())
    }

    fn reserve_slice(&mut self, n: usize) -> Option<&mut [u8]> {
        if self.try_reserve(n).is_err() {
            return None;
        }
        let len = self.len();
        self.resize(len + n, 0);
        Some(&mut self[len..])
    }
}

/// `std.Io.Writer.fixed`: writes into a caller buffer and fails with
/// `WriteFailed` once it is full.
pub struct FixedWriter<'a> {
    buffer: &'a mut [u8],
    /// Number of bytes written.
    pub end: usize,
}

impl<'a> FixedWriter<'a> {
    pub fn new(buffer: &'a mut [u8]) -> FixedWriter<'a> {
        FixedWriter { buffer, end: 0 }
    }

    /// The bytes written so far.
    pub fn buffered(&self) -> &[u8] {
        &self.buffer[..self.end]
    }
}

impl Writer for FixedWriter<'_> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        let available = self.buffer.len() - self.end;
        let n = bytes.len().min(available);
        self.buffer[self.end..self.end + n].copy_from_slice(&bytes[..n]);
        self.end += n;
        if n < bytes.len() {
            return Err(Error::WriteFailed);
        }
        Ok(())
    }

    fn reserve_slice(&mut self, n: usize) -> Option<&mut [u8]> {
        if self.buffer.len() - self.end < n {
            return None;
        }
        let start = self.end;
        self.end += n;
        Some(&mut self.buffer[start..start + n])
    }
}

/// `std.Io.Writer.Discarding`: counts and drops every byte.
#[derive(Debug, Default)]
pub struct Discarding {
    pub count: u64,
}

impl Writer for Discarding {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.count += bytes.len() as u64;
        Ok(())
    }
}

/// `std.Io.Writer.Hashing(Crc32c)`: feeds every byte to a CRC32C hasher.
#[derive(Debug, Clone, Copy)]
pub struct HashingWriter {
    pub hasher: Crc32c,
}

impl Writer for HashingWriter {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.hasher.update(bytes);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Reader

/// Port of the subset of `std.Io.Reader` used by the snapshot codecs.
///
/// A reader exposes a (possibly empty) window of already buffered bytes
/// that callers may inspect in place (`buffered`) and consume (`toss`).
pub trait Reader {
    /// The bytes currently buffered (Zig: `buffered()`).
    fn buffered(&self) -> &[u8];

    /// Consume `n` buffered bytes. `n` must not exceed `buffered().len()`.
    fn toss(&mut self, n: usize);

    /// Buffer at least one more byte, or fail with `EndOfStream` when the
    /// stream has ended (or `ReadFailed`).
    fn fill_more(&mut self) -> Result<()>;

    /// Read at least one byte into `dest` (which must be nonempty), using
    /// buffered bytes first. Returns the number of bytes read.
    fn read_direct(&mut self, dest: &mut [u8]) -> Result<usize> {
        if self.buffered().is_empty() {
            self.fill_more()?;
        }
        let buffered = self.buffered();
        let n = buffered.len().min(dest.len());
        dest[..n].copy_from_slice(&buffered[..n]);
        self.toss(n);
        Ok(n)
    }

    /// Number of buffered bytes (Zig: `bufferedLen`).
    fn buffered_len(&self) -> usize {
        self.buffered().len()
    }

    /// Fill `dest` completely (Zig: `readSliceAll`).
    fn read_slice_all(&mut self, dest: &mut [u8]) -> Result<()> {
        let mut offset = 0;
        while offset < dest.len() {
            offset += self.read_direct(&mut dest[offset..])?;
        }
        Ok(())
    }

    /// Read one byte (Zig: `takeByte`).
    fn take_byte(&mut self) -> Result<u8> {
        if let Some(&b) = self.buffered().first() {
            self.toss(1);
            return Ok(b);
        }
        let mut buf = [0u8; 1];
        self.read_slice_all(&mut buf)?;
        Ok(buf[0])
    }

    /// Return the next byte without consuming it (Zig: `peekByte`).
    fn peek_byte(&mut self) -> Result<u8> {
        if self.buffered().is_empty() {
            self.fill_more()?;
        }
        Ok(self.buffered()[0])
    }

    /// Read exactly `n` bytes into a new vector (Zig: `take`, which returns
    /// a slice of the reader buffer).
    fn take(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut out = vec![0u8; n];
        self.read_slice_all(&mut out)?;
        Ok(out)
    }

    /// Discard exactly `n` bytes (Zig: `discardAll`).
    fn discard_all(&mut self, n: usize) -> Result<()> {
        let mut remaining = n;
        let mut scratch = [0u8; 256];
        while remaining > 0 {
            let buffered = self.buffered_len();
            if buffered > 0 {
                let k = buffered.min(remaining);
                self.toss(k);
                remaining -= k;
                continue;
            }
            let len = remaining.min(scratch.len());
            remaining -= self.read_direct(&mut scratch[..len])?;
        }
        Ok(())
    }

    /// Discard every remaining byte, returning how many were discarded
    /// (Zig: `discardRemaining`).
    fn discard_remaining(&mut self) -> Result<usize> {
        let mut total = 0;
        let mut scratch = [0u8; 256];
        loop {
            let buffered = self.buffered_len();
            if buffered > 0 {
                self.toss(buffered);
                total += buffered;
                continue;
            }
            match self.read_direct(&mut scratch) {
                Ok(n) => total += n,
                Err(Error::EndOfStream) => return Ok(total),
                Err(e) => return Err(e),
            }
        }
    }
}

impl<R: Reader + ?Sized> Reader for &mut R {
    fn buffered(&self) -> &[u8] {
        (**self).buffered()
    }
    fn toss(&mut self, n: usize) {
        (**self).toss(n)
    }
    fn fill_more(&mut self) -> Result<()> {
        (**self).fill_more()
    }
    fn read_direct(&mut self, dest: &mut [u8]) -> Result<usize> {
        (**self).read_direct(dest)
    }
}

/// `std.Io.Reader.fixed`: reads from an in-memory slice, which is entirely
/// buffered.
#[derive(Debug, Clone)]
pub struct FixedReader<'a> {
    buffer: &'a [u8],
    /// Number of bytes consumed.
    pub seek: usize,
}

impl<'a> FixedReader<'a> {
    pub fn new(buffer: &'a [u8]) -> FixedReader<'a> {
        FixedReader { buffer, seek: 0 }
    }
}

impl Reader for FixedReader<'_> {
    fn buffered(&self) -> &[u8] {
        &self.buffer[self.seek..]
    }
    fn toss(&mut self, n: usize) {
        debug_assert!(n <= self.buffer.len() - self.seek);
        self.seek += n;
    }
    fn fill_more(&mut self) -> Result<()> {
        Err(Error::EndOfStream)
    }
}

/// `std.Io.Reader.Limited`: reads at most `limit` bytes (or everything
/// with `None`) from `source`, buffering through a caller-sized buffer.
pub struct Limited<R> {
    source: R,
    /// Bytes that may still be pulled from `source`; `None` is unlimited.
    pub remaining: Option<usize>,
    buffer: Box<[u8]>,
    start: usize,
    end: usize,
}

impl<R: Reader> Limited<R> {
    /// `source.limited(limit, &buffer)` with a buffer of `buffer_len` bytes.
    pub fn new(source: R, limit: Option<usize>, buffer_len: usize) -> Limited<R> {
        Limited {
            source,
            remaining: limit,
            buffer: vec![0u8; buffer_len].into_boxed_slice(),
            start: 0,
            end: 0,
        }
    }

    fn allowed(&self, want: usize) -> usize {
        match self.remaining {
            Some(r) => r.min(want),
            None => want,
        }
    }

    fn consumed(&mut self, n: usize) {
        if let Some(r) = &mut self.remaining {
            *r -= n;
        }
    }
}

impl<R: Reader> Reader for Limited<R> {
    fn buffered(&self) -> &[u8] {
        &self.buffer[self.start..self.end]
    }
    fn toss(&mut self, n: usize) {
        debug_assert!(n <= self.end - self.start);
        self.start += n;
    }
    fn fill_more(&mut self) -> Result<()> {
        if self.start == self.end {
            self.start = 0;
            self.end = 0;
        } else if self.end == self.buffer.len() && self.start > 0 {
            self.buffer.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        let space = self.buffer.len() - self.end;
        let want = self.allowed(space);
        if want == 0 {
            // A zero-capacity buffer cannot hold bytes; a reached limit
            // is the end of this stream.
            return Err(Error::EndOfStream);
        }
        let end = self.end;
        let n = self.source.read_direct(&mut self.buffer[end..end + want])?;
        self.consumed(n);
        self.end += n;
        Ok(())
    }
    fn read_direct(&mut self, dest: &mut [u8]) -> Result<usize> {
        if self.start < self.end {
            let n = (self.end - self.start).min(dest.len());
            dest[..n].copy_from_slice(&self.buffer[self.start..self.start + n]);
            self.start += n;
            return Ok(n);
        }
        let want = self.allowed(dest.len());
        if want == 0 {
            return Err(Error::EndOfStream);
        }
        let n = self.source.read_direct(&mut dest[..want])?;
        self.consumed(n);
        Ok(n)
    }
}

/// `std.Io.Reader.Hashed(Crc32c)`: every byte pulled from `source` updates
/// `hasher`.
pub struct Hashed<R> {
    source: R,
    pub hasher: Crc32c,
    buffer: [u8; 256],
    start: usize,
    end: usize,
}

impl<R: Reader> Hashed<R> {
    pub fn new(source: R, hasher: Crc32c) -> Hashed<R> {
        Hashed {
            source,
            hasher,
            buffer: [0u8; 256],
            start: 0,
            end: 0,
        }
    }

    /// The wrapped source reader.
    pub fn source(&self) -> &R {
        &self.source
    }
}

impl<R: Reader> Reader for Hashed<R> {
    fn buffered(&self) -> &[u8] {
        &self.buffer[self.start..self.end]
    }
    fn toss(&mut self, n: usize) {
        debug_assert!(n <= self.end - self.start);
        self.start += n;
    }
    fn fill_more(&mut self) -> Result<()> {
        if self.start == self.end {
            self.start = 0;
            self.end = 0;
        } else if self.end == self.buffer.len() && self.start > 0 {
            self.buffer.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        let end = self.end;
        let n = self.source.read_direct(&mut self.buffer[end..])?;
        self.hasher.update(&self.buffer[end..end + n]);
        self.end += n;
        Ok(())
    }
    fn read_direct(&mut self, dest: &mut [u8]) -> Result<usize> {
        if self.start < self.end {
            let n = (self.end - self.start).min(dest.len());
            dest[..n].copy_from_slice(&self.buffer[self.start..self.start + n]);
            self.start += n;
            return Ok(n);
        }
        // Bulk reads bypass the buffer entirely.
        if dest.len() >= self.buffer.len() {
            let n = self.source.read_direct(dest)?;
            self.hasher.update(&dest[..n]);
            return Ok(n);
        }
        self.fill_more()?;
        self.read_direct(dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_round_trip_with_a_one_byte_reader_buffer() {
        let mut encoded = [0u8; 6];
        let mut writer = FixedWriter::new(&mut encoded);
        write_int(&mut writer, 0x1234u16).unwrap();
        write_int(&mut writer, 0x56789abcu32).unwrap();

        // Keep the reader buffer smaller than either integer to verify readInt
        // does not inherit std.Io.Reader.takeInt's buffer-size requirement.
        let mut source = FixedReader::new(&encoded);
        let mut limited = Limited::new(&mut source, None, 1);

        assert_eq!(0x1234u16, read_int::<u16>(&mut limited).unwrap());
        assert_eq!(0x56789abcu32, read_int::<u32>(&mut limited).unwrap());
    }
}
