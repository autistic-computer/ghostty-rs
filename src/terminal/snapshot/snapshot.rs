//! Port of `src/terminal/snapshot/snapshot.zig`.
//!
//! STATUS: ported
//!
//! Complete terminal snapshot encoding and restoration.
//!
//! Notes on the port:
//! - `std.Io`/allocator parameters are dropped, and with them the
//!   `BufferFirstAllocator` stack buffer the Zig encoder uses for small
//!   record payloads (an allocation-strategy detail).
//! - `Decoded.deinit` is `Drop`; the continuation is an owned
//!   `continuation::OwnedValue`.
//! - `std.EnumMap`/`EnumArray`/`EnumSet` keyed by the screen key are arrays
//!   indexed by the key.
//! - `Decoder.Progress.Page`/`.Image` are [`ProgressPage`]/[`ProgressImage`].
//! - The test "complete snapshot decode allocation failures are
//!   transactional" exercises Zig's `FailingAllocator` and has no Rust
//!   counterpart (allocation failure aborts).

use super::checkpoint;
use super::continuation;
use super::envelope;
use super::history;
use super::io::{self, Error};
use super::kitty;
use super::page;
use super::record;
use super::screen;
use super::terminal;
use crate::terminal::build_options;
use crate::terminal::page_list::Allocator;
use crate::terminal::screen::{Options as ScreenOptions, Screen as TerminalScreen};
use crate::terminal::screen_set::{Key as TerminalScreenKey, ScreenPtr};
use crate::terminal::size;
use crate::terminal::terminal::Terminal;

/// Re-export continuation to make it a bit more ergonomic to reference.
pub type Continuation<'a> = continuation::Value<'a>;

/// Errors possible while encoding one complete terminal snapshot: the
/// union of the TERMINAL, SCREEN, HISTORY, Kitty graphics, checkpoint and
/// continuation encode errors.
pub type EncodeError = Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeOptions<'a> {
    pub continuation: Continuation<'a>,
}

const KEY_COUNT: usize = TerminalScreenKey::COUNT;

/// Borrow the screen for `key` for the lifetime of the terminal borrow.
fn screen_ref(t: &Terminal, key: TerminalScreenKey) -> Option<&TerminalScreen> {
    // SAFETY: the screen set owns its screens while `t` is borrowed.
    t.screens.get(key).map(|ptr| unsafe { &*ptr.as_ptr() })
}

/// Encode one complete terminal snapshot.
///
/// Encoding starts at the destination's current position. Only one record
/// payload is buffered at a time; completed records stream immediately. On
/// failure, the destination may contain a snapshot prefix without its required
/// FINISH marker, and an output failure may have written part of a record.
pub fn encode(destination: &mut dyn io::Writer, t: &Terminal, options: EncodeOptions<'_>) -> Result<(), EncodeError> {
    // Continuation errors must not emit even the snapshot envelope.
    continuation::validate(options.continuation)?;

    let mut stream = record::Writer::init(destination);

    // Plan each screen's Kitty graphics records up front. A screen without
    // images has no plan and gets no Kitty graphics records at all.
    let mut kitty_plans: [Option<kitty::Plan<'_>>; KEY_COUNT] = [None, None];
    if build_options::KITTY_GRAPHICS {
        for key in TerminalScreenKey::ALL {
            let Some(s) = screen_ref(t, key) else {
                continue;
            };
            kitty_plans[key as usize] = kitty::ScreenPlan::init(s, key);
        }
    }

    // 1. Envelope
    envelope::encode(stream.writer())?;

    // 2. Terminal
    terminal::encode(t, &mut stream)?;

    // 3. Each screen, followed by its Kitty graphics images and
    // placements. Screens may appear in any key order, so this visits
    // every key and skips screens that don't exist.
    for key in TerminalScreenKey::ALL {
        let Some(s) = screen_ref(t, key) else {
            continue;
        };
        screen::encode(s, key, &mut stream)?;
        if build_options::KITTY_GRAPHICS {
            if let Some(plan) = &kitty_plans[key as usize] {
                plan.encode_storage(&mut stream)?;
            }
        }
    }

    // 4. Standard Stream continuation.
    continuation::encode(options.continuation, &mut stream)?;

    // 5. Ready marker.
    checkpoint::encode(checkpoint::Kind::Ready, &mut stream)?;

    // Kitty graphics pixels are written in the order the reader needs
    // them, and the screen that is shown comes first in that order, so
    // what the reader can see arrives before what it cannot.
    let active_key = t.screens.active_key;
    let other_keys = TerminalScreenKey::ALL.into_iter().filter(|&key| key != active_key);

    // 6. The pixels of the Kitty graphics images in the shown screen's
    // viewport. They come after READY so the decoder can show the terminal
    // without waiting for them, and before history so the reader can draw
    // what is on screen as soon as possible.
    if build_options::KITTY_GRAPHICS {
        if let Some(plan) = &kitty_plans[active_key as usize] {
            plan.encode_viewport_payloads(&mut stream)?;
        }
    }

    // 7. Each screen's history, with the Kitty graphics placements pinned
    // to each page and the pixels of the images first placed there.
    for key in TerminalScreenKey::ALL {
        let Some(s) = screen_ref(t, key) else {
            continue;
        };
        history::encode(s, key, &mut stream, kitty_plans[key as usize].as_mut())?;
    }

    // 8. The pixels nothing is waiting on: images in the viewport of a
    // screen that is not shown, then every image that is not placed where
    // the reader can see it.
    if build_options::KITTY_GRAPHICS {
        for key in other_keys.clone() {
            if let Some(plan) = &kitty_plans[key as usize] {
                plan.encode_viewport_payloads(&mut stream)?;
            }
        }
        if let Some(plan) = &kitty_plans[active_key as usize] {
            plan.encode_remaining_payloads(&mut stream)?;
        }
        for key in other_keys {
            if let Some(plan) = &kitty_plans[key as usize] {
                plan.encode_remaining_payloads(&mut stream)?;
            }
        }
    }

    // 9. Finish
    checkpoint::encode(checkpoint::Kind::Finish, &mut stream)
}

/// Errors possible while restoring one complete terminal snapshot
/// (`Decoder.ReadyError || Decoder.NextError`).
pub type DecodeError = Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecodeOptions {
    /// Largest non-ground continuation the decoder may allocate and return.
    /// Set this to zero when only ground-state snapshots are acceptable.
    pub max_continuation_bytes: usize,

    /// Compress each history page as soon as it is restored.
    ///
    /// Without this, restored history stays uncompressed until the caller
    /// runs `Terminal.compress`. With it, the decode holds at most one
    /// uncompressed history page at a time and the restored terminal starts
    /// out compressed.
    ///
    /// A page that is on screen when it is restored stays uncompressed.
    /// This only happens with `Decoder` if the caller scrolls the viewport
    /// to the top of the scrollback between `next` calls.
    ///
    /// The snapshot format is unchanged, so this works with any snapshot.
    /// It has no effect on platforms that do not support compression.
    pub compress_history: bool,

    /// The Kitty graphics image storage limit, in bytes, for each restored
    /// screen, or null for the default limit of a new screen.
    ///
    /// The limit of the terminal that wrote the snapshot is not part of the
    /// snapshot, so the decoder always applies this one. Images are restored
    /// oldest first, and an image that does not fit evicts older images,
    /// exactly as a live transmission would. The newest images are kept.
    /// Zero disables Kitty graphics on the restored terminal.
    pub kitty_image_storage_limit: Option<usize>,
}

/// One decoded Terminal and the bytes needed to resume its Stream.
///
/// `decode` returns this with all history present; `Decoder::ready` returns
/// it at READY, when the terminal is complete for interactive use but its
/// scrollback is still arriving through `Decoder::next`.
///
/// A successful decode owns both values. Keep this result alive until the
/// Terminal's Stream has been restored. The usual restoration sequence is:
///
///   1. Inspect `continuation` and use its length to size continuation tracking.
///   2. Call `to_owned` once and store the returned Terminal at its final address.
///   3. Create the persistent, read-only standard TerminalStream for that
///      address. If `continuation` contains bytes, feed them exactly once and
///      verify that re-exporting the continuation returns the same bytes.
///   4. Drop this value to release the decoded continuation. The transferred
///      Terminal and its Stream remain owned by the caller.
///   5. Process PTY bytes that came after the snapshot cut.
///
/// Do not create a Stream against the address of `terminal` in this struct.
/// `to_owned` moves the Terminal, which would leave such a Stream pointing at
/// its old address.
pub struct Decoded {
    /// Present until `to_owned` transfers the Terminal to the caller. Callers
    /// may inspect it, but must transfer it before attaching a persistent
    /// TerminalStream.
    pub terminal: Option<Terminal>,

    /// For decoded results, nonempty bytes are owned and remain valid until
    /// this value is dropped. Ground needs no replay. Bytes must be replayed
    /// exactly once after the Terminal has reached its final address.
    pub continuation: continuation::OwnedValue,

    /// Advisory complete logical history extent declared by each restored
    /// screen, including the resident overlap already carried by that
    /// screen's own pages. A client may size scrollback UI from this at
    /// READY, before HISTORY pages arrive; native row totals always remain
    /// derived from the pages actually applied.
    pub history_rows: [Option<u64>; KEY_COUNT],

    /// For each screen, how many Kitty graphics images were restored and
    /// how many bytes of their pixels are still to come through
    /// `Decoder::next`. Screens without images, and every screen in a build
    /// without Kitty graphics, report zeros.
    ///
    /// This is meant for showing progress while the pixels arrive:
    ///
    /// ```ignore
    /// let total = decoded.kitty[Key::Primary as usize].pending_bytes;
    /// let mut received: u64 = 0;
    /// while let Some(progress) = decoder.next(&mut terminal)? {
    ///     if let Progress::Image(image) = progress {
    ///         if image.key == Key::Primary {
    ///             received += image.bytes as u64;
    ///         }
    ///     }
    /// }
    /// ```
    ///
    /// Pixels that the decoder drops report zero bytes, so `received` can
    /// end up below `total`. Pixels may arrive between history pages, so
    /// they are only known to be done once `next` returns null.
    pub kitty: [kitty::Counts; KEY_COUNT],
}

impl Decoded {
    /// Transfer the Terminal while retaining the continuation for replay.
    ///
    /// This may be called exactly once. Store the returned value directly at
    /// its final address before creating the TerminalStream that will replay
    /// `continuation`.
    #[allow(clippy::wrong_self_convention, clippy::should_implement_trait)]
    pub fn to_owned(&mut self) -> Terminal {
        self.terminal.take().unwrap()
    }

    /// The continuation bytes, or empty for ground (test convenience for
    /// Zig's `continuation.bytes`).
    pub fn continuation_bytes(&self) -> &[u8] {
        match &self.continuation {
            continuation::OwnedValue::Ground => &[],
            continuation::OwnedValue::Bytes(bytes) => bytes,
        }
    }
}

/// Incrementally restore one snapshot: renderable state first, then history.
///
/// The snapshot record order makes a terminal renderable at the READY
/// marker, after only a small prefix of the stream. `Decoder` exposes
/// that boundary: `ready` returns a complete, integrity-checked `Decoded` while
/// scrollback is still in flight, and each `next` call then applies one
/// history PAGE record off the critical path. `snapshot::decode` is the
/// one-shot form and is implemented over this type.
///
/// ```ignore
/// let mut decoder = snapshot::Decoder::init(&mut reader);
/// let mut decoded = decoder.ready(&DecodeOptions {
///     max_continuation_bytes: 1024 * 1024,
///     ..Default::default()
/// })?;
///
/// let mut terminal = decoded.to_owned();
/// // The terminal is usable now: restore its Stream from the decoded
/// // continuation and size scrollback UI from `decoded.history_rows`.
///
/// while let Some(_progress) = decoder.next(&mut terminal)? {
///     // Scrollback grew; e.g. refresh the scrollbar.
/// }
/// // FINISH validated: the complete snapshot has been decoded.
/// ```
///
/// The terminal is live between `next` calls: the caller may render it and
/// even feed it PTY bytes that arrived after the snapshot cut. `next`
/// re-validates its destination on every call and degrades to discarding,
/// consuming and validating wire bytes without applying them, when live
/// changes have made a page inapplicable. See `next` for the drop rules.
///
/// The decoder owns no allocations. It retains the source given to `init`;
/// the source must stay valid, and must not be read elsewhere, until
/// decoding ends. After any returned error both the decoder and the stream
/// position are invalid: abandon the decoder, and destroy an already
/// transferred Terminal or keep it with partial history, but do not resume.
pub struct Decoder<'a> {
    source: &'a mut dyn io::Reader,
    state: State,
}

