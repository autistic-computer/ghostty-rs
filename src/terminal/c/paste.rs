//! Port of `src/terminal/c/paste.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: a refused sink write can only come from the read
//! buffer failing to grow, which can't fail in Rust; the [`PASTE_TW`]
//! tripwire stands in for it in tests (Zig uses a failing allocator).

use std::ffi::c_void;

use super::io;
use super::result::Result;
use super::terminal::Terminal;
use super::types::{slice_or_empty, CApiEnum};
use crate::terminal::paste as terminal_paste_pkg;
use crate::terminal::stream_terminal::PasteError;
use crate::input::paste;
use crate::lib::r#enum::LibEnum;
use crate::lib::String as LibString;
use crate::terminal::clipboard;

crate::lib_enum! {
    /// Why a paste happened. The flat C form of the Zig tagged union
    /// (terminal.paste.Source); the location rides alongside in
    /// GhosttyPaste and only applies to a clipboard paste.
    ///
    /// C: GhosttyPasteSource
    pub enum Source: c {
        Clipboard,
        Text,
    }
}

impl CApiEnum for Source {
    fn zig_fields() -> Vec<(String, i64)> {
        Source::FIELDS
            .iter()
            .map(|(name, v)| (super::types::zig_name(name), v.backing() as i64))
            .collect()
    }
}

/// A paste of clipboard contents into the terminal. Sized struct.
///
/// C: GhosttyPaste
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Request {
    pub size: usize,
    pub location: clipboard::Location,
    pub source: Source,
    pub mimes: *const LibString,
    pub mimes_len: usize,
    pub reader: io::MimeReader,
    pub allow_unsafe: bool,
}

impl Request {
    /// A request with the `size` field set (Zig: the field default).
    pub fn new(
        location: clipboard::Location,
        source: Source,
        mimes: *const LibString,
        mimes_len: usize,
        reader: io::MimeReader,
        allow_unsafe: bool,
    ) -> Request {
        Request {
            size: std::mem::size_of::<Request>(),
            location,
            source,
            mimes,
            mimes_len,
            reader,
            allow_unsafe,
        }
    }
}

/// C: ghostty_terminal_paste
///
/// # Safety
/// `terminal_` and `req_` must be NULL or valid; the request's MIME list
/// and reader must follow the `GhosttyPaste` contract; a non-null
/// `out_written` must be writable.
#[export_name = "ghostty_terminal_paste"]
pub unsafe extern "C" fn terminal_paste(terminal_: Terminal, req_: *const Request, out_written: *mut bool) -> Result {
    let Some(wrapper) = terminal_.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(req) = req_.as_ref() else {
        return Result::InvalidValue;
    };

    // Every field is required; a smaller size is a caller from a
    // different ABI version than any this struct has had.
    if req.size < std::mem::size_of::<Request>() {
        return Result::InvalidValue;
    }

    // The handler always has a write_pty trampoline that no-ops without
    // a C callback, so the "nothing can be written" check is ours.
    if wrapper.effects.write_pty.is_none() {
        return Result::InvalidValue;
    }

    let c_mimes: &[LibString] = slice_or_empty(req.mimes, req.mimes_len);

    // Only a paste with something to read needs a reader.
    if !c_mimes.is_empty() && !req.reader.valid() {
        return Result::InvalidValue;
    }

    let mimes: Vec<&[u8]> = c_mimes.iter().map(|m| slice_or_empty(m.ptr, m.len)).collect();

    let written = wrapper.stream.handler.paste(terminal_paste_pkg::Request {
        source: match req.source {
            Source::Clipboard => terminal_paste_pkg::Source::Clipboard(req.location),
            Source::Text => terminal_paste_pkg::Source::Text,
        },
        contents: terminal_paste_pkg::Contents::Reader(terminal_paste_pkg::Reader {
            mimes: &mimes,
            read: clipboard::MimeReader {
                ctx: req as *const Request as *mut c_void,
                read_fn: read_trampoline,
            },
        }),
        allow_unsafe: req.allow_unsafe,
    });
    let written = match written {
        Ok(w) => w,
        Err(err) => {
            return match err {
                PasteError::UnsafePaste => Result::Rejected,
                PasteError::NoWritePty => Result::InvalidValue,
                PasteError::OutOfMemory => Result::OutOfMemory,
                PasteError::ReadFailed | PasteError::EntropyUnavailable => Result::IoError,
            }
        }
    };
    if let Some(ptr) = out_written.as_mut() {
        *ptr = written;
    }
    Result::Success
}

