//! Port of `src/terminal/c/snapshot.zig`.
//!
//! STATUS: ported
//!
//! C ABI wrappers for snapshot encoding and incremental restoration.
//!
//! Notes on the port:
//! - The core `snapshot::Decoder<'a>` borrows its source reader, which the
//!   wrapper owns (Zig: an inline source with a stable address inside the
//!   heap wrapper). The Rust wrapper keeps the source in its own heap
//!   allocation (`Box::into_raw`) and the decoder with its lifetime erased
//!   to `'static`; `Drop` releases the decoder before the source. The
//!   source offset and callback flags are read through the raw pointer.
//! - Enum arguments (`DecoderOption`, `DecoderData`) cross the ABI as
//!   `c_int`; unknown values return `invalid_value` (Zig: non-exhaustive
//!   enums with `_` arms). `bool` option values are read as bytes. A null
//!   `out` pointer for `decoder_get` returns `invalid_value`.
//! - `encode_alloc` encodes into a Rust `Vec` and copies the result into
//!   a buffer from the caller's allocator (Zig: an allocating writer on
//!   that allocator); an allocation failure maps to `out_of_memory` either
//!   way.

use std::ffi::{c_int, c_void};
use std::mem::ManuallyDrop;

use super::io as io_c;
use super::result::Result;
use super::terminal::{self as terminal_c, TerminalScreen};
use super::types::HandleAllocator;
use crate::lib::allocator::{self as lib_alloc, Allocator as CAllocator, ZigAllocator};
use crate::terminal::apc;
use crate::terminal::screen::Options as TerminalScreenOptions;
use crate::terminal::screen_set::Key as ScreenKey;
use crate::terminal::snapshot as snapshot_core;
use crate::terminal::snapshot::io as sio;

/// Snapshot readers accept the largest default APC payload. Deriving this from
/// the APC protocols keeps snapshot validation in sync as protocols change.
/// Opt-in decoded-terminal retention reuses this configured validation bound
/// as its runtime tracking limit.
fn default_max_continuation_bytes() -> usize {
    apc::Protocol::max_default_bytes()
}

crate::c_api_enum! {
    /// C: GhosttySnapshotDecoderOption.
    ///
    /// Options configure validation and returned-terminal policy before snapshot
    /// decoding begins. Value types: `max_continuation_bytes` → `usize`,
    /// `retain_continuation`/`compress_history` → `bool`,
    /// `kitty_image_storage_limit` → `u64`.
    pub enum DecoderOption {
        MaxContinuationBytes = 0,
        RetainContinuation = 1,
        CompressHistory = 2,
        KittyImageStorageLimit = 3,
    }
}

crate::c_api_enum! {
    /// C: GhosttySnapshotDecoderProgressKind.
    ///
    /// What the most recent successful `decoder_next` call decoded.
    pub enum DecoderProgressKind {
        Page = 0,
        Image = 1,
    }
}

crate::c_api_enum! {
    /// C: GhosttySnapshotDecoderData.
    ///
    /// Data keys select typed values exposed by the decoder getter APIs.
    /// Output types: `max_continuation_bytes`/`source_offset`/
    /// `progress_rows`/`progress_image_bytes` → `usize`,
    /// `history_rows_*`/`kitty_image_storage_limit`/`kitty_pending_bytes_*`
    /// → `u64`, `progress_screen` → `GhosttyTerminalScreen`,
    /// `progress_remaining`/`progress_image_id`/`progress_image_frame`/
    /// `kitty_images_*` → `u32`, `retain_continuation`/`compress_history`
    /// → `bool`, `progress_kind` → [`DecoderProgressKind`].
    pub enum DecoderData {
        Invalid = 0,
        MaxContinuationBytes = 1,
        SourceOffset = 2,
        HistoryRowsPrimary = 3,
        HistoryRowsAlternate = 4,
        ProgressScreen = 5,
        ProgressRows = 6,
        ProgressRemaining = 7,
        RetainContinuation = 8,
        CompressHistory = 9,
        KittyImageStorageLimit = 10,
        ProgressKind = 11,
        ProgressImageId = 12,
        ProgressImageFrame = 13,
        ProgressImageBytes = 14,
        KittyImagesPrimary = 15,
        KittyImagesAlternate = 16,
        KittyPendingBytesPrimary = 17,
        KittyPendingBytesAlternate = 18,
    }
}

/// A callback or borrowed-buffer source with uniform reader access.
/// (Zig: `DecoderWrapper.Source`.)
pub enum Source {
    Callback(io_c::ReaderAdapter),
    /// Borrowed caller bytes (lifetime erased; the C contract keeps them
    /// alive until the decoder is freed).
    Buffer(sio::FixedReader<'static>),
}

impl Source {
    /// Return the number of source bytes consumed without reading ahead.
    fn offset(&self) -> usize {
        match self {
            Source::Callback(value) => value.offset,
            Source::Buffer(value) => value.seek,
        }
    }
}

impl sio::Reader for Source {
    fn buffered(&self) -> &[u8] {
        match self {
            Source::Callback(value) => value.buffered(),
            Source::Buffer(value) => value.buffered(),
        }
    }

    fn toss(&mut self, n: usize) {
        match self {
            Source::Callback(value) => value.toss(n),
            Source::Buffer(value) => value.toss(n),
        }
    }

    fn fill_more(&mut self) -> sio::Result<()> {
        match self {
            Source::Callback(value) => value.fill_more(),
            Source::Buffer(value) => value.fill_more(),
        }
    }

    fn read_direct(&mut self, dest: &mut [u8]) -> sio::Result<usize> {
        match self {
            Source::Callback(value) => value.read_direct(dest),
            Source::Buffer(value) => value.read_direct(dest),
        }
    }
}

/// READY metadata that remains queryable through FINISH or a later error.
#[derive(Debug, Clone, Copy)]
pub struct Metadata {
    /// Indexed by screen key.
    history_rows: [Option<u64>; ScreenKey::COUNT],
    kitty: [snapshot_core::kitty::Counts; ScreenKey::COUNT],
}

/// Decoder lifecycle and the data available in each phase.
#[derive(Debug, Clone, Copy)]
enum State {
    Configuring,

    History {
        terminal: terminal_c::Terminal,
        metadata: Metadata,
        progress: Option<snapshot_core::Progress>,
    },

    Finished(Metadata),

    /// READY failures have no metadata. Failures from `next` retain the
    /// READY metadata even though decoding cannot continue.
    Failed(Option<Metadata>),
}

/// Owns the source adapter and all state needed by an opaque C decoder handle.
pub struct DecoderWrapper {
    alloc: HandleAllocator,
    /// Borrows `*source`; dropped before it (see `Drop`).
    decoder: ManuallyDrop<snapshot_core::Decoder<'static>>,
    /// A heap-stable source (`Box::into_raw`).
    source: *mut Source,
    state: State,
    max_continuation_bytes: usize,
    retain_continuation: bool,
    compress_history: bool,

    /// The Kitty graphics image storage limit for restored screens, or null
    /// for the default limit of a new screen.
    kitty_image_storage_limit: Option<u64>,
}

impl DecoderWrapper {
    fn source(&self) -> &Source {
        // SAFETY: the source lives until the wrapper is dropped.
        unsafe { &*self.source }
    }
}

impl Drop for DecoderWrapper {
    fn drop(&mut self) {
        // SAFETY: the decoder (which borrows the source) is dropped before
        // the source, exactly once.
        unsafe {
            ManuallyDrop::drop(&mut self.decoder);
            drop(Box::from_raw(self.source));
        }
    }
}

/// C: GhosttySnapshotDecoder, an opaque nullable decoder handle.
pub type Decoder = *mut DecoderWrapper;

/// Create a decoder backed by a synchronous C reader callback.
///
/// # Safety
/// `out_` must be NULL or valid; `reader` must follow the
/// `GhosttyReader` contract.
#[export_name = "ghostty_snapshot_decoder_new"]
pub unsafe extern "C" fn decoder_new(alloc_: Option<&CAllocator>, out_: *mut Decoder, reader: io_c::Reader) -> Result {
    // Validate C-owned output and callback pointers before allocating anything.
    let Some(out) = out_.as_mut() else {
        return Result::InvalidValue;
    };
    *out = std::ptr::null_mut();
    if reader.read.is_none() {
        return Result::InvalidValue;
    }

    decoder_new_source(alloc_, out, Source::Callback(io_c::ReaderAdapter::init(reader)))
}

/// Create a decoder backed by immutable, caller-owned bytes.
///
/// # Safety
/// `out_` must be NULL or valid; a non-null `ptr` must be valid for `len`
/// reads until the decoder is freed.
#[export_name = "ghostty_snapshot_decoder_new_buf"]
pub unsafe extern "C" fn decoder_new_buf(
    alloc_: Option<&CAllocator>,
    out_: *mut Decoder,
    ptr: *const u8,
    len: usize,
) -> Result {
    // Normalize the C pointer and length into a borrowed slice.
    let Some(out) = out_.as_mut() else {
        return Result::InvalidValue;
    };
    *out = std::ptr::null_mut();
    let bytes: &'static [u8] = if !ptr.is_null() {
        std::slice::from_raw_parts(ptr, len)
    } else if len == 0 {
        &[]
    } else {
        return Result::InvalidValue;
    };