enum State {
    /// `ready` has not been called.
    Start,

    /// READY validated; HISTORY sequences remain before FINISH.
    History(History),

    /// FINISH validated. `next` returns null.
    Finished,

    /// A call returned an error; the stream position is unknown.
    Failed,
}

struct History {
    /// The generation observed at READY for every screen the snapshot
    /// declared. Presence is the declaration check for HISTORY routing;
    /// the value detects screens removed or replaced by live terminal
    /// use between calls.
    generations: [Option<usize>; KEY_COUNT],

    /// Keys whose HISTORY sequence was already consumed.
    routed: [bool; KEY_COUNT],

    /// HISTORY sequences that have not begun streaming yet.
    pending: usize,

    /// The sequence currently streaming PAGE records.
    current: Option<Sequence>,

    /// Terminal width at READY. Encoded pages assume this width and are
    /// dropped rather than mixed into a screen reflowed to another.
    cols: size::CellCountInt,

    /// Options for each history page, taken from `ready`.
    page_options: history::DecodeOptions,

    /// The Kitty graphics image storage generation of each screen at
    /// READY. History placements only attach to images that were
    /// restored at READY. See `kitty::PageTarget.ready_generation`.
    kitty_generations: [Option<u64>; KEY_COUNT],

    /// A record header already read from the source but not decoded yet.
    /// After each history PAGE, the decoder reads the next header to see
    /// whether that page's KITTY_IMAGE_PAGE_PLACEMENTS follows. If it is
    /// some other record, its header waits here for the next call.
    pending_header: Option<record::Header>,

    /// An error from reading that next header. The page before it was
    /// already applied, so the call that applied it still reports the
    /// page, and the error is returned by the following call instead.
    pending_error: Option<record::NextHeaderError>,
}

#[derive(Debug, Clone, Copy)]
struct Sequence {
    key: TerminalScreenKey,

    /// PAGE records not yet consumed from this sequence.
    remaining: u32,

    /// False once any page in this sequence was dropped. Later pages
    /// are older, and applying one above a gap would corrupt scrollback
    /// order, so a single drop discards the remainder of the sequence.
    apply: bool,
}

/// Errors possible while decoding the renderable prefix through READY:
/// the Kitty graphics, envelope, TERMINAL, SCREEN, continuation and
/// checkpoint decode errors, plus:
///
/// - `UnexpectedScreenKey`: a SCREEN names a key not declared by TERMINAL.
/// - `DuplicateScreen`: more than one SCREEN names the same key.
pub type ReadyError = Error;

/// One record consumed by `next`.
///
/// ```ignore
/// while let Some(progress) = decoder.next(&mut terminal)? {
///     match progress {
///         Progress::Image(image) => log::debug!("image {} arrived", image.image_id),
///         Progress::Page(page) => log::debug!("{} rows of scrollback", page.rows),
///     }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// A history PAGE, together with its Kitty graphics placements.
    Page(ProgressPage),

    /// The pixels of a Kitty graphics image or animation frame.
    Image(ProgressImage),
}

/// `Decoder.Progress.Page`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressPage {
    /// The screen whose HISTORY contained the page.
    pub key: TerminalScreenKey,

    /// Rows prepended above that screen's existing content, or zero
    /// when the page was consumed and validated but dropped.
    pub rows: usize,

    /// PAGE records still to come in the same HISTORY.
    pub remaining: u32,
}

/// `Decoder.Progress.Image`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressImage {
    /// The screen the image is on.
    pub key: TerminalScreenKey,

    /// The ID of the image the pixels are for.
    pub image_id: u32,

    /// The frame the pixels are for, as the Kitty graphics protocol
    /// counts frames. 1 is the image itself and 2 and up are its
    /// animation frames.
    pub frame: u32,

    /// The number of pixel bytes attached to the image, or zero
    /// when the pixels were consumed and validated but dropped.
    pub bytes: usize,
}

impl Progress {
    /// The page payload (Zig: `progress.page`, which asserts the tag).
    pub fn page(self) -> ProgressPage {
        match self {
            Progress::Page(page) => page,
            Progress::Image(_) => panic!("progress is not a page"),
        }
    }

    /// The image payload (Zig: `progress.image`, which asserts the tag).
    pub fn image(self) -> ProgressImage {
        match self {
            Progress::Image(image) => image,
            Progress::Page(_) => panic!("progress is not an image"),
        }
    }
}

/// Errors possible while decoding history records through FINISH: record
/// header, Kitty graphics, checkpoint, history and page errors, plus:
///
/// - `DecoderNotReady`: `next` was called before `ready` completed.
/// - `DecoderFailed`: a prior `next` call failed and invalidated the
///   stream position.
/// - `UnexpectedHistoryKey`: a HISTORY names a key not declared by TERMINAL.
/// - `DuplicateHistory`: more than one HISTORY names the same key.
/// - `InvalidPageDimensions`, `RowCountOverflow`, `PageSizeOverflow`: a
///   history PAGE cannot join a native PageList. These propagate from
///   `next_page`'s non-limit finalize errors; only limit failures are
///   deliberately converted into drops.
pub type NextError = Error;