/// Test failure points (Rust-only, see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailPoint {
    /// The sink the text is read into refuses a write (Zig: its buffer
    /// fails to grow).
    SinkWrite,
}

/// Tripwire for [`FailPoint`].
pub static PASTE_TW: crate::tripwire::Module<FailPoint, ()> = crate::tripwire::Module::new();

/// The sink handed to the C read callback: a GhosttyWriter over the
/// Rust sink, remembering whether the sink itself failed so that can be
/// told apart from the callback failing to read.
struct Sink<'a> {
    writer: &'a mut dyn std::io::Write,
    write_failed: bool,
}

impl Sink<'_> {
    unsafe extern "C" fn write(userdata: *mut c_void, data: *const u8, len: usize) -> bool {
        let this = &mut *(userdata as *mut Sink<'_>);
        if PASTE_TW.check(FailPoint::SinkWrite).is_err() || this.writer.write_all(slice_or_empty(data, len)).is_err() {
            this.write_failed = true;
            return false;
        }
        true
    }
}

fn read_trampoline(
    ctx: *mut c_void,
    mime: &[u8],
    writer: &mut dyn std::io::Write,
) -> std::result::Result<(), clipboard::MimeReaderError> {
    // SAFETY: ctx is the request passed to `terminal_paste`, live for the
    // paste.
    let req = unsafe { &*(ctx as *const Request) };
    let mut sink = Sink {
        writer,
        write_failed: false,
    };
    let read = req.reader.read.expect("validated by terminal_paste");
    // SAFETY: C callback contract.
    let ok = unsafe {
        read(
            req.reader.userdata,
            LibString::init(mime),
            io::Writer {
                write: Some(Sink::write),
                userdata: &mut sink as *mut Sink<'_> as *mut c_void,
            },
        )
    };
    // A reader that ignores a refused write would otherwise paste what
    // was written with the refused data missing.
    if sink.write_failed {
        return Err(clipboard::MimeReaderError::WriteFailed);
    }
    if !ok {
        return Err(clipboard::MimeReaderError::ReadFailed);
    }
    Ok(())
}

/// C: ghostty_paste_is_safe
///
/// # Safety
/// A non-null `data` must be valid for `len` reads.
#[export_name = "ghostty_paste_is_safe"]
pub unsafe extern "C" fn is_safe(data: *const u8, len: usize) -> bool {
    paste::is_safe(slice_or_empty(data, len))
}

