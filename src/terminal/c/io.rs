//! Port of `src/terminal/c/io.zig`.
//!
//! STATUS: ported
//!
//! C byte-stream callbacks and adapters for the Rust I/O interfaces.
//!
//! The public callbacks deliberately expose a smaller contract than Zig's I/O
//! interfaces: reads are synchronous progress/EOF/error, and writes accept an
//! entire slice or fail. These adapters translate that contract while keeping
//! enough state to map failures back to distinct C result codes.
//!
//! Notes on the port: Zig's `std.Io.Reader`/`std.Io.Writer` are the
//! snapshot I/O traits ([`crate::terminal::snapshot::io::Reader`] and
//! [`crate::terminal::snapshot::io::Writer`]); [`WriterAdapter`] also
//! implements `std::io::Write` for the formatter. Rust adapters are not
//! self-referential, so the Zig `interface` field and its vtable are the
//! trait impls, and the one-byte peek buffer is attached from the start.
//! The vectored `drain(data, splat)` primitive is exposed as
//! [`WriterAdapter::drain`] with the `writeVecAll`/`writeSplatAll`/
//! `splatByteAll`/`flush` helpers the callers use.

use std::ffi::c_void;

use crate::lib::String as LibString;
use crate::terminal::snapshot::io as sio;

/// C: GhosttyReaderFn
pub type ReaderFn =
    unsafe extern "C" fn(userdata: *mut c_void, buffer: *mut u8, capacity: usize, out_read: *mut usize) -> bool;

/// C: GhosttyWriterFn
pub type WriterFn = unsafe extern "C" fn(userdata: *mut c_void, data: *const u8, len: usize) -> bool;

/// C: GhosttyReader
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Reader {
    pub read: Option<ReaderFn>,
    pub userdata: *mut c_void,
}

impl Default for Reader {
    fn default() -> Self {
        Reader {
            read: None,
            userdata: std::ptr::null_mut(),
        }
    }
}

impl Reader {
    pub fn valid(&self) -> bool {
        self.read.is_some()
    }
}

/// C: GhosttyWriter
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Writer {
    pub write: Option<WriterFn>,
    pub userdata: *mut c_void,
}

impl Default for Writer {
    fn default() -> Self {
        Writer {
            write: None,
            userdata: std::ptr::null_mut(),
        }
    }
}

impl Writer {
    pub fn valid(&self) -> bool {
        self.write.is_some()
    }
}

/// C: GhosttyMimeReaderFn
pub type MimeReaderFn = unsafe extern "C" fn(userdata: *mut c_void, mime: LibString, writer: Writer) -> bool;

/// C: GhosttyMimeReader
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MimeReader {
    pub read: Option<MimeReaderFn>,
    pub userdata: *mut c_void,
}

impl Default for MimeReader {
    fn default() -> Self {
        MimeReader {
            read: None,
            userdata: std::ptr::null_mut(),
        }
    }
}

impl MimeReader {
    pub fn valid(&self) -> bool {
        self.read.is_some()
    }
}

/// Adapts a `GhosttyReader` to the Rust reader interface.
///
/// A one-byte buffer backs operations such as `peek_byte` that require
/// buffering. One byte is intentional: it prevents reads beyond a requested
/// framing boundary.
pub struct ReaderAdapter {
    /// Copy of the C callback pair. The pointed-to userdata remains borrowed.
    pub source: Reader,

    /// Bytes successfully obtained from the callback.
    pub offset: usize,

    /// The callback returned false. This distinguishes an I/O failure from
    /// the callback returning true with zero bytes at end-of-file.
    pub callback_failed: bool,

    /// The callback was NULL, returned too many bytes, or overflowed offset.
    pub invalid_read: bool,

    /// The callback returned true with zero bytes. EOF is permanent.
    pub eof: bool,

    /// Backing storage for `peek_byte`. One byte is intentional: it
    /// satisfies that operation without allowing the adapter to pull a
    /// second byte across a snapshot checkpoint or FINISH boundary.
    peek_buffer: [u8; 1],
    /// Bytes of `peek_buffer` that are buffered (Zig: `interface.end -
    /// interface.seek`).
    peek_len: usize,