impl<'a> Decoder<'a> {
    /// The decoder begins reading only when `ready` is called.
    pub fn init(source: &'a mut dyn io::Reader) -> Decoder<'a> {
        Decoder {
            source,
            state: State::Start,
        }
    }

    /// Decode through the READY marker and return the renderable state.
    ///
    /// The result is transactional and complete for interactive use: the
    /// Terminal, its continuation, and the advisory history extents are all
    /// validated through READY. Scrollback history is not present yet and
    /// arrives through `next`. Asserts this is the first call.
    ///
    /// The Terminal also has every Kitty graphics image from the snapshot,
    /// and the placements on its screens. The pixels arrive later through
    /// `next`, and the images are pending until then. Placements on history
    /// pages also arrive through `next`, together with their pages.
    /// `Decoded.kitty` says how many pixels are still to come.
    pub fn ready(&mut self, options: &DecodeOptions) -> Result<Decoded, ReadyError> {
        assert!(matches!(self.state, State::Start));
        let result = self.ready_inner(options);
        if result.is_err() {
            self.state = State::Failed;
        }
        result
    }

    fn ready_inner(&mut self, options: &DecodeOptions) -> Result<Decoded, ReadyError> {
        let reader: &mut dyn io::Reader = &mut *self.source;

        // Read the envelope, which is currently just a verification step.
        envelope::decode(reader)?;

        // TERMINAL establishes terminal-wide state and allocates empty
        // screen slots with their final routing. SCREEN values replace those
        // slots in place so ScreenSet pointers, including the active
        // pointer, stay valid.
        let header = record::next_header(reader)?;
        let result = terminal::decode(reader, header)?;

        // TERMINAL initializes exactly the number of screen slots it
        // declared. Decode that many SCREEN sequences and route each one by
        // its encoded key.
        let screen_count = result.screens.iter().count();
        let mut screen_options: ScreenOptions = {
            let primary = result.screens.get(TerminalScreenKey::Primary).unwrap();
            let explicit_bytes = primary.pages.limits.bytes.explicit;
            let explicit_lines = primary.pages.limits.lines.explicit;
            ScreenOptions {
                cols: result.cols,
                rows: result.rows,
                max_scrollback_bytes: if explicit_bytes == usize::MAX {
                    None
                } else {
                    Some(explicit_bytes)
                },
                max_scrollback_lines: if explicit_lines == usize::MAX {
                    None
                } else {
                    Some(explicit_lines)
                },
                ..ScreenOptions::default()
            }
        };
        if let Some(limit) = options.kitty_image_storage_limit {
            screen_options.kitty_image_storage_limit = limit;
        }

        // Each SCREEN sequence may be followed by that screen's optional
        // KITTY_IMAGE_STORAGE record. The header after it is read ahead to find
        // out, so each loop starts with the next record's header in hand.
        // The header left over at the end belongs to CONTINUATION.
        let mut routed = [false; KEY_COUNT];
        let mut history_rows: [Option<u64>; KEY_COUNT] = [None; KEY_COUNT];
        let mut header = record::next_header(reader)?;
        for _ in 0..screen_count {
            // Decode the screen and move it into the slot for its key.
            let key = {
                let decoded = screen::decode(reader, header, Allocator::GLOBAL, &screen_options)?;

                let Some(mut slot) = result.screens.get(decoded.key) else {
                    return Err(Error::UnexpectedScreenKey);
                };
                if routed[decoded.key as usize] {
                    return Err(Error::DuplicateScreen);
                }
                routed[decoded.key as usize] = true;
                history_rows[decoded.key as usize] = Some(decoded.history_rows);

                // Replacing the slot's value drops the placeholder screen.
                *slot = decoded.screen;
                decoded.key
            };

            // A KITTY_IMAGE_STORAGE record right after the screen holds its
            // images and placements.
            header = record::next_header(reader)?;
            if build_options::KITTY_GRAPHICS && header.tag == record::Tag::KITTY_IMAGE_STORAGE {
                let mut screen = result.screens.get(key).unwrap();
                kitty::decode_storage(reader, header, key, &mut screen)?;
                header = record::next_header(reader)?;
            }
        }

        // CONTINUATION is required after every active screen. Nonempty bytes
        // are owned locally until the READY prefix validates.
        let decoded_continuation = continuation::decode(reader, header, options.max_continuation_bytes)?;

        // READY marks the exact envelope-through-CONTINUATION boundary.
        let header = record::next_header(reader)?;
        checkpoint::decode(checkpoint::Kind::Ready, reader, header)?;

        // Record where history may be applied. The declared keys route the
        // HISTORY sequences, and the generations detect declared screens
        // that live terminal use destroys or replaces before their history
        // arrives.
        let mut generations: [Option<usize>; KEY_COUNT] = [None; KEY_COUNT];
        let mut kitty_generations: [Option<u64>; KEY_COUNT] = [None; KEY_COUNT];
        let mut kitty_counts: [kitty::Counts; KEY_COUNT] = [kitty::Counts::default(); KEY_COUNT];
        for (key, screen) in result.screens.iter() {
            generations[key as usize] = Some(result.screens.generation(key));
            if build_options::KITTY_GRAPHICS {
                let storage = &screen.kitty_images;
                kitty_generations[key as usize] = Some(storage.generation);
                kitty_counts[key as usize] = kitty::Counts::init(storage);
            }
        }

        self.state = State::History(History {
            generations,
            routed: [false; KEY_COUNT],
            pending: screen_count,
            current: None,
            cols: result.cols,
            page_options: history::DecodeOptions {
                compress: options.compress_history,
            },
            kitty_generations,
            pending_header: None,
            pending_error: None,
        });
        Ok(Decoded {
            terminal: Some(result),
            continuation: decoded_continuation,
            history_rows,
            kitty: kitty_counts,
        })
    }

    /// Decode one history PAGE record and prepend it to its screen in `t`,
    /// or decode the pixels of one Kitty graphics image or animation frame.
    ///
    /// `t` must be the Terminal restored by this decoder's `ready` call,
    /// wherever the caller now stores it. Returns null once FINISH has
    /// validated, which completes the snapshot; the source is
    /// left positioned exactly after it. Further calls return null.
    ///
    /// Wire validation stays strict: framing, CRCs, record order, routing,
    /// and both marker records are always enforced. Application is
    /// forgiving, because the terminal is live and may have changed since
    /// READY. A page is consumed but dropped, reported as zero rows, when:
    ///
    ///   * its screen was removed, or removed and recreated, since READY
    ///     (detected through ScreenSet generations);
    ///   * the terminal was resized since READY, since encoded pages assume
    ///     the encoded width;
    ///   * prepending it would exceed the screen's scrollback limits, e.g.
    ///     because post-cut output already consumed that budget, making
    ///     immediate eviction the only alternative.
    ///
    /// Once a page in a sequence is dropped, the rest of that sequence is
    /// discarded too: later pages are older, and applying one above a gap
    /// would corrupt scrollback ordering. History for other screens is
    /// unaffected.
    ///
    /// The Kitty graphics placements pinned to a page are restored by the
    /// same call as the page. When a page is dropped, its placements are
    /// dropped too.
    ///
    /// Kitty graphics pixels may come at any point, even between history
    /// pages, one image or frame per call. `next` reports them as
    /// `Progress::Image`. Like pages, they are never applied where they no
    /// longer belong. The pixels are consumed but dropped, reported as zero
    /// bytes, when:
    ///
    ///   * their screen was removed, or removed and recreated, since READY;
    ///   * live input deleted the image, or sent a new image with the same
    ///     ID, since READY.
    pub fn next(&mut self, t: &mut Terminal) -> Result<Option<Progress>, NextError> {
        match self.state {
            // `ready` must succeed before history exists to decode, and a
            // failed decoder no longer knows its stream position.
            State::Start => return Err(Error::DecoderNotReady),
            State::Failed => return Err(Error::DecoderFailed),
            State::Finished => return Ok(None),
            State::History(_) => {}
        }
        let result = self.next_inner(t);
        if result.is_err() {
            self.state = State::Failed;
        }
        result
    }

    fn next_inner(&mut self, t: &mut Terminal) -> Result<Option<Progress>, NextError> {
        let State::History(state) = &mut self.state else {
            unreachable!()
        };
        let source: &mut dyn io::Reader = &mut *self.source;

        loop {
            // Read the next header before deciding how to decode the
            // record. Unknown records before it were skipped.
            let header = take_header(state, source)?;

            // Kitty graphics pixels may come anywhere after READY, even
            // between the pages of a HISTORY sequence, in any screen order.
            if build_options::KITTY_GRAPHICS && header.tag == record::Tag::KITTY_IMAGE_PAYLOAD {
                return next_payload(state, source, t, header).map(Some);
            }

            // Stream the current sequence's next PAGE if one remains.
            if let Some(sequence) = state.current {
                if sequence.remaining > 0 {
                    return next_page(state, source, t, header).map(Some);
                }
                state.current = None;
            }

            // All sequences are consumed: FINISH ends the snapshot, leaving
            // trailing transport bytes unread.
            if state.pending == 0 {
                checkpoint::decode(checkpoint::Kind::Finish, source, header)?;
                if build_options::SLOW_RUNTIME_SAFETY {
                    for key in TerminalScreenKey::ALL {
                        if state.generations[key as usize].is_none() {
                            continue;
                        }
                        let Some(restored) = t.screens.get(key) else {
                            continue;
                        };
                        restored.pages.assert_integrity();
                        restored.assert_integrity();
                    }
                }
                self.state = State::Finished;
                return Ok(None);
            }

            // Begin the next HISTORY sequence. Keys route the sequences
            // order-independently, but each must name a declared screen and
            // may not repeat.
            state.pending -= 1;
            let manifest = history::Decoder::init(&mut *source, header)?;
            let key = manifest.header.key;
            let page_count = manifest.header.page_count;
            if state.generations[key as usize].is_none() {
                return Err(Error::UnexpectedHistoryKey);
            }
            if state.routed[key as usize] {
                return Err(Error::DuplicateHistory);
            }
            state.routed[key as usize] = true;
            state.current = Some(Sequence {
                key,
                remaining: page_count,
                apply: true,
            });
        }
    }
}

/// Return the next record header: the one a previous call read ahead,
/// or else the next one from the source. A failure from reading ahead
/// is returned here.
fn take_header(state: &mut History, source: &mut dyn io::Reader) -> Result<record::Header, NextError> {
    if let Some(err) = state.pending_error {
        return Err(err);
    }
    if let Some(header) = state.pending_header.take() {
        return Ok(header);
    }
    record::next_header(source)
}

/// The screen for `key` in `t`, if it is still the screen restored at
/// READY. Returns null if live input removed it, or removed it and
/// created a new one, since then.
fn restored_screen(state: &History, t: &Terminal, key: TerminalScreenKey) -> Option<ScreenPtr> {
    let restored = t.screens.get(key)?;
    if Some(t.screens.generation(key)) != state.generations[key as usize] {
        return None;
    }
    Some(restored)
}

/// Consume one KITTY_IMAGE_PAYLOAD record, attaching its pixels to their
/// image when it is still the one restored at READY and dropping them
/// otherwise.
fn next_payload(
    state: &mut History,
    source: &mut dyn io::Reader,
    t: &mut Terminal,
    header: record::Header,
) -> Result<Progress, NextError> {
    debug_assert!(build_options::KITTY_GRAPHICS);

    // Pixels may go to any screen restored at READY that live input
    // hasn't removed or replaced since.
    let mut targets: kitty::PayloadTargets<'_> = [None, None];
    for key in TerminalScreenKey::ALL {
        if state.generations[key as usize].is_none() {
            continue;
        }
        let Some(restored) = restored_screen(state, t, key) else {
            continue;
        };
        targets[key as usize] = Some(kitty::PayloadTarget {
            // SAFETY: each key names a distinct live screen owned by `t`,
            // which is exclusively borrowed for this call.
            screen: unsafe { restored.as_mut() },
            ready_generation: state.kitty_generations[key as usize].unwrap(),
        });
    }

    let payload = kitty::decode_payload(source, header, &mut targets)?;
    if state.generations[payload.key as usize].is_none() {
        return Err(Error::UnexpectedKittyKey);
    }
    Ok(Progress::Image(ProgressImage {
        key: payload.key,
        image_id: payload.image_id,
        frame: payload.frame,
        bytes: payload.bytes,
    }))
}

/// Consume one PAGE record from the current sequence, whose header
/// `next` already read, applying it to the live terminal when it is
/// still applicable and discarding it otherwise. The page's Kitty
/// graphics placements are restored with it, or dropped with it.
fn next_page(
    state: &mut History,
    source: &mut dyn io::Reader,
    t: &mut Terminal,
    header: record::Header,
) -> Result<Progress, NextError> {
    let mut sequence = state.current.unwrap();

    // Application is re-decided on every page so live terminal changes
    // between calls degrade to discarding instead of failing decoding.
    let destination: Option<ScreenPtr> = if !sequence.apply || t.cols != state.cols {
        None
    } else {
        restored_screen(state, t, sequence.key)
    };

    sequence.remaining -= 1;
    let (applied, rows): (Option<ScreenPtr>, usize) = match destination {
        None => {
            // The record must still be consumed so decoding remains
            // aligned with the following records and FINISH marker.
            sequence.apply = false;
            state.current = Some(sequence);
            page::discard(source, header)?;
            (None, 0)
        }
        Some(mut restored) => {
            // The one-shot history decoder rejects a Screen which already has
            // complete history. This incremental path intentionally bypasses
            // that guard: the terminal has been live since READY and may have
            // accumulated new history. Generation and width checks above keep
            // the destination compatible, while per-page finalization enforces
            // its current scrollback limits.
            state.current = Some(sequence);
            match history::decode_page(source, header, &mut restored, state.page_options) {
                Ok(rows) => (Some(restored), rows),
                // The page no longer fits the screen's scrollback limits,
                // e.g. because post-cut output consumed the budget, so it
                // would be evicted immediately anyway. Its record was fully
                // consumed, leaving the stream aligned on the next record.
                Err(Error::MaxSizeExceeded | Error::MaxLinesExceeded) => {
                    sequence.apply = false;
                    state.current = Some(sequence);
                    (None, 0)
                }
                Err(other) => return Err(other),
            }
        }
    };

    let progress = Progress::Page(ProgressPage {
        key: sequence.key,
        rows,
        remaining: sequence.remaining,
    });

    // The page's Kitty graphics placements, if any, follow it. Read the
    // next header to find out. Anything else waits for the next call,
    // and so does a failure to read it, because this page is already
    // applied and its progress should not be lost.
    let following = match record::next_header(source) {
        Ok(header) => header,
        Err(err) => {
            state.pending_error = Some(err);
            return Ok(progress);
        }
    };
    if build_options::KITTY_GRAPHICS && following.tag == record::Tag::KITTY_IMAGE_PAGE_PLACEMENTS {
        // The page was just added at the top of the screen, which
        // makes it the first page of the screen's page list.
        let target = applied.map(|s| {
            let page = s.pages.pages.first.unwrap();
            kitty::PageTarget {
                // SAFETY: the screen is live and owned by `t`, which is
                // exclusively borrowed for this call.
                screen: unsafe { s.as_mut() },
                page,
                ready_generation: state.kitty_generations[sequence.key as usize].unwrap(),
            }
        });
        kitty::decode_page_placements(source, following, sequence.key, target)?;
        return Ok(progress);
    }

    state.pending_header = Some(following);
    Ok(progress)
}

/// Restore one complete snapshot into a native Terminal and continuation.
///
/// This consumes one snapshot through FINISH and leaves any following bytes
/// in the reader for the containing transport. Restoration is transactional:
/// the returned result is either complete and ready or not (error return).
/// Individual record codecs normalize optional semantic state, and history
/// beyond the declared scrollback limits is dropped rather than rejected,
/// while framing, markers, declared sequence counts, and unique
/// cross-record screen routing remain strict.
///
/// This is the one-shot form of `Decoder`, which can additionally hand the
/// renderable terminal to the caller at READY, before history arrives.
pub fn decode(source: &mut dyn io::Reader, options: &DecodeOptions) -> Result<Decoded, DecodeError> {
    let mut decoder = Decoder::init(source);
    let mut result = decoder.ready(options)?;

    // The Terminal has not been transferred, so history applies in place.
    let t = result.terminal.as_mut().unwrap();
    while decoder.next(t)?.is_some() {}
    Ok(result)
}

/// Errors possible while restoring a snapshot that must end at end-of-file:
/// decode and read errors, plus `TrailingData` when FINISH was followed by
/// additional bytes.
pub type DecodeExactError = Error;

/// Restore one snapshot and require FINISH to be followed by end-of-file.
///
/// This is intended for bounded snapshot files and buffers. On a live stream,
/// checking for end-of-file may block; use `decode` to stop at FINISH instead.
pub fn decode_exact(source: &mut dyn io::Reader, options: &DecodeOptions) -> Result<Decoded, DecodeExactError> {
    let result = decode(&mut *source, options)?;
    match source.peek_byte() {
        Err(Error::EndOfStream) => Ok(result),
        Err(err) => Err(err),
        Ok(_) => Err(Error::TrailingData),
    }
}

/// Build a native Screen around `pages` with every other field at its
/// default value, with the cursor at the active area's top-left (Zig
/// tests: a `Screen` struct literal). Shared by the snapshot test suites.
#[cfg(test)]
pub(crate) fn test_screen_from_pages(mut pages: crate::terminal::page_list::PageList) -> TerminalScreen {
    use crate::terminal::point::{Coordinate, Point};
    use crate::terminal::screen::{CharsetState, Cursor, Dirty, SemanticPrompt};

    let cursor_pin = pages
        .track_pin(pages.pin(Point::Active(Coordinate::default())).unwrap())
        .unwrap();
    // SAFETY: the pin was just tracked in `pages`.
    let cursor_rac = unsafe { cursor_pin.as_ref() }.row_and_cell();
    TerminalScreen {
        pages,
        no_scrollback: false,
        cursor: Cursor::new(cursor_pin, cursor_rac.row, cursor_rac.cell),
        saved_cursor: None,
        selection: None,
        charset: CharsetState::default(),
        protected_mode: crate::terminal::ansi::ProtectedMode::Off,
        kitty_keyboard: Default::default(),
        kitty_images: Default::default(),
        semantic_prompt: SemanticPrompt::DISABLED,
        dirty: Dirty::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::color::Rgb;
    use crate::terminal::kitty::graphics_image::Image;
    use crate::terminal::kitty::graphics_storage::{Location as PlacementLocation, Placement};
    use crate::terminal::kitty::graphics_unicode;
    use crate::terminal::osc;
    use crate::terminal::page::{Capacity, Cell};
    use crate::terminal::page_list::{self, CompressMode, LimitKey, Node, NodePtr};
    use crate::terminal::point::{Coordinate, Point, Tag};
    use crate::terminal::sgr;
    use crate::terminal::snapshot::fixture;
    use crate::terminal::snapshot::io::{FixedReader, Limited, Reader as _};
    use crate::terminal::stream;
    use crate::terminal::stream_terminal;
    use crate::terminal::terminal::{Options as TerminalOptions, Resize};

    type Key = TerminalScreenKey;
    type TerminalStream<'a> = stream_terminal::Stream<'a>;

    const TEST_ENCODE_OPTIONS: EncodeOptions<'static> = EncodeOptions {
        continuation: Continuation::Ground,
    };
    const TEST_DECODE_OPTIONS: DecodeOptions = DecodeOptions {
        max_continuation_bytes: 1024,
        compress_history: false,
        kitty_image_storage_limit: None,
    };

    fn test_complete_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/complete-v1.hex"))
    }

