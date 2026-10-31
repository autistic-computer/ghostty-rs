//! Port of `src/terminal/stream_continuation.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::io;

use crate::terminal::osc;
use crate::terminal::parser::{self, Parser, State as ParserState};
use crate::terminal::utf8_decoder::Utf8Decoder;

/// Errors possible while validating a snapshot continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidateError {
    /// Nonempty input left both the VT parser and UTF-8 decoder at ground.
    ///
    /// A continuation exists only to reconstruct state that was unfinished at
    /// the snapshot cut. Input that returns to ground is a complete PTY
    /// fragment, not a continuation. Replaying it would repeat work already
    /// represented by the Terminal snapshot. For example, `ESC [ 3 1 m`
    /// completes an SGR command and must not be stored as a continuation.
    NoPendingState,

    /// The input contains bytes that aren't needed to rebuild the
    /// unfinished state.
    ///
    /// A continuation must start exactly where the unfinished state starts.
    /// For a VT sequence, that is the byte that began the sequence. For a
    /// UTF-8 codepoint, that is its first (lead) byte. Anything earlier is
    /// extra input that a restored Stream would drop when it exports its
    /// continuation again, so the round trip would not give back the same
    /// bytes. Extra input could also replay unrelated earlier work.
    ///
    /// For example, `ESC [ 1 ESC [ 2` is rejected because the second
    /// `ESC` starts a new sequence. The valid continuation is `ESC [ 2`.
    ///
    /// The same applies to 8-bit C1 introducers such as 0x9B, the
    /// single-byte form of `ESC [`. Input such as `ESC _ data 0x9B` is
    /// rejected, and its valid continuation is `ESC [`. See `Tracker::write`
    /// for why C1 introducers are always written in their `ESC` form.
    NonCanonicalContinuation,

    /// Replaying the input would perform handler-visible work.
    ///
    /// The Terminal snapshot already contains every mutation committed before
    /// its capture cut. A continuation may rebuild unfinished parser or
    /// builder state, but it must not mutate the Terminal or repeat an external
    /// effect while doing so. For example, BEL inside an unfinished CSI would
    /// ring again even though the CSI itself remains pending.
    ReplayWouldCommit,
}

impl fmt::Display for ValidateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ValidateError::NoPendingState => "NoPendingState",
            ValidateError::NonCanonicalContinuation => "NonCanonicalContinuation",
            ValidateError::ReplayWouldCommit => "ReplayWouldCommit",
        })
    }
}

impl std::error::Error for ValidateError {}

/// Check that `bytes` is a valid continuation, the exact form that
/// `Tracker::write` produces. Empty input is valid and means the stream is
/// at ground with nothing to replay.
///
/// Valid continuation bytes:
///
///   - leave a VT sequence or UTF-8 codepoint unfinished
///   - start exactly where that unfinished state starts
///   - write any 8-bit C1 introducer in its 7-bit `ESC` form
///   - do nothing visible when replayed, such as printing text or
///     ringing the bell
///
/// For example:
///
/// ```text
///     ESC [ 3 1          valid, an unfinished CSI
///     ESC [ 3 1 m        NoPendingState, the CSI is complete
///     A ESC [ 3 1        NonCanonicalContinuation, `A` is extra
///     ESC _ G 0x9B 1     NonCanonicalContinuation, use `ESC [ 1`
///     ESC [ 3 BEL 1      ReplayWouldCommit, BEL rings on replay
/// ```
pub fn validate(bytes: &[u8]) -> Result<(), ValidateError> {
    // Empty explicitly requests ground state and needs no replay.
    if bytes.is_empty() {
        return Ok(());
    }

    // We need the final parser and decoder states to classify the input, so a
    // committed byte cannot return early. Remember it while scanning the rest.
    let mut scanner = BoundaryScanner::init();
    let mut committed_work = false;
    let mut c1_restart = false;
    for &byte in bytes {
        if scanner.next(byte) != Effect::Uncommitted {
            committed_work = true;
        }
        if scanner.c1_restart.is_some() {
            c1_restart = true;
        }
    }

    // Nonempty continuation bytes must leave state that future input needs.
    if scanner.ground() {
        return Err(ValidateError::NoPendingState);
    }

    // The tracker exports the minimal suffix beginning at the effective replay
    // start. A different prefix would not survive byte-identical re-export.
    //
    // The tracker writes C1 introducers in their ESC form, so a raw one that
    // started a new sequence means the bytes did not come from the tracker.
    // It also can't be byte zero, because a Stream at ground reads C1 bytes
    // as UTF-8 text, so there is always extra input before it.
    if c1_restart {
        return Err(ValidateError::NonCanonicalContinuation);
    }
    let replay_start = if scanner.parser.state != ParserState::Ground {
        find_vt_replay_start(bytes)
    } else {
        find_utf8_replay_start(bytes)
    };
    if replay_start != Some(0) {
        return Err(ValidateError::NonCanonicalContinuation);
    }

    // At this point the input is unfinished and minimal, but replay must also
    // be inert with respect to the already-restored Terminal and its effects.
    if committed_work {
        return Err(ValidateError::ReplayWouldCommit);
    }

    Ok(())
}