    // The fixed reader records its current seek position as the source offset.
    decoder_new_source(alloc_, out, Source::Buffer(sio::FixedReader::new(bytes)))
}

/// Allocate and initialize a decoder around either supported source type.
fn decoder_new_source(alloc_: Option<&CAllocator>, out: &mut Decoder, source: Source) -> Result {
    // Box the source first so it and its reader state never move again.
    let source = Box::into_raw(Box::new(source));
    // SAFETY: the source is heap-stable and outlives the decoder (Drop).
    let reader: &'static mut dyn sio::Reader = unsafe { &mut *source };

    let alloc = HandleAllocator::new(alloc_);
    let wrapper = alloc.create(DecoderWrapper {
        alloc,
        decoder: ManuallyDrop::new(snapshot_core::Decoder::init(reader)),
        source,
        state: State::Configuring,
        max_continuation_bytes: default_max_continuation_bytes(),
        retain_continuation: false,
        compress_history: false,
        kitty_image_storage_limit: None,
    });
    if wrapper.is_null() {
        // `create` dropped the unwritten wrapper value, whose `Drop`
        // released the source.
        return Result::OutOfMemory;
    }
    *out = wrapper;
    Result::Success
}

/// Destroy a decoder without taking ownership of any returned terminal.
///
/// # Safety
/// `decoder_` must be NULL or a decoder not used afterwards.
#[export_name = "ghostty_snapshot_decoder_free"]
pub unsafe extern "C" fn decoder_free(decoder_: Decoder) {
    let Some(decoder) = decoder_.as_ref() else {
        return;
    };
    let alloc = decoder.alloc;
    alloc.destroy(decoder_);
}

/// Set a decoder option while the decoder is still configuring.
///
/// # Safety
/// `decoder_` must be NULL or live; `value_` must be NULL or point to the
/// option's value type.
#[export_name = "ghostty_snapshot_decoder_set"]
pub unsafe extern "C" fn decoder_set(decoder_: Decoder, option: c_int, value_: *const c_void) -> Result {
    // Options freeze as soon as any decoding attempt starts.
    let Some(decoder) = decoder_.as_mut() else {
        return Result::InvalidValue;
    };
    match decoder.state {
        State::Configuring => {}
        _ => return Result::InvalidValue,
    }

    // Interpret the erased C pointer according to the selected option.
    if value_.is_null() {
        return Result::InvalidValue;
    }
    match DecoderOption::from_int(option) {
        Some(DecoderOption::MaxContinuationBytes) => decoder.max_continuation_bytes = *(value_ as *const usize),
        Some(DecoderOption::RetainContinuation) => decoder.retain_continuation = *(value_ as *const u8) != 0,
        Some(DecoderOption::CompressHistory) => decoder.compress_history = *(value_ as *const u8) != 0,
        Some(DecoderOption::KittyImageStorageLimit) => {
            decoder.kitty_image_storage_limit = Some(*(value_ as *const u64))
        }
        None => return Result::InvalidValue,
    }
    Result::Success
}

/// Read one typed decoder value through the type-erased C API.
///
/// # Safety
/// `decoder_` must be NULL or live; `out` must be NULL or point to the
/// data key's output type.
#[export_name = "ghostty_snapshot_decoder_get"]
pub unsafe extern "C" fn decoder_get(decoder_: Decoder, data: c_int, out: *mut c_void) -> Result {
    // Enumerate known keys so each branch recovers the correct output type.
    let Some(data) = DecoderData::from_int(data) else {
        return Result::InvalidValue;
    };
    decoder_get_typed(decoder_, data, out)
}

/// Implement a getter after the key has selected its concrete output type.
unsafe fn decoder_get_typed(decoder_: Decoder, data: DecoderData, out: *mut c_void) -> Result {
    let Some(decoder) = decoder_.as_ref() else {
        return Result::InvalidValue;
    };
    if out.is_null() {
        return Result::InvalidValue;
    }

    macro_rules! write {
        ($ty:ty, $v:expr) => {
            *(out as *mut $ty) = $v
        };
    }

    match data {
        // Decoder-wide configuration and source state.
        DecoderData::Invalid => return Result::InvalidValue,
        DecoderData::MaxContinuationBytes => {
            if decoder_failed(decoder) {
                return Result::NoValue;
            }
            write!(usize, decoder.max_continuation_bytes);
        }
        DecoderData::SourceOffset => {
            if decoder_failed(decoder) {
                return Result::NoValue;
            }
            write!(usize, decoder.source().offset());
        }
        DecoderData::RetainContinuation => {
            if decoder_failed(decoder) {
                return Result::NoValue;
            }
            write!(bool, decoder.retain_continuation);
        }
        DecoderData::CompressHistory => {
            if decoder_failed(decoder) {
                return Result::NoValue;
            }
            write!(bool, decoder.compress_history);
        }
        DecoderData::KittyImageStorageLimit => {
            if decoder_failed(decoder) {
                return Result::NoValue;
            }
            write!(
                u64,
                decoder
                    .kitty_image_storage_limit
                    .unwrap_or_else(default_kitty_image_storage_limit)
            );
        }

        // Kitty graphics totals at READY, for screens the snapshot declares.
        DecoderData::KittyImagesPrimary
        | DecoderData::KittyImagesAlternate
        | DecoderData::KittyPendingBytesPrimary
        | DecoderData::KittyPendingBytesAlternate => {
            let Some(metadata) = decoder_metadata(decoder) else {
                return Result::NoValue;
            };
            let key = match data {
                DecoderData::KittyImagesPrimary | DecoderData::KittyPendingBytesPrimary => TerminalScreen::Primary,
                _ => TerminalScreen::Alternate,
            }
            .to_zig();
            if metadata.history_rows[key as usize].is_none() {
                return Result::NoValue;
            }
            let counts = metadata.kitty[key as usize];
            match data {
                DecoderData::KittyImagesPrimary | DecoderData::KittyImagesAlternate => write!(u32, counts.images),
                _ => write!(u64, counts.pending_bytes),
            }
        }

        // READY metadata remains available after history decoding completes.
        DecoderData::HistoryRowsPrimary => {
            let Some(metadata) = decoder_metadata(decoder) else {
                return Result::NoValue;
            };
            let Some(rows) = metadata.history_rows[ScreenKey::Primary as usize] else {
                return Result::NoValue;
            };
            write!(u64, rows);
        }
        DecoderData::HistoryRowsAlternate => {
            let Some(metadata) = decoder_metadata(decoder) else {
                return Result::NoValue;
            };
            let Some(rows) = metadata.history_rows[ScreenKey::Alternate as usize] else {
                return Result::NoValue;
            };
            write!(u64, rows);
        }

        // Progress describes only the most recent successful `next` call.
        // The screen and kind apply to both kinds of record. The rest apply
        // to one kind only, and report no value after the other.
        DecoderData::ProgressScreen => {
            let Some(progress) = decoder_progress(decoder) else {
                return Result::NoValue;
            };
            let key = match progress {
                snapshot_core::Progress::Page(page) => page.key,
                snapshot_core::Progress::Image(image) => image.key,
            };
            write!(TerminalScreen, TerminalScreen::from_zig(key));
        }
        DecoderData::ProgressKind => {
            let Some(progress) = decoder_progress(decoder) else {
                return Result::NoValue;
            };
            write!(
                DecoderProgressKind,
                match progress {
                    snapshot_core::Progress::Page(_) => DecoderProgressKind::Page,
                    snapshot_core::Progress::Image(_) => DecoderProgressKind::Image,
                }
            );
        }
        DecoderData::ProgressRows => {
            let Some(snapshot_core::Progress::Page(page)) = decoder_progress(decoder) else {
                return Result::NoValue;
            };
            write!(usize, page.rows);
        }
        DecoderData::ProgressRemaining => {
            let Some(snapshot_core::Progress::Page(page)) = decoder_progress(decoder) else {
                return Result::NoValue;
            };
            write!(u32, page.remaining);
        }
        DecoderData::ProgressImageId => {
            let Some(snapshot_core::Progress::Image(image)) = decoder_progress(decoder) else {
                return Result::NoValue;
            };
            write!(u32, image.image_id);
        }
        DecoderData::ProgressImageFrame => {
            let Some(snapshot_core::Progress::Image(image)) = decoder_progress(decoder) else {
                return Result::NoValue;
            };
            write!(u32, image.frame);
        }
        DecoderData::ProgressImageBytes => {
            let Some(snapshot_core::Progress::Image(image)) = decoder_progress(decoder) else {
                return Result::NoValue;
            };
            write!(usize, image.bytes);
        }
    }
    Result::Success
}