    fn nd<'a>(node: NodePtr) -> &'a mut Node {
        // SAFETY: node pointers come from a live PageList.
        unsafe { &mut *node.as_ptr() }
    }

    fn test_encode(t: &Terminal, options: EncodeOptions<'_>) -> Vec<u8> {
        let mut encoded = Vec::new();
        encode(&mut encoded, t, options).unwrap();
        encoded
    }

    fn terminal_init(cols: u16, rows: u16, unlimited: bool) -> Terminal {
        let mut options = TerminalOptions::new(cols, rows);
        if unlimited {
            options.max_scrollback_bytes = None;
            options.max_scrollback_lines = None;
        }
        Terminal::init(options).unwrap()
    }

    fn terminal_stream(t: &mut Terminal, continuation_max_bytes: usize) -> TerminalStream<'_> {
        TerminalStream::init(stream::Options {
            allocator: Some(osc::Allocator::GLOBAL),
            continuation_max_bytes: Some(continuation_max_bytes),
            ..stream::Options::new(stream_terminal::Handler::init(t))
        })
    }

    fn write_continuation(s: &TerminalStream<'_>) -> Vec<u8> {
        let mut out = Vec::new();
        s.write_continuation(&mut out).unwrap();
        out
    }

    fn set_cell(page: &mut crate::terminal::page::Page, y: usize, cp: char) {
        // SAFETY: (0, y) is within the page.
        unsafe { *page.get_row_and_cell(0, y).cell = Cell::init(cp as u32) };
    }

    /// Build a 2x3 test terminal whose primary screen carries `history_pages`
    /// complete two-row pages above a three-row active page. The top-left cell
    /// of each page is marked 'A', 'B', ... from oldest history to active.
    fn test_history_terminal(history_pages: u8) -> Terminal {
        let t = terminal_init(2, 3, true);

        let mut primary = t.screens.get(Key::Primary).unwrap();
        let replacement = {
            let mut builder = page_list::Builder::init(page_list::Options {
                cols: t.cols,
                rows: t.rows,
                max_size: None,
                max_lines: None,
            })
            .unwrap();

            for index in 0..history_pages {
                let built = builder.allocate_page(Capacity::new(2, 2)).unwrap();
                built.size.rows = 2;
                set_cell(built, 0, (b'A' + index) as char);
            }
            let active = builder.allocate_page(Capacity::new(2, 3)).unwrap();
            active.size.rows = 3;
            set_cell(active, 0, (b'A' + history_pages) as char);

            test_screen_from_pages(builder.finish().unwrap())
        };
        *primary = replacement;
        t
    }

    /// The top-left screen cell, which lands in the oldest applied history page.
    fn test_top_left_codepoint(terminal_screen: &TerminalScreen) -> u32 {
        let node = terminal_screen.pages.get_top_left(Tag::Screen).node;
        // SAFETY: (0, 0) is within the page.
        unsafe { (*nd(node).page().get_row_and_cell(0, 0).cell).codepoint() }
    }

    /// Writes to a counting discard writer and a CRC32C hasher at once
    /// (Zig: `discard.writer.hashed(Crc32c.init(), &.{})`).
    struct HashedDiscarding {
        discard: io::Discarding,
        hashing: io::HashingWriter,
    }

    impl io::Writer for HashedDiscarding {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.hashing.write_all(bytes)?;
            self.discard.write_all(bytes)
        }
    }

    #[test]
    fn complete_snapshot_round_trip_with_history_and_alternate_screen() {
        let complete_fixture = test_complete_fixture();
        let mut t = terminal_init(2, 3, true);

        // Exercise terminal-wide state.
        t.width_px = 800;
        t.height_px = 600;
        t.colors.palette.set(7, Rgb { r: 1, g: 2, b: 3 });
        t.modes.values.set_bracketed_paste(true);
        t.set_pwd(b"file:///tmp/snapshot").unwrap();
        t.set_title(b"complete snapshot").unwrap();

        let mut primary = t.screens.get(Key::Primary).unwrap();

        // Use small exact capacities so this compound golden remains practical to
        // review while still containing two complete history pages and one active
        // page. Replacing the Screen in place preserves ScreenSet routing.
        let replacement = {
            let mut builder = page_list::Builder::init(page_list::Options {
                cols: t.cols,
                rows: t.rows,
                max_size: None,
                max_lines: None,
            })
            .unwrap();

            let oldest = builder.allocate_page(Capacity::new(2, 2)).unwrap();
            oldest.size.rows = 2;
            set_cell(oldest, 0, 'A');

            let recent = builder.allocate_page(Capacity::new(2, 2)).unwrap();
            recent.size.rows = 2;
            set_cell(recent, 0, 'B');

            let active = builder.allocate_page(Capacity::new(2, 3)).unwrap();
            active.size.rows = 3;
            set_cell(active, 0, 'C');
            set_cell(active, 1, 'D');
            set_cell(active, 2, 'E');

            test_screen_from_pages(builder.finish().unwrap())
        };
        *primary = replacement;

        assert!(primary.pages.scrollbar().total > t.rows as usize);

        // Compression is an internal source representation and must remain
        // unchanged while the complete history is inspected for encoding.
        let _ = primary.pages.compress(CompressMode::Full);
        let source_memory = primary.pages.memory_stats();

        // The optional alternate screen participates in both phases and remains
        // the active screen after restoration.
        t.switch_screen(Key::Alternate).unwrap();
        t.print_string("alternate").unwrap();

        let encoded = test_encode(&t, TEST_ENCODE_OPTIONS);
        assert_eq!(source_memory, primary.pages.memory_stats());
        fixture::expect_equal(
            fixture::Kind::Snapshot,
            "src/terminal/snapshot/testdata/complete-v1.hex",
            "snapshot_fixture-complete-v1.hex",
            &complete_fixture,
            &encoded,
        );

        // A complete snapshot can stream through a non-allocating destination.
        // Independently hash that output so both its length and complete byte
        // sequence are checked without retaining a second snapshot copy.
        let mut hashing = HashedDiscarding {
            discard: io::Discarding::default(),
            hashing: io::HashingWriter {
                hasher: record::Crc32c::init(),
            },
        };
        encode(&mut hashing, &t, TEST_ENCODE_OPTIONS).unwrap();
        assert_eq!(complete_fixture.len() as u64, hashing.discard.count);
        assert_eq!(record::Crc32c::hash(&complete_fixture), hashing.hashing.hasher.final_());

        // Restore the checked-in reference rather than the just-generated bytes.
        let mut encoded_source = FixedReader::new(&complete_fixture);
        let mut limited = Limited::new(&mut encoded_source, None, 1);
        let restored = decode(&mut limited, &TEST_DECODE_OPTIONS).unwrap();
        let restored_terminal = restored.terminal.as_ref().unwrap();

        assert_eq!(Key::Alternate, restored_terminal.screens.active_key);
        assert_eq!(
            restored_terminal.screens.get(Key::Alternate).unwrap(),
            restored_terminal.screens.active
        );
        assert_eq!(&b"file:///tmp/snapshot"[..], restored_terminal.get_pwd().unwrap());
        assert_eq!(&b"complete snapshot"[..], restored_terminal.get_title().unwrap());
        assert_eq!(
            primary.pages.scrollbar().total,
            restored_terminal.screens.get(Key::Primary).unwrap().pages.scrollbar().total
        );

        // Re-encoding is a compact semantic equality check over all TERMINAL,
        // SCREEN, PAGE, and HISTORY fields and both marker boundaries.
        let reencoded = test_encode(restored_terminal, TEST_ENCODE_OPTIONS);
        assert_eq!(complete_fixture, reencoded);

        // SCREEN and HISTORY keys make both sequence groups order independent.
        let alternate = screen_ref(&t, Key::Alternate).unwrap();
        let mut reversed: Vec<u8> = Vec::new();
        {
            let mut reversed_stream = record::Writer::init(&mut reversed);
            envelope::encode(reversed_stream.writer()).unwrap();
            terminal::encode(&t, &mut reversed_stream).unwrap();
            screen::encode(alternate, Key::Alternate, &mut reversed_stream).unwrap();
            screen::encode(&primary, Key::Primary, &mut reversed_stream).unwrap();
            continuation::encode(continuation::Value::Ground, &mut reversed_stream).unwrap();
            checkpoint::encode(checkpoint::Kind::Ready, &mut reversed_stream).unwrap();
            history::encode(alternate, Key::Alternate, &mut reversed_stream, None).unwrap();
            history::encode(&primary, Key::Primary, &mut reversed_stream, None).unwrap();
            checkpoint::encode(checkpoint::Kind::Finish, &mut reversed_stream).unwrap();
        }

        let mut reversed_source = FixedReader::new(&reversed);
        let reversed_restored = decode(&mut reversed_source, &TEST_DECODE_OPTIONS).unwrap();
        let reversed_terminal = reversed_restored.terminal.as_ref().unwrap();
        assert_eq!(Key::Alternate, reversed_terminal.screens.active_key);
        assert_eq!(0, reversed_terminal.screens.generation(Key::Primary));
        assert_eq!(0, reversed_terminal.screens.generation(Key::Alternate));
    }

    #[test]
    fn complete_snapshot_restores_a_canonical_stream_continuation() {
        let mut source_terminal = terminal_init(8, 2, false);
        let mut source_stream = terminal_stream(&mut source_terminal, 1024);

        source_stream.next_slice(b"A\x1b[31");
        let exported = write_continuation(&source_stream);
        assert_eq!(&b"\x1b[31"[..], &exported[..]);

        let encoded = test_encode(
            source_stream.handler.terminal,
            EncodeOptions {
                continuation: Continuation::Bytes(&exported),
            },
        );

        let mut encoded_source = FixedReader::new(&encoded);
        let mut decoded = decode(
            &mut encoded_source,
            &DecodeOptions {
                max_continuation_bytes: 1024,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(&exported[..], decoded.continuation_bytes());

        let mut restored_terminal = decoded.to_owned();
        assert!(decoded.terminal.is_none());
        let mut restored_stream = terminal_stream(&mut restored_terminal, 1024);

        restored_stream.next_slice(decoded.continuation_bytes());
        let reexported = write_continuation(&restored_stream);
        assert_eq!(exported, reexported);

        // The identical post-cut bytes may use different feed chunking without
        // changing terminal semantics.
        source_stream.next_slice(b"mB");
        restored_stream.next(b'm');
        restored_stream.next(b'B');
        let source_text = source_stream.handler.terminal.plain_string().unwrap();
        let restored_text = restored_stream.handler.terminal.plain_string().unwrap();
        assert_eq!(source_text, restored_text);
        assert_eq!(
            source_stream.handler.terminal.screens.active.cursor.style_id,
            restored_stream.handler.terminal.screens.active.cursor.style_id
        );
        source_stream.deinit();
        restored_stream.deinit();
    }

    #[test]
    fn complete_snapshot_validates_continuation_before_writing() {
        let t = terminal_init(2, 1, false);

        let mut destination: Vec<u8> = b"prefix".to_vec();
        assert_eq!(
            Err(Error::NoPendingState),
            encode(
                &mut destination,
                &t,
                EncodeOptions {
                    continuation: Continuation::Bytes(b"\x1b[31m"),
                },
            )
        );
        assert_eq!(&b"prefix"[..], &destination[..]);

        let mut tail = [0x1b, b'[', b'3', b'1'];
        let encoded = test_encode(
            &t,
            EncodeOptions {
                continuation: Continuation::Bytes(&tail),
            },
        );
        tail[2] = b'4';
        let _ = tail;

        let mut source = FixedReader::new(&encoded);
        let decoded = decode(&mut source, &TEST_DECODE_OPTIONS).unwrap();
        assert_eq!(&b"\x1b[31"[..], decoded.continuation_bytes());
    }

    #[test]
    fn complete_snapshot_waits_for_continuation_tracking_recovery() {
        let mut t = terminal_init(2, 1, false);
        let mut s = terminal_stream(&mut t, 4);

        s.next_slice(b"\x1b[123");
        let mut unavailable = Vec::new();
        assert_eq!(
            Err(stream::WriteContinuationError::ContinuationUnavailable),
            s.write_continuation(&mut unavailable)
        );

        // A new replay start replaces the lost suffix and makes a later cut
        // publishable without affecting normal terminal parsing.
        s.next_slice(b"\x1b[");
        let recovered = write_continuation(&s);
        assert_eq!(&b"\x1b["[..], &recovered[..]);

        test_encode(
            s.handler.terminal,
            EncodeOptions {
                continuation: Continuation::Bytes(&recovered),
            },
        );
        s.deinit();
    }

    fn continuation_of(bytes: &[u8]) -> Continuation<'_> {
        if bytes.is_empty() {
            Continuation::Ground
        } else {
            Continuation::Bytes(bytes)
        }
    }

    #[test]
    fn complete_snapshot_preserves_every_supported_continuation_cut() {
        let corpora: [&[u8]; 13] = [
            b"plain \xF0\x9F\x98\x84 utf8",
            b"bad \xE0\xA0\xF0\x9F\x98\x84 utf8",
            b"\x1b[1\x07;2mstyled\x1b[0m",
            b"\x1b]2;window title\x1b\\text",
            b"\x1bP$qm\x1b\\text",
            b"\x1b_Ga=q;payload\x1b\\text",
            b"\x1b_25a1;s\x1b\\text",
            b"\x1b]2;first\x1b\\\x1b_Gsecond",
            b"\x1b[12\x9D2;title\x1b\\text",
            b"\x1b_hello\x9b31mtext",
            b"\x1b_Ga=q\x90$qm\x1b\\text",
            b"\x1b_hello\x9d2;title\x07text",
            b"\x1b[12\x18text\x1b[1\x1Atext",
        ];

        for corpus in corpora {
            for cut in 0..corpus.len() + 1 {
                let mut source_terminal = terminal_init(20, 4, false);
                let mut source_stream = terminal_stream(&mut source_terminal, 4096);
                source_stream.next_slice(&corpus[..cut]);

                let cut_bytes = write_continuation(&source_stream);
                let snapshot_bytes = test_encode(
                    source_stream.handler.terminal,
                    EncodeOptions {
                        continuation: continuation_of(&cut_bytes),
                    },
                );

                let mut snapshot_source = FixedReader::new(&snapshot_bytes);
                let mut decoded = decode(
                    &mut snapshot_source,
                    &DecodeOptions {
                        max_continuation_bytes: 4096,
                        ..Default::default()
                    },
                )
                .unwrap();
                let mut restored_terminal = decoded.to_owned();
                let mut restored_stream = terminal_stream(&mut restored_terminal, 4096);
                match &decoded.continuation {
                    continuation::OwnedValue::Ground => {}
                    continuation::OwnedValue::Bytes(bytes) => restored_stream.next_slice(bytes),
                }

                let reexport = write_continuation(&restored_stream);
                assert_eq!(cut_bytes, reexport);

                source_stream.next_slice(&corpus[cut..]);
                let mut offset = cut;
                let mut partition: usize = cut.wrapping_add(corpus.len()).wrapping_add(1);
                while offset < corpus.len() {
                    partition = partition.wrapping_mul(1664525).wrapping_add(1013904223);
                    let len = (1 + partition % 7).min(corpus.len() - offset);
                    restored_stream.next_slice(&corpus[offset..offset + len]);
                    offset += len;
                }

                let source_final = write_continuation(&source_stream);
                let restored_final = write_continuation(&restored_stream);
                assert_eq!(source_final, restored_final);

                let final_continuation = continuation_of(&source_final);
                let source_final_snapshot = test_encode(
                    source_stream.handler.terminal,
                    EncodeOptions {
                        continuation: final_continuation,
                    },
                );
                let restored_final_snapshot = test_encode(
                    restored_stream.handler.terminal,
                    EncodeOptions {
                        continuation: final_continuation,
                    },
                );
                assert_eq!(source_final_snapshot, restored_final_snapshot);
                source_stream.deinit();
                restored_stream.deinit();
            }
        }
    }

    #[test]
    fn complete_snapshot_preserves_kitty_virtual_placeholders() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut t = terminal_init(2, 1, false);

        // Register a real virtual placement, then write its grid representation:
        // U+10EEEE followed by row and column diacritics. The image and placement
        // registry is intentionally omitted, but the grid content must remain
        // decodable.
        {
            let TerminalScreen {
                pages, kitty_images, ..
            } = &mut *t.screens.active;
            kitty_images
                .add_image(
                    pages,
                    Image {
                        id: 1,
                        ..Image::default()
                    },
                )
                .unwrap();
            kitty_images
                .add_placement(
                    pages,
                    1,
                    0,
                    Placement {
                        columns: 1,
                        rows: 1,
                        ..Placement::new(PlacementLocation::Virtual)
                    },
                )
                .unwrap();
        }
        t.set_attribute(sgr::Attribute::Fg256(1)).unwrap();
        t.print_string("\u{10EEEE}\u{0305}\u{0305}").unwrap();
        let source_cell = t
            .screens
            .active
            .pages
            .get_cell(Point::Screen(Coordinate::default()))
            .unwrap();
        // SAFETY: the cell and row are live.
        unsafe {
            assert_eq!(graphics_unicode::PLACEHOLDER, (*source_cell.cell).codepoint());
            assert!((*source_cell.row).kitty_virtual_placeholder());
        }

        let encoded = test_encode(&t, TEST_ENCODE_OPTIONS);

        let mut encoded_source = FixedReader::new(&encoded);
        let restored = decode(&mut encoded_source, &TEST_DECODE_OPTIONS).unwrap();
        let restored_terminal = restored.terminal.as_ref().unwrap();

        let restored_cell = restored_terminal
            .screens
            .active
            .pages
            .get_cell(Point::Screen(Coordinate::default()))
            .unwrap();
        // SAFETY: the cell and row are live.
        unsafe {
            assert_eq!(graphics_unicode::PLACEHOLDER, (*restored_cell.cell).codepoint());
            assert!((*restored_cell.cell).has_grapheme());
            assert!((*restored_cell.row).kitty_virtual_placeholder());
        }
        assert_eq!(0, restored_terminal.screens.active.kitty_images.images.len());
        assert_eq!(0, restored_terminal.screens.active.kitty_images.placements.len());
    }

    #[test]
    fn complete_snapshot_encoding_streams_from_the_current_writer_position() {
        let mut t = terminal_init(2, 1, false);

        // Encoding begins with this call's envelope, independent of bytes that
        // were already present in the destination.
        let mut nonempty: Vec<u8> = b"prefix".to_vec();
        let snapshot_offset = nonempty.len();
        encode(&mut nonempty, &t, TEST_ENCODE_OPTIONS).unwrap();
        assert_eq!(&b"prefix"[..], &nonempty[..snapshot_offset]);
        let mut appended_source = FixedReader::new(&nonempty[snapshot_offset..]);
        decode(&mut appended_source, &TEST_DECODE_OPTIONS).unwrap();

        // Payload validation happens in the record-local scratch allocation. The
        // already-streamed envelope remains, but no partial TERMINAL is emitted.
        t.colors.palette.current[7] = Rgb { r: 1, g: 2, b: 3 };
        let mut destination: Vec<u8> = b"prefix".to_vec();
        assert_eq!(
            Err(Error::InvalidPalette),
            encode(&mut destination, &t, TEST_ENCODE_OPTIONS)
        );
        let mut expected_envelope: Vec<u8> = Vec::new();
        envelope::encode(&mut expected_envelope).unwrap();
        assert_eq!(expected_envelope.len(), envelope::ENCODED_LEN);
        assert_eq!(expected_envelope, &destination[b"prefix".len()..]);
    }

    /// Encode records with `f` into a fresh snapshot buffer.
    fn crafted(f: impl FnOnce(&mut record::Writer<'_>)) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut stream = record::Writer::init(&mut out);
            envelope::encode(stream.writer()).unwrap();
            f(&mut stream);
        }
        out
    }

    fn decode_err(bytes: &[u8]) -> Error {
        let mut source = FixedReader::new(bytes);
        match decode(&mut source, &TEST_DECODE_OPTIONS) {
            Ok(_) => panic!("decode unexpectedly succeeded"),
            Err(err) => err,
        }
    }

    #[test]
    fn complete_snapshot_rejects_ordering_and_invalid_markers() {
        let mut t = terminal_init(2, 1, false);
        let primary_ptr = t.screens.get(Key::Primary).unwrap();
        // SAFETY: the primary screen lives as long as `t`.
        let primary: &TerminalScreen = unsafe { &*primary_ptr.as_ptr() };

        // Old version-1 snapshots proceeded directly from SCREEN to READY. READY
        // is individually valid here, but CONTINUATION is now required first.
        let old_order = crafted(|s| {
            terminal::encode(&t, s).unwrap();
            screen::encode(primary, Key::Primary, s).unwrap();
            checkpoint::encode(checkpoint::Kind::Ready, s).unwrap();
        });
        assert_eq!(Error::UnexpectedRecordTag, decode_err(&old_order));

        // A second CONTINUATION is rejected where READY is required.
        let duplicate_continuation = crafted(|s| {
            terminal::encode(&t, s).unwrap();
            screen::encode(primary, Key::Primary, s).unwrap();
            continuation::encode(continuation::Value::Ground, s).unwrap();
            continuation::encode(continuation::Value::Ground, s).unwrap();
        });
        assert_eq!(Error::UnexpectedRecordTag, decode_err(&duplicate_continuation));

        // HISTORY is individually valid here, but the full decoder requires the
        // primary SCREEN before READY.
        let reordered = crafted(|s| {
            terminal::encode(&t, s).unwrap();
            history::encode(primary, Key::Primary, s, None).unwrap();
        });
        assert_eq!(Error::UnexpectedRecordTag, decode_err(&reordered));

        // Construct a correctly framed READY with a nonempty payload so the full
        // driver rejects a marker that does not match the required empty shape.
        let invalid_ready = crafted(|s| {
            terminal::encode(&t, s).unwrap();
            screen::encode(primary, Key::Primary, s).unwrap();
            continuation::encode(continuation::Value::Ground, s).unwrap();
            let ready_payload = s.begin(record::Tag::READY);
            ready_payload.push(0);
            s.finish().unwrap();
        });
        assert_eq!(Error::PayloadNotExhausted, decode_err(&invalid_ready));

        // A SCREEN key must name one of the slots declared by TERMINAL.
        let undeclared = crafted(|s| {
            terminal::encode(&t, s).unwrap();
            screen::encode(primary, Key::Alternate, s).unwrap();
        });
        assert_eq!(Error::UnexpectedScreenKey, decode_err(&undeclared));

        // HISTORY sequences are also routed by key, which must name a declared
        // screen even when the sequence contains no PAGE records.
        let undeclared_history = crafted(|s| {
            terminal::encode(&t, s).unwrap();
            screen::encode(primary, Key::Primary, s).unwrap();
            continuation::encode(continuation::Value::Ground, s).unwrap();
            checkpoint::encode(checkpoint::Kind::Ready, s).unwrap();
            history::encode(primary, Key::Alternate, s, None).unwrap();
        });
        assert_eq!(Error::UnexpectedHistoryKey, decode_err(&undeclared_history));

        // The declared count cannot be satisfied by repeating the same key.
        t.switch_screen(Key::Alternate).unwrap();
        let alternate = screen_ref(&t, Key::Alternate).unwrap();
        let duplicate = crafted(|s| {
            terminal::encode(&t, s).unwrap();
            screen::encode(primary, Key::Primary, s).unwrap();
            screen::encode(primary, Key::Primary, s).unwrap();
        });
        assert_eq!(Error::DuplicateScreen, decode_err(&duplicate));

        // The declared count cannot be satisfied by repeating one HISTORY key.
        let duplicate_history = crafted(|s| {
            terminal::encode(&t, s).unwrap();
            screen::encode(primary, Key::Primary, s).unwrap();
            screen::encode(alternate, Key::Alternate, s).unwrap();
            continuation::encode(continuation::Value::Ground, s).unwrap();
            checkpoint::encode(checkpoint::Kind::Ready, s).unwrap();
            history::encode(primary, Key::Primary, s, None).unwrap();
            history::encode(primary, Key::Primary, s, None).unwrap();
        });
        assert_eq!(Error::DuplicateHistory, decode_err(&duplicate_history));
    }

    #[test]
    fn complete_snapshot_skips_unknown_records_in_every_position() {
        let complete_fixture = test_complete_fixture();

        // Put an unknown record before every record of the complete golden,
        // including before FINISH, so one sits between every pair of records.
        // The tags vary, and so do the payload lengths, including empty.
        let mut with_unknown: Vec<u8> = Vec::new();
        let mut unknown_offsets: Vec<usize> = Vec::new();
        {
            let bytes: &[u8] = &complete_fixture;
            with_unknown.extend_from_slice(&bytes[..envelope::ENCODED_LEN]);
            let mut offset = envelope::ENCODED_LEN;
            let mut i: usize = 0;
            while offset < bytes.len() {
                let len = u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap());
                let end = offset + record::Header::LEN + len as usize;
                unknown_offsets.push(with_unknown.len());
                let payload = [i as u8; 8];
                record::test_write_raw(&mut with_unknown, 0x4000 + i as u16, &payload[..i % payload.len()]).unwrap();
                with_unknown.extend_from_slice(&bytes[offset..end]);
                offset = end;
                i += 1;
            }
        }

        // Both decoders restore exactly the terminal of the golden.
        {
            let mut source = FixedReader::new(&with_unknown);
            let decoded = decode_exact(&mut source, &TEST_DECODE_OPTIONS).unwrap();
            let reencoded = test_encode(decoded.terminal.as_ref().unwrap(), TEST_ENCODE_OPTIONS);
            assert_eq!(complete_fixture, reencoded);
        }
        {
            let mut source = FixedReader::new(&with_unknown);
            let mut limited = Limited::new(&mut source, None, 1);
            let mut decoder = Decoder::init(&mut limited);
            let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
            let restored = decoded.terminal.as_mut().unwrap();
            while decoder.next(restored).unwrap().is_some() {}
            let reencoded = test_encode(restored, TEST_ENCODE_OPTIONS);
            assert_eq!(complete_fixture, reencoded);
        }

        // A corrupted unknown record still fails its CRC, wherever it is. The
        // first payload byte is corrupted, or the tag when the payload is empty.
        for offset in unknown_offsets {
            let mut corrupted = with_unknown.clone();
            let len = u32::from_le_bytes(corrupted[offset + 2..offset + 6].try_into().unwrap());
            corrupted[if len > 0 { offset + record::Header::LEN } else { offset }] ^= 0x01;
            assert_eq!(Error::InvalidChecksum, decode_err(&corrupted));
        }
    }

    #[test]
    fn complete_snapshot_leaves_continuation_bytes_unread() {
        let t = terminal_init(2, 1, false);

        let mut encoded = test_encode(&t, TEST_ENCODE_OPTIONS);
        let snapshot_len = encoded.len();
        encoded.extend_from_slice(b"pty");

        let mut source = FixedReader::new(&encoded);
        let _restored = decode(&mut source, &TEST_DECODE_OPTIONS).unwrap();

        let mut trailing = [0u8; 3];
        source.read_slice_all(&mut trailing).unwrap();
        assert_eq!(b"pty", &trailing);

        let mut exact_source = FixedReader::new(&encoded);
        assert_eq!(
            Err(Error::TrailingData),
            decode_exact(&mut exact_source, &TEST_DECODE_OPTIONS).map(|_| ())
        );

        let mut bounded_source = FixedReader::new(&encoded[..snapshot_len]);
        decode_exact(&mut bounded_source, &TEST_DECODE_OPTIONS).unwrap();
    }

    #[test]
    fn complete_snapshots_decode_sequentially_from_one_reader() {
        let t = terminal_init(2, 1, false);

        let mut encoded = test_encode(&t, TEST_ENCODE_OPTIONS);
        encode(&mut encoded, &t, TEST_ENCODE_OPTIONS).unwrap();

        let mut source = FixedReader::new(&encoded);
        let _first = decode(&mut source, &TEST_DECODE_OPTIONS).unwrap();
        let _second = decode(&mut source, &TEST_DECODE_OPTIONS).unwrap();
        assert_eq!(Err(Error::EndOfStream), source.take_byte());
    }

    #[test]
    fn incremental_decode_restores_a_renderable_terminal_at_ready() {
        // Trailing transport bytes must remain unread after FINISH.
        let complete_fixture = test_complete_fixture();
        let mut transport = complete_fixture.clone();
        transport.extend_from_slice(b"pty");

        let mut source = FixedReader::new(&transport);
        let mut decoder = Decoder::init(&mut source);
        let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();

        // READY restored the complete interactive state, so the Terminal can
        // move to its final address before any history has been decoded.
        let mut restored = decoded.to_owned();
        assert_eq!(Key::Alternate, restored.screens.active_key);
        assert_eq!(&b"complete snapshot"[..], restored.get_title().unwrap());

        // The advisory extents size scrollback UI immediately even though only
        // the resident SCREEN pages exist. The alternate screen's extent is
        // entirely resident overlap, so it must match its live row count.
        let mut primary = restored.screens.get(Key::Primary).unwrap();
        let mut alternate = restored.screens.get(Key::Alternate).unwrap();
        assert_eq!(3, primary.pages.scrollbar().total);
        assert_eq!(Some(4), decoded.history_rows[Key::Primary as usize]);
        assert_eq!(
            Some((alternate.pages.scrollbar().total - restored.rows as usize) as u64),
            decoded.history_rows[Key::Alternate as usize]
        );

        // Each call prepends exactly one history page, newest first.
        let first = decoder.next(&mut restored).unwrap().unwrap();
        assert_eq!(
            Progress::Page(ProgressPage {
                key: Key::Primary,
                rows: 2,
                remaining: 1,
            }),
            first
        );
        assert_eq!(5, primary.pages.scrollbar().total);
        assert_eq!('B' as u32, test_top_left_codepoint(&primary));

        let second = decoder.next(&mut restored).unwrap().unwrap();
        assert_eq!(
            Progress::Page(ProgressPage {
                key: Key::Primary,
                rows: 2,
                remaining: 0,
            }),
            second
        );
        assert_eq!(7, primary.pages.scrollbar().total);
        assert_eq!('A' as u32, test_top_left_codepoint(&primary));

        // The alternate screen's empty sequence produces no event: the next
        // call validates FINISH, and further calls remain null.
        assert!(decoder.next(&mut restored).unwrap().is_none());
        assert!(decoder.next(&mut restored).unwrap().is_none());
        drop(decoder);
        assert_eq!(b"pty".to_vec(), source.take(3).unwrap());

        // The incremental path restores the same complete terminal as the
        // one-shot decoder, byte for byte.
        let reencoded = test_encode(&restored, TEST_ENCODE_OPTIONS);
        assert_eq!(complete_fixture, reencoded);
    }

    #[test]
    fn incremental_next_reports_invalid_decoder_states() {
        let mut source = FixedReader::new(b"");
        let mut decoder = Decoder::init(&mut source);
        let mut terminal_value = terminal_init(1, 1, false);

        assert_eq!(Err(Error::DecoderNotReady), decoder.next(&mut terminal_value));
    }

    #[test]
    fn incremental_decode_applies_history_below_live_pty_output() {
        let t = test_history_terminal(2);
        let encoded = test_encode(&t, TEST_ENCODE_OPTIONS);

        let mut source = FixedReader::new(&encoded);
        let mut decoder = Decoder::init(&mut source);
        let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
        let mut restored = decoded.to_owned();

        // The terminal is interactive at READY: post-cut PTY bytes append to
        // the bottom while snapshot history is still pending.
        {
            let mut live = restored.vt_stream();
            live.next_slice(b"1\r\n2\r\n3\r\n4\r\n5\r\n");
            live.deinit();
        }
        let mut primary = restored.screens.get(Key::Primary).unwrap();
        let live_total = primary.pages.scrollbar().total;
        assert!(live_total > 3);

        // History still lands above both the snapshot content and the live
        // output, preserving complete scrollback ordering.
        while let Some(progress) = decoder.next(&mut restored).unwrap() {
            assert_eq!(Key::Primary, progress.page().key);
            assert_eq!(2, progress.page().rows);
        }
        assert_eq!(live_total + 4, primary.pages.scrollbar().total);
        assert_eq!('A' as u32, test_top_left_codepoint(&primary));
        let newest_history = nd(primary.pages.get_top_left(Tag::Screen).node).next.unwrap();
        // SAFETY: (0, 0) is within the page.
        unsafe {
            assert_eq!(
                'B' as u32,
                (*nd(newest_history).page().get_row_and_cell(0, 0).cell).codepoint()
            );

            // The page below the applied history is the snapshot's active page,
            // whose top-left cell the live output already overwrote.
            assert_eq!(
                '1' as u32,
                (*nd(nd(newest_history).next.unwrap()).page().get_row_and_cell(0, 0).cell).codepoint()
            );
        }
    }

    #[test]
    fn incremental_decode_discards_history_for_screens_changed_since_ready() {
        let mut t = terminal_init(2, 3, false);
        t.switch_screen(Key::Alternate).unwrap();
        t.print_string("alt").unwrap();
        t.switch_screen(Key::Primary).unwrap();

        // The encoder never produces alternate-screen history, but the format
        // permits it, so the decoder must stay stream-aligned even when the
        // pages can no longer be applied. Order independence lets the discarded
        // sequence come first.
        let primary = screen_ref(&t, Key::Primary).unwrap();
        let mut alternate = t.screens.get(Key::Alternate).unwrap();
        let crafted_bytes = crafted(|s| {
            terminal::encode(&t, s).unwrap();
            screen::encode(primary, Key::Primary, s).unwrap();
            screen::encode(&alternate, Key::Alternate, s).unwrap();
            continuation::encode(continuation::Value::Ground, s).unwrap();
            checkpoint::encode(checkpoint::Kind::Ready, s).unwrap();
            {
                let payload = s.begin(record::Tag::HISTORY);
                history::Header {
                    key: Key::Alternate,
                    page_count: 1,
                }
                .encode(payload)
                .unwrap();
                s.finish().unwrap();
            }
            let node = alternate.pages.get_top_left(Tag::Screen).node;
            page::encode(nd(node).page_assume_resident(), s).unwrap();
            history::encode(primary, Key::Primary, s, None).unwrap();
            checkpoint::encode(checkpoint::Kind::Finish, s).unwrap();
        });
        let _ = &mut alternate;

        // An unchanged declared screen accepts crafted history, establishing
        // the baseline that the following runs degrade from.
        {
            let mut source = FixedReader::new(&crafted_bytes);
            let mut decoder = Decoder::init(&mut source);
            let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
            let restored = decoded.terminal.as_mut().unwrap();

            let progress = decoder.next(restored).unwrap().unwrap();
            assert_eq!(Key::Alternate, progress.page().key);
            assert!(progress.page().rows > 0);
            assert!(decoder.next(restored).unwrap().is_none());
            assert_eq!(2, restored.screens.get(Key::Alternate).unwrap().pages.total_pages());
        }

        // A screen removed since READY discards its history. FINISH still
        // validates, proving discarded records leave the stream aligned.
        {
            let mut source = FixedReader::new(&crafted_bytes);
            let mut decoder = Decoder::init(&mut source);
            let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
            let restored = decoded.terminal.as_mut().unwrap();
            restored.screens.remove(Key::Alternate);

            let progress = decoder.next(restored).unwrap().unwrap();
            assert_eq!(
                Progress::Page(ProgressPage {
                    key: Key::Alternate,
                    rows: 0,
                    remaining: 0,
                }),
                progress
            );
            assert!(decoder.next(restored).unwrap().is_none());
        }

        // A screen removed and recreated since READY has a new generation, so
        // stale history never mixes into its replacement's content.
        {
            let mut source = FixedReader::new(&crafted_bytes);
            let mut decoder = Decoder::init(&mut source);
            let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
            let restored = decoded.terminal.as_mut().unwrap();
            restored.screens.remove(Key::Alternate);
            restored.switch_screen(Key::Alternate).unwrap();

            let progress = decoder.next(restored).unwrap().unwrap();
            assert_eq!(Key::Alternate, progress.page().key);
            assert_eq!(0, progress.page().rows);
            assert!(decoder.next(restored).unwrap().is_none());
            assert_eq!(1, restored.screens.get(Key::Alternate).unwrap().pages.total_pages());
        }
    }

    #[test]
    fn incremental_decode_discards_history_after_a_resize() {
        let t = test_history_terminal(2);
        let encoded = test_encode(&t, TEST_ENCODE_OPTIONS);

        let mut source = FixedReader::new(&encoded);
        let mut decoder = Decoder::init(&mut source);
        let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
        let mut restored = decoded.to_owned();

        // Encoded pages assume the READY width. After a live resize reflows the
        // restored screen, pending history is consumed but never applied.
        restored.resize(Resize::new(3, 3)).unwrap();
        let mut primary = restored.screens.get(Key::Primary).unwrap();
        let resized_total = primary.pages.scrollbar().total;

        let mut pages: usize = 0;
        while let Some(progress) = decoder.next(&mut restored).unwrap() {
            assert_eq!(Key::Primary, progress.page().key);
            assert_eq!(0, progress.page().rows);
            pages += 1;
        }
        assert_eq!(2, pages);
        assert_eq!(resized_total, primary.pages.scrollbar().total);
    }

    #[test]
    fn decode_drops_history_beyond_the_declared_scrollback_limits() {
        // Declare a byte limit the complete history cannot satisfy. The limit
        // is validated at page granularity on decode, so the snapshot itself
        // remains well formed and fully validated.
        let t = test_history_terminal(3);
        t.screens
            .get(Key::Primary)
            .unwrap()
            .pages
            .limits
            .set(LimitKey::Bytes, Some(1));
        let encoded = test_encode(&t, TEST_ENCODE_OPTIONS);

        // Incremental: the newest page fits the effective floor, the next page
        // exceeds it, and one drop discards the remainder of the sequence so no
        // ordering gap can form.
        {
            let mut source = FixedReader::new(&encoded);
            let mut decoder = Decoder::init(&mut source);
            let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
            let restored = decoded.terminal.as_mut().unwrap();

            let mut rows: Vec<usize> = Vec::new();
            while let Some(progress) = decoder.next(restored).unwrap() {
                rows.push(progress.page().rows);
            }
            assert_eq!(3, rows.len());
            assert_eq!(vec![2, 0, 0], rows);

            let primary = restored.screens.get(Key::Primary).unwrap();
            assert_eq!(2, primary.pages.total_pages());
            assert_eq!('C' as u32, test_top_left_codepoint(&primary));
        }

        // The one-shot decoder degrades identically instead of rejecting the
        // snapshot.
        {
            let mut source = FixedReader::new(&encoded);
            let decoded = decode(&mut source, &TEST_DECODE_OPTIONS).unwrap();
            let primary = decoded.terminal.as_ref().unwrap().screens.get(Key::Primary).unwrap();
            assert_eq!(2, primary.pages.total_pages());
            assert_eq!('C' as u32, test_top_left_codepoint(&primary));
        }
    }

    #[test]
    fn incremental_decode_failure_leaves_applied_history_usable() {
        let t = test_history_terminal(2);
        let encoded = test_encode(&t, TEST_ENCODE_OPTIONS);

        // Truncate FINISH. READY and both history pages decode, then the
        // failure surfaces on the call that would have completed the snapshot.
        let mut source = FixedReader::new(&encoded[..encoded.len() - 1]);
        let mut decoder = Decoder::init(&mut source);
        let mut decoded = decoder.ready(&TEST_DECODE_OPTIONS).unwrap();
        let mut restored = decoded.to_owned();

        for _ in 0..2 {
            let progress = decoder.next(&mut restored).unwrap().unwrap();
            assert_eq!(2, progress.page().rows);
        }
        assert_eq!(Err(Error::EndOfStream), decoder.next(&mut restored));
        assert_eq!(Err(Error::DecoderFailed), decoder.next(&mut restored));

        // The transferred terminal was never owned by the decoder: it remains
        // valid with the contiguous history prefix that did apply; only the
        // incomplete snapshot stream is lost.
        let primary = restored.screens.get(Key::Primary).unwrap();
        assert_eq!(3, primary.pages.total_pages());
        assert_eq!('A' as u32, test_top_left_codepoint(&primary));
        primary.pages.assert_integrity();
        primary.assert_integrity();
    }
}