/// Retains the input needed to reconstruct unfinished Stream parser state.
///
/// A feed is one chunk of bytes given to a Stream. It can end in the middle of
/// a VT sequence or a UTF-8 codepoint. To continue in another Stream, that
/// Stream starts from ground (with no sequence in progress) and reads the
/// saved bytes again. The first saved byte is called the replay start.
///
/// Stream updates this tracker once per feed based on where that replay start
/// is:
///
///   - Ground (parser and UTF-8 decoder): `reset`, no suffix is needed.
///   - Anything unfinished: `append`, which replaces the suffix with the
///     bytes from the replay start onward when that start is inside this
///     feed, and otherwise extends the suffix with this whole feed because
///     the unfinished state began in an earlier one.
///
/// The saved bytes always begin at the last ESC or UTF-8 lead byte. Finding
/// those only takes a quick search of each feed, so the tracker adds almost
/// no cost to normal input processing.
///
/// The saved bytes can hold more than replay needs. `write` trims and
/// rewrites them on export, so the exported bytes are always minimal. There
/// are two cases:
///
///   - An 8-bit C1 introducer, such as 0x9B for `ESC [`, can start a new
///     sequence without an ESC. Whether a C1 byte does this depends on the
///     parser state, so finding it would mean parsing every feed a second
///     time. `write` finds it on export instead.
///   - A byte may have already had a visible effect, such as BEL inside an
///     unfinished CSI sequence. `write` leaves these out so replay does not
///     repeat the effect.
///
/// If the suffix exceeds `max_bytes`, or retaining it fails, `broken`
/// is set until a later feed ends at ground (`reset`) or contains a new
/// replay start (`replace`), both of which need nothing that was lost.
///
/// Replay semantics are guaranteed for the standard TerminalStream handler.
/// Custom handlers, including handlers that intercept vtRaw, are unsupported.
#[derive(Debug)]
pub struct Tracker {
    /// The allocator used to retain bytes. Memory always comes from the
    /// global allocator; this only carries Zig's allocation failure
    /// semantics (see `osc::Allocator`).
    pub alloc: osc::Allocator,
    pub max_bytes: usize,
    pub bytes: Vec<u8>,
    pub broken: bool,
}

/// Which state machine is unfinished at the end of a feed. When the VT
/// parser is outside ground the unfinished state is an escape sequence.
/// When it is grounded, only the UTF-8 decoder can be unfinished, with
/// an incomplete codepoint ending the feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    Vt,
    Utf8,
}

impl Tracker {
    const INITIAL_CAPACITY: usize = 4096;

    /// Initialize a tracker.
    pub fn init(alloc: osc::Allocator, max_bytes: usize) -> Tracker {
        let mut result = Tracker {
            alloc,
            max_bytes,
            bytes: Vec::new(),
            broken: false,
        };

        // We ignore memory errors here. They'll mark the tracker
        // as broken in a future append.
        let _ = result.ensure_total_capacity(max_bytes.min(Self::INITIAL_CAPACITY));
        result
    }

    pub fn deinit(&mut self) {
        self.bytes = Vec::new();
    }

    /// Clear the current continuation suffix and broken state while keeping
    /// its allocation. Stream calls this after reaching ground because no
    /// earlier input is needed to reconstruct the next unfinished state.
    pub fn reset(&mut self) {
        self.broken = false;
        self.bytes.clear();
    }

