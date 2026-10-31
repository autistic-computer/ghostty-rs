//! Port of `src/terminal/clipboard.zig`.
//!
//! STATUS: ported
//!
//! The callback structs keep Zig's C-ABI-shaped design (an opaque context
//! pointer plus a function pointer) because they cross the libghostty-vt C
//! API; borrowed slices carry a lifetime instead of "valid for the duration
//! of the callback" comments alone.

use std::ffi::c_void;
use std::fmt;

/// The clipboard destination for a write.
///
/// Non-exhaustive (`enum(c_int)` with `_`): any c_int is representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Location(pub std::ffi::c_int);

impl Location {
    pub const STANDARD: Location = Location(0);
    pub const SELECTION: Location = Location(1);
    pub const PRIMARY: Location = Location(2);
}

/// MIME types that name plain text across the platforms terminals run
/// on. We accept the union everywhere since serving text under any of these
/// names is harmless.
pub fn is_text_mime(mime: &[u8]) -> bool {
    const NAMES: &[&[u8]] = &[
        b"text/plain",
        b"text/plain;charset=utf-8",
        b"UTF8_STRING",
        b"TEXT",
        b"STRING",
    ];
    NAMES.iter().any(|&n| n == mime)
}

/// A single representation of clipboard data.
///
/// The MIME type and data are borrowed and only valid for the duration of a
/// clipboard write callback. Data is binary-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Content<'a> {
    pub mime: &'a [u8],
    pub data: &'a [u8],
}

/// Errors for [`MimeReader`] (`MimeReader.Error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MimeReaderError {
    /// The data could not be read.
    ReadFailed,
    /// Writing to the sink failed (`std.Io.Writer.Error`).
    WriteFailed,
}

impl fmt::Display for MimeReaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ReadFailed => "ReadFailed",
            Self::WriteFailed => "WriteFailed",
        })
    }
}

impl std::error::Error for MimeReaderError {}

impl From<std::io::Error> for MimeReaderError {
    fn from(_: std::io::Error) -> Self {
        MimeReaderError::WriteFailed
    }
}

/// The `read_fn` callback type of a [`MimeReader`].
pub type MimeReadFn = fn(
    ctx: *mut c_void,
    mime: &[u8],
    sink: &mut dyn std::io::Write,
) -> Result<(), MimeReaderError>;

/// Requests content of a specific mime-type. For now this is used for
/// on-demand clipboard access since content can be large (particularly
/// non-text content), but it is generic so that this could handle other
/// mime-typed sources in the future like maybe drag-and-drop.
///
/// C: GhosttyMimeReader
#[derive(Debug, Clone, Copy)]
pub struct MimeReader {
    /// Passed through to `read_fn`.
    pub ctx: *mut c_void,

    /// Write all the data of the representation named by `mime` to
    /// `sink`, in as many writes as is convenient. The mime and sink
    /// are borrowed, only valid for the duration of the call, and
    /// nothing written to the sink is retained, so the data may be
    /// borrowed from anywhere. Return error.ReadFailed if the data
    /// can't be produced and propagate error.WriteFailed from the
    /// sink.
    pub read_fn: MimeReadFn,
}

impl MimeReader {
    pub fn read(
        &self,
        mime: &[u8],
        sink: &mut dyn std::io::Write,
    ) -> Result<(), MimeReaderError> {
        (self.read_fn)(self.ctx, mime, sink)
    }
}

