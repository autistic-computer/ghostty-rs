//! Port of `src/input/paste.zig`.
//!
//! STATUS: ported
//!
//! Deviations:
//! - Zig's `encode(data: anytype, ...)` switches on the data type: a
//!   `[]u8` returns the frame directly while a `[]const u8` returns
//!   `Error![3][]const u8`. In Rust this is the [`EncodeData`] trait,
//!   implemented for `&mut [u8]` and `&[u8]` with the same return types.
//! - `*std.Io.Writer` is any `std::io::Write`. `encodeWriter` copies the
//!   data into the writer's own buffer (`writableSliceGreedy`) in Zig;
//!   `std::io::Write` exposes no such buffer, so the data is copied chunk
//!   by chunk into a fixed stack buffer, encoded there, and written out.

use std::fmt;
use std::io::{self, Write};

use crate::terminal::modes::Mode;
use crate::terminal::terminal::Terminal;

/// The bracketed paste (mode 2004) frame written around the data.
pub const BRACKETED_PREFIX: &[u8] = b"\x1b[200~";
pub const BRACKETED_SUFFIX: &[u8] = b"\x1b[201~";

/// The maximum number of bytes `encode` adds around the data, so callers
/// can size a buffer for the full encoded result.
pub const MAX_FRAME_SIZE: usize = BRACKETED_PREFIX.len() + BRACKETED_SUFFIX.len();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    /// True if bracketed paste mode is on.
    pub bracketed: bool,
}

impl Options {
    /// Return the encoding options based on the current terminal state.
    pub fn from_terminal(t: &Terminal) -> Options {
        Options {
            bracketed: t.modes.get(Mode::BracketedPaste),
        }
    }
}

// These are the set of byte values that are always replaced by
// a space (per xterm's behavior) for any text insertion method e.g.
// a paste, drag and drop, etc. These are copied directly from xterm's
// source.
const STRIP: &[u8] = &[
    0x00, // NUL
    0x08, // BS
    0x05, // ENQ
    0x04, // EOT
    0x1B, // ESC
    0x7F, // DEL
    // These can be overridden by the running terminal program
    // via tcsetattr, so they aren't totally safe to hardcode like
    // this. In practice, I haven't seen modern programs change these
    // and its a much bigger architectural change to pass these through
    // so for now they're hardcoded.
    0x03, // VINTR (Ctrl+C)
    0x1C, // VQUIT (Ctrl+\)
    0x15, // VKILL (Ctrl+U)
    0x1A, // VSUSP (Ctrl+Z)
    0x11, // VSTART (Ctrl+Q)
    0x13, // VSTOP (Ctrl+S)
    0x17, // VWERASE (Ctrl+W)
    0x16, // VLNEXT (Ctrl+V)
    0x12, // VREPRINT (Ctrl+R)
    0x0F, // VDISCARD (Ctrl+O)
];

/// Returns true if `data` contains any byte of `set`
/// (Zig: `std.mem.indexOfAny(...) != null`).
fn contains_any(data: &[u8], set: &[u8]) -> bool {
    data.iter().any(|b| set.contains(b))
}

/// Returns true if `needle` occurs in `haystack` (Zig: `std.mem.indexOf`).
fn contains_slice(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

/// The data types accepted by [`encode`] (Zig: the `anytype` switch on
/// `[]u8` vs `[]const u8`).
pub trait EncodeData<'a> {
    /// `[3][]const u8` for mutable data, `Error![3][]const u8` for
    /// const data.
    type Output;

    fn encode(self, opts: Options) -> Self::Output;
}

impl<'a> EncodeData<'a> for &'a mut [u8] {
    type Output = [&'a [u8]; 3];

    fn encode(self, opts: Options) -> Self::Output {
        let data = self;

        // If we have any of the strip values, then we need to replace them
        // with spaces. This is what xterm does and it does it regardless
        // of bracketed paste mode. This is a security measure to prevent pastes
        // from containing bytes that could be used to inject commands.
        for b in data.iter_mut() {
            if STRIP.contains(b) {
                *b = b' ';
            }
        }

        // Bracketed paste mode (mode 2004) wraps pasted data in
        // fenceposts so that the terminal can ignore things like newlines.
        if opts.bracketed {
            return [BRACKETED_PREFIX, data, BRACKETED_SUFFIX];
        }

        // Non-bracketed. We have to replace newline with `\r`. This matches
        // the behavior of xterm and other terminals. For `\r\n` this will
        // result in `\r\r` which does match xterm.
        for b in data.iter_mut() {
            if *b == b'\n' {
                *b = b'\r';
            }
        }

        [b"", data, b""]
    }
}