    /// Retain the part of `input` needed to replay a feed that ended with
    /// `pending` state unfinished.
    ///
    /// When the input contains the replay start for that state, it replaces
    /// the retained suffix outright because nothing earlier is needed. The
    /// new suffix is complete on its own, so this also repairs a broken
    /// tracker. Otherwise the unfinished state began in an earlier feed and
    /// this whole input extends the current suffix.
    pub fn append(&mut self, pending: Pending, input: &[u8]) {
        let start = match pending {
            Pending::Vt => find_vt_replay_start(input),
            Pending::Utf8 => find_utf8_replay_start(input),
        };
        match start {
            Some(start) => self.replace(&input[start..]),
            None => self.extend(input),
        }
    }

    /// Replace the continuation suffix with one that has a new replay start.
    fn replace(&mut self, suffix: &[u8]) {
        self.bytes.clear();
        if suffix.len() > self.max_bytes {
            self.mark_broken();
            return;
        }
        if !self.append_slice(suffix) {
            self.mark_broken();
            return;
        }
        self.broken = false;
    }

    /// Extend the continuation suffix with a fragment that contains no replay
    /// start of its own. The stream stayed unfinished for the whole fragment,
    /// so the existing suffix plus this fragment reproduces the current state.
    fn extend(&mut self, fragment: &[u8]) {
        if self.broken {
            return;
        }
        if fragment.len() > self.max_bytes.saturating_sub(self.bytes.len()) {
            self.mark_broken();
            return;
        }
        if !self.append_slice(fragment) {
            self.mark_broken();
        }
    }