/// A request from the running program to write a clipboard.
///
/// Writes are synchronous: the effect callback must answer through `reply`
/// before it returns, and the request (including its reply context) is
/// invalid afterwards. An embedder that needs user consent must block until
/// it has an answer; the VT stream waits with it. Protocols without a write
/// acknowledgement (OSC 52, OSC 1337 Copy) discard the reply.
///
/// Contents are borrowed and only valid for the duration of the callback.
/// An empty contents slice clears the destination.
#[derive(Debug, Clone, Copy)]
pub struct Write<'a> {
    pub location: Location,
    pub contents: &'a [Content<'a>],

    /// Name of the writing program for permission prompts, if the
    /// protocol carries one. Empty otherwise.
    pub name: &'a [u8],

    /// True if the terminal already holds a session grant for this
    /// request (kitty clipboard protocol passwords). The embedder should
    /// skip any permission prompt and perform the write.
    pub granted: bool,

    /// True if the program supplied a session password, so the embedder
    /// may offer to remember the user's decision via
    /// Result.Success.remember. When false, remember is ignored.
    pub can_remember: bool,

    /// Terminal-owned reply state, only valid during the callback.
    pub reply_ctx: *mut c_void,
    pub reply_fn: fn(*mut c_void, WriteResult),
}

impl Write<'_> {
    /// Answer the write. May be called at most once; later calls are
    /// ignored.
    pub fn reply(&self, result: WriteResult) {
        (self.reply_fn)(self.reply_ctx, result)
    }
}

/// The status of a clipboard write reply (`Write.Status`).
///
/// Non-exhaustive (`enum(c_int)` with `_`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct WriteStatus(pub std::ffi::c_int);

impl WriteStatus {
    pub const SUCCESS: WriteStatus = WriteStatus(0);
    pub const DENIED: WriteStatus = WriteStatus(1);
    pub const UNSUPPORTED: WriteStatus = WriteStatus(2);
    pub const BUSY: WriteStatus = WriteStatus(3);
    pub const INVALID_DATA: WriteStatus = WriteStatus(4);
    pub const IO_ERROR: WriteStatus = WriteStatus(5);
}

/// The reply to a clipboard write (`Write.Result`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteResult {
    /// The write was denied by policy or the user.
    Denied,

    /// The embedder cannot write this clipboard.
    Unsupported,

    /// The clipboard is temporarily unavailable.
    Busy,

    /// One or more representations contain invalid data.
    InvalidData,

    /// Writing the clipboard failed.
    IoError,

    /// The write succeeded.
    Success(WriteSuccess),
}

/// `Write.Result.Success`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WriteSuccess {
    /// Record a session grant so future requests from the same
    /// program skip the permission prompt. Only honored when the
    /// request set `can_remember`.
    pub remember: bool,
}

/// A request from the running program to read a clipboard.
///
/// Reads are synchronous: the effect callback must answer through `reply`
/// before it returns, and the request (including its reply context) is
/// invalid afterwards. An embedder that needs user consent must block until
/// it has an answer; the VT stream waits with it.
#[derive(Debug, Clone, Copy)]
pub struct Read<'a> {
    pub location: Location,

    /// The MIME types the program wants, in order of preference. The
    /// reply should carry every requested representation the clipboard
    /// has; unrequested ones are ignored. Protocols that only carry text
    /// (OSC 52) request "text/plain".
    pub mimes: &'a [&'a [u8]],

    /// The program also wants the list of MIME types available on the
    /// clipboard, delivered as Result.Success.available.
    pub list: bool,

    /// Name of the requesting program for permission prompts, if the
    /// protocol carries one. Empty otherwise.
    pub name: &'a [u8],

    /// True if the terminal already holds a session grant for this
    /// request (kitty clipboard protocol passwords). The embedder should
    /// skip any permission prompt and serve the read.
    ///
    /// Always false when mimes is empty: such a request is served
    /// without a prompt (kitty's targets-listing exemption), so the
    /// terminal never consults grants for it and a one-time password
    /// is preserved for the follow-up data read.
    pub granted: bool,

    /// True if the program supplied a session password, so the embedder
    /// may offer to remember the user's decision via
    /// Result.Success.remember. When false, remember is ignored.
    pub can_remember: bool,

    /// Terminal-owned reply state, only valid during the callback.
    ///
    /// The result is delivered through a call rather than returned so
    /// the terminal consumes it while the embedder's memory is still
    /// alive; a returned slice would have to outlive the callback.
    pub reply_ctx: *mut c_void,
    pub reply_fn: for<'r> fn(*mut c_void, ReadResult<'r>),
}

