//! Port of `src/terminal/paste.zig`.
//!
//! STATUS: ported
//!
//! Pasting into a terminal.
//!
//! This is the single place that turns "the user pasted" into bytes for
//! the pty, applying the terminal's current state:
//!
//!   * Mode 5522 (Kitty clipboard protocol paste events) set, a
//!     user-initiated clipboard paste, and the embedder able to serve
//!     the program's follow-up clipboard read: send a paste event
//!     listing the clipboard's MIME types with a fresh one-time password
//!     and record a one-time read grant for it. No data is read.
//!   * Otherwise: write the first text representation, with unsafe bytes
//!     replaced (xterm behavior), framed per mode 2004 (bracketed paste)
//!     or with newlines converted to carriage returns if not.
//!
//! The precedence (5522 event, else 2004 framing, else plain) and the
//! safety rule live only here so every embedder of the terminal shares
//! one implementation.
//!
//! Port notes / deviations:
//! - Allocators are dropped. `std.Io` is represented by its entropy
//!   source (`sys::IoRandomSecureFn`), as in `kitty/clipboard_grants.rs`.
//! - The writer is any `std::io::Write`; a write failure is
//!   [`Error::WriteFailed`].
//! - `Error` keeps Zig's members that can occur in Rust: allocation
//!   failure is not recoverable in Rust, but `OutOfMemory` is kept for
//!   a reader whose sink reports a write failure (Zig maps the
//!   allocating writer's only failure to `OutOfMemory`).

use std::fmt;
use std::io::Write;

use crate::input::paste as input_paste;
use crate::terminal::clipboard;
use crate::terminal::kitty::clipboard as kitty_clipboard;
use crate::terminal::kitty::clipboard_grants::Direction;
use crate::terminal::modes::Mode;
use crate::terminal::sys;
use crate::terminal::Terminal;

/// Why a paste happened. Only clipboard pastes may become paste
/// events, which is why only they carry a location: it's meaningless
/// for text insertion.
///
/// C: GhosttyPasteSource, flattened next to the location since C has
/// no tagged unions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The user pasted from a clipboard: keybind, menu, middle click.
    /// The payload is the clipboard the contents came from.
    Clipboard(clipboard::Location),

    /// Text inserted some other way: IME commit, drag and drop,
    /// scripted input.
    Text,
}

impl Default for Source {
    fn default() -> Self {
        Source::Clipboard(clipboard::Location::STANDARD)
    }
}

/// A paste of clipboard contents into the terminal. What actually gets
/// written depends on terminal state; see `paste`.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// Why this paste happened, and for a clipboard paste, from which
    /// clipboard. Only a user-initiated clipboard paste may become a
    /// paste event; text insertion always writes text.
    pub source: Source,

    /// The representations available. Borrowed only during the
    /// duration of the paste function call.
    pub contents: Contents<'a>,

    /// Write data that could inject commands (see `paste`). The usual
    /// flow is to call with false, confirm with the user on
    /// error.UnsafePaste, and call again with true.
    pub allow_unsafe: bool,
}

impl<'a> Request<'a> {
    /// A request with every field but `contents` at its default.
    pub fn new(contents: Contents<'a>) -> Request<'a> {
        Request {
            source: Source::default(),
            contents,
            allow_unsafe: false,
        }
    }
}

/// The representations available for a paste, in the embedder's
/// preferred order.
#[derive(Debug, Clone, Copy)]
pub enum Contents<'a> {
    /// Every representation already in memory. For text the embedder
    /// holds anyway (an IME commit, dropped text) or small clipboards.
    Memory(&'a [clipboard::Content<'a>]),

    /// Representations read on demand, so nothing is loaded that isn't
    /// pasted. This is the form for a real clipboard, whose non-text
    /// items may be huge.
    Reader(Reader<'a>),
}

/// The on-demand form: the MIME types available plus the reader
/// that produces the data of any one of them. (`Contents.Reader`)
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    /// The MIME types available, in preferred order.
    pub mimes: &'a [&'a [u8]],

    /// Produces the data of any entry of `mimes`, which is passed
    /// through to it as is. A paste reads at most once: the text
    /// representation being pasted, never anything else and never
    /// anything for a paste event. There is no requirement across
    /// paste calls, so a source that changes between an unsafe
    /// refusal and the embedder's confirmed retry simply pastes
    /// its current contents.
    pub read: clipboard::MimeReader,
}

impl<'a> Contents<'a> {
    /// The number of representations.
    pub fn len(&self) -> usize {
        match self {
            Contents::Memory(v) => v.len(),
            Contents::Reader(v) => v.mimes.len(),
        }
    }

    /// True if there are no representations.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The MIME type of representation `index`.
    pub fn mime(&self, index: usize) -> &'a [u8] {
        match *self {
            Contents::Memory(v) => v[index].mime,
            Contents::Reader(v) => v.mimes[index],
        }
    }
}

/// What a caller supplies to `paste`: the terminal state the decision
/// depends on, the session state an event records into, and the sink.
pub struct Context<'a> {
    /// The terminal whose modes decide the encoding.
    pub terminal: &'a Terminal,

    /// Receives the encoded text in chunks, or the event. Is not
    /// flushed by `paste`; the caller flushes once it returns.
    pub writer: &'a mut dyn Write,

    /// Kitty clipboard protocol session state, or null if the embedder
    /// does not serve Kitty clipboard reads (`clipboard.Read`). A
    /// paste event is that protocol: it is only useful if the
    /// program's follow-up read can be answered, since otherwise the
    /// read would be refused and the user's paste would vanish. With
    /// null, `paste` always writes text.
    pub kitty_clipboard: Option<KittyClipboard<'a>>,
}