    /// Write the continuation to `writer`. Feeding the written bytes to a
    /// new Stream puts it in the same unfinished state as this one, without
    /// repeating any work this Stream already did. The tracker must not be
    /// broken.
    ///
    /// The output can differ from the saved bytes in two ways.
    ///
    /// First, bytes that already had a visible effect are left out. A BEL
    /// inside an unfinished CSI rings the bell but doesn't change the CSI,
    /// so writing it would only ring the bell again on replay:
    ///
    /// ```text
    ///     saved:   ESC [ 1 BEL ; 2
    ///     written: ESC [ 1 ; 2
    /// ```
    ///
    /// Second, output starts at the last 8-bit C1 introducer that began a
    /// new sequence, written in its `ESC` form. In this example, 0x9B ends
    /// the APC and starts a CSI. Writing the APC would make replay end it a
    /// second time:
    ///
    /// ```text
    ///     saved:   ESC _ data 0x9B 1
    ///     written: ESC [ 1
    /// ```
    ///
    /// The `ESC` form is required because a Stream at ground reads a raw C1
    /// byte as UTF-8 text, not as a control. Both forms leave the parser in
    /// the same state.
    pub fn write<W: io::Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        // Find the last C1 introducer that started a new sequence. This
        // scans the saved bytes a second time, but only on export.
        struct Restart {
            idx: usize,
            r#final: u8,
        }
        let restart: Option<Restart> = {
            let mut result: Option<Restart> = None;
            let mut scanner = BoundaryScanner::init();
            for (i, &c) in self.bytes.iter().enumerate() {
                let _ = scanner.next(c);
                if let Some(r#final) = scanner.c1_restart {
                    result = Some(Restart { idx: i, r#final });
                }
            }
            result
        };

        // Write the introducer in its ESC form and skip everything before
        // it. The scanner sees the same ESC form so it classifies the rest
        // of the bytes from the same parser state that replay will have.
        let mut scanner = BoundaryScanner::init();
        let rest: &[u8] = match restart {
            None => &self.bytes,
            Some(r) => {
                for c in [0x1B, r.r#final] {
                    let effect = scanner.next(c);
                    debug_assert!(effect == Effect::Uncommitted);
                    writer.write_all(&[c])?;
                }
                &self.bytes[r.idx + 1..]
            }
        };

        for &c in rest {
            if scanner.next(c) == Effect::Omittable {
                continue;
            }
            writer.write_all(&[c])?;
        }

        Ok(())
    }

    fn mark_broken(&mut self) {
        self.broken = true;
        self.bytes.clear();
    }

    /// Append to `bytes`, growing through `alloc` (Zig `appendSlice`).
    /// Returns false on allocation failure.
    fn append_slice(&mut self, s: &[u8]) -> bool {
        let needed = self.bytes.len() + s.len();
        if needed > self.bytes.capacity() {
            let better = grow_capacity(self.bytes.capacity(), needed);
            if !self.ensure_total_capacity(better) {
                return false;
            }
        }
        self.bytes.extend_from_slice(s);
        true
    }

    /// Ensure `bytes` has at least `cap` capacity (Zig
    /// `ensureTotalCapacityPrecise`). Returns false on allocation failure.
    fn ensure_total_capacity(&mut self, cap: usize) -> bool {
        if self.bytes.capacity() >= cap {
            return true;
        }
        if self.bytes.capacity() == 0 {
            match self.alloc.alloc_vec(cap) {
                Some(v) => {
                    self.bytes = v;
                    true
                }
                None => false,
            }
        } else {
            self.alloc.realloc(&mut self.bytes, cap)
        }
    }
}

/// Zig's `ArrayList.growCapacity` for `u8` elements.
fn grow_capacity(current: usize, minimum: usize) -> usize {
    const INIT_CAPACITY: usize = 64;
    let mut new = current;
    loop {
        new = new.saturating_add(new / 2 + INIT_CAPACITY);
        if new >= minimum {
            return new;
        }
    }
}

/// Find where replay must begin when a feed ends inside a VT sequence.
///
/// VT parsers treat ESC specially: it abandons the previous parser state and
/// starts a fresh escape state no matter what was being parsed. Because of
/// that rule, feeding the bytes from the last ESC onward into a grounded
/// Stream recreates the unfinished state at the end of `input`.
///
/// The caller must only use this when the VT parser ended outside ground.
/// The returned value is the index of the last ESC in `input`. A None result
/// means the sequence began in an earlier feed, so this entire input must be
/// appended to the continuation bytes already retained.
fn find_vt_replay_start(input: &[u8]) -> Option<usize> {
    // Escape sequences are short, so when the stream ends inside one the
    // replay start is usually within the last few bytes. A reverse search
    // (memchr's vectorized memrchr) keeps pure payload inputs (no ESC at
    // all) memory-bound, like the Zig unrolled vector scan.
    memchr::memrchr(0x1B, input)
}

/// Find where replay must begin when a feed ends inside a UTF-8 codepoint.
///
/// A UTF-8 codepoint starts with a lead byte and may have up to three
/// continuation bytes. If the lead byte is in this input, replay must begin
/// there so the decoder sees the complete partial codepoint again. Since an
/// incomplete codepoint can contain at most three bytes, only the final three
/// input bytes need to be searched.
///
/// The caller must only use this when the VT parser is at ground and the
/// UTF-8 decoder ended mid-codepoint. The returned value is the lead byte's
/// index. A None result means the lead byte was in an earlier feed, so this
/// entire input must be appended to the continuation bytes already retained.
fn find_utf8_replay_start(input: &[u8]) -> Option<usize> {
    const MAX_PENDING: usize = 3;
    let mut idx = input.len();
    while idx > 0 && input.len() - idx < MAX_PENDING {
        idx -= 1;
        if input[idx] >= 0xC0 {
            return Some(idx);
        }
    }
    None
}

/// How one byte affects committed work and the continuation suffix.
/// (Zig: `BoundaryScanner.Effect`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    /// The byte commits no handler-visible work. It must remain because it
    /// may still build the unfinished VT, UTF-8, APC, or DCS state.
    Uncommitted,

    /// The byte commits handler-visible work but also changes a parser
    /// state tag or reaches ground. It cannot be omitted in isolation;
    /// boundary analysis decides whether the surrounding prefix is safe.
    Committed,

    /// The byte commits handler-visible work without changing either
    /// state-machine tag or reaching ground. It can be omitted without
    /// changing the unfinished state, avoiding a duplicate effect.
    Omittable,
}

/// Classifies bytes of a continuation suffix for export.
///
/// The retained suffix reconstructs unfinished state exactly, but it can
/// contain bytes whose handler-visible work already committed the first
/// time (e.g. a C0 control executed inside an unfinished CSI sequence).
/// This implements a minimal VT stream processor (to avoid circular imports
/// with stream.rs) so `Tracker::write` can omit those bytes and replay
/// never repeats a terminal effect. It only runs at export time, never on
/// the feed path.
///
/// This is allocation-free.
struct BoundaryScanner {
    parser: Parser,
    utf8decoder: Utf8Decoder,