/// Return whether a consuming operation has permanently failed.
fn decoder_failed(decoder: &DecoderWrapper) -> bool {
    matches!(decoder.state, State::Failed(_))
}

/// Return READY metadata in every state where it remains meaningful.
fn decoder_metadata(decoder: &DecoderWrapper) -> Option<Metadata> {
    match decoder.state {
        State::History { metadata, .. } => Some(metadata),
        State::Finished(value) => Some(value),
        State::Failed(value) => value,
        State::Configuring => None,
    }
}

/// Return progress only during incremental history decoding.
fn decoder_progress(decoder: &DecoderWrapper) -> Option<snapshot_core::Progress> {
    match decoder.state {
        State::History { progress, .. } => progress,
        _ => None,
    }
}

/// The Kitty graphics image storage limit a restored screen gets when the
/// decoder option is not set: the default limit of a new screen.
fn default_kitty_image_storage_limit() -> u64 {
    TerminalScreenOptions {
        cols: 1,
        rows: 1,
        ..TerminalScreenOptions::default()
    }
    .kitty_image_storage_limit as u64
}

/// Query several typed decoder values in order, stopping at the first error.
///
/// # Safety
/// `keys_` and `values_` must be NULL or valid for `count` entries; see
/// [`decoder_get`].
#[export_name = "ghostty_snapshot_decoder_get_multi"]
pub unsafe extern "C" fn decoder_get_multi(
    decoder_: Decoder,
    count: usize,
    keys_: *const c_int,
    values_: *const *mut c_void,
    out_written: *mut usize,
) -> Result {
    // Invalid arrays and first-key failures both report zero completed writes.
    if let Some(written) = out_written.as_mut() {
        *written = 0;
    }
    if keys_.is_null() || values_.is_null() {
        return Result::InvalidValue;
    }

    // Reuse the single-key path so availability rules remain identical.
    for i in 0..count {
        let result = decoder_get(decoder_, *keys_.add(i), *values_.add(i));
        if result != Result::Success {
            if let Some(written) = out_written.as_mut() {
                *written = i;
            }
            return result;
        }
    }
    if let Some(written) = out_written.as_mut() {
        *written = count;
    }
    Result::Success
}

/// Decode through READY and return a renderable terminal for incremental use.
///
/// # Safety
/// `decoder_` must be NULL or live; `out_` must be NULL or valid.
#[export_name = "ghostty_snapshot_decoder_ready"]
pub unsafe extern "C" fn decoder_ready(decoder_: Decoder, out_: *mut terminal_c::Terminal) -> Result {
    // Validate the output and require an untouched decoder lifecycle.
    let Some(out) = out_.as_mut() else {
        return Result::InvalidValue;
    };
    *out = std::ptr::null_mut();
    let Some(decoder) = decoder_.as_mut() else {
        return Result::InvalidValue;
    };
    match decoder.state {
        State::Configuring => {}
        _ => return Result::InvalidValue,
    }
    // Decode READY and transfer the decoded state into a C-owned terminal.
    let ready = match decoder_ready_terminal(decoder) {
        Ok(ready) => ready,
        Err(err) => {
            decoder.state = State::Failed(None);
            return decoder_map_error(decoder, err);
        }
    };

    // Keep only the terminal handle and queryable metadata for page decoding.
    decoder.state = State::History {
        terminal: ready.terminal,
        metadata: ready.metadata,
        progress: None,
    };
    *out = ready.terminal;
    Result::Success
}

/// Decode and apply exactly one history page, or validate FINISH.
///
/// # Safety
/// `decoder_` must be NULL or live, and the terminal returned by READY
/// must still be live.
#[export_name = "ghostty_snapshot_decoder_next"]
pub unsafe extern "C" fn decoder_next(decoder_: Decoder) -> Result {
    // FINISH is idempotent; every other non-history state rejects `next`.
    let Some(decoder) = decoder_.as_mut() else {
        return Result::InvalidValue;
    };
    let (terminal, metadata) = match decoder.state {
        State::Finished(_) => return Result::NoValue,
        State::History { terminal, metadata, .. } => (terminal, metadata),
        _ => return Result::InvalidValue,
    };

    // Resolve the terminal retained by READY and clear stale page progress.
    let Some(native) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    decoder.state = State::History {
        terminal,
        metadata,
        progress: None,
    };

    // Consume one validated record and preserve READY metadata on failure.
    let progress = match decoder.decoder.next(native) {
        Ok(progress) => progress,
        Err(err) => {
            decoder.state = State::Failed(Some(metadata));
            return decoder_map_error(decoder, DecodeFailure::Snapshot(err));
        }
    };

    // Publish progress, or transition permanently to validated FINISH.
    if let Some(value) = progress {
        decoder.state = State::History {
            terminal,
            metadata,
            progress: Some(value),
        };
        return Result::Success;
    }

    decoder.state = State::Finished(metadata);
    Result::NoValue
}

/// Decode and validate the complete snapshot transactionally.
///
/// # Safety
/// `decoder_` must be NULL or live; `out_` must be NULL or valid.
#[export_name = "ghostty_snapshot_decoder_decode"]
pub unsafe extern "C" fn decoder_decode(decoder_: Decoder, out_: *mut terminal_c::Terminal) -> Result {
    // Validate the output and require an untouched decoder lifecycle.
    let Some(out) = out_.as_mut() else {
        return Result::InvalidValue;
    };
    *out = std::ptr::null_mut();
    let Some(decoder) = decoder_.as_mut() else {
        return Result::InvalidValue;
    };
    match decoder.state {
        State::Configuring => {}
        _ => return Result::InvalidValue,
    }
    // Build the terminal from the renderable READY prefix.
    let ready = match decoder_ready_terminal(decoder) {
        Ok(ready) => ready,
        Err(err) => {
            decoder.state = State::Failed(None);
            return decoder_map_error(decoder, err);
        }
    };
    let native = terminal_c::zig_terminal(ready.terminal).unwrap();

    // Apply every history page and require a valid FINISH record.
    loop {
        match decoder.decoder.next(native) {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(err) => {
                terminal_c::free(ready.terminal);
                decoder.state = State::Failed(Some(ready.metadata));
                return decoder_map_error(decoder, DecodeFailure::Snapshot(err));
            }
        }
    }

    // Publish the terminal only after the entire snapshot succeeds.
    decoder.state = State::Finished(ready.metadata);
    *out = ready.terminal;
    Result::Success
}

/// Values produced while moving a core READY result into a C terminal.
struct ReadyTerminal {
    terminal: terminal_c::Terminal,
    metadata: Metadata,
}

/// The errors of READY decoding (Zig: `anyerror`).
#[derive(Debug, Clone, Copy)]
enum DecodeFailure {
    Snapshot(sio::Error),
    FromDecoded(terminal_c::FromDecodedError),
}

/// Decode READY with terminal-owned I/O and construct the C terminal.
fn decoder_ready_terminal(decoder: &mut DecoderWrapper) -> std::result::Result<ReadyTerminal, DecodeFailure> {
    let io = terminal_c::Io::INIT;
    let mut decoded = decoder
        .decoder
        .ready(&snapshot_core::DecodeOptions {
            max_continuation_bytes: decoder.max_continuation_bytes,
            compress_history: decoder.compress_history,
            kitty_image_storage_limit: decoder
                .kitty_image_storage_limit
                .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX)),
        })
        .map_err(DecodeFailure::Snapshot)?;

    // Copy small query metadata before `from_decoded` consumes the core result.
    let metadata = Metadata {
        history_rows: decoded.history_rows,
        kitty: decoded.kitty,
    };

    // Move terminal state and I/O into their final C-owned allocation.
    let terminal = terminal_c::from_decoded(
        decoder.alloc,
        io,
        &mut decoded,
        if decoder.retain_continuation {
            decoder.max_continuation_bytes
        } else {
            0
        },
    )
    .map_err(DecodeFailure::FromDecoded)?;
    Ok(ReadyTerminal { terminal, metadata })
}