/// (`Context.KittyClipboard`)
pub struct KittyClipboard<'a> {
    /// Session grants. A paste event records its one-time password
    /// here so the program's follow-up read is served without a
    /// prompt.
    pub grants: &'a mut kitty_clipboard::Grants,

    /// Secure entropy for one-time passwords. See generateOtp for
    /// why there is no fallback when this has none.
    pub io: sys::IoRandomSecureFn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Allocation failure (a reader whose sink failed to grow).
    OutOfMemory,

    /// No secure entropy for a paste event's one-time password.
    EntropyUnavailable,

    /// The contents reader failed.
    ReadFailed,

    /// The writer failed.
    WriteFailed,

    /// The data could inject commands and allow_unsafe was false.
    UnsafePaste,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

impl From<sys::RandomSecureError> for Error {
    fn from(err: sys::RandomSecureError) -> Self {
        match err {
            sys::RandomSecureError::EntropyUnavailable => Error::EntropyUnavailable,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Error::WriteFailed
    }
}

/// Paste into the terminal, applying the terminal's current state as
/// described in the module docs. Returns true if anything was written
/// to `ctx.writer`: the encoded text or a paste event. False means
/// there was nothing to paste (no non-empty text representation).
///
/// The safety rule for a text paste (`input.paste.isSafeWith`): a
/// bracketed paste is unsafe only if it contains the bracket terminator
/// (CSI 201~); an unbracketed paste is unsafe if it contains a line
/// terminator (newline or carriage return) or the bracket terminator.
/// Embedders wanting a stricter rule check
/// `input.paste.isSafe` themselves before calling. A paste event never
/// puts the data on the input stream, so the rule doesn't apply to it.
///
/// The contents are read at most once per call and buffered whole, so
/// the source needs no stability across reads. Nothing reaches the
/// writer until the read completed and the text passed the rule:
/// every error writes nothing, and a failed event records no grant.
pub fn paste(ctx: Context<'_>, req: Request<'_>) -> Result<bool, Error> {
    let Context {
        terminal,
        writer,
        kitty_clipboard,
    } = ctx;

    // If the source is a clipboard and mode 5522 is enabled and
    // the caller can handle kitty events, then do a kitty event.
    if matches!(req.source, Source::Clipboard(_)) && terminal.modes.get(Mode::KittyPasteEvents) {
        if let Some(kitty) = kitty_clipboard {
            paste_kitty_event(writer, kitty, &req)?;
            return Ok(true);
        }
    }

    // For non-Kitty paste events we can only accept text content.
    let Some(index) = (0..req.contents.len()).find(|&i| clipboard::is_text_mime(req.contents.mime(i))) else {
        return Ok(false);
    };

    let opts = input_paste::Options::from_terminal(terminal);

    // In-memory contents are used directly to avoid a double copy but
    // reader-based contents are read into memory so we can do the unsafe
    // scan.
    let mut aw: Vec<u8> = Vec::new();
    let text: &[u8] = match req.contents {
        Contents::Memory(v) => v[index].data,
        Contents::Reader(v) => {
            match v.read.read(v.mimes[index], &mut aw) {
                Ok(()) => {}
                // An allocating writer only fails to allocate.
                Err(clipboard::MimeReaderError::WriteFailed) => return Err(Error::OutOfMemory),
                Err(clipboard::MimeReaderError::ReadFailed) => return Err(Error::ReadFailed),
            }
            &aw
        }
    };
    if text.is_empty() {
        return Ok(false);
    }
    if !req.allow_unsafe && !input_paste::is_safe_with(text, opts) {
        return Err(Error::UnsafePaste);
    }

    input_paste::encode_writer(writer, text, opts)?;
    Ok(true)
}

fn paste_kitty_event(writer: &mut dyn Write, kitty: KittyClipboard<'_>, req: &Request<'_>) -> Result<(), Error> {
    let otp = kitty_clipboard::generate_otp(kitty.io)?;

    // Every representation is listed, never read. The listing is
    // bounded; a clipboard with more types than that is not a thing.
    let mut mimes_buf: [&[u8]; kitty_clipboard::MAX_LISTING_MIMES] = [b""; kitty_clipboard::MAX_LISTING_MIMES];
    let mimes_len = req.contents.len().min(mimes_buf.len());
    for (i, mime) in mimes_buf[..mimes_len].iter_mut().enumerate() {
        *mime = req.contents.mime(i);
    }

    // The grant is recorded before the event can reach the program,
    // and revoked if the event fails to be written so a failure never
    // leaves a grant for an event that was never sent. Using a
    // one-time grant consumes it, which is the revocation.
    kitty.grants.grant(&otp, Direction::Read, true);

    let primary = match req.source {
        Source::Clipboard(loc) => loc != clipboard::Location::STANDARD,
        Source::Text => unreachable!(),
    };

    let mut event = kitty_clipboard::PasteEvent::new(&otp, &mimes_buf[..mimes_len]);
    // The protocol only distinguishes the clipboard from the
    // primary selection, so both non-standard locations report as
    // primary. Only a clipboard paste gets here; see `paste`.
    event.primary = primary;
    if let Err(err) = event.encode(writer) {
        let _ = kitty.grants.r#use(&otp, Direction::Read);
        return Err(err.into());
    }
    Ok(())
}

// The behavior is tested end to end through the stream handler
// (stream_terminal.rs), which is the primary caller.