    /// Set by each call to `next`. It is non-None when that byte was an
    /// 8-bit C1 introducer that started a new sequence and discarded the
    /// previous parser state. The value is the byte that follows `ESC` in
    /// the introducer's 7-bit form. For example, 0x9B sets this to `[`
    /// because 0x9B and `ESC [` both start a CSI.
    c1_restart: Option<u8>,
}

impl BoundaryScanner {
    /// Initialize both state machines at runtime.
    fn init() -> BoundaryScanner {
        BoundaryScanner {
            parser: Parser::init(),
            utf8decoder: Utf8Decoder::default(),
            c1_restart: None,
        }
    }

    /// True only when neither state machine needs prior bytes to continue.
    fn ground(&self) -> bool {
        self.parser.state == ParserState::Ground && self.utf8decoder.state == 0
    }

    /// Consume one byte using the same scalar UTF-8 retry and VT transitions
    /// as Stream, then classify its effect on continuation construction.
    fn next(&mut self, c: u8) -> Effect {
        // Preserve the state tags so we can recognize committed controls that
        // do not contribute to the unfinished sequence.
        let parser_state = self.parser.state;
        let utf8_state = self.utf8decoder.state;
        self.c1_restart = None;

        // A byte is committed when replaying it would repeat handler-visible
        // work already captured outside the continuation suffix. Actions that
        // only build unfinished APC or DCS state are not committed because
        // replay must reconstruct that state.
        let committed = 'committed: {
            if self.parser.state == ParserState::Ground {
                // Match Stream's scalar UTF-8 path, including retrying a byte that
                // follows a malformed sequence.
                let mut committed = false;
                let res = self.utf8decoder.next(c);
                if let Some(cp) = res.0 {
                    committed = self.codepoint(cp) || committed;
                }
                if !res.1 {
                    let retry = self.utf8decoder.next(c);
                    debug_assert!(retry.1);
                    if let Some(cp) = retry.0 {
                        committed = self.codepoint(cp) || committed;
                    }
                }

                break 'committed committed;
            }

            // Check whether this byte is a C1 introducer that started a new
            // sequence. `final` is the byte that follows `ESC` in the
            // equivalent 7-bit form, and `state` is the parser state both
            // forms lead to. Other C1 bytes, such as 0x9C (string
            // terminator), end a sequence instead of starting one.
            let intro: Option<(u8, ParserState)> = match c {
                0x90 => Some((b'P', ParserState::DcsEntry)),
                0x98 => Some((b'X', ParserState::SosPmApcString)),
                0x9B => Some((b'[', ParserState::CsiEntry)),
                0x9D => Some((b']', ParserState::OscString)),
                0x9E => Some((b'^', ParserState::SosPmApcString)),
                0x9F => Some((b'_', ParserState::SosPmApcString)),
                _ => None,
            };

            let actions = self.parser.next(c);
            let mut committed = false;
            for action in actions.into_iter().flatten() {
                match action {
                    // These actions only build standard handler state. Their
                    // matching end/unhook actions are committed work.
                    parser::Action::DcsHook(_)
                    | parser::Action::DcsPut(_)
                    | parser::Action::ApcStart
                    | parser::Action::ApcPut(_) => {}
                    _ => {
                        committed = true;
                        break;
                    }
                }
            }

            // An introducer only starts a new sequence when it moves the
            // parser into the state it introduces. Two cases don't count.
            // OSC and DCS strings treat high bytes as ordinary data, so the
            // parser stays where it was. And a byte that introduces the
            // state the parser is already in, such as 0x9F inside an APC,
            // changes nothing.
            if let Some((r#final, state)) = intro {
                if self.parser.state == state && parser_state != state {
                    self.c1_restart = Some(r#final);
                }
            }

            committed
        };

        if !committed {
            return Effect::Uncommitted;
        }

        // A committed byte is independently omittable only while the stream
        // remains unfinished and neither state-machine tag changes.
        let state_changed =
            parser_state != self.parser.state || utf8_state != self.utf8decoder.state;
        if !state_changed && !self.ground() {
            Effect::Omittable
        } else {
            Effect::Committed
        }
    }

    /// Apply the Stream.handleCodepoint behavior relevant to replay. Returns
    /// whether the accepted codepoint would commit handler-visible work.
    fn codepoint(&mut self, cp: u32) -> bool {
        if cp == 0x1B {
            self.parser.state = ParserState::Escape;
            self.parser.clear();
            return false;
        }

        // Match Stream.handleCodepoint: all other accepted codepoints have
        // already caused a handler-visible action (including supported C0s).
        true
    }
}

/// A fixed-buffer writer for tests (Zig `std.Io.Writer.fixed`): writes
/// fail with `WriteZero` once the buffer is full.
#[cfg(test)]
pub(crate) struct FixedWriter<'a> {
    buf: &'a mut [u8],
    pub end: usize,
}