    /// Scratch space used only when the destination writer has no writable
    /// buffer of its own. 4 KiB amortizes callback overhead for streaming
    /// operations while keeping every adapter's fixed allocation modest. The
    /// caller's `limit` always truncates this slice, so the size cannot cause
    /// framing read-ahead.
    transfer_buffer: Box<[u8; 4096]>,
}

impl ReaderAdapter {
    pub fn init(source: Reader) -> ReaderAdapter {
        ReaderAdapter {
            source,
            offset: 0,
            callback_failed: false,
            invalid_read: false,
            eof: false,
            peek_buffer: [0],
            peek_len: 0,
            transfer_buffer: Box::new([0; 4096]),
        }
    }

    /// Perform exactly one C callback invocation into caller-selected storage.
    /// This is the single place that interprets the public callback contract.
    fn read_into(&mut self, destination: &mut [u8]) -> sio::Result<usize> {
        debug_assert!(!destination.is_empty());

        // Failure and EOF are sticky. In particular, a callback that reports
        // EOF is never polled again because the C API defines it as permanent.
        if self.callback_failed || self.invalid_read {
            return Err(sio::Error::ReadFailed);
        }
        if self.eof {
            return Err(sio::Error::EndOfStream);
        }

        // A missing callback is an invalid C value rather than external I/O
        // failure, so record it separately for the public result mapper.
        let Some(read_fn) = self.source.read else {
            self.invalid_read = true;
            return Err(sio::Error::ReadFailed);
        };

        // Initialize this ourselves: a callback returning false is not
        // required to initialize out_read.
        let mut read_len: usize = 0;
        // SAFETY: calling the embedder's callback with a valid buffer.
        let ok = unsafe {
            read_fn(
                self.source.userdata,
                destination.as_mut_ptr(),
                destination.len(),
                &mut read_len,
            )
        };
        if !ok {
            self.callback_failed = true;
            return Err(sio::Error::ReadFailed);
        }

        // Treat a callback that violates the capacity contract as an invalid
        // reader without constructing an out-of-bounds slice.
        if read_len > destination.len() {
            self.invalid_read = true;
            return Err(sio::Error::ReadFailed);
        }
        // Successful zero-length reads are the C representation of EOF.
        if read_len == 0 {
            self.eof = true;
            return Err(sio::Error::EndOfStream);
        }

        // Offset counts only bytes actually supplied by successful callbacks.
        // Saturating or wrapping would make SOURCE_OFFSET untrustworthy.
        self.offset = match self.offset.checked_add(read_len) {
            Some(v) => v,
            None => {
                self.invalid_read = true;
                return Err(sio::Error::ReadFailed);
            }
        };
        Ok(read_len)
    }

    /// The fundamental read primitive (Zig: the `stream` vtable function).
    /// It moves at most `limit` bytes from the C source to `writer` and
    /// reports the number transferred.
    pub fn stream(&mut self, writer: &mut dyn sio::Writer, limit: usize) -> sio::Result<usize> {
        // A zero limit is a successful no-op and must not invoke user code.
        if limit == 0 {
            return Ok(0);
        }

        // Buffered bytes are handed out first.
        if self.peek_len > 0 {
            writer.write_all(&self.peek_buffer[..1])?;
            self.peek_len = 0;
            return Ok(1);
        }

        // Read into bounded adapter storage and then let the writer take
        // ownership of the complete slice.
        let len = limit.min(self.transfer_buffer.len());
        let mut transfer = std::mem::replace(&mut self.transfer_buffer, Box::new([0; 4096]));
        let result = self.read_into(&mut transfer[..len]);
        let out = match result {
            // write_all either transfers this complete callback result or
            // reports failure; returning a partial count would violate
            // stream's contract.
            Ok(read_len) => writer.write_all(&transfer[..read_len]).map(|_| read_len),
            Err(e) => Err(e),
        };
        self.transfer_buffer = transfer;
        out
    }