impl Read<'_> {
    /// Answer the read. May be called at most once; later calls are
    /// ignored. Result memory is borrowed only for the duration of this
    /// call.
    pub fn reply(&self, result: ReadResult<'_>) {
        (self.reply_fn)(self.reply_ctx, result)
    }
}

/// The status of a clipboard read reply (`Read.Status`).
///
/// Non-exhaustive (`enum(c_int)` with `_`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ReadStatus(pub std::ffi::c_int);

impl ReadStatus {
    pub const SUCCESS: ReadStatus = ReadStatus(0);
    pub const DENIED: ReadStatus = ReadStatus(1);
    pub const UNSUPPORTED: ReadStatus = ReadStatus(2);
    pub const BUSY: ReadStatus = ReadStatus(3);
    pub const IO_ERROR: ReadStatus = ReadStatus(4);
}

/// The reply to a clipboard read (`Read.Result`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadResult<'a> {
    /// The read was denied by policy or the user.
    Denied,

    /// The embedder cannot read this clipboard.
    Unsupported,

    /// The clipboard is temporarily unavailable.
    Busy,

    /// Reading the clipboard failed.
    IoError,

    /// The read succeeded. All memory is borrowed for the duration
    /// of the reply call.
    Success(ReadSuccess<'a>),
}

/// `Read.Result.Success`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadSuccess<'a> {
    /// Representations of the clipboard contents, one per
    /// requested MIME type the clipboard has. Protocols that
    /// carry a single text value use the first entry with a
    /// text MIME type (see is_text_mime).
    pub contents: &'a [Content<'a>],

    /// All MIME types available on the clipboard. Only used when
    /// the request set `list`.
    pub available: &'a [&'a [u8]],

    /// Record a session grant so future requests from the same
    /// program skip the permission prompt. Only honored when the
    /// request set `can_remember`.
    pub remember: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_text_mime_test() {
        assert!(is_text_mime(b"text/plain"));
        assert!(is_text_mime(b"UTF8_STRING"));
        assert!(!is_text_mime(b"image/png"));
        assert!(!is_text_mime(b"."));
    }

    #[test]
    fn mime_reader_and_reply_callbacks() {
        fn read_fn(
            ctx: *mut c_void,
            mime: &[u8],
            sink: &mut dyn std::io::Write,
        ) -> Result<(), MimeReaderError> {
            // SAFETY: the test passes a pointer to a live u32.
            let calls = unsafe { &mut *(ctx as *mut u32) };
            *calls += 1;
            if mime != b"text/plain" {
                return Err(MimeReaderError::ReadFailed);
            }
            sink.write_all(b"hello")?;
            Ok(())
        }

        let mut calls: u32 = 0;
        let reader = MimeReader {
            ctx: &mut calls as *mut u32 as *mut c_void,
            read_fn,
        };
        let mut out = Vec::new();
        reader.read(b"text/plain", &mut out).unwrap();
        assert_eq!(out, b"hello");
        assert_eq!(
            reader.read(b"image/png", &mut out),
            Err(MimeReaderError::ReadFailed)
        );
        assert_eq!(calls, 2);

        fn reply_fn(ctx: *mut c_void, result: ReadResult<'_>) {
            // SAFETY: the test passes a pointer to a live usize.
            let out = unsafe { &mut *(ctx as *mut usize) };
            if let ReadResult::Success(s) = result {
                *out = s.contents.len();
            }
        }
        let mut n: usize = 0;
        let req = Read {
            location: Location::STANDARD,
            mimes: &[b"text/plain"],
            list: false,
            name: b"",
            granted: false,
            can_remember: false,
            reply_ctx: &mut n as *mut usize as *mut c_void,
            reply_fn,
        };
        let contents = [Content {
            mime: b"text/plain",
            data: b"x",
        }];
        req.reply(ReadResult::Success(ReadSuccess {
            contents: &contents,
            ..Default::default()
        }));
        assert_eq!(n, 1);
    }
}