#[cfg(test)]
impl<'a> FixedWriter<'a> {
    pub fn fixed(buf: &'a mut [u8]) -> FixedWriter<'a> {
        FixedWriter { buf, end: 0 }
    }

    pub fn buffered(&self) -> &[u8] {
        &self.buf[..self.end]
    }
}

#[cfg(test)]
impl io::Write for FixedWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let n = data.len().min(self.buf.len() - self.end);
        self.buf[self.end..self.end + n].copy_from_slice(&data[..n]);
        self.end += n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_scanner_classifies_effects_and_ground() {
        let mut scanner = BoundaryScanner::init();

        assert!(scanner.ground());
        assert_eq!(Effect::Committed, scanner.next(b'A'));
        assert!(scanner.ground());

        assert_eq!(Effect::Uncommitted, scanner.next(0x1B));
        assert!(!scanner.ground());
        assert_eq!(Effect::Uncommitted, scanner.next(b'['));
        assert_eq!(Effect::Uncommitted, scanner.next(b'1'));

        // BEL commits an execute action without changing the unfinished CSI.
        assert_eq!(Effect::Omittable, scanner.next(0x07));
        assert!(!scanner.ground());

        assert_eq!(Effect::Committed, scanner.next(b'm'));
        assert!(scanner.ground());
    }

    #[test]
    fn boundary_scanner_preserves_unfinished_builder_actions() {
        let mut apc = BoundaryScanner::init();
        assert_eq!(Effect::Uncommitted, apc.next(0x1B));
        assert_eq!(Effect::Uncommitted, apc.next(b'_'));
        assert_eq!(Effect::Uncommitted, apc.next(b'G'));
        assert_eq!(Effect::Committed, apc.next(0x1B));

        let mut dcs = BoundaryScanner::init();
        assert_eq!(Effect::Uncommitted, dcs.next(0x1B));
        assert_eq!(Effect::Uncommitted, dcs.next(b'P'));
        assert_eq!(Effect::Uncommitted, dcs.next(b'q'));
        assert_eq!(Effect::Uncommitted, dcs.next(b'x'));
        assert_eq!(Effect::Committed, dcs.next(0x1B));
    }

    #[test]
    fn boundary_scanner_handles_utf_8_and_malformed_retries() {
        let mut valid = BoundaryScanner::init();
        assert_eq!(Effect::Uncommitted, valid.next(0xF0));
        assert!(!valid.ground());
        assert_eq!(Effect::Uncommitted, valid.next(0x9F));
        assert_eq!(Effect::Uncommitted, valid.next(0x98));
        assert_eq!(Effect::Committed, valid.next(0x84));
        assert!(valid.ground());

        let mut malformed = BoundaryScanner::init();
        assert_eq!(Effect::Uncommitted, malformed.next(0xE0));
        assert_eq!(Effect::Uncommitted, malformed.next(0xA0));
        assert!(malformed.next(0xF0) != Effect::Uncommitted);
        assert!(!malformed.ground());
    }