impl<'a> EncodeData<'a> for &'a [u8] {
    type Output = Result<[&'a [u8]; 3], Error>;

    fn encode(self, opts: Options) -> Self::Output {
        let data = self;

        // See the mutable implementation: any strip value requires
        // modifying the data, which const data can't do.
        if contains_any(data, STRIP) {
            return Err(Error::MutableRequired);
        }

        if opts.bracketed {
            return Ok([BRACKETED_PREFIX, data, BRACKETED_SUFFIX]);
        }

        if data.contains(&b'\n') {
            return Err(Error::MutableRequired);
        }

        Ok([b"", data, b""])
    }
}

/// Encode the given data for pasting. The resulting value can be written
/// to the pty to perform a paste of the input data.
///
/// The data can be either a `&mut [u8]` or a `&[u8]`. If the data
/// type is const then `Error` may be returned. If the data type
/// is mutable then this function can't return an error.
///
/// This slightly complex calling style allows for initially const
/// data to be passed in without an allocation, since it is rare in normal
/// use cases that the data will need to be modified. In the unlikely case
/// data does need to be modified, the caller can make a mutable copy
/// after seeing the error.
///
/// The data is returned as a set of slices to limit allocations. The caller
/// can combine the slices into a single buffer if desired.
///
/// WARNING: The input data is not checked for safety. See the `is_safe`
/// function to check if the data is safe to paste.
pub fn encode<'a, D: EncodeData<'a>>(data: D, opts: Options) -> D::Output {
    data.encode(opts)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Returned if encoding requires a mutable copy of the data. This
    /// can only be returned if the input data type is const.
    MutableRequired,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::MutableRequired => f.write_str("MutableRequired"),
        }
    }
}

impl std::error::Error for Error {}

/// Encode the given data for pasting directly into a writer. This is
/// the same transformation as `encode` (unsafe bytes replaced, bracketed
/// frame or newline conversion per `opts`) but the data is never copied
/// into a separate allocation: it is copied chunk by chunk into a small
/// staging buffer, where it is modified in place, and written out.
/// This is the form to use when the data is const and the result is
/// being assembled into a single buffer anyway.
///
/// Any writer works; a writer with less total capacity than the encoded
/// data reports its write error as usual.
///
/// WARNING: The input data is not checked for safety. See `is_safe`
/// and `is_safe_with` to check if the data is safe to paste.
pub fn encode_writer<W: Write + ?Sized>(
    writer: &mut W,
    data: &[u8],
    opts: Options,
) -> io::Result<()> {
    if opts.bracketed {
        writer.write_all(BRACKETED_PREFIX)?;
    }

    // The byte transformations are position-independent, so the data
    // can be copied and encoded chunk by chunk. The frame returned by
    // encode is ignored since it's written around the whole data here.
    let mut staging = [0u8; 1024];
    let mut remaining = data;
    while !remaining.is_empty() {
        let n = staging.len().min(remaining.len());
        let dest = &mut staging[..n];
        dest.copy_from_slice(&remaining[..n]);
        let _ = encode(&mut *dest, opts);
        writer.write_all(dest)?;
        remaining = &remaining[n..];
    }

    if opts.bracketed {
        writer.write_all(BRACKETED_SUFFIX)?;
    }

    Ok(())
}