    /// Stream exactly `n` bytes into `writer` (Zig: `streamExact`).
    pub fn stream_exact(&mut self, writer: &mut dyn sio::Writer, n: usize) -> sio::Result<()> {
        let mut remaining = n;
        while remaining > 0 {
            remaining -= self.stream(writer, remaining)?;
        }
        Ok(())
    }
}

impl sio::Reader for ReaderAdapter {
    fn buffered(&self) -> &[u8] {
        &self.peek_buffer[..self.peek_len]
    }

    fn toss(&mut self, n: usize) {
        debug_assert!(n <= self.peek_len);
        self.peek_len -= n;
    }

    fn fill_more(&mut self) -> sio::Result<()> {
        if self.peek_len == self.peek_buffer.len() {
            return Ok(());
        }
        let mut buf = [0u8; 1];
        let n = self.read_into(&mut buf)?;
        debug_assert_eq!(n, 1);
        self.peek_buffer[0] = buf[0];
        self.peek_len = 1;
        Ok(())
    }

    fn read_direct(&mut self, dest: &mut [u8]) -> sio::Result<usize> {
        // Use the destination's storage directly when possible. This is
        // the normal path for readSlice operations.
        if self.peek_len > 0 {
            dest[0] = self.peek_buffer[0];
            self.peek_len = 0;
            return Ok(1);
        }
        self.read_into(dest)
    }
}

/// Adapts a `GhosttyWriter` to the Rust writer interfaces.
///
/// The adapter may be given a buffer via `init_buffered` so that the many
/// small writes typical of streaming producers (e.g. the formatter) are
/// batched into far fewer C callback invocations. A buffered adapter's owner
/// must call `flush` before reporting success so that the public contract
/// holds: success means the C callback has already accepted every byte.
pub struct WriterAdapter<'a> {
    /// Copy of the C callback pair. The pointed-to userdata remains borrowed.
    pub destination: Writer,

    /// Bytes accepted by successful callback invocations. Buffered bytes are
    /// counted only once a drain or flush hands them to the callback.
    pub offset: usize,

    /// The callback returned false.
    pub callback_failed: bool,

    /// The callback was NULL or offset accounting overflowed.
    pub invalid_write: bool,

    /// The batching buffer (Zig: `interface.buffer`).
    buffer: &'a mut [u8],
    /// Bytes parked in `buffer` (Zig: `interface.end`).
    end: usize,
}

impl<'a> WriterAdapter<'a> {
    /// The buffer length used by the C API entry points that stream through
    /// a callback. 4 KiB amortizes callback overhead while keeping every
    /// adapter's stack footprint modest, mirroring ReaderAdapter's
    /// transfer_buffer sizing.
    pub const RECOMMENDED_BUFFER_LEN: usize = 4096;