    #[test]
    fn find_vt_replay_start_finds_the_latest_esc_across_vector_boundaries() {
        assert_eq!(None, find_vt_replay_start(b""));
        assert_eq!(None, find_vt_replay_start(b"no escapes here"));
        assert_eq!(Some(0), find_vt_replay_start(b"\x1b"));
        assert_eq!(Some(5), find_vt_replay_start(b"\x1b[1m\x20\x1b[2"));

        // Exercise both the vector loop and the scalar remainder with replay
        // starts at every position of a buffer larger than any vector width.
        let mut buf = [b'a'; 193];
        assert_eq!(None, find_vt_replay_start(&buf));
        for idx in 0..buf.len() {
            buf.fill(b'a');
            buf[idx] = 0x1B;
            assert_eq!(Some(idx), find_vt_replay_start(&buf));

            // The latest ESC wins.
            if idx > 0 {
                buf[idx - 1] = 0x1B;
                assert_eq!(Some(idx), find_vt_replay_start(&buf));
            }
        }
    }

    #[test]
    fn find_utf8_replay_start_finds_the_pending_lead_byte() {
        assert_eq!(None, find_utf8_replay_start(b""));
        assert_eq!(Some(4), find_utf8_replay_start(b"text\xF0"));
        assert_eq!(Some(4), find_utf8_replay_start(b"text\xF0\x9F"));
        assert_eq!(Some(4), find_utf8_replay_start(b"text\xF0\x9F\x98"));
        assert_eq!(Some(0), find_utf8_replay_start(b"\xE0\xA0"));

        // A malformed prefix rejected earlier doesn't hide the pending lead.
        assert_eq!(Some(2), find_utf8_replay_start(b"\xE0\xA0\xF0"));

        // Continuation bytes of a sequence that started in an earlier input have
        // no replay start of their own.
        assert_eq!(None, find_utf8_replay_start(b"\x9F"));
        assert_eq!(None, find_utf8_replay_start(b"\x9F\x98"));
    }

    #[test]
    fn tracker_retains_and_normalizes_replay_safe_bytes() {
        let mut tracker = Tracker::init(osc::Allocator::GLOBAL, 64);

        tracker.append(Pending::Vt, b"committed\x1b[1\x07");
        tracker.append(Pending::Vt, b";2");
        assert!(!tracker.broken);

        let mut buf = [0u8; 64];
        let mut writer = FixedWriter::fixed(&mut buf);
        tracker.write(&mut writer).unwrap();
        assert_eq!(b"\x1b[1;2", writer.buffered());

        // A feed with a later replay start drops the previous suffix entirely.
        tracker.append(Pending::Vt, b"committed\x1b]0;t");
        let mut replaced_buf = [0u8; 64];
        let mut replaced_writer = FixedWriter::fixed(&mut replaced_buf);
        tracker.write(&mut replaced_writer).unwrap();
        assert_eq!(b"\x1b]0;t", replaced_writer.buffered());

        // An incomplete UTF-8 codepoint seeds at its lead byte and grows with
        // continuation bytes from later feeds.
        tracker.append(Pending::Utf8, b"committed\xF0");
        tracker.append(Pending::Utf8, b"\x9F");
        let mut utf8_buf = [0u8; 4];
        let mut utf8_writer = FixedWriter::fixed(&mut utf8_buf);
        tracker.write(&mut utf8_writer).unwrap();
        assert_eq!(b"\xF0\x9F", utf8_writer.buffered());
    }

    #[test]
    fn tracker_restarts_at_a_c1_introducer_that_aborts_a_string() {
        let mut tracker = Tracker::init(osc::Allocator::GLOBAL, 64);

        // Each C1 introducer ends the APC and exports as its 7-bit form.
        let cases: &[(&[u8], &[u8])] = &[
            (b"\x1b_hello\x9b", b"\x1b["),
            (b"\x1b_hello\x9b1;2", b"\x1b[1;2"),
            (b"\x1b_hello\x90q", b"\x1bPq"),
            (b"\x1b_hello\x9d2;t", b"\x1b]2;t"),
            (b"\x1b[1\x9fx", b"\x1b_x"),
            (b"\x1b[1\x9b2", b"\x1b[2"),
            (b"\x1b\x98", b"\x1bX"),
            (b"\x1b[1\x9e", b"\x1b^"),
            // String payload states take high bytes as data, and re-entering
            // the same state does not restart it.
            (b"\x1b]2;\x9b", b"\x1b]2;\x9b"),
            (b"\x1bPq\x9b", b"\x1bPq\x9b"),
            (b"\x1b_a\x9fb", b"\x1b_a\x9fb"),
        ];
        for &(input, expected) in cases {
            tracker.reset();
            tracker.append(Pending::Vt, input);

            let mut buf = [0u8; 64];
            let mut writer = FixedWriter::fixed(&mut buf);
            tracker.write(&mut writer).unwrap();
            assert_eq!(expected, writer.buffered());
            validate(writer.buffered()).unwrap();
        }

        // The introducer can arrive in a later feed than the aborted string.
        tracker.reset();
        tracker.append(Pending::Vt, b"\x1b_hello");
        tracker.append(Pending::Vt, b"\x9b");
        tracker.append(Pending::Vt, b"3");
        let mut buf = [0u8; 64];
        let mut writer = FixedWriter::fixed(&mut buf);
        tracker.write(&mut writer).unwrap();
        assert_eq!(b"\x1b[3", writer.buffered());
    }