/// Map decoder and source-adapter failures to public C result codes.
fn decoder_map_error(decoder: &DecoderWrapper, err: DecodeFailure) -> Result {
    // Callback protocol failures are more specific than the core read error.
    match decoder.source() {
        Source::Callback(adapter) => {
            if adapter.invalid_read {
                return Result::InvalidValue;
            }
            if adapter.callback_failed {
                return Result::IoError;
            }
        }
        Source::Buffer(_) => {}
    }

    // A successful zero-byte read is clean EOF by contract. Reaching it before
    // the required marker therefore means truncated snapshot data, not an
    // external I/O failure, and deliberately maps to invalid_value below.
    match err {
        DecodeFailure::Snapshot(sio::Error::OutOfMemory) => Result::OutOfMemory,
        DecodeFailure::Snapshot(sio::Error::ContinuationLimitExceeded) => Result::LimitExceeded,
        DecodeFailure::FromDecoded(terminal_c::FromDecodedError::OutOfMemory) => Result::OutOfMemory,
        DecodeFailure::FromDecoded(terminal_c::FromDecodedError::InvalidContinuation) => Result::InvalidValue,
        // ContinuationDisabled, ContinuationUnavailable, DecoderNotReady,
        // DecoderFailed, InvalidContinuation and everything else.
        DecodeFailure::Snapshot(_) => Result::InvalidValue,
    }
}

/// Export the owned continuation required by snapshot encoding.
unsafe fn continuation_owned(terminal: terminal_c::Terminal) -> std::result::Result<Vec<u8>, Result> {
    if terminal_c::zig_terminal(terminal).is_none() {
        return Err(Result::InvalidValue);
    }

    // Snapshotting can prove an untracked ground stream needs no replay bytes;
    // only non-ground streams require tracking to have retained their prefix.
    terminal_c::continuation_alloc_io(terminal, true).map_err(|err| match err {
        terminal_c::ContinuationAllocError::InvalidValue => Result::InvalidValue,
        terminal_c::ContinuationAllocError::OutOfMemory => Result::OutOfMemory,
        terminal_c::ContinuationAllocError::ContinuationDisabled
        | terminal_c::ContinuationAllocError::ContinuationUnavailable => Result::InvalidValue,
    })
}

/// Convert exported bytes to the core snapshot continuation representation.
fn continuation_value(bytes: &[u8]) -> snapshot_core::Continuation<'_> {
    if bytes.is_empty() {
        snapshot_core::Continuation::Ground
    } else {
        snapshot_core::Continuation::Bytes(bytes)
    }
}

/// Encode a terminal snapshot to a synchronous C writer callback.
///
/// # Safety
/// `terminal` must be NULL or live; `writer` must follow the
/// `GhosttyWriter` contract.
#[export_name = "ghostty_snapshot_encode"]
pub unsafe extern "C" fn encode(terminal: terminal_c::Terminal, writer: io_c::Writer) -> Result {
    // Validate both handles and preflight the terminal continuation.
    let Some(native) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    if writer.write.is_none() {
        return Result::InvalidValue;
    }
    let continuation = match continuation_owned(terminal) {
        Ok(bytes) => bytes,
        Err(result) => return result,
    };

    // Stream the snapshot through the callback adapter, batching the
    // encoder's writes into few callback invocations.
    let mut buffer = [0u8; io_c::WriterAdapter::RECOMMENDED_BUFFER_LEN];
    let mut adapter = io_c::WriterAdapter::init_buffered(writer, &mut buffer);
    if let Err(err) = snapshot_core::encode(
        &mut adapter,
        native,
        snapshot_core::EncodeOptions {
            continuation: continuation_value(&continuation),
        },
    ) {
        return map_encode_error(err, Some(&adapter));
    }
    if adapter.flush().is_err() {
        return map_encode_error(sio::Error::WriteFailed, Some(&adapter));
    }
    Result::Success
}

/// Encode a terminal snapshot into a caller-provided fixed buffer.
///
/// # Safety
/// `terminal` must be NULL or live; a non-null `buf` must be valid for
/// `buf_len` writes; `out_written_` must be NULL or valid.
#[export_name = "ghostty_snapshot_encode_buf"]
pub unsafe extern "C" fn encode_buf(
    terminal: terminal_c::Terminal,
    buf: *mut u8,
    buf_len: usize,
    out_written_: *mut usize,
) -> Result {
    // Validate output storage and preflight the terminal continuation.
    let Some(out_written) = out_written_.as_mut() else {
        return Result::InvalidValue;
    };
    *out_written = 0;
    if buf.is_null() && buf_len != 0 {
        return Result::InvalidValue;
    }
    let Some(native) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    let continuation = match continuation_owned(terminal) {
        Ok(bytes) => bytes,
        Err(result) => return result,
    };

    // A null size query becomes a zero-capacity fixed writer and follows the
    // same WriteFailed counting path as every other undersized destination.
    let destination: &mut [u8] = if buf.is_null() {
        &mut []
    } else {
        std::slice::from_raw_parts_mut(buf, buf_len)
    };
    let mut writer = sio::FixedWriter::new(destination);
    match snapshot_core::encode(
        &mut writer,
        native,
        snapshot_core::EncodeOptions {
            continuation: continuation_value(&continuation),
        },
    ) {
        Ok(()) => {}
        Err(sio::Error::WriteFailed) => {
            // The fixed writer may already contain a valid snapshot prefix.
            // Repeat only on this failure path to report the exact capacity.
            let mut counter = sio::Discarding::default();
            if let Err(count_err) = snapshot_core::encode(
                &mut counter,
                native,
                snapshot_core::EncodeOptions {
                    continuation: continuation_value(&continuation),
                },
            ) {
                return map_encode_error(count_err, None);
            }
            let Ok(count) = usize::try_from(counter.count) else {
                return Result::LimitExceeded;
            };
            *out_written = count;
            return Result::OutOfSpace;
        }
        Err(err) => return map_encode_error(err, None),
    }
    *out_written = writer.end;
    Result::Success
}

/// Encode a terminal snapshot into a newly allocated C-owned buffer.
///
/// # Safety
/// `terminal` must be NULL or live; the output pointers must be NULL or
/// valid.
#[export_name = "ghostty_snapshot_encode_alloc"]
pub unsafe extern "C" fn encode_alloc(
    terminal: terminal_c::Terminal,
    alloc_: Option<&CAllocator>,
    out_ptr_: *mut *mut u8,
    out_len_: *mut usize,
) -> Result {
    // Initialize outputs before validating terminal state or allocating bytes.
    let Some(out_ptr) = out_ptr_.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(out_len) = out_len_.as_mut() else {
        return Result::InvalidValue;
    };
    *out_ptr = std::ptr::null_mut();
    *out_len = 0;

    // Preflight continuation state before building an allocating writer.
    let Some(native) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    let continuation = match continuation_owned(terminal) {
        Ok(bytes) => bytes,
        Err(result) => return result,
    };

    // Encode once, then transfer the completed bytes to the C allocator.
    let mut writer: Vec<u8> = Vec::new();
    if let Err(err) = snapshot_core::encode(
        &mut writer,
        native,
        snapshot_core::EncodeOptions {
            continuation: continuation_value(&continuation),
        },
    ) {
        return map_encode_error(err, None);
    }
    if writer.is_empty() {
        return Result::Success;
    }
    let alloc = lib_alloc::default(alloc_);
    let ptr = alloc.alloc(writer.len(), 0, 0);
    if ptr.is_null() {
        return Result::OutOfMemory;
    }
    std::ptr::copy_nonoverlapping(writer.as_ptr(), ptr, writer.len());
    *out_ptr = ptr;
    *out_len = writer.len();
    Result::Success
}