    /// An unbuffered adapter: every write reaches the callback immediately
    /// and no flush is required before returning.
    pub fn init(destination: Writer) -> WriterAdapter<'static> {
        WriterAdapter {
            destination,
            offset: 0,
            callback_failed: false,
            invalid_write: false,
            buffer: &mut [],
            end: 0,
        }
    }

    /// A buffered adapter. The buffer is referenced, not copied. The owner
    /// must flush before treating the operation as successful.
    pub fn init_buffered(destination: Writer, buffer: &'a mut [u8]) -> WriterAdapter<'a> {
        WriterAdapter {
            destination,
            offset: 0,
            callback_failed: false,
            invalid_write: false,
            buffer,
            end: 0,
        }
    }

    /// Submit one ordinary slice through the all-or-nothing C write callback.
    fn write_all_callback(&mut self, data: &[u8]) -> sio::Result<()> {
        // Empty writes carry no information and should not call foreign code.
        if data.is_empty() {
            return Ok(());
        }

        // Make failures sticky so a higher-level retry cannot produce a
        // misleading second callback after the output is already partial.
        if self.callback_failed || self.invalid_write {
            return Err(sio::Error::WriteFailed);
        }

        // Missing callbacks are invalid arguments; false callback returns are
        // external I/O errors. Preserve that distinction for the C wrapper.
        let Some(write_fn) = self.destination.write else {
            self.invalid_write = true;
            return Err(sio::Error::WriteFailed);
        };
        // SAFETY: calling the embedder's callback with a valid slice.
        if !unsafe { write_fn(self.destination.userdata, data.as_ptr(), data.len()) } {
            self.callback_failed = true;
            return Err(sio::Error::WriteFailed);
        }

        // Count bytes only after the callback accepts the complete slice.
        self.offset = match self.offset.checked_add(data.len()) {
            Some(v) => v,
            None => {
                self.invalid_write = true;
                return Err(sio::Error::WriteFailed);
            }
        };
        Ok(())
    }

    /// Hand the parked buffer bytes to the callback.
    fn drain_buffer(&mut self) -> sio::Result<()> {
        if self.end > 0 {
            let end = self.end;
            let buffer = std::mem::take(&mut self.buffer);
            let result = self.write_all_callback(&buffer[..end]);
            self.buffer = buffer;
            result?;
            self.end = 0;
        }
        Ok(())
    }

    /// Write one slice, batching through the buffer when one is attached.
    /// Slices that fit are parked in the buffer (a later drain or flush
    /// hands them to the callback); anything larger goes to the callback
    /// directly after the buffer is emptied to preserve ordering.
    fn write_slice(&mut self, slice: &[u8]) -> sio::Result<()> {
        if slice.len() <= self.buffer.len() - self.end {
            self.buffer[self.end..self.end + slice.len()].copy_from_slice(slice);
            self.end += slice.len();
            return Ok(());
        }

        self.drain_buffer()?;

        if slice.len() <= self.buffer.len() {
            self.buffer[..slice.len()].copy_from_slice(slice);
            self.end = slice.len();
            Ok(())
        } else {
            self.write_all_callback(slice)
        }
    }

    /// The sole write primitive (Zig: the `drain` vtable function).
    ///
    /// Zig represents a vector write as ordinary slices followed by the last
    /// slice repeated `splat` times. The C callback has no vector form, so
    /// this method expands that representation, batching through the
    /// buffer when one is attached, and returns the total logical byte
    /// count consumed from `data`. Bytes parked in the buffer count as
    /// consumed.
    pub fn drain(&mut self, data: &[&[u8]], splat: usize) -> sio::Result<usize> {
        debug_assert!(!data.is_empty()); // The final element is always the splat pattern.

        // Buffered bytes must reach the destination before any of `data`.
        self.drain_buffer()?;

        let mut consumed: usize = 0;
        // Every element except the last is written exactly once.
        for slice in &data[..data.len() - 1] {
            self.write_slice(slice)?;
            consumed = match consumed.checked_add(slice.len()) {
                Some(v) => v,
                None => {
                    self.invalid_write = true;
                    return Err(sio::Error::WriteFailed);
                }
            };
        }

        // The final element is a compact repeated pattern. A splat of zero
        // means the final element is not part of this write.
        let pattern = data[data.len() - 1];
        if pattern.len() == 1 && !self.buffer.is_empty() {
            // Single-byte splats (e.g. runs of blank cells) are memset
            // into the buffer in bulk rather than repeated one write at
            // a time.
            let mut remaining = splat;
            while remaining > 0 {
                if self.end == self.buffer.len() {
                    self.drain_buffer()?;
                }
                let n = remaining.min(self.buffer.len() - self.end);
                self.buffer[self.end..self.end + n].fill(pattern[0]);
                self.end += n;
                remaining -= n;
            }
            consumed = match consumed.checked_add(splat) {
                Some(v) => v,
                None => {
                    self.invalid_write = true;
                    return Err(sio::Error::WriteFailed);
                }
            };
        } else {
            for _ in 0..splat {
                self.write_slice(pattern)?;
                consumed = match consumed.checked_add(pattern.len()) {
                    Some(v) => v,
                    None => {
                        self.invalid_write = true;
                        return Err(sio::Error::WriteFailed);
                    }
                };
            }
        }
        Ok(consumed)
    }

    /// Write every slice (Zig: `writeVecAll`).
    pub fn write_vec_all(&mut self, data: &[&[u8]]) -> sio::Result<()> {
        for slice in data {
            self.write_all_bytes(slice)?;
        }
        Ok(())
    }

    /// Write the last slice of `data` `splat` times after the others (Zig:
    /// `writeSplatAll`).
    pub fn write_splat_all(&mut self, data: &[&[u8]], splat: usize) -> sio::Result<()> {
        self.drain(data, splat).map(|_| ())
    }

    /// Write `byte` `n` times (Zig: `splatByteAll`).
    pub fn splat_byte_all(&mut self, byte: u8, n: usize) -> sio::Result<()> {
        self.drain(&[&[byte]], n).map(|_| ())
    }

    /// Write one slice (Zig: `writeAll`), going through the buffer.
    pub fn write_all_bytes(&mut self, data: &[u8]) -> sio::Result<()> {
        if data.len() <= self.buffer.len() - self.end {
            self.buffer[self.end..self.end + data.len()].copy_from_slice(data);
            self.end += data.len();
            return Ok(());
        }
        self.drain(&[data, &[]], 0).map(|_| ())
    }

    /// Hand every buffered byte to the callback (Zig: `flush`).
    pub fn flush(&mut self) -> sio::Result<()> {
        self.drain_buffer()
    }
}