    #[test]
    fn validate_matches_its_documented_examples() {
        validate(b"").unwrap();
        validate(b"\x1b[31").unwrap();
        assert_eq!(Err(ValidateError::NoPendingState), validate(b"\x1b[31m"));
        assert_eq!(
            Err(ValidateError::NonCanonicalContinuation),
            validate(b"A\x1b[31")
        );
        assert_eq!(
            Err(ValidateError::NonCanonicalContinuation),
            validate(b"\x1b_G\x9b1")
        );
        assert_eq!(Err(ValidateError::ReplayWouldCommit), validate(b"\x1b[3\x071"));
    }

    #[test]
    fn validate_rejects_c1_introducers_as_non_canonical() {
        // A grounded stream decodes C1 bytes as UTF-8, so a bare C1 introducer
        // can't recreate the sequence it starts.
        assert_eq!(Err(ValidateError::NoPendingState), validate(b"\x9b"));

        assert_eq!(
            Err(ValidateError::NonCanonicalContinuation),
            validate(b"\x1b_hello\x9b")
        );
        assert_eq!(
            Err(ValidateError::NonCanonicalContinuation),
            validate(b"\x1b[1\x9b2")
        );

        // High bytes in string payloads are data, not introducers.
        validate(b"\x1b]2;\x9b").unwrap();
        validate(b"\x1bPq\x9b").unwrap();
    }

    #[test]
    fn tracker_cap_reset_and_broken_recovery() {
        let mut tracker = Tracker::init(osc::Allocator::GLOBAL, 4);

        tracker.append(Pending::Vt, b"\x1b[123");
        assert!(tracker.broken);

        // Feeds without a replay start are dropped while broken.
        tracker.append(Pending::Vt, b"4");
        assert!(tracker.broken);
        assert_eq!(0, tracker.bytes.len());

        // Suffixes that grow past the cap break tracking.
        tracker.reset();
        tracker.append(Pending::Vt, b"\x1b[1");
        tracker.append(Pending::Vt, b"23");
        assert!(tracker.broken);

        // A new replay start that fits recovers without a reset.
        tracker.append(Pending::Vt, b"\x1b[");
        assert!(!tracker.broken);
        let mut buf = [0u8; 4];
        let mut writer = FixedWriter::fixed(&mut buf);
        tracker.write(&mut writer).unwrap();
        assert_eq!(b"\x1b[", writer.buffered());

        tracker.append(Pending::Vt, b"\x1b[123");
        assert!(tracker.broken);
        tracker.reset();
        assert!(!tracker.broken);
        let mut empty_buf = [0u8; 1];
        let mut empty_writer = FixedWriter::fixed(&mut empty_buf);
        tracker.write(&mut empty_writer).unwrap();
        assert_eq!(0, empty_writer.end);
    }

    #[test]
    fn tracker_reports_writer_failure_and_defers_allocation_failure() {
        let mut tracker = Tracker::init(osc::Allocator::GLOBAL, 64);
        tracker.append(Pending::Vt, b"\x1b[");

        let mut buf = [0u8; 1];
        let mut writer = FixedWriter::fixed(&mut buf);
        assert!(tracker.write(&mut writer).is_err());

        let mut failing_tracker = Tracker::init(osc::Allocator::failing(0, usize::MAX), 64);
        assert!(!failing_tracker.broken);

        // The best-effort initial reservation failed, so the first required
        // retention retries allocation and marks the tracker broken.
        failing_tracker.append(Pending::Vt, b"\x1b[");
        assert!(failing_tracker.broken);
    }
}