/// Map core encoder and callback-adapter failures to public result codes.
fn map_encode_error(err: sio::Error, adapter: Option<&io_c::WriterAdapter<'_>>) -> Result {
    // Callback validation happens before encoding, so invalid_write can only
    // mean output accounting overflow; callback_failed is external I/O.
    if let Some(value) = adapter {
        if value.invalid_write {
            return Result::LimitExceeded;
        }
        if value.callback_failed {
            return Result::IoError;
        }
    }

    // Fixed and allocating writers surface only core allocation or size errors.
    match err {
        sio::Error::OutOfMemory | sio::Error::WriteFailed => Result::OutOfMemory,
        sio::Error::Overflow
        | sio::Error::PayloadTooLarge
        | sio::Error::PageCountOverflow
        | sio::Error::ScrollbackLimitOverflow
        | sio::Error::StringTooLong => Result::LimitExceeded,
        _ => Result::InvalidValue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;
    use crate::terminal::c::kitty_graphics as kitty_graphics_c;
    use crate::terminal::c::terminal::tests::FAILING_C_ALLOCATOR;
    use crate::terminal::c::terminal::{Option as TermOption, TerminalData};
    use crate::terminal::kitty::graphics_storage::PlacementKey;
    use crate::terminal::point::Tag;
    use std::collections::HashMap;

    fn vp<T>(v: &mut T) -> *mut c_void {
        v as *mut T as *mut c_void
    }

    fn cp<T>(v: &T) -> *const c_void {
        v as *const T as *const c_void
    }

    fn new_terminal(cols: u16, rows: u16) -> terminal_c::Terminal {
        let mut t: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut t, cols, rows));
        t
    }

    fn write(t: terminal_c::Terminal, s: &[u8]) {
        unsafe { terminal_c::vt_write(t, s.as_ptr(), s.len()) };
    }

    unsafe fn term_set<T>(t: terminal_c::Terminal, option: TermOption, value: &T) -> Result {
        terminal_c::set(t, option as c_int, cp(value))
    }

    unsafe fn term_get<T>(t: terminal_c::Terminal, data: TerminalData, out: &mut T) -> Result {
        terminal_c::get(t, data as c_int, vp(out))
    }

    unsafe fn dget<T>(decoder: Decoder, data: DecoderData, out: &mut T) -> Result {
        decoder_get(decoder, data as c_int, vp(out))
    }

    unsafe fn dset<T>(decoder: Decoder, option: DecoderOption, value: &T) -> Result {
        decoder_set(decoder, option as c_int, cp(value))
    }

    /// Enable continuation tracking for tests that encode C terminal snapshots.
    fn test_enable_continuation(terminal: terminal_c::Terminal) {
        let limit: usize = default_max_continuation_bytes();
        assert_eq!(Result::Success, unsafe { term_set(terminal, TermOption::ContinuationMaxBytes, &limit) });
    }

    fn new_buf_decoder(bytes: &[u8]) -> Decoder {
        let mut decoder: Decoder = std::ptr::null_mut();
        assert_eq!(Result::Success, unsafe {
            decoder_new_buf(Some(&C_ALLOCATOR), &mut decoder, bytes.as_ptr(), bytes.len())
        });
        decoder
    }

    /// `encode_alloc` into an owned copy (the C buffer is freed).
    unsafe fn encode_owned(terminal: terminal_c::Terminal) -> Vec<u8> {
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len: usize = 0;
        assert_eq!(Result::Success, encode_alloc(terminal, Some(&C_ALLOCATOR), &mut ptr, &mut len));
        let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
        crate::terminal::c::allocator::free(Some(&C_ALLOCATOR), ptr, len);
        bytes
    }

    #[test]
    fn decoder_option_and_empty_source() {
        unsafe {
            let mut decoder: Decoder = std::ptr::null_mut();
            assert_eq!(Result::Success, decoder_new_buf(Some(&C_ALLOCATOR), &mut decoder, std::ptr::null(), 0));

            let mut limit: usize = 0;
            assert_eq!(Result::Success, dget(decoder, DecoderData::MaxContinuationBytes, &mut limit));
            assert_eq!(default_max_continuation_bytes(), limit);

            limit = 1234;
            assert_eq!(Result::Success, dset(decoder, DecoderOption::MaxContinuationBytes, &limit));
            limit = 0;
            assert_eq!(Result::Success, dget(decoder, DecoderData::MaxContinuationBytes, &mut limit));
            assert_eq!(1234, limit);

            let mut retain = true;
            assert_eq!(Result::Success, dget(decoder, DecoderData::RetainContinuation, &mut retain));
            assert!(!retain);
            retain = true;
            assert_eq!(Result::Success, dset(decoder, DecoderOption::RetainContinuation, &retain));
            retain = false;
            assert_eq!(Result::Success, dget(decoder, DecoderData::RetainContinuation, &mut retain));
            assert!(retain);

            let mut compress = true;
            assert_eq!(Result::Success, dget(decoder, DecoderData::CompressHistory, &mut compress));
            assert!(!compress);
            compress = true;
            assert_eq!(Result::Success, dset(decoder, DecoderOption::CompressHistory, &compress));
            compress = false;
            assert_eq!(Result::Success, dget(decoder, DecoderData::CompressHistory, &mut compress));
            assert!(compress);

            let mut written: usize = 99;
            assert_eq!(
                Result::InvalidValue,
                decoder_get_multi(decoder, 1, std::ptr::null(), std::ptr::null(), &mut written)
            );
            assert_eq!(0, written);

            let mut terminal: terminal_c::Terminal = std::ptr::null_mut();
            assert_eq!(Result::InvalidValue, decoder_decode(decoder, &mut terminal));
            assert!(terminal.is_null());
            decoder_free(decoder);
        }
    }

    #[test]
    fn snapshot_c_api_full_round_trip_restores_continuation() {
        unsafe {
            let source = new_terminal(20, 4);
            test_enable_continuation(source);
            write(source, b"hello\x1b[31");

            let encoded = encode_owned(source);

            let decoder = new_buf_decoder(&encoded);
            let mut restored: terminal_c::Terminal = std::ptr::null_mut();
            assert_eq!(Result::Success, decoder_decode(decoder, &mut restored));

            let source_text = terminal_c::zig_terminal(source).unwrap().plain_string().unwrap();
            let restored_text = terminal_c::zig_terminal(restored).unwrap().plain_string().unwrap();
            assert_eq!(source_text, restored_text);

            let mut restored_limit: usize = usize::MAX;
            assert_eq!(Result::Success, term_get(restored, TerminalData::ContinuationMaxBytes, &mut restored_limit));
            assert_eq!(0, restored_limit);

            // Parser state is restored, but its tracking policy is not. The terminal
            // cannot be snapshotted again until the restored sequence reaches ground.
            let mut restored_required: usize = 0;
            assert_eq!(
                Result::InvalidValue,
                encode_buf(restored, std::ptr::null_mut(), 0, &mut restored_required)
            );
            write(restored, b"m");
            assert_eq!(
                Result::OutOfSpace,
                encode_buf(restored, std::ptr::null_mut(), 0, &mut restored_required)
            );
            assert!(restored_required > 0);

            let mut offset: usize = 0;
            assert_eq!(Result::Success, dget(decoder, DecoderData::SourceOffset, &mut offset));
            assert_eq!(encoded.len(), offset);
            assert_eq!(Result::NoValue, decoder_next(decoder));

            let retained_decoder = new_buf_decoder(&encoded);
            let retained_limit: usize = 1024;
            assert_eq!(Result::Success, dset(retained_decoder, DecoderOption::MaxContinuationBytes, &retained_limit));
            let retain = true;
            assert_eq!(Result::Success, dset(retained_decoder, DecoderOption::RetainContinuation, &retain));

            // The returned tracker owns the exact decoded continuation, so the
            // decoder can be freed before callers export those terminal-owned bytes.
            let mut retained: terminal_c::Terminal = std::ptr::null_mut();
            assert_eq!(Result::Success, decoder_decode(retained_decoder, &mut retained));
            decoder_free(retained_decoder);

            let mut retained_terminal_limit: usize = 0;
            assert_eq!(
                Result::Success,
                term_get(retained, TerminalData::ContinuationMaxBytes, &mut retained_terminal_limit)
            );
            assert_eq!(retained_limit, retained_terminal_limit);
            let mut continuation_buf = [0u8; 16];
            let mut continuation_len: usize = 0;
            assert_eq!(
                Result::Success,
                terminal_c::continuation_buf(
                    retained,
                    continuation_buf.as_mut_ptr(),
                    continuation_buf.len(),
                    &mut continuation_len
                )
            );
            assert_eq!(b"\x1b[31", &continuation_buf[..continuation_len]);

            // Disabling tracking releases the exported prefix without changing the
            // unfinished parser state. The final byte still completes the SGR.
            let disabled: usize = 0;
            assert_eq!(Result::Success, term_set(retained, TermOption::ContinuationMaxBytes, &disabled));
            assert_eq!(
                Result::InvalidValue,
                terminal_c::continuation_buf(
                    retained,
                    continuation_buf.as_mut_ptr(),
                    continuation_buf.len(),
                    &mut continuation_len
                )
            );
            write(retained, b"m");
            let mut retained_ground = false;
            assert_eq!(Result::Success, term_get(retained, TerminalData::VtGround, &mut retained_ground));
            assert!(retained_ground);

            let limited = new_buf_decoder(&encoded);
            let limit: usize = 3;
            assert_eq!(Result::Success, dset(limited, DecoderOption::MaxContinuationBytes, &limit));
            let mut rejected: terminal_c::Terminal = std::ptr::null_mut();
            assert_eq!(Result::LimitExceeded, decoder_decode(limited, &mut rejected));
            assert!(rejected.is_null());
            assert_eq!(Result::InvalidValue, dset(limited, DecoderOption::MaxContinuationBytes, &limit));
            let mut failed_offset: usize = 0;
            assert_eq!(Result::NoValue, dget(limited, DecoderData::SourceOffset, &mut failed_offset));

            assert_eq!(Result::Success, term_set(source, TermOption::ContinuationMaxBytes, &disabled));
            let mut required: usize = 0;
            assert_eq!(Result::InvalidValue, encode_buf(source, std::ptr::null_mut(), 0, &mut required));

            decoder_free(limited);
            terminal_c::free(retained);
            terminal_c::free(restored);
            decoder_free(decoder);
            terminal_c::free(source);
        }
    }

    #[test]
    fn snapshot_decoder_retains_every_supported_continuation_class() {
        let cases: [(&str, &[u8], &[u8]); 6] = [
            ("ground", b"", b"X"),
            ("CSI", b"\x1b[31", b"mX"),
            ("OSC", b"\x1b]2;title", b"\x07"),
            ("DCS", b"\x1bP$qm", b"\x1b\\"),
            ("APC", b"\x1b_Ga=q;payload", b"\x1b\\"),
            ("UTF-8", b"\xF0\x9F", b"\x98\x84"),
        ];
        let limit: usize = 4096;
        let disabled: usize = 0;
        let retain = true;

        for (name, prefix, suffix) in cases {
            unsafe {
                let source = new_terminal(20, 4);
                assert_eq!(Result::Success, term_set(source, TermOption::ContinuationMaxBytes, &limit), "{name}");
                write(source, prefix);

                let snapshot = encode_owned(source);

                let decoder = new_buf_decoder(&snapshot);
                assert_eq!(Result::Success, dset(decoder, DecoderOption::MaxContinuationBytes, &limit), "{name}");
                assert_eq!(Result::Success, dset(decoder, DecoderOption::RetainContinuation, &retain), "{name}");

                let mut restored: terminal_c::Terminal = std::ptr::null_mut();
                assert_eq!(Result::Success, decoder_decode(decoder, &mut restored), "{name}");
                decoder_free(decoder);

                let mut restored_limit: usize = 0;
                assert_eq!(Result::Success, term_get(restored, TerminalData::ContinuationMaxBytes, &mut restored_limit));
                assert_eq!(limit, restored_limit, "{name}");

                let mut continuation = vec![0u8; 4096];
                let mut continuation_len: usize = 0;
                assert_eq!(
                    Result::Success,
                    terminal_c::continuation_buf(restored, continuation.as_mut_ptr(), continuation.len(), &mut continuation_len),
                    "{name}"
                );
                assert_eq!(prefix, &continuation[..continuation_len], "{name}");

                // Releasing the retained bytes must not reset the parser. Both the
                // source and replica should reach the same fully serializable state
                // after consuming the suffix that followed the snapshot cut.
                assert_eq!(Result::Success, term_set(restored, TermOption::ContinuationMaxBytes, &disabled));
                assert_eq!(
                    Result::InvalidValue,
                    terminal_c::continuation_buf(restored, continuation.as_mut_ptr(), continuation.len(), &mut continuation_len),
                    "{name}"
                );
                write(source, suffix);
                write(restored, suffix);

                let mut source_ground = false;
                let mut restored_ground = false;
                assert_eq!(Result::Success, term_get(source, TerminalData::VtGround, &mut source_ground));
                assert_eq!(Result::Success, term_get(restored, TerminalData::VtGround, &mut restored_ground));
                assert!(source_ground, "{name}");
                assert!(restored_ground, "{name}");

                let source_final = encode_owned(source);
                let restored_final = encode_owned(restored);
                assert_eq!(source_final, restored_final, "{name}");

                terminal_c::free(restored);
                terminal_c::free(source);
            }
        }
    }

    #[test]
    fn snapshot_encoding_accepts_an_untracked_ground_terminal() {
        unsafe {
            let source = new_terminal(10, 3);

            let mut limit: usize = usize::MAX;
            assert_eq!(Result::Success, term_get(source, TerminalData::ContinuationMaxBytes, &mut limit));
            assert_eq!(0, limit);

            write(source, b"ground");
            let mut required: usize = 0;
            assert_eq!(Result::OutOfSpace, encode_buf(source, std::ptr::null_mut(), 0, &mut required));
            assert!(required > 0);

            // Once untracked input leaves ground, the missing prefix is unknowable.
            write(source, b"\x1b[31");
            assert_eq!(Result::InvalidValue, encode_buf(source, std::ptr::null_mut(), 0, &mut required));
            terminal_c::free(source);
        }
    }

    struct WriteContext {
        destination: Vec<u8>,
        offset: usize,
    }

    unsafe extern "C" fn context_write(userdata: *mut c_void, data: *const u8, len: usize) -> bool {
        let this = &mut *(userdata as *mut WriteContext);
        if len > this.destination.len() - this.offset {
            return false;
        }
        let off = this.offset;
        this.destination[off..off + len].copy_from_slice(std::slice::from_raw_parts(data, len));
        this.offset += len;
        true
    }

    unsafe extern "C" fn fail_write(_: *mut c_void, _: *const u8, _: usize) -> bool {
        false
    }

    struct ReadContext {
        source: Vec<u8>,
        offset: usize,
    }

    unsafe extern "C" fn context_read(
        userdata: *mut c_void,
        destination: *mut u8,
        capacity: usize,
        out_read: *mut usize,
    ) -> bool {
        let this = &mut *(userdata as *mut ReadContext);
        let remaining = &this.source[this.offset..];
        let len = remaining.len().min(capacity).min(3);
        std::ptr::copy_nonoverlapping(remaining.as_ptr(), destination, len);
        this.offset += len;
        *out_read = len;
        true
    }

    unsafe extern "C" fn fail_read(_: *mut c_void, _: *mut u8, _: usize, _: *mut usize) -> bool {
        false
    }

    unsafe extern "C" fn eof_read(_: *mut c_void, _: *mut u8, _: usize, out_read: *mut usize) -> bool {
        *out_read = 0;
        true
    }

    unsafe extern "C" fn invalid_read(_: *mut c_void, _: *mut u8, capacity: usize, out_read: *mut usize) -> bool {
        *out_read = capacity + 1;
        true
    }

    fn reader(read: io_c::ReaderFn, userdata: *mut c_void) -> io_c::Reader {
        io_c::Reader {
            read: Some(read),
            userdata,
        }
    }

    #[test]
    fn snapshot_callbacks_stop_at_finish_and_map_io_failures() {
        unsafe {
            let source = new_terminal(10, 3);
            test_enable_continuation(source);
            write(source, b"callback");

            let mut required: usize = 0;
            assert_eq!(Result::OutOfSpace, encode_buf(source, std::ptr::null_mut(), 0, &mut required));
            let mut buffer_encoded = vec![0u8; required];
            let mut buffer_written: usize = 0;
            assert_eq!(
                Result::Success,
                encode_buf(source, buffer_encoded.as_mut_ptr(), buffer_encoded.len(), &mut buffer_written)
            );
            assert_eq!(required, buffer_written);

            // A non-null undersized buffer takes the fixed-first path. It may contain
            // the prefix written before capacity exhaustion while still reporting the
            // exact complete size.
            let mut short = [0xAAu8; 10];
            let mut short_required: usize = 0;
            assert_eq!(
                Result::OutOfSpace,
                encode_buf(source, short.as_mut_ptr(), short.len(), &mut short_required)
            );
            assert_eq!(required, short_required);
            assert_eq!(b"GHOSTSNP", &short[..8]);

            let mut write_context = WriteContext {
                destination: vec![0u8; required],
                offset: 0,
            };
            assert_eq!(
                Result::Success,
                encode(
                    source,
                    io_c::Writer {
                        write: Some(context_write),
                        userdata: &mut write_context as *mut WriteContext as *mut c_void,
                    }
                )
            );
            assert_eq!(required, write_context.offset);
            assert_eq!(buffer_encoded, write_context.destination);
            let encoded = write_context.destination.clone();

            let failing_c: &CAllocator = &FAILING_C_ALLOCATOR;
            let mut failed_ptr: *mut u8 = std::ptr::null_mut();
            let mut failed_len: usize = 99;
            assert_eq!(
                Result::OutOfMemory,
                encode_alloc(source, Some(failing_c), &mut failed_ptr, &mut failed_len)
            );
            assert!(failed_ptr.is_null());
            assert_eq!(0, failed_len);

            assert_eq!(
                Result::IoError,
                encode(
                    source,
                    io_c::Writer {
                        write: Some(fail_write),
                        userdata: std::ptr::null_mut(),
                    }
                )
            );

            let mut combined = encoded.clone();
            combined.extend_from_slice(b"tail");

            let mut read_context = ReadContext {
                source: combined,
                offset: 0,
            };
            let mut decoder: Decoder = std::ptr::null_mut();
            assert_eq!(
                Result::Success,
                decoder_new(
                    Some(&C_ALLOCATOR),
                    &mut decoder,
                    reader(context_read, &mut read_context as *mut ReadContext as *mut c_void)
                )
            );
            let mut restored: terminal_c::Terminal = std::ptr::null_mut();
            assert_eq!(Result::Success, decoder_decode(decoder, &mut restored));
            assert_eq!(encoded.len(), read_context.offset);
            assert_eq!(b"tail", &read_context.source[read_context.offset..]);

            let mut failed_decoder: Decoder = std::ptr::null_mut();
            assert_eq!(
                Result::Success,
                decoder_new(Some(&C_ALLOCATOR), &mut failed_decoder, reader(fail_read, std::ptr::null_mut()))
            );
            let mut failed_terminal: terminal_c::Terminal = std::ptr::null_mut();
            assert_eq!(Result::IoError, decoder_decode(failed_decoder, &mut failed_terminal));

            let mut eof_decoder: Decoder = std::ptr::null_mut();
            assert_eq!(
                Result::Success,
                decoder_new(Some(&C_ALLOCATOR), &mut eof_decoder, reader(eof_read, std::ptr::null_mut()))
            );
            // Successful EOF before READY/FINISH is truncated snapshot data. It is
            // intentionally INVALID_VALUE; only a false callback is IO_ERROR.
            assert_eq!(Result::InvalidValue, decoder_decode(eof_decoder, &mut failed_terminal));

            let mut invalid_decoder: Decoder = std::ptr::null_mut();
            assert_eq!(
                Result::Success,
                decoder_new(Some(&C_ALLOCATOR), &mut invalid_decoder, reader(invalid_read, std::ptr::null_mut()))
            );
            assert_eq!(Result::InvalidValue, decoder_decode(invalid_decoder, &mut failed_terminal));

            decoder_free(invalid_decoder);
            decoder_free(eof_decoder);
            decoder_free(failed_decoder);
            terminal_c::free(restored);
            decoder_free(decoder);
            terminal_c::free(source);
        }
    }

    /// The number of page rows of the first page of the primary screen.
    fn first_page_rows(terminal: terminal_c::Terminal) -> usize {
        let t = unsafe { terminal_c::zig_terminal(terminal).unwrap() };
        let screen = t.screens.get(ScreenKey::Primary).unwrap();
        let first = screen.pages.pages.first.unwrap();
        // SAFETY: the node belongs to the live page list.
        unsafe { first.as_ref() }.capacity().rows as usize
    }

    #[test]
    fn snapshot_decoder_compresses_history_when_requested() {
        unsafe {
            let source = new_terminal(215, 2);
            assert_eq!(Result::Success, terminal_c::set(source, TermOption::ScrollbackMaxBytes as c_int, std::ptr::null()));
            for _ in 0..first_page_rows(source) * 3 {
                write(source, b"x\r\n");
            }
            let source_total_rows = terminal_c::zig_terminal(source)
                .unwrap()
                .screens
                .get(ScreenKey::Primary)
                .unwrap()
                .pages
                .total_rows;

            let encoded = encode_owned(source);

            for compress in [false, true] {
                let decoder = new_buf_decoder(&encoded);
                assert_eq!(Result::Success, dset(decoder, DecoderOption::CompressHistory, &compress));

                let mut restored: terminal_c::Terminal = std::ptr::null_mut();
                assert_eq!(Result::Success, decoder_decode(decoder, &mut restored));

                // Options freeze once decoding starts, but remain readable.
                assert_eq!(Result::InvalidValue, dset(decoder, DecoderOption::CompressHistory, &compress));
                let mut actual = !compress;
                assert_eq!(Result::Success, dget(decoder, DecoderData::CompressHistory, &mut actual));
                assert_eq!(compress, actual);

                // The restored history is complete either way. Only how it is stored
                // differs. Every page above the active area can be compressed because
                // the restored viewport shows the active area.
                let rt = terminal_c::zig_terminal(restored).unwrap();
                let screen = rt.screens.get(ScreenKey::Primary).unwrap();
                let pages = &screen.pages;
                assert_eq!(source_total_rows, pages.total_rows);
                let active = pages.get_top_left(Tag::Active).node;
                let mut history_pages: usize = 0;
                let mut current = pages.pages.first;
                while let Some(node) = current {
                    if node == active {
                        break;
                    }
                    history_pages += 1;
                    current = node.as_ref().next;
                }
                assert!(history_pages > 0);
                assert_eq!(
                    if compress { history_pages } else { 0 },
                    pages.memory_stats().compressed_pages
                );

                terminal_c::free(restored);
                decoder_free(decoder);
            }
            terminal_c::free(source);
        }
    }

    #[test]
    fn snapshot_incremental_decoder_exposes_ready_and_page_progress() {
        unsafe {
            let source = new_terminal(215, 2);
            test_enable_continuation(source);
            // Keep multiple complete pages. The ordinary C-terminal byte limit keeps
            // at least one page but can evict older ones before snapshot encoding.
            assert_eq!(Result::Success, terminal_c::set(source, TermOption::ScrollbackMaxBytes as c_int, std::ptr::null()));
            for _ in 0..first_page_rows(source) * 3 {
                write(source, b"x\r\n");
            }
            let ready_continuation: &[u8] = b"\x1b]2;ready";
            write(source, ready_continuation);

            let encoded = encode_owned(source);

            let decoder = new_buf_decoder(&encoded);
            let retained_limit: usize = 1024;
            assert_eq!(Result::Success, dset(decoder, DecoderOption::MaxContinuationBytes, &retained_limit));
            let retain = true;
            assert_eq!(Result::Success, dset(decoder, DecoderOption::RetainContinuation, &retain));

            let mut restored: terminal_c::Terminal = std::ptr::null_mut();
            assert_eq!(Result::Success, decoder_ready(decoder, &mut restored));

            // READY applies the same retention policy as one-shot decode and makes
            // the unfinished snapshot continuation immediately exportable.
            let mut restored_limit: usize = 0;
            assert_eq!(Result::Success, term_get(restored, TerminalData::ContinuationMaxBytes, &mut restored_limit));
            assert_eq!(retained_limit, restored_limit);
            let mut ready_buf = [0u8; 32];
            let mut ready_len: usize = 0;
            assert_eq!(
                Result::Success,
                terminal_c::continuation_buf(restored, ready_buf.as_mut_ptr(), ready_buf.len(), &mut ready_len)
            );
            assert_eq!(ready_continuation, &ready_buf[..ready_len]);

            let mut history_rows: u64 = 0;
            assert_eq!(Result::Success, dget(decoder, DecoderData::HistoryRowsPrimary, &mut history_rows));
            assert!(history_rows > 0);

            let mut page_count: usize = 0;
            loop {
                let result = decoder_next(decoder);
                if result == Result::NoValue {
                    break;
                }
                assert_eq!(Result::Success, result);
                page_count += 1;

                let mut screen = TerminalScreen::Alternate;
                let mut rows: usize = 0;
                let mut remaining: u32 = 0;
                let keys = [
                    DecoderData::ProgressScreen as c_int,
                    DecoderData::ProgressRows as c_int,
                    DecoderData::ProgressRemaining as c_int,
                ];
                let values = [vp(&mut screen), vp(&mut rows), vp(&mut remaining)];
                let mut written: usize = 0;
                assert_eq!(
                    Result::Success,
                    decoder_get_multi(decoder, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
                );
                assert_eq!(keys.len(), written);
                assert_eq!(TerminalScreen::Primary, screen);
                assert!(rows > 0);
            }
            assert!(page_count > 0);
            let mut rows: usize = 0;
            assert_eq!(Result::NoValue, dget(decoder, DecoderData::ProgressRows, &mut rows));
            assert_eq!(Result::NoValue, decoder_next(decoder));

            let mut source_total: usize = 0;
            let mut restored_total: usize = 0;
            assert_eq!(Result::Success, term_get(source, TerminalData::TotalRows, &mut source_total));
            assert_eq!(Result::Success, term_get(restored, TerminalData::TotalRows, &mut restored_total));
            assert_eq!(source_total, restored_total);

            let dropped_decoder = new_buf_decoder(&encoded);
            let mut dropped: terminal_c::Terminal = std::ptr::null_mut();
            assert_eq!(Result::Success, decoder_ready(dropped_decoder, &mut dropped));
            assert_eq!(Result::Success, terminal_c::resize(dropped, 214, 2, 0, 0));
            assert_eq!(Result::Success, decoder_next(dropped_decoder));
            let mut dropped_rows: usize = 1;
            assert_eq!(Result::Success, dget(dropped_decoder, DecoderData::ProgressRows, &mut dropped_rows));
            assert_eq!(0, dropped_rows);
            while decoder_next(dropped_decoder) == Result::Success {}
            assert_eq!(Result::NoValue, decoder_next(dropped_decoder));

            terminal_c::free(dropped);
            decoder_free(dropped_decoder);
            terminal_c::free(restored);
            decoder_free(decoder);
            terminal_c::free(source);
        }
    }

    /// Collect the render info of every Kitty graphics placement on a terminal's
    /// active screen, keyed by image and placement ID.
    unsafe fn test_placement_render_info(
        terminal: terminal_c::Terminal,
    ) -> HashMap<PlacementKey, kitty_graphics_c::PlacementRenderInfo> {
        let mut result = HashMap::new();

        let mut graphics: kitty_graphics_c::KittyGraphics = std::ptr::null_mut();
        assert_eq!(Result::Success, term_get(terminal, TerminalData::KittyGraphics, &mut graphics));
        let mut iter: kitty_graphics_c::PlacementIterator = std::ptr::null_mut();
        assert_eq!(Result::Success, kitty_graphics_c::placement_iterator_new(Some(&C_ALLOCATOR), &mut iter));
        assert_eq!(
            Result::Success,
            kitty_graphics_c::get(graphics, kitty_graphics_c::Data::PlacementIterator as c_int, vp(&mut iter))
        );
        while kitty_graphics_c::placement_iterator_next(iter) {
            let key = *(*iter).entry.unwrap().0;
            let img = kitty_graphics_c::image_get_handle(graphics, key.image_id);
            let mut info = kitty_graphics_c::PlacementRenderInfo::default();
            assert_eq!(
                Result::Success,
                kitty_graphics_c::placement_render_info(iter, img, terminal, &mut info)
            );
            result.insert(key, info);
        }
        kitty_graphics_c::placement_iterator_free(iter);
        result
    }

    #[test]
    fn snapshot_decoder_restores_kitty_graphics_images_and_reports_their_payloads() {
        unsafe {
            let source = new_terminal(20, 5);
            assert_eq!(Result::Success, terminal_c::resize(source, 20, 5, 10, 20));

            // Two placed images: one scaled to cells, one cropped and offset.
            write(
                source,
                b"\x1b_Ga=T,f=24,i=1,p=1,s=1,v=2,c=4,r=2,q=2;////////\x1b\\\
                  \x1b_Ga=T,f=32,i=2,p=3,s=2,v=1,x=1,w=1,X=3,Y=4,z=-1,q=2;AAAA/wAAAP8=\x1b\\",
            );

            let encoded = encode_owned(source);

            let decoder = new_buf_decoder(&encoded);

            let limit: u64 = 1024 * 1024;
            assert_eq!(Result::Success, dset(decoder, DecoderOption::KittyImageStorageLimit, &limit));
            let mut limit_out: u64 = 0;
            assert_eq!(Result::Success, dget(decoder, DecoderData::KittyImageStorageLimit, &mut limit_out));
            assert_eq!(limit, limit_out);

            let mut images: u32 = 0;
            assert_eq!(Result::NoValue, dget(decoder, DecoderData::KittyImagesPrimary, &mut images));

            let mut restored: terminal_c::Terminal = std::ptr::null_mut();
            assert_eq!(Result::Success, decoder_ready(decoder, &mut restored));

            // READY restores both images with their placements and render geometry.
            assert_eq!(Result::Success, dget(decoder, DecoderData::KittyImagesPrimary, &mut images));
            assert_eq!(2, images);
            let mut pending_bytes: u64 = 0;
            assert_eq!(Result::Success, dget(decoder, DecoderData::KittyPendingBytesPrimary, &mut pending_bytes));
            assert_eq!(6 + 8, pending_bytes);
            assert_eq!(Result::NoValue, dget(decoder, DecoderData::KittyImagesAlternate, &mut images));
            assert_eq!(
                limit as usize,
                terminal_c::zig_terminal(restored).unwrap().screens.active.kitty_images.total_limit
            );

            let expected = test_placement_render_info(source);
            {
                let actual = test_placement_render_info(restored);
                assert_eq!(2, expected.len());
                assert_eq!(expected.len(), actual.len());
                for (key, value) in &expected {
                    assert_eq!(*value, *actual.get(key).unwrap());
                }
            }

            // Each image's pixels are one progress event. The page values report
            // no value for them.
            let mut kind = DecoderProgressKind::Page;
            assert_eq!(Result::NoValue, dget(decoder, DecoderData::ProgressKind, &mut kind));
            let mut attached: u64 = 0;
            for _ in 0..2 {
                assert_eq!(Result::Success, decoder_next(decoder));
                assert_eq!(Result::Success, dget(decoder, DecoderData::ProgressKind, &mut kind));
                assert_eq!(DecoderProgressKind::Image, kind);
                let mut screen = TerminalScreen::Alternate;
                let mut image_id: u32 = 0;
                let mut frame: u32 = 0;
                let mut bytes: usize = 0;
                assert_eq!(Result::Success, dget(decoder, DecoderData::ProgressScreen, &mut screen));
                assert_eq!(Result::Success, dget(decoder, DecoderData::ProgressImageId, &mut image_id));
                assert_eq!(Result::Success, dget(decoder, DecoderData::ProgressImageFrame, &mut frame));
                assert_eq!(Result::Success, dget(decoder, DecoderData::ProgressImageBytes, &mut bytes));
                assert_eq!(TerminalScreen::Primary, screen);
                assert!(image_id == 1 || image_id == 2);
                assert_eq!(1, frame);
                attached += bytes as u64;

                let mut rows: usize = 0;
                assert_eq!(Result::NoValue, dget(decoder, DecoderData::ProgressRows, &mut rows));
            }
            assert_eq!(pending_bytes, attached);

            // History pages report the page kind and no image details.
            loop {
                let result = decoder_next(decoder);
                if result == Result::NoValue {
                    break;
                }
                assert_eq!(Result::Success, result);
                assert_eq!(Result::Success, dget(decoder, DecoderData::ProgressKind, &mut kind));
                assert_eq!(DecoderProgressKind::Page, kind);
                let mut image_id: u32 = 0;
                assert_eq!(Result::NoValue, dget(decoder, DecoderData::ProgressImageId, &mut image_id));
            }

            let mut graphics: kitty_graphics_c::KittyGraphics = std::ptr::null_mut();
            assert_eq!(Result::Success, term_get(restored, TerminalData::KittyGraphics, &mut graphics));
            let mut data_len: usize = 0;
            assert_eq!(
                Result::Success,
                kitty_graphics_c::image_get(
                    kitty_graphics_c::image_get_handle(graphics, 2),
                    kitty_graphics_c::ImageData::DataLen as c_int,
                    vp(&mut data_len)
                )
            );
            assert_eq!(8, data_len);
            let mut data_ptr: *const u8 = std::ptr::null();
            assert_eq!(
                Result::Success,
                kitty_graphics_c::image_get(
                    kitty_graphics_c::image_get_handle(graphics, 2),
                    kitty_graphics_c::ImageData::DataPtr as c_int,
                    vp(&mut data_ptr)
                )
            );
            assert_eq!(&[0, 0, 0, 255, 0, 0, 0, 255], std::slice::from_raw_parts(data_ptr, 8));

            terminal_c::free(restored);
            decoder_free(decoder);
            terminal_c::free(source);
        }
    }
}
