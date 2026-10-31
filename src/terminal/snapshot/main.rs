//! Port of `src/terminal/snapshot/main.zig`.
//!
//! STATUS: ported
//!
//! Terminal snapshot binary representation and codecs.
//!
//! This is NOT a full transport-ready format to implement generic replay
//! software such as multiplexers, recorders (e.g. asciinema), etc. The goal
//! of this package is to provide a documented, binary-compatible representation
//! for a terminal state.
//!
//! We call this a "snapshot." The snapshot is purposely laid out in a way
//! that prioritizes making a terminal functional as quickly as possible.
//! To do that, it sends the active terminal state followed by a READY record,
//! then complete history.
//!
//! READY denotes that enough state has been sent down to render the
//! terminal and reconstruct its unfinished VT Stream state.
//!
//! After READY, we send history pages (scrollback). Finally, the
//! snapshot ends with an empty FINISH marker.
//!
//! ## Snapshot Format
//!
//! This documents snapshot format 1. Version 1 is the work-in-progress
//! format that we intended to continue to break until we can promise
//! binary compatibility.
//!
//! A snapshot is one envelope followed by a sequence of records. The envelope
//! occurs once at byte zero. Every record is independently framed as a fixed
//! header followed by the number of payload bytes declared by that header.
//!
//! ```text
//! +------------------+
//! | Envelope         |
//! +------------------+
//! | Record 1 header  |
//! +------------------+
//! | Record 1 payload |
//! +------------------+
//! | Record 2 header  |
//! +------------------+
//! | Record 2 payload |
//! +------------------+
//! | ...              |
//! +------------------+
//! ```
//!
//! Record groups have a strict order:
//!
//! ```text
//! +----------------------------------------+
//! | TERMINAL                               |
//! +----------------------------------------+
//! | SCREEN * terminal.screen_count         |
//! | PAGE * each screen.page_count          |
//! | KITTY_IMAGE_STORAGE (optional, per     |
//! |   screen)                              |
//! +----------------------------------------+
//! | CONTINUATION                           |
//! +----------------------------------------+
//! | READY                                  |
//! +----------------------------------------+
//! | KITTY_IMAGE_PAYLOAD (optional, per     |
//! |   image or frame, anywhere until       |
//! |   FINISH)                              |
//! +----------------------------------------+
//! | HISTORY * terminal.screen_count        |
//! | PAGE * each history.page_count         |
//! | KITTY_IMAGE_PAGE_PLACEMENTS (optional, |
//! |   after a PAGE with placements on it)  |
//! +----------------------------------------+
//! | FINISH                                 |
//! +----------------------------------------+
//! ```
//!
//! The optional Kitty graphics records carry each screen's images, pixels
//! and placements. They are only written for screens that have images.
//! The pixels of the images on screen come right after READY, the pixels
//! of images in scrollback come right after their history page, and the
//! rest come after all history. See `kitty.rs`.
//!
//! A decoder skips any record whose tag it does not know, wherever it
//! appears, after checking its length and CRC32C. This lets newer encoders
//! add optional kinds of records that older decoders ignore, without
//! changing the snapshot version. The version only changes when the layout
//! of an existing record changes, or when a new kind of record is needed for
//! the terminal to be correct. See "Unknown Records" in `record.rs`.
//!
//! The SCREEN and HISTORY sequence groups may each appear in any key order.
//! Each key must occur exactly once in each group and must identify one of the
//! screens declared by TERMINAL. SCREEN contains the complete pages needed to
//! restore each active area. HISTORY contains the older complete pages for its
//! screen in newest-to-oldest order so they can be prepended as they arrive.
//! Every SCREEN has one corresponding HISTORY, even when its history page count
//! is zero. FINISH terminates the snapshot. Bytes after FINISH belong to the
//! containing transport and are not consumed by snapshot decoding.
//!
//! CONTINUATION contains the bytes required to bring the terminal's
//! VT parser/stream up to the same state, or no bytes if it should be
//! in the ground state.
//!
//! READY and FINISH are empty marker records. READY separates the renderable
//! active state and continuation from history; FINISH terminates the complete
//! snapshot. Every record, including both markers, is independently protected
//! by CRC32C. Each SCREEN declares its complete logical history extent, allowing
//! a client to size its scrollbar at READY even though older PAGE records arrive
//! afterward.
//!
//! ## Encoding
//!
//! Encode a complete snapshot into any writer:
//!
//! ```ignore
//! let mut output: Vec<u8> = Vec::new();
//! snapshot::encode(&mut output, &terminal, snapshot::EncodeOptions {
//!     continuation: snapshot::Continuation::Ground,
//! })?;
//!
//! let bytes = &output[..];
//! ```
//!
//! Encoding begins at the writer's current position, so unrelated bytes may
//! precede the snapshot. The encoder buffers only the current record payload
//! to calculate its length and CRC32C; completed records stream immediately.
//! Buffering is an encoder implementation detail, not a requirement of the
//! wire format.
//!
//! A failure may leave prior complete records, or a partial record if the
//! destination itself fails. Such a prefix has no FINISH marker and
//! cannot be restored as a complete snapshot.
//!
//! Each record type usually exposes an `encode` function that encodes
//! a complete record, such as `screen::encode`.
//!
//! ## Decoding
//!
//! `snapshot::decode` consumes exactly one snapshot through FINISH and leaves
//! any following bytes unread. This permits multiple snapshots or live protocol
//! data to share a stream without waiting for the peer to close it. Transports
//! that deliver live PTY data before history finishes must multiplex that data
//! outside this ordered snapshot record sequence.
//!
//! ```ignore
//! let mut decoded = snapshot::decode(&mut reader, &snapshot::DecodeOptions {
//!     max_continuation_bytes: 1024 * 1024,
//!     ..Default::default()
//! })?;
//! let terminal = decoded.to_owned();
//! ```
//!
//! Use `snapshot::decode_exact` for a bounded file or buffer that must contain
//! only one snapshot. It preserves the stricter end-of-file check, which may
//! block when used with a live stream.
//!
//! `snapshot::Decoder` decodes the same stream incrementally so the terminal
//! becomes usable at READY, before history has arrived. Each `next` call
//! applies one history page, or the pixels of one image, to the by-then
//! live terminal. A null result means FINISH was validated:
//!
//! ```ignore
//! let mut decoder = snapshot::Decoder::init(&mut reader);
//! let mut decoded = decoder.ready(&snapshot::DecodeOptions {
//!     max_continuation_bytes: 1024 * 1024,
//!     ..Default::default()
//! })?;
//!
//! let mut terminal = decoded.to_owned();
//!
//! // Render, replay the continuation, process input...
//! while let Some(progress) = decoder.next(&mut terminal)? {
//!     match progress {
//!         snapshot::Progress::Image(_) => {} // An image's pixels arrived.
//!         snapshot::Progress::Page(_) => {}  // Scrollback grew.
//!     }
//! }
//! ```
//!
//! Kitty graphics images are restored at READY with their pixels pending.
//! `next` then delivers the pixels, which may be mixed in with history.
//!
//! ### Compressed History
//!
//! Decoded history is uncompressed by default, even if the encoded terminal
//! had compressed it. Set `compress_history` to compress each history page
//! as soon as it is restored. The restored terminal is then compressed from
//! the start, and the decode never holds the complete history uncompressed
//! in memory:
//!
//! ```ignore
//! let decoded = snapshot::decode(&mut reader, &snapshot::DecodeOptions {
//!     max_continuation_bytes: 1024 * 1024,
//!     compress_history: true,
//!     ..Default::default()
//! })?;
//! ```
//!
//! This works the same way with `snapshot::Decoder`. The snapshot format does
//! not change, so any snapshot can be decoded with or without it.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "checkpoint.rs"]
pub mod checkpoint;
#[path = "continuation.rs"]
pub mod continuation;
#[path = "envelope.rs"]
pub mod envelope;
#[path = "fixture.rs"]
pub mod fixture;
#[path = "grid.rs"]
pub mod grid;
#[path = "history.rs"]
pub mod history;
#[path = "hyperlink.rs"]
pub mod hyperlink;
#[path = "io.rs"]
pub mod io;
#[path = "kitty.rs"]
pub mod kitty;
#[path = "page.rs"]
pub mod page;
#[path = "record.rs"]
pub mod record;
#[path = "screen.rs"]
pub mod screen;
#[path = "snapshot.rs"]
pub mod snapshot;
#[path = "style.rs"]
pub mod style;
#[path = "terminal.rs"]
pub mod terminal;
// @generated-mods-end

pub use self::snapshot::{
    decode, decode_exact, encode, Continuation, DecodeError, DecodeExactError, DecodeOptions, Decoded, Decoder,
    EncodeError, EncodeOptions, Progress, ProgressImage, ProgressPage,
};