/// Returns true if the data looks safe to paste. Data is considered
/// unsafe if it contains any of the following:
///
/// - `\n` or `\r`: Line terminators can be used to inject commands.
///   A carriage return is what the Enter key sends, and an unbracketed
///   paste converts newlines to carriage returns anyway, so the two are
///   equivalent on the input stream. Other terminals that warn on
///   multi-line pastes (iTerm2, Windows Terminal, kitty) treat both
///   bytes as line terminators as well.
/// - `\x1b[201~`: This is the end of a bracketed paste. This can be used
///   to exit a bracketed paste and inject commands.
///
/// We consider any scenario unsafe regardless of current terminal state.
/// For example, even if bracketed paste mode is not active, we still
/// consider `\x1b[201~` unsafe. The existence of these types of bytes
/// should raise suspicion that the producer of the paste data is
/// acting strangely.
pub fn is_safe(data: &[u8]) -> bool {
    !contains_any(data, b"\r\n") && !contains_slice(data, b"\x1b[201~")
}

/// Returns true if the data looks safe to paste given how it will be
/// encoded. This is the terminal-state-aware counterpart of `is_safe`:
///
/// - Bracketed (mode 2004 on): the program receives the data as one
///   framed unit, so line terminators are fine. The data is unsafe only if it
///   contains the end of the frame (`\x1b[201~`), which would let the
///   rest of the data escape the frame and inject commands.
/// - Unbracketed: the same rule as `is_safe`.
///
/// Callers wanting the conservative rule regardless of terminal state
/// should use `is_safe` instead.
pub fn is_safe_with(data: &[u8], opts: Options) -> bool {
    if opts.bracketed {
        return !contains_slice(data, BRACKETED_SUFFIX);
    }
    is_safe(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRACKETED: Options = Options { bracketed: true };
    const UNBRACKETED: Options = Options { bracketed: false };

    #[test]
    fn is_safe_test() {
        assert!(is_safe(b"hello"));
        assert!(is_safe(b"hello\tworld"));
        assert!(!is_safe(b"hello\n"));
        assert!(!is_safe(b"hello\nworld"));
        assert!(!is_safe(b"he\x1b[201~llo"));

        // A carriage return is the Enter key on the input stream, so it
        // can execute a command exactly like a newline.
        assert!(!is_safe(b"hello\r"));
        assert!(!is_safe(b"hello\rworld"));
        assert!(!is_safe(b"hello\r\nworld"));
        assert!(!is_safe(b"\r"));
    }

    #[test]
    fn is_safe_with_test() {
        // Bracketed: line terminators are fine, the frame terminator is not.
        assert!(is_safe_with(b"hello", BRACKETED));
        assert!(is_safe_with(b"hello\nworld", BRACKETED));
        assert!(is_safe_with(b"hello\rworld", BRACKETED));
        assert!(is_safe_with(b"hello\r\nworld", BRACKETED));
        assert!(!is_safe_with(b"he\x1b[201~llo", BRACKETED));
        assert!(!is_safe_with(b"hello\n\x1b[201~", BRACKETED));

        // Unbracketed: the conservative rule.
        assert!(is_safe_with(b"hello", UNBRACKETED));
        assert!(!is_safe_with(b"hello\nworld", UNBRACKETED));
        assert!(!is_safe_with(b"hello\rworld", UNBRACKETED));
        assert!(!is_safe_with(b"hello\r", UNBRACKETED));
        assert!(!is_safe_with(b"he\x1b[201~llo", UNBRACKETED));
    }

    /// Zig `std.Io.Writer.fixed`: a writer over a fixed buffer.
    fn fixed(buf: &mut [u8]) -> io::Cursor<&mut [u8]> {
        io::Cursor::new(buf)
    }

    fn buffered<'a>(w: &'a io::Cursor<&mut [u8]>) -> &'a [u8] {
        &w.get_ref()[..w.position() as usize]
    }

    #[test]
    fn encode_writer_bracketed() {
        let mut buf = [0u8; 64];
        let mut writer = fixed(&mut buf);
        encode_writer(&mut writer, b"hel\x1blo\nworld", BRACKETED).unwrap();
        assert_eq!(buffered(&writer), b"\x1b[200~hel lo\nworld\x1b[201~");
    }

    #[test]
    fn encode_writer_unbracketed() {
        let mut buf = [0u8; 64];
        let mut writer = fixed(&mut buf);
        encode_writer(&mut writer, b"hel\x00lo\r\nworld", UNBRACKETED).unwrap();
        assert_eq!(buffered(&writer), b"hel lo\r\rworld");
    }

    #[test]
    fn encode_writer_empty() {
        let mut buf = [0u8; 64];
        {
            let mut writer = fixed(&mut buf);
            encode_writer(&mut writer, b"", BRACKETED).unwrap();
            assert_eq!(buffered(&writer), b"\x1b[200~\x1b[201~");
        }
        let mut writer = fixed(&mut buf);
        encode_writer(&mut writer, b"", UNBRACKETED).unwrap();
        assert_eq!(buffered(&writer), b"");
    }

    #[test]
    fn encode_writer_chunks_through_a_small_writer_buffer() {
        // A writer with a 4-byte staging buffer that drains into a list,
        // so the data is copied and encoded in several chunks.
        let mut sink = io::BufWriter::with_capacity(4, Vec::<u8>::new());

        let data = b"line one\nline\x1btwo\nline three\n";
        encode_writer(&mut sink, data, BRACKETED).unwrap();
        sink.flush().unwrap();
        assert_eq!(
            &sink.get_ref()[..],
            b"\x1b[200~line one\nline two\nline three\n\x1b[201~"
        );
    }

    #[test]
    fn encode_writer_too_small() {
        let mut buf = [0u8; 4];
        let mut writer = fixed(&mut buf);
        assert!(encode_writer(&mut writer, b"hello", BRACKETED).is_err());
    }

    #[test]
    fn max_frame_size() {
        let result = encode(&b""[..], BRACKETED).unwrap();
        assert_eq!(MAX_FRAME_SIZE, result[0].len() + result[2].len());
    }

    #[test]
    fn encode_bracketed() {
        let result = encode(&b"hello"[..], BRACKETED).unwrap();
        assert_eq!(result[0], b"\x1b[200~");
        assert_eq!(result[1], b"hello");
        assert_eq!(result[2], b"\x1b[201~");
    }

    #[test]
    fn encode_unbracketed_no_newlines() {
        let result = encode(&b"hello"[..], UNBRACKETED).unwrap();
        assert_eq!(result[0], b"");
        assert_eq!(result[1], b"hello");
        assert_eq!(result[2], b"");
    }

    #[test]
    fn encode_unbracketed_newlines_const() {
        assert_eq!(
            encode(&b"hello\nworld"[..], UNBRACKETED),
            Err(Error::MutableRequired)
        );
    }

    #[test]
    fn encode_unbracketed_newlines() {
        let mut data = b"hello\nworld".to_vec();
        let result = encode(data.as_mut_slice(), UNBRACKETED);
        assert_eq!(result[0], b"");
        assert_eq!(result[1], b"hello\rworld");
        assert_eq!(result[2], b"");
    }

    #[test]
    fn encode_unbracketed_windows_stye_newline() {
        let mut data = b"hello\r\nworld".to_vec();
        let result = encode(data.as_mut_slice(), UNBRACKETED);
        assert_eq!(result[0], b"");
        assert_eq!(result[1], b"hello\r\rworld");
        assert_eq!(result[2], b"");
    }

    #[test]
    fn encode_strip_unsafe_bytes_const() {
        assert_eq!(
            encode(&b"hello\x00world"[..], BRACKETED),
            Err(Error::MutableRequired)
        );
    }

    #[test]
    fn encode_strip_unsafe_bytes_mutable_bracketed() {
        let mut data = b"hel\x1blo\x00world".to_vec();
        let result = encode(data.as_mut_slice(), BRACKETED);
        assert_eq!(result[0], b"\x1b[200~");
        assert_eq!(result[1], b"hel lo world");
        assert_eq!(result[2], b"\x1b[201~");
    }

    #[test]
    fn encode_strip_unsafe_bytes_mutable_unbracketed() {
        let mut data = b"hel\x03lo".to_vec();
        let result = encode(data.as_mut_slice(), UNBRACKETED);
        assert_eq!(result[0], b"");
        assert_eq!(result[1], b"hel lo");
        assert_eq!(result[2], b"");
    }

    #[test]
    fn encode_strip_multiple_unsafe_bytes() {
        let mut data = b"\x00\x08\x7f".to_vec();
        let result = encode(data.as_mut_slice(), BRACKETED);
        assert_eq!(result[1], b"   ");
    }
}