impl sio::Writer for WriterAdapter<'_> {
    fn write_all(&mut self, bytes: &[u8]) -> sio::Result<()> {
        self.write_all_bytes(bytes)
    }

    fn splat_byte_all(&mut self, byte: u8, n: usize) -> sio::Result<()> {
        WriterAdapter::splat_byte_all(self, byte, n)
    }
}

impl std::io::Write for WriterAdapter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_all_bytes(buf)
            .map(|_| buf.len())
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        WriterAdapter::flush(self).map_err(|e| std::io::Error::other(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::snapshot::io::Reader as _;
    use std::mem::{offset_of, size_of};

    fn ud<T>(v: &mut T) -> *mut c_void {
        v as *mut T as *mut c_void
    }

    #[test]
    fn c_reader_and_writer_layouts_keep_callback_first() {
        assert_eq!(0, offset_of!(Reader, read));
        assert_eq!(size_of::<Option<ReaderFn>>(), offset_of!(Reader, userdata));
        assert_eq!(size_of::<Option<ReaderFn>>() + size_of::<*mut c_void>(), size_of::<Reader>());

        assert_eq!(0, offset_of!(Writer, write));
        assert_eq!(0, offset_of!(MimeReader, read));
        assert_eq!(size_of::<Option<MimeReaderFn>>(), offset_of!(MimeReader, userdata));
        assert_eq!(size_of::<Option<WriterFn>>(), offset_of!(Writer, userdata));
        assert_eq!(size_of::<Option<WriterFn>>() + size_of::<*mut c_void>(), size_of::<Writer>());
    }

    struct ReadContext {
        data: &'static [u8],
        offset: usize,
        calls: usize,
        last_capacity: usize,
        max_chunk: usize,
    }

    impl ReadContext {
        fn new(data: &'static [u8], max_chunk: usize) -> Self {
            ReadContext {
                data,
                offset: 0,
                calls: 0,
                last_capacity: 0,
                max_chunk,
            }
        }

        unsafe extern "C" fn read(userdata: *mut c_void, buffer: *mut u8, capacity: usize, out_read: *mut usize) -> bool {
            let this = &mut *(userdata as *mut ReadContext);
            this.calls += 1;
            this.last_capacity = capacity;
            let remaining = &this.data[this.offset..];
            let len = remaining.len().min(capacity).min(this.max_chunk);
            std::ptr::copy_nonoverlapping(remaining.as_ptr(), buffer, len);
            this.offset += len;
            *out_read = len;
            true
        }
    }

    #[test]
    fn reader_adapter_supports_short_reads_and_permanent_eof() {
        let mut context = ReadContext::new(b"abcdef", 2);
        let mut adapter = ReaderAdapter::init(Reader {
            read: Some(ReadContext::read),
            userdata: ud(&mut context),
        });

        let mut actual = [0u8; 6];
        adapter.read_slice_all(&mut actual).unwrap();
        assert_eq!(b"abcdef", &actual);
        assert_eq!(6, adapter.offset);
        assert!(!adapter.eof);

        assert_eq!(Err(sio::Error::EndOfStream), adapter.take_byte());
        assert!(adapter.eof);
        assert!(!adapter.callback_failed);

        let calls_at_eof = context.calls;
        assert_eq!(Err(sio::Error::EndOfStream), adapter.take_byte());
        assert_eq!(calls_at_eof, context.calls);
    }

    #[test]
    fn reader_adapter_peek_reads_only_the_requested_byte() {
        let mut context = ReadContext::new(b"ab", usize::MAX);
        let mut adapter = ReaderAdapter::init(Reader {
            read: Some(ReadContext::read),
            userdata: ud(&mut context),
        });

        assert_eq!(Ok(b'a'), adapter.peek_byte());
        assert_eq!(1, context.last_capacity);
        assert_eq!(1, adapter.offset);
        assert_eq!(Ok(b'a'), adapter.take_byte());
        assert_eq!(1, adapter.offset);
    }

    #[test]
    fn reader_adapter_distinguishes_callback_failure_and_invalid_length() {
        unsafe extern "C" fn failing(_: *mut c_void, _: *mut u8, _: usize, _: *mut usize) -> bool {
            false
        }
        let mut failing = ReaderAdapter::init(Reader {
            read: Some(failing),
            userdata: std::ptr::null_mut(),
        });
        assert_eq!(Err(sio::Error::ReadFailed), failing.take_byte());
        assert!(failing.callback_failed);
        assert!(!failing.eof);
        assert!(!failing.invalid_read);

        unsafe extern "C" fn oversized(_: *mut c_void, _: *mut u8, capacity: usize, out_read: *mut usize) -> bool {
            *out_read = capacity + 1;
            true
        }
        let mut oversized = ReaderAdapter::init(Reader {
            read: Some(oversized),
            userdata: std::ptr::null_mut(),
        });
        assert_eq!(Err(sio::Error::ReadFailed), oversized.take_byte());
        assert!(!oversized.callback_failed);
        assert!(!oversized.eof);
        assert!(oversized.invalid_read);
    }

    #[test]
    fn reader_adapter_rejects_a_null_callback() {
        let mut adapter = ReaderAdapter::init(Reader::default());
        assert_eq!(Err(sio::Error::ReadFailed), adapter.take_byte());
        assert!(adapter.invalid_read);
        assert!(!adapter.callback_failed);
    }

    struct WriteContext {
        data: [u8; 256],
        len: usize,
        calls: usize,
        fail: bool,
    }

    impl WriteContext {
        fn new() -> Self {
            WriteContext {
                data: [0; 256],
                len: 0,
                calls: 0,
                fail: false,
            }
        }

        fn written(&self) -> &[u8] {
            &self.data[..self.len]
        }

        unsafe extern "C" fn write(userdata: *mut c_void, data: *const u8, len: usize) -> bool {
            let this = &mut *(userdata as *mut WriteContext);
            this.calls += 1;
            if this.fail {
                return false;
            }
            std::ptr::copy_nonoverlapping(data, this.data[this.len..].as_mut_ptr(), len);
            this.len += len;
            true
        }
    }

    #[test]
    fn writer_adapter_writes_vectors_and_splats() {
        let mut context = WriteContext::new();
        let mut adapter = WriterAdapter::init(Writer {
            write: Some(WriteContext::write),
            userdata: ud(&mut context),
        });

        adapter.write_vec_all(&[b"ab", b"cd"]).unwrap();
        adapter.write_splat_all(&[b"!"], 2).unwrap();

        assert_eq!(b"abcd!!", context.written());
        assert_eq!(6, adapter.offset);
        assert!(!adapter.callback_failed);
    }

    #[test]
    fn writer_adapter_buffered_batches_small_writes() {
        let mut context = WriteContext::new();
        let mut buffer = [0u8; 64];
        let mut adapter = WriterAdapter::init_buffered(
            Writer {
                write: Some(WriteContext::write),
                userdata: ud(&mut context),
            },
            &mut buffer,
        );

        // Many small writes fit in the buffer: no callback yet.
        for _ in 0..16 {
            adapter.write_all_bytes(b"ab").unwrap();
        }
        assert_eq!(0, context.calls);

        // Flush delivers everything in a single call.
        adapter.flush().unwrap();
        assert_eq!(1, context.calls);

        for i in 0..16 {
            assert_eq!(b"ab", &context.data[i * 2..i * 2 + 2]);
        }
        assert_eq!(32, adapter.offset);
    }

    #[test]
    fn writer_adapter_buffered_splats_and_large_writes() {
        let mut context = WriteContext::new();
        let mut buffer = [0u8; 16];
        let mut adapter = WriterAdapter::init_buffered(
            Writer {
                write: Some(WriteContext::write),
                userdata: ud(&mut context),
            },
            &mut buffer,
        );

        // A splat larger than the buffer is chunked, not one call per byte.
        adapter.splat_byte_all(b' ', 40).unwrap();
        // A write larger than the buffer flushes and goes to the callback
        // directly.
        adapter.write_all_bytes(b"0123456789abcdef0").unwrap(); // 17 > 16
        adapter.write_all_bytes(b"xy").unwrap();
        adapter.flush().unwrap();
        let offset = adapter.offset;

        let mut expected = vec![b' '; 40];
        expected.extend_from_slice(b"0123456789abcdef0");
        expected.extend_from_slice(b"xy");
        assert_eq!(&expected[..], context.written());
        assert_eq!(context.len, offset);
        // Chunked delivery: far fewer calls than bytes.
        assert!(context.calls <= 6);
    }

    #[test]
    fn writer_adapter_makes_callback_failure_sticky() {
        let mut context = WriteContext::new();
        context.fail = true;
        let mut adapter = WriterAdapter::init(Writer {
            write: Some(WriteContext::write),
            userdata: ud(&mut context),
        });
        assert_eq!(Err(sio::Error::WriteFailed), adapter.write_all_bytes(b"x"));
        assert!(adapter.callback_failed);
        assert_eq!(0, adapter.offset);

        assert_eq!(Err(sio::Error::WriteFailed), adapter.write_all_bytes(b"y"));
        assert_eq!(1, context.calls);
    }

    #[test]
    fn writer_adapter_rejects_a_null_callback() {
        let mut adapter = WriterAdapter::init(Writer::default());
        assert_eq!(Err(sio::Error::WriteFailed), adapter.write_all_bytes(b"x"));
        assert!(adapter.invalid_write);
        assert!(!adapter.callback_failed);
    }

    #[test]
    fn reader_adapter_streams_into_an_unbuffered_writer_adapter() {
        let mut read_context = ReadContext::new(b"stream", usize::MAX);
        let mut reader = ReaderAdapter::init(Reader {
            read: Some(ReadContext::read),
            userdata: ud(&mut read_context),
        });
        let mut write_context = WriteContext::new();
        let mut writer = WriterAdapter::init(Writer {
            write: Some(WriteContext::write),
            userdata: ud(&mut write_context),
        });

        reader.stream_exact(&mut writer, b"stream".len()).unwrap();
        let writer_offset = writer.offset;
        assert_eq!(b"stream", write_context.written());
        assert_eq!(b"stream".len(), reader.offset);
        assert_eq!(b"stream".len(), writer_offset);
    }
}