/// C: ghostty_paste_encode
///
/// # Safety
/// A non-null `data` must be valid for `data_len` reads and writes (unsafe
/// bytes are replaced in place); a non-null `out_` must be valid for
/// `out_len` writable bytes.
#[export_name = "ghostty_paste_encode"]
pub unsafe extern "C" fn encode(
    data: *mut u8,
    data_len: usize,
    bracketed: bool,
    out_: *mut u8,
    out_len: usize,
    out_written: &mut usize,
) -> Result {
    let slice: &mut [u8] = if data.is_null() || data_len == 0 {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(data, data_len)
    };
    let result = paste::encode(slice, paste::Options { bracketed });

    let total = result[0].len() + result[1].len() + result[2].len();
    *out_written = total;

    let out: &mut [u8] = if out_.is_null() {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(out_, out_len)
    };
    if out.len() < total {
        return Result::OutOfSpace;
    }

    let mut offset: usize = 0;
    for segment in result {
        out[offset..offset + segment.len()].copy_from_slice(segment);
        offset += segment.len();
    }

    Result::Success
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_int;

    fn run_encode(input: &[u8], bracketed: bool, buf: Option<&mut [u8]>) -> (Result, usize) {
        let mut data = input.to_vec();
        let mut written: usize = 0;
        let result = unsafe {
            match buf {
                Some(buf) => encode(
                    data.as_mut_ptr(),
                    data.len(),
                    bracketed,
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut written,
                ),
                None => encode(data.as_mut_ptr(), data.len(), bracketed, std::ptr::null_mut(), 0, &mut written),
            }
        };
        (result, written)
    }

    #[test]
    fn encode_bracketed() {
        let mut buf = [0u8; 64];
        let (result, written) = run_encode(b"hello", true, Some(&mut buf));
        assert_eq!(Result::Success, result);
        assert_eq!(b"\x1b[200~hello\x1b[201~", &buf[..written]);
    }

    #[test]
    fn encode_unbracketed_no_newlines() {
        let mut buf = [0u8; 64];
        let (result, written) = run_encode(b"hello", false, Some(&mut buf));
        assert_eq!(Result::Success, result);
        assert_eq!(b"hello", &buf[..written]);
    }

    #[test]
    fn encode_unbracketed_newlines() {
        let mut buf = [0u8; 64];
        let (result, written) = run_encode(b"hello\nworld", false, Some(&mut buf));
        assert_eq!(Result::Success, result);
        assert_eq!(b"hello\rworld", &buf[..written]);
    }

    #[test]
    fn encode_strip_unsafe_bytes() {
        let mut buf = [0u8; 64];
        let (result, written) = run_encode(b"hel\x1blo\x00world", true, Some(&mut buf));
        assert_eq!(Result::Success, result);
        assert_eq!(b"\x1b[200~hel lo world\x1b[201~", &buf[..written]);
    }

    #[test]
    fn encode_with_insufficient_buffer() {
        let mut buf = [0u8; 1];
        let (result, written) = run_encode(b"hello", true, Some(&mut buf));
        assert_eq!(Result::OutOfSpace, result);
        assert_eq!(17, written);
    }

    #[test]
    fn encode_with_null_buffer() {
        let (result, written) = run_encode(b"hello", true, None);
        assert_eq!(Result::OutOfSpace, result);
        assert_eq!(17, written);
    }

    #[test]
    fn is_safe_with_safe_data() {
        let safe = b"hello world";
        assert!(unsafe { is_safe(safe.as_ptr(), safe.len()) });
    }

    #[test]
    fn is_safe_with_newline() {
        let unsafe_ = b"hello\nworld";
        assert!(!unsafe { is_safe(unsafe_.as_ptr(), unsafe_.len()) });
    }

    #[test]
    fn is_safe_with_carriage_return() {
        let unsafe_ = b"hello\rworld";
        assert!(!unsafe { is_safe(unsafe_.as_ptr(), unsafe_.len()) });
    }

    #[test]
    fn is_safe_with_bracketed_paste_end() {
        let unsafe_ = b"hello\x1b[201~world";
        assert!(!unsafe { is_safe(unsafe_.as_ptr(), unsafe_.len()) });
    }

    #[test]
    fn is_safe_with_empty_data() {
        let empty = b"";
        assert!(unsafe { is_safe(empty.as_ptr(), 0) });
    }

    #[test]
    fn is_safe_with_null_empty_data() {
        assert!(unsafe { is_safe(std::ptr::null(), 0) });
    }

    // Capture state for the terminal_paste tests: every pty write and the
    // clipboard reads that follow a paste event. Tests run on their own
    // threads, so thread-locals are per test.
    mod capture {
        use super::super::super::terminal::{
            ClipboardContent, ClipboardRead, ClipboardReadReply, Terminal,
        };
        use crate::lib::String as LibString;
        use crate::terminal::clipboard;
        use std::cell::{Cell, RefCell};
        use std::ffi::c_void;

        thread_local! {
            pub static WRITTEN: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
            pub static WRITE_COUNT: Cell<usize> = const { Cell::new(0) };
            pub static READ_COUNT: Cell<usize> = const { Cell::new(0) };
            pub static LAST_READ_GRANTED: Cell<bool> = const { Cell::new(false) };
        }

        pub fn reset() {
            WRITTEN.with(|w| w.borrow_mut().clear());
            WRITE_COUNT.set(0);
            READ_COUNT.set(0);
            LAST_READ_GRANTED.set(false);
        }

        pub fn written() -> Vec<u8> {
            WRITTEN.with(|w| w.borrow().clone())
        }

        pub unsafe extern "C" fn write_pty(_: Terminal, _: *mut c_void, ptr: *const u8, len: usize) {
            WRITTEN.with(|w| w.borrow_mut().extend_from_slice(std::slice::from_raw_parts(ptr, len)));
            WRITE_COUNT.set(WRITE_COUNT.get() + 1);
        }

        pub unsafe extern "C" fn clipboard_read(_: Terminal, _: *mut c_void, request: *const ClipboardRead) {
            READ_COUNT.set(READ_COUNT.get() + 1);
            LAST_READ_GRANTED.set((*request).granted);
            let contents = [ClipboardContent {
                mime: LibString::init(b"text/plain"),
                data: LibString::init(b"Ghostty"),
            }];
            ((*request).reply)(
                request,
                &ClipboardReadReply {
                    size: std::mem::size_of::<ClipboardReadReply>(),
                    result: clipboard::ReadStatus::SUCCESS,
                    contents: contents.as_ptr(),
                    contents_len: contents.len(),
                    available: std::ptr::null(),
                    available_len: 0,
                    remember: false,
                },
            );
        }
    }

    /// The representations a test paste serves: the MIME list for the
    /// request and the data the read callback streams, in pieces.
    struct Contents {
        mimes: [LibString; 2],
        data: [&'static [u8]; 2],
        len: usize,
        reads: [usize; 2],
        fail: bool,
        /// Keep writing and report success after a refused write, which
        /// the contract forbids.
        ignore_refused: bool,
    }

    impl Contents {
        fn init(entries: &[(&'static [u8], &'static [u8])]) -> Contents {
            let mut this = Contents {
                mimes: [LibString::init(b""); 2],
                data: [b""; 2],
                len: 0,
                reads: [0; 2],
                fail: false,
                ignore_refused: false,
            };
            for (mime, data) in entries {
                this.mimes[this.len] = LibString::init(mime);
                this.data[this.len] = data;
                this.len += 1;
            }
            this
        }

        fn request(&mut self) -> Request {
            Request::new(
                clipboard::Location::STANDARD,
                Source::Clipboard,
                self.mimes.as_ptr(),
                self.len,
                io::MimeReader {
                    read: Some(Self::read),
                    userdata: self as *mut Contents as *mut c_void,
                },
                false,
            )
        }

        unsafe extern "C" fn read(userdata: *mut c_void, mime: LibString, writer: io::Writer) -> bool {
            let this = &mut *(userdata as *mut Contents);
            // The mime is the exact string from the request's list, so
            // identifying it by pointer works.
            let Some(index) = this.mimes[..this.len]
                .iter()
                .position(|m| m.ptr == mime.ptr && m.len == mime.len)
            else {
                return false;
            };
            this.reads[index] += 1;
            if this.fail {
                return false;
            }
            let data = this.data[index];
            let mut offset = 0;
            while offset < data.len() {
                let n = 3.min(data.len() - offset);
                let accepted = (writer.write.unwrap())(writer.userdata, data[offset..].as_ptr(), n);
                if !accepted && !this.ignore_refused {
                    return false;
                }
                offset += n;
            }
            true
        }
    }

    fn paste_terminal(with_write_pty: bool) -> super::super::terminal::Terminal {
        use super::super::terminal as terminal_c;
        let mut t: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(
            Result::Success,
            terminal_c::new(Some(&crate::lib::allocator::C_ALLOCATOR), &mut t, 80, 24)
        );
        if with_write_pty {
            assert_eq!(Result::Success, unsafe {
                terminal_c::set(
                    t,
                    terminal_c::Option::WritePty as c_int,
                    capture::write_pty as terminal_c::WritePtyFn as *const c_void,
                )
            });
        }
        t
    }

    #[test]
    fn terminal_paste_null_handling() {
        let t = paste_terminal(true);
        unsafe {
            let mut written = true;
            let mut contents = Contents::init(&[(b"text/plain", b"hello")]);
            let req = contents.request();
            assert_eq!(Result::InvalidValue, terminal_paste(std::ptr::null_mut(), &req, &mut written));
            assert_eq!(Result::InvalidValue, terminal_paste(t, std::ptr::null(), &mut written));
            assert!(written);

            // A size smaller than the struct is rejected.
            let mut small = req;
            small.size = std::mem::size_of::<usize>();
            assert_eq!(Result::InvalidValue, terminal_paste(t, &small, &mut written));

            // MIME types without a reader are rejected; none at all is fine.
            let mut unreadable = req;
            unreadable.reader.read = None;
            assert_eq!(Result::InvalidValue, terminal_paste(t, &unreadable, &mut written));
            unreadable.mimes = std::ptr::null();
            unreadable.mimes_len = 0;
            assert_eq!(Result::Success, terminal_paste(t, &unreadable, &mut written));
            assert!(!written);
            super::super::terminal::free(t);
        }
    }

    #[test]
    fn terminal_paste_without_write_pty_is_invalid() {
        capture::reset();
        let t = paste_terminal(false);
        unsafe {
            let mut contents = Contents::init(&[(b"text/plain", b"hello")]);
            let req = contents.request();
            assert_eq!(Result::InvalidValue, terminal_paste(t, &req, std::ptr::null_mut()));
            super::super::terminal::free(t);
        }
    }

    #[test]
    fn terminal_paste_text_and_unsafe() {
        use super::super::terminal as terminal_c;
        capture::reset();
        let t = paste_terminal(true);
        unsafe {
            // Plain text, NULL out_written pointer is fine. The text is read
            // once, the image never.
            let mut contents = Contents::init(&[(b"image/png", b"\x89PNG"), (b"text/plain", b"hel\x1blo")]);
            let req = contents.request();
            assert_eq!(Result::Success, terminal_paste(t, &req, std::ptr::null_mut()));
            assert_eq!(b"hel lo".to_vec(), capture::written());
            assert_eq!(1, capture::WRITE_COUNT.get());
            assert_eq!(0, contents.reads[0]);
            assert_eq!(1, contents.reads[1]);

            // Unsafe is refused with nothing written, then allowed.
            capture::reset();
            let mut written = false;
            let mut unsafe_contents = Contents::init(&[(b"text/plain", b"rm -rf /\n")]);
            let mut unsafe_req = unsafe_contents.request();
            assert_eq!(Result::Rejected, terminal_paste(t, &unsafe_req, &mut written));
            assert_eq!(0, capture::WRITE_COUNT.get());
            assert!(!written);
            assert_eq!(1, unsafe_contents.reads[0]);

            unsafe_req.allow_unsafe = true;
            assert_eq!(Result::Success, terminal_paste(t, &unsafe_req, &mut written));
            assert!(written);
            assert_eq!(b"rm -rf /\r".to_vec(), capture::written());
            assert_eq!(2, unsafe_contents.reads[0]);

            // A carriage return is the Enter key on the input stream, so it is
            // refused exactly like a newline when unbracketed.
            capture::reset();
            written = false;
            let mut cr_contents = Contents::init(&[(b"text/plain", b"rm -rf /\r")]);
            let mut cr = cr_contents.request();
            assert_eq!(Result::Rejected, terminal_paste(t, &cr, &mut written));
            assert_eq!(0, capture::WRITE_COUNT.get());
            assert!(!written);

            cr.allow_unsafe = true;
            assert_eq!(Result::Success, terminal_paste(t, &cr, &mut written));
            assert!(written);
            assert_eq!(b"rm -rf /\r".to_vec(), capture::written());

            // Bracketed paste mode frames the text through the real mode path.
            capture::reset();
            let decset = b"\x1b[?2004h";
            terminal_c::vt_write(t, decset.as_ptr(), decset.len());
            assert_eq!(Result::Success, terminal_paste(t, &req, &mut written));
            assert!(written);
            assert_eq!(b"\x1b[200~hel lo\x1b[201~".to_vec(), capture::written());

            // No text representation writes nothing and reads nothing.
            capture::reset();
            let mut image = Contents::init(&[(b"image/png", b"\x89PNG")]);
            let image_req = image.request();
            assert_eq!(Result::Success, terminal_paste(t, &image_req, &mut written));
            assert!(!written);
            assert_eq!(0, capture::WRITE_COUNT.get());
            assert_eq!(0, image.reads[0]);

            // A failing reader is an I/O error.
            capture::reset();
            (*(req.reader.userdata as *mut Contents)).fail = true;
            assert_eq!(Result::IoError, terminal_paste(t, &req, &mut written));
            assert_eq!(0, capture::WRITE_COUNT.get());
            terminal_c::free(t);
        }
    }

    #[test]
    fn terminal_paste_refused_write_ignored_by_the_reader() {
        capture::reset();

        // A write is only refused when the buffer the text is read into
        // fails to grow. (Rust port: the PASTE_TW tripwire stands in for
        // Zig's failing allocator.)
        let t = paste_terminal(true);
        unsafe {
            let mut contents = Contents::init(&[(b"text/plain", b"hello")]);
            contents.ignore_refused = true;
            let req = contents.request();

            // Every write from here on is refused, the first one included.
            // The reader ignores that and reports success.
            PASTE_TW.error_always(FailPoint::SinkWrite, ());
            assert_eq!(Result::OutOfMemory, terminal_paste(t, &req, std::ptr::null_mut()));
            PASTE_TW.end(crate::tripwire::ResetMode::Reset).unwrap();
            assert_eq!(1, contents.reads[0]);
            assert_eq!(0, capture::WRITE_COUNT.get());
            super::super::terminal::free(t);
        }
    }

    #[test]
    fn terminal_paste_event() {
        use super::super::terminal as terminal_c;
        use crate::terminal::modes::Mode;

        fn find(hay: &[u8], needle: &[u8]) -> std::option::Option<usize> {
            hay.windows(needle.len()).position(|w| w == needle)
        }

        capture::reset();
        let t = paste_terminal(true);
        unsafe {
            (*t).terminal_mut().modes.set(Mode::KittyPasteEvents, true);

            // Without a clipboard_read callback the paste stays text.
            let mut written = false;
            let mut contents = Contents::init(&[(b"text/plain", b"secret"), (b"image/png", b"\x89PNG")]);
            let mut req = contents.request();
            req.location = clipboard::Location::PRIMARY;
            assert_eq!(Result::Success, terminal_paste(t, &req, &mut written));
            assert!(written);
            assert_eq!(b"secret".to_vec(), capture::written());

            // With one, an event is sent listing every MIME type and no data
            // is read, let alone written.
            capture::reset();
            contents.reads = [0; 2];
            assert_eq!(
                Result::Success,
                terminal_c::set(
                    t,
                    terminal_c::Option::ClipboardRead as c_int,
                    capture::clipboard_read as terminal_c::ClipboardReadFn as *const c_void,
                )
            );
            assert_eq!(Result::Success, terminal_paste(t, &req, &mut written));
            assert!(written);
            assert_eq!(1, capture::WRITE_COUNT.get());
            let out = capture::written();
            assert_eq!(3, out.windows(7).filter(|w| *w == b"\x1b]5522;").count());
            assert!(out.starts_with(b"\x1b]5522;type=read:status=OK:loc=primary:pw="));
            assert!(find(&out, b"secret").is_none());
            assert!(find(&out, b";dGV4dC9wbGFpbiBpbWFnZS9wbmcK\x1b\\").is_some());
            assert_eq!(0, contents.reads[0]);
            assert_eq!(0, contents.reads[1]);

            // The program's read with the event password is granted once.
            let ok_prefix = b"\x1b]5522;type=read:status=OK:loc=primary:pw=";
            let pw_end = ok_prefix.len() + find(&out[ok_prefix.len()..], b"\x1b\\").unwrap();
            let pw = std::str::from_utf8(&out[ok_prefix.len()..pw_end]).unwrap();
            let read = format!("\x1b]5522;type=read:pw={pw}:name=UGFzdGUgZXZlbnQ=;dGV4dC9wbGFpbg==\x1b\\");
            capture::reset();
            terminal_c::vt_write(t, read.as_ptr(), read.len());
            assert_eq!(1, capture::READ_COUNT.get());
            assert!(capture::LAST_READ_GRANTED.get());
            assert!(find(&capture::written(), b";R2hvc3R0eQ==\x1b\\").is_some());

            capture::reset();
            terminal_c::vt_write(t, read.as_ptr(), read.len());
            assert_eq!(1, capture::READ_COUNT.get());
            assert!(!capture::LAST_READ_GRANTED.get());

            // Text sources never become events.
            capture::reset();
            let mut ime = req;
            ime.source = Source::Text;
            assert_eq!(Result::Success, terminal_paste(t, &ime, &mut written));
            assert!(written);
            assert_eq!(b"secret".to_vec(), capture::written());
            terminal_c::free(t);
        }
    }

    #[test]
    fn request_layout() {
        let req = Request::new(
            clipboard::Location::STANDARD,
            Source::Clipboard,
            std::ptr::null(),
            0,
            io::MimeReader::default(),
            false,
        );
        assert_eq!(std::mem::size_of::<Request>(), req.size);
        assert_eq!(0, Source::Clipboard as c_int);
        assert_eq!(1, Source::Text as c_int);
    }
}
