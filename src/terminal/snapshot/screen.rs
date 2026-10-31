//! Port of `src/terminal/snapshot/screen.zig`.
//!
//! STATUS: ported
//!
//! SCREEN record payload encoding.
//!
//! One SCREEN record represents the live state for one terminal screen. The
//! record is followed immediately by the number of complete PAGE records
//! declared by `page_count`. They are the minimal suffix of native pages needed
//! to cover the active area and are ordered from oldest to newest. A decoder
//! uses the declared count, rather than another record tag, to find the end of
//! the screen's page sequence.
//!
//! The final screen-height rows are the active area. When its boundary falls
//! inside the first encoded page, that page also contains an incidental history
//! prefix. `history_rows` declares the complete logical history extent so a
//! client can size its scrollbar at READY without waiting for HISTORY. A client
//! may expose or ignore the resident overlap carried by SCREEN. The extent is
//! advisory; native row totals remain derived from decoded PAGE records.
//!
//! Screen dimensions are terminal-wide state and are not repeated here. Page
//! capacities, native page IDs and pointers, history availability, selection,
//! viewport, dirty state, and derived semantic-prompt state are also omitted.
//!
//! All integers are unsigned and little-endian.
//!
//! ## Binary Format
//!
//! A SCREEN record is followed immediately by its declared PAGE records:
//!
//! ```text
//!  +----------------------+
//!  | SCREEN record        |
//!  +----------------------+
//!  | PAGE record 0        |
//!  +----------------------+
//!  | ...                  |
//!  +----------------------+
//!  | PAGE record (n - 1)  |
//!  +----------------------+
//!
//!  n = page_count
//! ```
//!
//! The HISTORY record for this screen declares and sends the older complete
//! pages after the terminal becomes ready. The incidental prefix is already
//! present in SCREEN and is not sent again.
//!
//! The SCREEN payload begins with a fixed header. When the header says there is
//! no saved cursor, the payload is:
//!
//! ```text
//!   0 +----------------------+
//!     | Header               |
//!  53 +----------------------+
//!     | Cursor hyperlink     |
//!     | variable             |
//! end +----------------------+
//! ```
//!
//! When a saved cursor is present, it is inserted before the cursor hyperlink:
//!
//! ```text
//!   0 +----------------------+
//!     | Header               |
//!  53 +----------------------+
//!     | Saved cursor         |
//!  76 +----------------------+
//!     | Cursor hyperlink     |
//!     | variable             |
//! end +----------------------+
//! ```
//!
//! The cursor hyperlink begins with a one-byte kind. Zero means no hyperlink
//! and has no following bytes. Kinds one and two use the implicit and explicit
//! hyperlink encodings documented in `hyperlink.zig`.
//!
//! ### Header
//!
//! ```text
//!   0 +----------------------------------+
//!     | Screen key (u16)                 |
//!   2 +----------------------------------+
//!     | Page count (u16)                 |
//!   4 +----------------------------------+
//!     | Logical history rows (u64)       |
//!  12 +----------------------------------+
//!     | Cursor x (u16)                   |
//!  14 +----------------------------------+
//!     | Cursor y (u16)                   |
//!  16 +----------------------------------+
//!     | Cursor visual style (u8)         |
//!  17 +----------------------------------+
//!     | Cursor flags (u8)                |
//!  18 +----------------------------------+
//!     | Concrete cursor pen (Style)      |
//!  34 +----------------------------------+
//!     | Hyperlink implicit counter (u32) |
//!  38 +----------------------------------+
//!     | Current charset (CharsetState)   |
//!  40 +----------------------------------+
//!     | Protected mode (u8)              |
//!  41 +----------------------------------+
//!     | Kitty keyboard stack index (u8)  |
//!  42 +----------------------------------+
//!     | Kitty keyboard stack flags       |
//!     | 8 * u8                           |
//!  50 +----------------------------------+
//!     | Semantic-click kind (u8)         |
//!  51 +----------------------------------+
//!     | Semantic-click value (u8)        |
//!  52 +----------------------------------+
//!     | Saved-cursor-present (u8)        |
//!  53 +----------------------------------+
//! ```
//!
//! The concrete cursor pen uses the fixed style encoding from `style.zig`.
//!
//! ### Cursor flags
//!
//! ```text
//! bit 0 +--------------------------------+
//!       | Pending wrap                   |
//! bit 1 +--------------------------------+
//!       | Protected                      |
//! bit 2 +--------------------------------+
//!       | Semantic content               |
//!       | 2 bits                         |
//! bit 4 +--------------------------------+
//!       | Semantic-content-clear-EOL     |
//! bit 5 +--------------------------------+
//!       | Reserved, zero                 |
//!       | 3 bits                         |
//! bit 8 +--------------------------------+
//! ```
//!
//! ### Charset state
//!
//! The current and saved cursor charset states share this packed layout:
//!
//! ```text
//!  bit 0 +-------------------------------+
//!        | G0 charset                    |
//!        | 2 bits                        |
//!  bit 2 +-------------------------------+
//!        | G1 charset                    |
//!        | 2 bits                        |
//!  bit 4 +-------------------------------+
//!        | G2 charset                    |
//!        | 2 bits                        |
//!  bit 6 +-------------------------------+
//!        | G3 charset                    |
//!        | 2 bits                        |
//!  bit 8 +-------------------------------+
//!        | GL slot                       |
//!        | 2 bits                        |
//! bit 10 +-------------------------------+
//!        | GR slot                       |
//!        | 2 bits                        |
//! bit 12 +-------------------------------+
//!        | Single shift                  |
//!        | 3 bits                        |
//! bit 15 +-------------------------------+
//!        | Reserved, zero                |
//! bit 16 +-------------------------------+
//! ```
//!
//! The Kitty keyboard stack contains its current index followed by all eight
//! flag entries, including disabled entries.
//!
//! ### Saved cursor
//!
//! ```text
//!   0 +-----------------------------+
//!     | Saved cursor x (u16)        |
//!   2 +-----------------------------+
//!     | Saved cursor y (u16)        |
//!   4 +-----------------------------+
//!     | Saved cursor pen (Style)    |
//!  20 +-----------------------------+
//!     | Saved cursor flags (u8)     |
//!  21 +-----------------------------+
//!     | Saved charset state         |
//!  23 +-----------------------------+
//! ```
//!
//! ### Saved cursor flags
//!
//! ```text
//! bit 0 +---------------------------+
//!       | Protected                 |
//! bit 1 +---------------------------+
//!       | Pending wrap              |
//! bit 2 +---------------------------+
//!       | Origin                    |
//! bit 3 +---------------------------+
//!       | Reserved, zero            |
//!       | 5 bits                    |
//! bit 8 +---------------------------+
//! ```
//!
//! Native enum values used below are stable terminal registries. Their wire
//! widths are fixed independently by this payload. Changing those values or
//! any packed bit assignment requires a snapshot version bump.
//!
//! Notes on the port:
//! - `decode` takes a PageList `Allocator` (Zig: `alloc`) for the page list
//!   builder; the `std.Io` parameter is dropped (the native Screen has none).

use super::hyperlink;
use super::io::{self, Error};
use super::page;
use super::record;
use super::style;
use crate::terminal::ansi;
use crate::terminal::build_options;
use crate::terminal::charsets;
use crate::terminal::cursor::Style as CursorStyle;
use crate::terminal::hyperlink::{Hyperlink, HyperlinkId};
use crate::terminal::kitty;
use crate::terminal::osc::semantic_prompt as osc_sp;
use crate::terminal::page::SemanticContent;
use crate::terminal::page_list::{self, Allocator, PageList, Pin};
use crate::terminal::point::{self, Coordinate, Point};
use crate::terminal::screen::{
    CharsetState, Cursor, Dirty, Options, SavedCursor as TerminalSavedCursor, Screen, SemanticClick,
    SemanticClickKind, SemanticPrompt,
};
use crate::terminal::screen_set::Key as TerminalScreenKey;
use crate::terminal::style::Style as TerminalStyle;

/// Errors possible while encoding fixed SCREEN payload fields: hyperlink
/// encode errors, `InvalidCursorFlags`, `InvalidCharsetState`,
/// `InvalidKittyKeyboardIndex`, `InvalidKittyKeyboardFlags`,
/// `InvalidSavedCursorFlags`.
type PayloadEncodeError = Error;

/// Errors possible while encoding a SCREEN and its complete PAGE sequence,
/// including `PageCountOverflow` when the active area spans more pages than
/// the SCREEN header can declare.
pub type EncodeError = Error;

#[inline(always)]
fn nd<'a>(node: page_list::NodePtr) -> &'a mut page_list::Node {
    // SAFETY: node pointers come from the live PageList being walked.
    unsafe { &mut *node.as_ptr() }
}

/// Encode one SCREEN and its minimal suffix of complete native pages.
///
/// The suffix begins with the page containing the active area's first row and
/// ends with the newest page. Completed records may already be emitted if a
/// later record fails; the missing READY marker makes that prefix invalid.
pub fn encode(screen: &Screen, key: TerminalScreenKey, destination: &mut record::Writer<'_>) -> Result<(), EncodeError> {
    // The active top may fall inside this page, leaving an incidental history
    // prefix. Every earlier complete page is history and is omitted.
    let first = screen.pages.get_top_left(point::Tag::Active).node;
    let mut page_count: usize = 0;
    let mut node = Some(first);
    while let Some(current) = node {
        page_count += 1;
        node = nd(current).next;
    }
    let encoded_page_count = u16::try_from(page_count).map_err(|_| Error::PageCountOverflow)?;

    // SCREEN declares exactly how many immediately following PAGE records
    // belong to it.
    {
        let payload = destination.begin(record::Tag::SCREEN);
        if let Err(e) = encode_payload(screen, key, encoded_page_count, payload) {
            destination.cancel();
            return Err(e);
        }
        destination.finish()?;
    }

    // Active pages are resident today, but use the representation-safe access
    // path so a future PageList compression-policy change cannot turn this
    // wire encoder's optimization invariant into undefined behavior.
    node = Some(first);
    while let Some(current) = node {
        let mut preserved = nd(current).page_preserving_state(Allocator::GLOBAL)?;
        let result = page::encode(preserved.page(), destination);
        preserved.deinit();
        result?;
        node = nd(current).next;
    }
    Ok(())
}

/// Errors possible while restoring a SCREEN and its declared PAGE sequence,
/// including `UnexpectedRecordTag` (the record is not a SCREEN) and
/// `InvalidPageCount` (a SCREEN must declare at least one PAGE).
pub type DecodeError = Error;

/// One decoded SCREEN sequence and the native screen identified by its header.
pub struct Decoded {
    pub key: TerminalScreenKey,
    /// Expected history extent at READY, including resident SCREEN overlap.
    pub history_rows: u64,
    pub screen: Screen,
}

/// Restore one SCREEN and its declared PAGE records into native terminal state.
///
/// The caller supplies terminal-wide dimensions and policy. The record sequence
/// is decoded transactionally: on failure, all page, pin, hyperlink, and
/// temporary allocations are released and no partially restored Screen escapes.
/// The returned key selects the ScreenSet slot that owns the decoded screen.
///
/// `record_header` is the SCREEN record's header, already read from
/// `source`. A header for any other kind of record returns
/// `UnexpectedRecordTag`. This function reads the headers of the PAGE
/// records that follow itself. See "Decoding Records" in `record.rs`.
pub fn decode(
    source: &mut dyn io::Reader,
    record_header: record::Header,
    alloc: Allocator,
    options: &Options,
) -> Result<Decoded, DecodeError> {
    // Decode and finish the self-contained SCREEN record before consuming any
    // of the PAGE records which follow it.
    let (header, saved_cursor, cursor_hyperlink) = {
        let mut record_reader = record::Reader::init_header(source, record_header);
        if record_reader.header.tag != record::Tag::SCREEN {
            return Err(Error::UnexpectedRecordTag);
        }

        // The optional saved cursor and cursor hyperlink are both part of the
        // SCREEN payload. Keep ownership of the decoded hyperlink locally until
        // it has been copied into the native Screen.
        let payload_reader = record_reader.payload_reader();
        let header = Header::decode(payload_reader)?;
        let saved_cursor = if header.saved_cursor_present {
            Some(SavedCursor::decode(payload_reader)?)
        } else {
            None
        };
        let cursor_hyperlink = decode_cursor_hyperlink(payload_reader)?;
        record_reader.finish()?;
        (header, saved_cursor, cursor_hyperlink)
    };

    let key = header.key;
    if header.page_count == 0 {
        return Err(Error::InvalidPageCount);
    }

    // Dimensions are terminal-wide state supplied by the enclosing snapshot.
    // Validate them, and all cursor invariants which depend on them, before
    // allocating the declared pages.
    if options.cols == 0 || options.rows == 0 {
        return Err(Error::InvalidDimensions);
    }

    // Build the native page list transactionally. Alternate screens never
    // retain scrollback, while primary screens inherit the caller's limits.
    let mut result: Screen = {
        let mut builder = page_list::Builder::init_alloc(
            alloc,
            page_list::Options {
                cols: options.cols,
                rows: options.rows,
                max_size: if key == TerminalScreenKey::Alternate {
                    Some(0)
                } else {
                    options.max_scrollback_bytes
                },
                max_lines: if key == TerminalScreenKey::Alternate {
                    Some(0)
                } else {
                    options.max_scrollback_lines
                },
            },
        )?;

        // Allocate each PAGE at its declared capacity and decode directly into
        // builder-owned storage, avoiding an intermediate page representation.
        for _ in 0..header.page_count {
            let page_header = record::next_header(source)?;
            let mut decoder = page::Decoder::init(source, page_header)?;
            let native_page = builder.allocate_page(decoder.capacity())?;
            decoder.decode(native_page)?;
        }

        // Finishing validates that the decoded suffix covers the active area
        // and establishes the PageList's active viewport.
        let mut pages: PageList = builder.finish()?;

        // Convert the payload-relative cursor position into the tracked native
        // pin and page-local row/cell pointers required by Screen.
        let cursor: Cursor = {
            let y = header.cursor_y.min(options.rows - 1);

            // The active area can temporarily contain mixed-width pages after
            // a reflow. Locate the row at column zero, then clamp the encoded
            // x coordinate to the width of that physical page.
            let row_pin = pages
                .pin(Point::Active(Coordinate { x: 0, y: y as u32 }))
                .expect("active row exists");
            let node_cols = nd(row_pin.node).cols();
            let x = header.cursor_x.min(node_cols - 1);
            let mut pin = Pin::new(row_pin.node);
            pin.x = x;
            pin.y = row_pin.y;
            let tracked = pages.track_pin(pin)?;
            // SAFETY: the pin was just tracked in `pages`.
            let rac = unsafe { tracked.as_ref() }.row_and_cell();

            let mut cursor = Cursor::new(tracked, rac.row, rac.cell);
            cursor.x = x;
            cursor.y = y;
            cursor.cursor_style = header.cursor_style;
            cursor.pending_wrap = header.cursor_flags.pending_wrap && x == node_cols - 1;
            cursor.protected = header.cursor_flags.protected;
            cursor.style = header.cursor_pen;
            cursor.hyperlink_implicit_id = header.hyperlink_implicit_id;
            cursor.semantic_content = header.cursor_flags.semantic_content;
            cursor.semantic_content_clear_eol = header.cursor_flags.semantic_content_clear_eol;
            cursor
        };

        // Assemble the remaining native state from stable snapshot registries.
        // Derived state and page-local table references are repaired below.
        Screen {
            pages,
            no_scrollback: key == TerminalScreenKey::Alternate || options.max_scrollback_bytes == Some(0),
            cursor,
            saved_cursor: saved_cursor.map(|value| value.terminal(options.cols, options.rows)),
            selection: None,
            charset: header.charset,
            protected_mode: header.protected_mode,
            kitty_keyboard: header.kitty_keyboard.terminal(),
            kitty_images: Default::default(),
            semantic_prompt: SemanticPrompt {
                seen: false,
                click: header.semantic_click,
            },
            dirty: Dirty::default(),
        }
    };

    // Reinsert the concrete cursor style into its page-local style table. This
    // existing Screen path grows or splits the page when the decoded table is
    // already full.
    result.manual_style_update()?;

    // Reinsert the cursor hyperlink into its page-local hyperlink table. For an
    // implicit link, temporarily seed the counter with the encoded link ID,
    // then restore the independent next-ID counter from the header.
    if let Some(value) = cursor_hyperlink {
        match &value.id {
            HyperlinkId::Explicit(id) => result.start_hyperlink(&value.uri, Some(id))?,
            HyperlinkId::Implicit(id) => {
                result.cursor.hyperlink_implicit_id = *id;
                result.start_hyperlink(&value.uri, None)?;
                result.cursor.hyperlink_implicit_id = header.hyperlink_implicit_id;
            }
        }
    }

    // `seen` is derived state. Only resident prompt metadata participates
    // because omitted history is outside the restored Screen model.
    result.semantic_prompt.seen = 'seen: {
        if result.cursor.semantic_content == SemanticContent::Prompt {
            break 'seen true;
        }

        let mut node = result.pages.get_top_left(point::Tag::Screen).node;
        loop {
            let native_page = nd(node).page_assume_resident();
            for y in 0..native_page.size.rows as usize {
                // SAFETY: `y` is within the page; its cells are live.
                let row = unsafe { &*native_page.get_row(y) };
                if row.semantic_prompt() != crate::terminal::page::SemanticPrompt::None {
                    break 'seen true;
                }

                // SAFETY: as above.
                let cells = unsafe { &*native_page.get_cells(row) };
                for cell in cells {
                    if cell.semantic_content() == SemanticContent::Prompt {
                        break 'seen true;
                    }
                }
            }

            match nd(node).next {
                Some(next) => node = next,
                None => break 'seen false,
            }
        }
    };

    // Kitty image payloads are outside this record, but the restored Screen
    // must still receive the caller's terminal-wide storage policy.
    if build_options::KITTY_GRAPHICS {
        let Screen {
            pages, kitty_images, ..
        } = &mut result;
        kitty_images.set_limit(pages, options.kitty_image_storage_limit);
        kitty_images.image_limits = options.kitty_image_loading_limits.clone();
    }

    // All fallible reconstruction is complete; verify the native invariants
    // before transferring ownership to the caller.
    result.pages.assert_integrity();
    result.assert_integrity();
    Ok(Decoded {
        key,
        history_rows: header.history_rows,
        screen: result,
    })
}

/// Errors possible while decoding fixed SCREEN payload fields: read errors
/// and `InvalidKey`.
type PayloadDecodeError = Error;

/// Flags encoded after the cursor's visual shape (Zig: `packed struct(u8)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorFlags {
    pub pending_wrap: bool,
    pub protected: bool,
    pub semantic_content: SemanticContent,
    pub semantic_content_clear_eol: bool,
    /// u3
    pub _padding: u8,
}

impl Default for CursorFlags {
    fn default() -> Self {
        CursorFlags {
            pending_wrap: false,
            protected: false,
            semantic_content: SemanticContent::Output,
            semantic_content_clear_eol: false,
            _padding: 0,
        }
    }
}

impl CursorFlags {
    /// Bit offset of `semantic_content`.
    const SEMANTIC_CONTENT_OFFSET: u32 = 2;

    pub fn to_bits(self) -> u8 {
        (self.pending_wrap as u8)
            | (self.protected as u8) << 1
            | (self.semantic_content as u8) << Self::SEMANTIC_CONTENT_OFFSET
            | (self.semantic_content_clear_eol as u8) << 4
            | (self._padding & 0x7) << 5
    }

    fn init(cursor: &Cursor) -> CursorFlags {
        CursorFlags {
            pending_wrap: cursor.pending_wrap,
            protected: cursor.protected,
            semantic_content: cursor.semantic_content,
            semantic_content_clear_eol: cursor.semantic_content_clear_eol,
            _padding: 0,
        }
    }

    /// Decode the cursor flag registry, normalizing unknown semantic state.
    pub fn decode(reader: &mut dyn io::Reader) -> io::Result<CursorFlags> {
        let raw = reader.take_byte()?;
        let semantic_content_raw = (raw >> Self::SEMANTIC_CONTENT_OFFSET) & 0x3;
        let semantic_content = semantic_content_from_int(semantic_content_raw).unwrap_or(SemanticContent::Output);

        Ok(CursorFlags {
            pending_wrap: raw & 1 != 0,
            protected: raw & 2 != 0,
            semantic_content,
            semantic_content_clear_eol: raw & (1 << 4) != 0,
            _padding: 0,
        })
    }
}

fn semantic_content_from_int(v: u8) -> Option<SemanticContent> {
    match v {
        0 => Some(SemanticContent::Output),
        1 => Some(SemanticContent::Input),
        2 => Some(SemanticContent::Prompt),
        _ => None,
    }
}

fn slot_from_int(v: u64) -> Option<charsets::Slots> {
    charsets::Slots::ALL.get(usize::try_from(v).ok()?).copied()
}

fn charset_from_int(v: u64) -> Option<charsets::Charset> {
    charsets::Charset::ALL.get(usize::try_from(v).ok()?).copied()
}

fn key_from_int(v: u64) -> Option<TerminalScreenKey> {
    TerminalScreenKey::ALL.get(usize::try_from(v).ok()?).copied()
}

fn cursor_style_from_int(v: u64) -> Option<CursorStyle> {
    match v {
        0 => Some(CursorStyle::Bar),
        1 => Some(CursorStyle::Block),
        2 => Some(CursorStyle::Underline),
        3 => Some(CursorStyle::BlockHollow),
        _ => None,
    }
}

fn protected_mode_from_int(v: u64) -> Option<ansi::ProtectedMode> {
    match v {
        0 => Some(ansi::ProtectedMode::Off),
        1 => Some(ansi::ProtectedMode::Iso),
        2 => Some(ansi::ProtectedMode::Dec),
        _ => None,
    }
}

fn click_kind_from_int(v: u64) -> Option<SemanticClickKind> {
    match v {
        0 => Some(SemanticClickKind::None),
        1 => Some(SemanticClickKind::ClickEvents),
        2 => Some(SemanticClickKind::Cl),
        _ => None,
    }
}

fn click_events_from_int(v: u64) -> Option<osc_sp::ClickEvents> {
    match v {
        0 => Some(osc_sp::ClickEvents::Absolute),
        1 => Some(osc_sp::ClickEvents::Relative),
        _ => None,
    }
}

fn click_from_int(v: u64) -> Option<osc_sp::Click> {
    match v {
        0 => Some(osc_sp::Click::Line),
        1 => Some(osc_sp::Click::Multiple),
        2 => Some(osc_sp::Click::ConservativeVertical),
        3 => Some(osc_sp::Click::SmartVertical),
        _ => None,
    }
}

/// The packed wire bits shared by current and saved cursor charset state
/// (Zig: `packed struct(u16)`).
///
/// Native enum values are stable, but their backing type is `c_int` in C ABI
/// builds. The wire therefore stores only checked integer values at its fixed
/// bit widths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CharsetBits {
    g0: u8,
    g1: u8,
    g2: u8,
    g3: u8,
    gl: u8,
    gr: u8,
    /// u3
    single_shift: u8,
}

impl Default for CharsetBits {
    fn default() -> Self {
        CharsetBits {
            g0: 0,
            g1: 0,
            g2: 0,
            g3: 0,
            gl: 0,
            gr: 2,
            single_shift: 0,
        }
    }
}

impl CharsetBits {
    fn to_bits(self) -> u16 {
        (self.g0 as u16 & 0x3)
            | (self.g1 as u16 & 0x3) << 2
            | (self.g2 as u16 & 0x3) << 4
            | (self.g3 as u16 & 0x3) << 6
            | (self.gl as u16 & 0x3) << 8
            | (self.gr as u16 & 0x3) << 10
            | (self.single_shift as u16 & 0x7) << 12
    }

    fn from_bits(raw: u16) -> CharsetBits {
        CharsetBits {
            g0: (raw & 0x3) as u8,
            g1: ((raw >> 2) & 0x3) as u8,
            g2: ((raw >> 4) & 0x3) as u8,
            g3: ((raw >> 6) & 0x3) as u8,
            gl: ((raw >> 8) & 0x3) as u8,
            gr: ((raw >> 10) & 0x3) as u8,
            single_shift: ((raw >> 12) & 0x7) as u8,
        }
    }
}

/// Checked narrowing of a native enum value into a two-bit wire field.
fn cast_u2(v: u8) -> Result<u8, Error> {
    if v > 3 {
        return Err(Error::InvalidCharsetState);
    }
    Ok(v)
}

fn encode_charset_state(value: &CharsetState) -> Result<u16, Error> {
    let g0 = cast_u2(value.charsets.get(charsets::Slots::G0) as u8)?;
    let g1 = cast_u2(value.charsets.get(charsets::Slots::G1) as u8)?;
    let g2 = cast_u2(value.charsets.get(charsets::Slots::G2) as u8)?;
    let g3 = cast_u2(value.charsets.get(charsets::Slots::G3) as u8)?;
    let gl = cast_u2(value.gl as u8)?;
    let gr = cast_u2(value.gr as u8)?;
    let single_shift = match value.single_shift {
        Some(slot) => cast_u2(slot as u8)? + 1,
        None => 0,
    };

    let bits = CharsetBits {
        g0,
        g1,
        g2,
        g3,
        gl,
        gr,
        single_shift,
    };
    Ok(bits.to_bits())
}

fn decode_charset_state(raw: u16) -> CharsetState {
    let bits = CharsetBits::from_bits(raw);

    let mut result = CharsetState::default();
    result.gl = slot_from_int(bits.gl as u64).unwrap_or(result.gl);
    result.gr = slot_from_int(bits.gr as u64).unwrap_or(result.gr);
    result.single_shift = if bits.single_shift == 0 {
        None
    } else if bits.single_shift <= 4 {
        slot_from_int(bits.single_shift as u64 - 1)
    } else {
        None
    };
    result.charsets.set(
        charsets::Slots::G0,
        charset_from_int(bits.g0 as u64).unwrap_or(charsets::Charset::Utf8),
    );
    result.charsets.set(
        charsets::Slots::G1,
        charset_from_int(bits.g1 as u64).unwrap_or(charsets::Charset::Utf8),
    );
    result.charsets.set(
        charsets::Slots::G2,
        charset_from_int(bits.g2 as u64).unwrap_or(charsets::Charset::Utf8),
    );
    result.charsets.set(
        charsets::Slots::G3,
        charset_from_int(bits.g3 as u64).unwrap_or(charsets::Charset::Utf8),
    );
    result
}

/// The complete fixed-size Kitty keyboard stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KittyKeyboard {
    pub index: u8,
    pub flags: [KittyKeyboardFlags; 8],
}

/// One byte in the fixed Kitty keyboard flag stack
/// (Zig: `KittyKeyboard.Flags`, `packed struct(u8)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KittyKeyboardFlags {
    pub disambiguate: bool,
    pub report_events: bool,
    pub report_alternates: bool,
    pub report_all: bool,
    pub report_associated: bool,
    /// u3
    pub _padding: u8,
}

impl KittyKeyboardFlags {
    pub fn to_bits(self) -> u8 {
        (self.disambiguate as u8)
            | (self.report_events as u8) << 1
            | (self.report_alternates as u8) << 2
            | (self.report_all as u8) << 3
            | (self.report_associated as u8) << 4
            | (self._padding & 0x7) << 5
    }

    pub fn from_bits(raw: u8) -> KittyKeyboardFlags {
        KittyKeyboardFlags {
            disambiguate: raw & 1 != 0,
            report_events: raw & 2 != 0,
            report_alternates: raw & 4 != 0,
            report_all: raw & 8 != 0,
            report_associated: raw & 16 != 0,
            _padding: (raw >> 5) & 0x7,
        }
    }

    /// Decode one Kitty keyboard flag byte, ignoring reserved bits.
    pub fn decode(reader: &mut dyn io::Reader) -> io::Result<KittyKeyboardFlags> {
        let mut result = KittyKeyboardFlags::from_bits(reader.take_byte()?);
        result._padding = 0;
        Ok(result)
    }
}

impl KittyKeyboard {
    fn init(value: &kitty::KeyFlagStack) -> KittyKeyboard {
        let mut result = KittyKeyboard {
            index: value.idx,
            ..Default::default()
        };
        for (native, snapshot) in value.flags.iter().zip(result.flags.iter_mut()) {
            *snapshot = KittyKeyboardFlags {
                disambiguate: native.disambiguate,
                report_events: native.report_events,
                report_alternates: native.report_alternates,
                report_all: native.report_all,
                report_associated: native.report_associated,
                _padding: 0,
            };
        }
        result
    }

    fn terminal(&self) -> kitty::KeyFlagStack {
        let mut result = kitty::KeyFlagStack {
            idx: self.index,
            ..Default::default()
        };
        for (snapshot, native) in self.flags.iter().zip(result.flags.iter_mut()) {
            *native = kitty::KeyFlags {
                disambiguate: snapshot.disambiguate,
                report_events: snapshot.report_events,
                report_alternates: snapshot.report_alternates,
                report_all: snapshot.report_all,
                report_associated: snapshot.report_associated,
            };
        }
        result
    }

    /// Decode the complete fixed Kitty keyboard stack.
    pub fn decode(reader: &mut dyn io::Reader) -> io::Result<KittyKeyboard> {
        let index_raw = reader.take_byte()?;
        let index = if index_raw < 8 { index_raw } else { 0 };

        let mut flags = [KittyKeyboardFlags::default(); 8];
        for entry in &mut flags {
            *entry = KittyKeyboardFlags::decode(reader)?;
        }
        Ok(KittyKeyboard { index, flags })
    }
}

/// Decode semantic-click state, falling back to disabled for unknown values.
fn decode_semantic_click(reader: &mut dyn io::Reader) -> io::Result<SemanticClick> {
    let kind = click_kind_from_int(reader.take_byte()? as u64);
    let value = reader.take_byte()? as u64;

    Ok(match kind {
        None | Some(SemanticClickKind::None) => SemanticClick::None,
        Some(SemanticClickKind::ClickEvents) => match click_events_from_int(value) {
            Some(v) => SemanticClick::ClickEvents(v),
            None => SemanticClick::None,
        },
        Some(SemanticClickKind::Cl) => match click_from_int(value) {
            Some(v) => SemanticClick::Cl(v),
            None => SemanticClick::None,
        },
    })
}

/// The optional fixed-size saved cursor state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SavedCursor {
    pub x: u16,
    pub y: u16,
    pub pen: TerminalStyle,
    pub flags: SavedCursorFlags,
    pub charset: CharsetState,
}

/// Flags encoded with an optional saved cursor
/// (Zig: `SavedCursor.Flags`, `packed struct(u8)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SavedCursorFlags {
    pub protected: bool,
    pub pending_wrap: bool,
    pub origin: bool,
    /// u5
    pub _padding: u8,
}

impl SavedCursorFlags {
    pub fn to_bits(self) -> u8 {
        (self.protected as u8) | (self.pending_wrap as u8) << 1 | (self.origin as u8) << 2 | (self._padding & 0x1f) << 3
    }

    /// Decode saved cursor flags, ignoring reserved bits.
    pub fn decode(reader: &mut dyn io::Reader) -> io::Result<SavedCursorFlags> {
        let raw = reader.take_byte()?;
        Ok(SavedCursorFlags {
            protected: raw & 1 != 0,
            pending_wrap: raw & 2 != 0,
            origin: raw & 4 != 0,
            _padding: 0,
        })
    }
}

impl SavedCursor {
    /// Number of bytes written by `encode`.
    pub const LEN: usize = 23;

    fn init(value: &TerminalSavedCursor) -> SavedCursor {
        SavedCursor {
            x: value.x,
            y: value.y,
            pen: value.style,
            flags: SavedCursorFlags {
                protected: value.protected,
                pending_wrap: value.pending_wrap,
                origin: value.origin,
                _padding: 0,
            },
            charset: value.charset,
        }
    }

    fn terminal(&self, cols: u16, rows: u16) -> TerminalSavedCursor {
        let x = self.x.min(cols - 1);
        TerminalSavedCursor {
            x,
            y: self.y.min(rows - 1),
            style: self.pen,
            protected: self.flags.protected,
            pending_wrap: self.flags.pending_wrap && x == cols - 1,
            origin: self.flags.origin,
            charset: self.charset,
        }
    }

    /// Encode one optional saved cursor value.
    pub fn encode(&self, writer: &mut dyn io::Writer) -> Result<(), PayloadEncodeError> {
        if self.flags._padding != 0 {
            return Err(Error::InvalidSavedCursorFlags);
        }
        let charset = encode_charset_state(&self.charset)?;

        io::write_int::<u16>(writer, self.x)?;
        io::write_int::<u16>(writer, self.y)?;
        style::encode(self.pen, writer)?;
        writer.write_byte(self.flags.to_bits())?;
        io::write_int::<u16>(writer, charset)?;
        Ok(())
    }

    /// Decode and validate one optional saved cursor value.
    pub fn decode(reader: &mut dyn io::Reader) -> Result<SavedCursor, PayloadDecodeError> {
        Ok(SavedCursor {
            x: io::read_int(reader)?,
            y: io::read_int(reader)?,
            pen: style::decode_or_null(reader)?.unwrap_or_default(),
            flags: SavedCursorFlags::decode(reader)?,
            charset: decode_charset_state(io::read_int(reader)?),
        })
    }
}

/// The fixed fields at the start of every SCREEN payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Header {
    pub key: TerminalScreenKey,
    /// Complete pages in the minimal suffix covering the active area.
    pub page_count: u16,
    /// Advisory logical rows before the active area, including resident
    /// SCREEN overlap.
    pub history_rows: u64,
    pub cursor_x: u16,
    pub cursor_y: u16,
    pub cursor_style: CursorStyle,
    pub cursor_flags: CursorFlags,
    pub cursor_pen: TerminalStyle,
    pub hyperlink_implicit_id: u32,
    pub charset: CharsetState,
    pub protected_mode: ansi::ProtectedMode,
    pub kitty_keyboard: KittyKeyboard,
    pub semantic_click: SemanticClick,
    pub saved_cursor_present: bool,
}

impl Header {
    /// Number of bytes written by `encode`.
    pub const LEN: usize = 53;

    /// Initialize the fixed header from one native screen.
    pub(crate) fn init(screen: &Screen, key: TerminalScreenKey, page_count: u16) -> Header {
        Header {
            key,
            page_count,
            history_rows: (screen.pages.total_rows - screen.pages.rows as usize) as u64,
            cursor_x: screen.cursor.x,
            cursor_y: screen.cursor.y,
            cursor_style: screen.cursor.cursor_style,
            cursor_flags: CursorFlags::init(&screen.cursor),
            cursor_pen: screen.cursor.style,
            hyperlink_implicit_id: screen.cursor.hyperlink_implicit_id,
            charset: screen.charset,
            protected_mode: screen.protected_mode,
            kitty_keyboard: KittyKeyboard::init(&screen.kitty_keyboard),
            semantic_click: screen.semantic_prompt.click,
            saved_cursor_present: screen.saved_cursor.is_some(),
        }
    }

    /// Encode the fixed SCREEN payload header field by field.
    pub fn encode(&self, writer: &mut dyn io::Writer) -> Result<(), PayloadEncodeError> {
        // Validate packed cursor state before writing any bytes.
        if self.cursor_flags._padding != 0 {
            return Err(Error::InvalidCursorFlags);
        }

        // Validate and narrow native charset enum values before writing.
        let charset = encode_charset_state(&self.charset)?;

        // Validate the complete Kitty keyboard stack.
        if self.kitty_keyboard.index as usize >= self.kitty_keyboard.flags.len() {
            return Err(Error::InvalidKittyKeyboardIndex);
        }
        for flags in &self.kitty_keyboard.flags {
            if flags._padding != 0 {
                return Err(Error::InvalidKittyKeyboardFlags);
            }
        }

        // Screen identity, expected history extent, and cursor position.
        io::write_int::<u16>(writer, self.key as u16)?;
        io::write_int::<u16>(writer, self.page_count)?;
        io::write_int::<u64>(writer, self.history_rows)?;
        io::write_int::<u16>(writer, self.cursor_x)?;
        io::write_int::<u16>(writer, self.cursor_y)?;

        // Current cursor rendering state.
        writer.write_byte(self.cursor_style as u8)?;
        writer.write_byte(self.cursor_flags.to_bits())?;
        style::encode(self.cursor_pen, writer)?;
        io::write_int::<u32>(writer, self.hyperlink_implicit_id)?;

        // Charset and selective-erase state.
        io::write_int::<u16>(writer, charset)?;
        writer.write_byte(self.protected_mode as u8)?;

        // Keyboard modes and semantic-click state.
        writer.write_byte(self.kitty_keyboard.index)?;
        for flags in &self.kitty_keyboard.flags {
            writer.write_byte(flags.to_bits())?;
        }
        writer.write_byte(self.semantic_click.kind() as u8)?;
        match self.semantic_click {
            SemanticClick::None => writer.write_byte(0)?,
            SemanticClick::ClickEvents(value) => writer.write_byte(value as u8)?,
            SemanticClick::Cl(value) => writer.write_byte(value as u8)?,
        }

        // Presence of the optional saved-cursor suffix.
        writer.write_byte(self.saved_cursor_present as u8)?;
        Ok(())
    }

    /// Decode the fixed SCREEN payload header.
    ///
    /// The screen key remains structural because it routes the following PAGE
    /// sequence. Unknown semantic fields use native defaults instead.
    pub fn decode(reader: &mut dyn io::Reader) -> Result<Header, PayloadDecodeError> {
        // Screen identity, expected history extent, and cursor position.
        let key = key_from_int(io::read_int::<u16>(reader)? as u64).ok_or(Error::InvalidKey)?;
        let page_count = io::read_int::<u16>(reader)?;
        let history_rows = io::read_int::<u64>(reader)?;
        let cursor_x = io::read_int::<u16>(reader)?;
        let cursor_y = io::read_int::<u16>(reader)?;

        // Current cursor rendering state.
        let cursor_style = cursor_style_from_int(reader.take_byte()? as u64).unwrap_or(CursorStyle::Block);
        let cursor_flags = CursorFlags::decode(reader)?;
        let cursor_pen = style::decode_or_null(reader)?.unwrap_or_default();
        let hyperlink_implicit_id = io::read_int::<u32>(reader)?;

        // Charset and selective-erase state.
        let charset = decode_charset_state(io::read_int::<u16>(reader)?);
        let protected_mode = protected_mode_from_int(reader.take_byte()? as u64).unwrap_or(ansi::ProtectedMode::Off);

        // Keyboard modes and semantic-click state.
        let kitty_keyboard = KittyKeyboard::decode(reader)?;
        let semantic_click = decode_semantic_click(reader)?;

        // Presence of the optional saved-cursor suffix.
        let saved_cursor_present = reader.take_byte()? != 0;

        Ok(Header {
            key,
            page_count,
            history_rows,
            cursor_x,
            cursor_y,

            cursor_style,
            cursor_flags,
            cursor_pen,
            hyperlink_implicit_id,

            charset,
            protected_mode,

            kitty_keyboard,
            semantic_click,

            saved_cursor_present,
        })
    }
}

/// Encode the SCREEN payload, excluding its following PAGE records.
fn encode_payload(
    screen: &Screen,
    key: TerminalScreenKey,
    page_count: u16,
    writer: &mut dyn io::Writer,
) -> Result<(), PayloadEncodeError> {
    Header::init(screen, key, page_count).encode(writer)?;

    if let Some(saved_cursor) = &screen.saved_cursor {
        SavedCursor::init(saved_cursor).encode(writer)?;
    }

    encode_cursor_hyperlink(screen.cursor.hyperlink.as_deref(), writer)
}

/// The variable cursor hyperlink at the end of every SCREEN payload.
pub type CursorHyperlink = Option<Hyperlink<'static>>;

/// Encode the optional cursor hyperlink at the end of a SCREEN payload.
pub fn encode_cursor_hyperlink(value: Option<&Hyperlink<'_>>, writer: &mut dyn io::Writer) -> Result<(), Error> {
    if let Some(entry) = value {
        return hyperlink::encode(entry, writer);
    }
    writer.write_byte(0)
}

/// Decode one owned optional cursor hyperlink.
pub fn decode_cursor_hyperlink(reader: &mut dyn io::Reader) -> io::Result<CursorHyperlink> {
    if reader.peek_byte()? == 0 {
        reader.take_byte()?;
        return Ok(None);
    }

    match hyperlink::decode(reader) {
        Ok(value) => Ok(Some(value)),
        Err(Error::ReadFailed) => Err(Error::ReadFailed),

        // The cursor hyperlink is the final SCREEN payload field, so its
        // record boundary lets us discard an invalid or unrepresentable value
        // without losing the following PAGE sequence.
        Err(
            Error::EndOfStream | Error::OutOfMemory | Error::InvalidKind | Error::InvalidUri | Error::InvalidExplicitId,
        ) => {
            reader.discard_remaining()?;
            Ok(None)
        }
        Err(other) => unreachable!("unexpected hyperlink decode error {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::color::Rgb;
    use crate::terminal::page::Page;
    use crate::terminal::point::Tag;
    use crate::terminal::sgr;
    use crate::terminal::snapshot::fixture;
    use crate::terminal::snapshot::grid;
    use crate::terminal::snapshot::io::{FixedReader, FixedWriter, Limited, Reader as _, Writer as _};
    use crate::terminal::style::{Color, Flags};

    fn header_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/screen-header-v1.hex"))
    }
    fn saved_cursor_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/screen-saved-cursor-v1.hex"))
    }

    fn opts(cols: u16, rows: u16, max_scrollback_bytes: Option<usize>) -> Options {
        Options {
            cols,
            rows,
            max_scrollback_bytes,
            ..Options::default()
        }
    }

    fn test_charset_state() -> CharsetState {
        let mut result = CharsetState {
            gl: charsets::Slots::G1,
            gr: charsets::Slots::G3,
            single_shift: Some(charsets::Slots::G2),
            ..Default::default()
        };
        result.charsets.set(charsets::Slots::G0, charsets::Charset::Utf8);
        result.charsets.set(charsets::Slots::G1, charsets::Charset::Ascii);
        result.charsets.set(charsets::Slots::G2, charsets::Charset::British);
        result.charsets.set(charsets::Slots::G3, charsets::Charset::DecSpecial);
        result
    }

    fn test_pen() -> TerminalStyle {
        TerminalStyle {
            fg_color: Color::None,
            bg_color: Color::Palette(0x7f),
            underline_color: Color::Rgb(Rgb {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            }),
            flags: Flags {
                bold: true,
                italic: true,
                faint: true,
                blink: true,
                inverse: true,
                invisible: true,
                strikethrough: true,
                overline: true,
                underline: sgr::Underline::Curly,
            },
        }
    }

    fn kf(d: bool, e: bool, a: bool, all: bool, assoc: bool) -> KittyKeyboardFlags {
        KittyKeyboardFlags {
            disambiguate: d,
            report_events: e,
            report_alternates: a,
            report_all: all,
            report_associated: assoc,
            _padding: 0,
        }
    }

    fn test_kitty_flags() -> [KittyKeyboardFlags; 8] {
        [
            kf(true, false, false, false, false),
            kf(false, true, false, false, false),
            kf(false, false, true, false, false),
            kf(false, false, false, true, false),
            kf(false, false, false, false, true),
            kf(true, true, true, true, true),
            kf(false, false, false, false, false),
            kf(true, false, false, false, true),
        ]
    }

    fn test_header() -> Header {
        Header {
            key: TerminalScreenKey::Alternate,
            page_count: 0x0203,
            history_rows: 0x0102030405060708,
            cursor_x: 0x0405,
            cursor_y: 0x0607,
            cursor_style: CursorStyle::BlockHollow,
            cursor_flags: CursorFlags {
                pending_wrap: true,
                semantic_content: SemanticContent::Prompt,
                semantic_content_clear_eol: true,
                ..Default::default()
            },
            cursor_pen: test_pen(),
            hyperlink_implicit_id: 0x0a0b0c0d,
            charset: test_charset_state(),
            protected_mode: ansi::ProtectedMode::Dec,
            kitty_keyboard: KittyKeyboard {
                index: 7,
                flags: test_kitty_flags(),
            },
            semantic_click: SemanticClick::Cl(osc_sp::Click::SmartVertical),
            saved_cursor_present: true,
        }
    }

    fn test_saved_cursor() -> SavedCursor {
        SavedCursor {
            x: 0x0102,
            y: 0x0304,
            pen: TerminalStyle::default(),
            flags: SavedCursorFlags {
                protected: true,
                pending_wrap: true,
                origin: true,
                _padding: 0,
            },
            charset: test_charset_state(),
        }
    }

    fn decode_all(bytes: &[u8], options: &Options) -> Result<Decoded, Error> {
        let mut source = FixedReader::new(bytes);
        let header = record::next_header(&mut source)?;
        decode(&mut source, header, Allocator::GLOBAL, options)
    }

    fn active_page(screen: &Screen) -> &'static Page {
        let node = screen.pages.get_top_left(Tag::Active).node;
        nd(node).page_assume_resident()
    }

    #[test]
    fn screen_header_golden_encoding_and_decoding() {
        let mut encoded = [0u8; Header::LEN];
        let mut writer = FixedWriter::new(&mut encoded);
        test_header().encode(&mut writer).unwrap();

        let reference = header_fixture();
        fixture::expect_equal(
            fixture::Kind::Bytes,
            "src/terminal/snapshot/testdata/screen-header-v1.hex",
            "snapshot_fixture-screen-header-v1.hex",
            &reference,
            writer.buffered(),
        );
        assert_eq!(Header::LEN, reference.len());

        let mut limited = Limited::new(FixedReader::new(&reference), None, 1);
        assert_eq!(test_header(), Header::decode(&mut limited).unwrap());
    }

    #[test]
    fn native_enum_values_used_by_the_screen_format() {
        for (expected, value) in [TerminalScreenKey::Primary, TerminalScreenKey::Alternate].into_iter().enumerate() {
            assert_eq!(expected, value as usize);
        }
        for (expected, value) in [
            CursorStyle::Bar,
            CursorStyle::Block,
            CursorStyle::Underline,
            CursorStyle::BlockHollow,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(expected, value as usize);
        }
        for (expected, value) in [
            SemanticContent::Output,
            SemanticContent::Input,
            SemanticContent::Prompt,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(expected, value as usize);
        }
        for (expected, value) in charsets::Charset::ALL.into_iter().enumerate() {
            assert_eq!(expected, value as usize);
        }
        assert_eq!(charsets::Charset::Utf8, charsets::Charset::ALL[0]);
        assert_eq!(charsets::Charset::Ascii, charsets::Charset::ALL[1]);
        assert_eq!(charsets::Charset::British, charsets::Charset::ALL[2]);
        assert_eq!(charsets::Charset::DecSpecial, charsets::Charset::ALL[3]);
        for (expected, value) in charsets::Slots::ALL.into_iter().enumerate() {
            assert_eq!(expected, value as usize);
        }
        for (expected, value) in [
            ansi::ProtectedMode::Off,
            ansi::ProtectedMode::Iso,
            ansi::ProtectedMode::Dec,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(expected, value as usize);
        }
        for (expected, value) in [
            SemanticClickKind::None,
            SemanticClickKind::ClickEvents,
            SemanticClickKind::Cl,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(expected, value as usize);
        }
        for (expected, value) in [osc_sp::ClickEvents::Absolute, osc_sp::ClickEvents::Relative]
            .into_iter()
            .enumerate()
        {
            assert_eq!(expected, value as usize);
        }
        for (expected, value) in [
            osc_sp::Click::Line,
            osc_sp::Click::Multiple,
            osc_sp::Click::ConservativeVertical,
            osc_sp::Click::SmartVertical,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(expected, value as usize);
        }
    }

    #[test]
    fn cursor_kitty_and_saved_cursor_flag_bit_layouts() {
        let d = CursorFlags::default();
        let cursor_cases = [
            (CursorFlags { pending_wrap: true, ..d }, 1u8 << 0),
            (CursorFlags { protected: true, ..d }, 1 << 1),
            (
                CursorFlags {
                    semantic_content: SemanticContent::Input,
                    ..d
                },
                1 << 2,
            ),
            (
                CursorFlags {
                    semantic_content: SemanticContent::Prompt,
                    ..d
                },
                2 << 2,
            ),
            (
                CursorFlags {
                    semantic_content_clear_eol: true,
                    ..d
                },
                1 << 4,
            ),
        ];
        for (value, expected) in cursor_cases {
            assert_eq!(expected, value.to_bits());
        }

        let kitty_cases = [
            (kf(true, false, false, false, false), 1u8 << 0),
            (kf(false, true, false, false, false), 1 << 1),
            (kf(false, false, true, false, false), 1 << 2),
            (kf(false, false, false, true, false), 1 << 3),
            (kf(false, false, false, false, true), 1 << 4),
        ];
        for (value, expected) in kitty_cases {
            assert_eq!(expected, value.to_bits());
        }

        let sd = SavedCursorFlags::default();
        let saved_cases = [
            (SavedCursorFlags { protected: true, ..sd }, 1u8 << 0),
            (SavedCursorFlags { pending_wrap: true, ..sd }, 1 << 1),
            (SavedCursorFlags { origin: true, ..sd }, 1 << 2),
        ];
        for (value, expected) in saved_cases {
            assert_eq!(expected, value.to_bits());
        }
    }

    #[test]
    fn header_encoding_rejects_invalid_state() {
        let mut encoded = [0u8; Header::LEN];

        let mut invalid_cursor_flags = test_header();
        invalid_cursor_flags.cursor_flags._padding = 1;
        let mut cursor_flags_writer = FixedWriter::new(&mut encoded);
        assert_eq!(
            Err(Error::InvalidCursorFlags),
            invalid_cursor_flags.encode(&mut cursor_flags_writer)
        );

        let mut invalid_kitty_index = test_header();
        invalid_kitty_index.kitty_keyboard.index = 8;
        let mut kitty_index_writer = FixedWriter::new(&mut encoded);
        assert_eq!(
            Err(Error::InvalidKittyKeyboardIndex),
            invalid_kitty_index.encode(&mut kitty_index_writer)
        );

        let mut invalid_kitty_flags = test_header();
        invalid_kitty_flags.kitty_keyboard.flags[3]._padding = 1;
        let mut kitty_flags_writer = FixedWriter::new(&mut encoded);
        assert_eq!(
            Err(Error::InvalidKittyKeyboardFlags),
            invalid_kitty_flags.encode(&mut kitty_flags_writer)
        );
    }

    #[test]
    fn header_decoding_rejects_structural_values() {
        let mut invalid_key = [0u8; Header::LEN];
        invalid_key[0] = 2;
        let mut key_reader = FixedReader::new(&invalid_key);
        assert_eq!(Err(Error::InvalidKey), Header::decode(&mut key_reader));
    }

    #[test]
    fn header_decoding_normalizes_unknown_semantic_values() {
        let mut fixture = [0u8; Header::LEN];

        fixture[16] = 4; // Unknown cursor style.
        fixture[17] = 0xFF; // Known flags, unknown semantic value, reserved bits.
        fixture[18] = 3; // Unknown cursor foreground color kind.
        fixture[39] = 0xD0; // Invalid single shift plus a reserved bit.
        fixture[40] = 3; // Unknown protected mode.
        fixture[41] = 8; // Out-of-range Kitty keyboard index.
        fixture[42..50].fill(0xFF); // Known flags plus reserved bits.
        fixture[50] = 3; // Unknown semantic-click kind.
        fixture[51] = 0xFF;
        fixture[52] = 2; // Noncanonical true.

        let mut reader = FixedReader::new(&fixture);
        let decoded = Header::decode(&mut reader).unwrap();

        assert_eq!(CursorStyle::Block, decoded.cursor_style);
        assert!(decoded.cursor_pen.is_default());
        assert!(decoded.cursor_flags.pending_wrap);
        assert!(decoded.cursor_flags.protected);
        assert_eq!(SemanticContent::Output, decoded.cursor_flags.semantic_content);
        assert!(decoded.cursor_flags.semantic_content_clear_eol);
        assert_eq!(0, decoded.cursor_flags._padding);

        assert_eq!(None, decoded.charset.single_shift);
        assert_eq!(charsets::Slots::G0, decoded.charset.gr);
        assert_eq!(ansi::ProtectedMode::Off, decoded.protected_mode);
        assert_eq!(0, decoded.kitty_keyboard.index);
        for flags in &decoded.kitty_keyboard.flags {
            assert!(flags.disambiguate);
            assert!(flags.report_events);
            assert!(flags.report_alternates);
            assert!(flags.report_all);
            assert!(flags.report_associated);
            assert_eq!(0, flags._padding);
        }
        assert_eq!(SemanticClick::None, decoded.semantic_click);
        assert!(decoded.saved_cursor_present);

        // Known semantic-click kinds with unknown or noncanonical values also
        // degrade to disabled while consuming both fixed bytes.
        for invalid in [[0u8, 1], [1, 2], [2, 4]] {
            let mut semantic_fixture = [0u8; Header::LEN];
            semantic_fixture[50] = invalid[0];
            semantic_fixture[51] = invalid[1];
            let mut semantic_reader = FixedReader::new(&semantic_fixture);
            let semantic_decoded = Header::decode(&mut semantic_reader).unwrap();
            assert_eq!(SemanticClick::None, semantic_decoded.semantic_click);
        }
    }

    #[test]
    fn header_decoding_rejects_every_truncation() {
        let reference = header_fixture();
        for fixture_len in 0..Header::LEN {
            let mut reader = FixedReader::new(&reference[..fixture_len]);
            assert_eq!(Err(Error::EndOfStream), Header::decode(&mut reader));
        }
    }

    #[test]
    fn native_screen_payload_omits_absent_optional_state() {
        let screen = Screen::init(opts(2, 2, Some(0))).unwrap();

        let mut encoded = [0u8; Header::LEN + 1];
        let mut writer = FixedWriter::new(&mut encoded);
        encode_payload(&screen, TerminalScreenKey::Primary, 1, &mut writer).unwrap();
        assert_eq!(Header::LEN + 1, writer.end);

        let mut reader = FixedReader::new(writer.buffered());
        let header = Header::decode(&mut reader).unwrap();
        assert_eq!(TerminalScreenKey::Primary, header.key);
        assert_eq!(1, header.page_count);
        assert_eq!(0, header.history_rows);
        assert!(!header.saved_cursor_present);
        assert_eq!(None, decode_cursor_hyperlink(&mut reader).unwrap());
    }

    #[test]
    fn saved_cursor_golden_encoding_and_decoding() {
        let mut encoded = [0u8; SavedCursor::LEN];
        let mut writer = FixedWriter::new(&mut encoded);
        test_saved_cursor().encode(&mut writer).unwrap();
        let reference = saved_cursor_fixture();
        fixture::expect_equal(
            fixture::Kind::Bytes,
            "src/terminal/snapshot/testdata/screen-saved-cursor-v1.hex",
            "snapshot_fixture-screen-saved-cursor-v1.hex",
            &reference,
            writer.buffered(),
        );
        assert_eq!(SavedCursor::LEN, reference.len());

        let mut limited = Limited::new(FixedReader::new(&reference), None, 1);
        assert_eq!(test_saved_cursor(), SavedCursor::decode(&mut limited).unwrap());
    }

    #[test]
    fn saved_cursor_encoding_rejects_and_decoding_normalizes_invalid_state() {
        let mut encoded = [0u8; SavedCursor::LEN];

        let mut invalid_flags = test_saved_cursor();
        invalid_flags.flags._padding = 1;
        let mut flags_writer = FixedWriter::new(&mut encoded);
        assert_eq!(Err(Error::InvalidSavedCursorFlags), invalid_flags.encode(&mut flags_writer));

        let mut invalid = [0u8; SavedCursor::LEN];
        invalid[4] = 3; // Unknown saved-cursor foreground color kind.
        invalid[20] = 0xF9; // Protected plus reserved bits.
        invalid[22] = 0xD0; // Invalid single shift plus a reserved bit.
        let mut reader = FixedReader::new(&invalid);
        let decoded = SavedCursor::decode(&mut reader).unwrap();

        assert!(decoded.pen.is_default());
        assert!(decoded.flags.protected);
        assert!(!decoded.flags.pending_wrap);
        assert!(!decoded.flags.origin);
        assert_eq!(0, decoded.flags._padding);
        assert_eq!(None, decoded.charset.single_shift);
        assert_eq!(charsets::Slots::G0, decoded.charset.gr);
    }

    #[test]
    fn saved_cursor_decoding_rejects_every_truncation() {
        let reference = saved_cursor_fixture();
        for fixture_len in 0..SavedCursor::LEN {
            let mut reader = FixedReader::new(&reference[..fixture_len]);
            assert_eq!(Err(Error::EndOfStream), SavedCursor::decode(&mut reader));
        }
    }

    #[test]
    fn cursor_hyperlink_null_encoding_and_decoding() {
        let mut none_encoded = [0u8; 1];
        let mut none_writer = FixedWriter::new(&mut none_encoded);
        encode_cursor_hyperlink(None, &mut none_writer).unwrap();
        assert_eq!(b"\x00", none_writer.buffered());

        let mut none_reader = FixedReader::new(none_writer.buffered());
        assert_eq!(None, decode_cursor_hyperlink(&mut none_reader).unwrap());
    }

    fn encode_screen(screen: &Screen, key: TerminalScreenKey, prefix: &[u8]) -> Vec<u8> {
        let mut destination: Vec<u8> = prefix.to_vec();
        {
            let mut stream = record::Writer::init(&mut destination);
            encode(screen, key, &mut stream).unwrap();
        }
        destination
    }

    #[test]
    fn framed_native_screen_and_page_sequence() {
        let mut screen = Screen::init(opts(8, 8, Some(0))).unwrap();

        // Configure the live cursor state represented by the fixed header.
        screen.cursor_absolute(7, 6);
        screen.cursor.cursor_style = CursorStyle::BlockHollow;
        screen.cursor.pending_wrap = true;
        screen.cursor.protected = false;
        screen.cursor.semantic_content = SemanticContent::Prompt;
        screen.cursor.semantic_content_clear_eol = true;
        screen.cursor.style = test_header().cursor_pen;
        screen.cursor.hyperlink_implicit_id = 0x0a0b0c0d;

        // Configure the screen-wide modes and packed charset state.
        let charset = test_charset_state();
        screen.charset = charset;
        screen.protected_mode = ansi::ProtectedMode::Dec;
        let mut kitty_stack = kitty::KeyFlagStack {
            idx: 7,
            ..Default::default()
        };
        for (native, snapshot) in kitty_stack.flags.iter_mut().zip(test_kitty_flags()) {
            native.disambiguate = snapshot.disambiguate;
            native.report_events = snapshot.report_events;
            native.report_alternates = snapshot.report_alternates;
            native.report_all = snapshot.report_all;
            native.report_associated = snapshot.report_associated;
        }
        screen.kitty_keyboard = kitty_stack;
        screen.semantic_prompt = SemanticPrompt {
            seen: true,
            click: SemanticClick::Cl(osc_sp::Click::SmartVertical),
        };

        // Configure the optional cursor state encoded after the fixed header.
        screen.saved_cursor = Some(TerminalSavedCursor {
            x: 0x0102,
            y: 0x0304,
            style: TerminalStyle::default(),
            protected: true,
            pending_wrap: true,
            origin: true,
            charset,
        });
        screen.start_hyperlink(b"cursor-uri", None).unwrap();

        let destination = encode_screen(&screen, TerminalScreenKey::Alternate, b"prefix");
        assert_eq!(b"prefix", &destination[..6]);

        // The complete sequence restores directly into native Screen/PageList
        // state, including page-local cursor references.
        let mut restore_source = FixedReader::new(&destination[6..]);
        let header = record::next_header(&mut restore_source).unwrap();
        let mut decoded = decode(&mut restore_source, header, Allocator::GLOBAL, &opts(8, 8, Some(0))).unwrap();
        assert_eq!(TerminalScreenKey::Alternate, decoded.key);
        let restored = &mut decoded.screen;

        assert!(restored.no_scrollback);
        assert_eq!(7, restored.cursor.x);
        assert_eq!(6, restored.cursor.y);
        assert_eq!(CursorStyle::BlockHollow, restored.cursor.cursor_style);
        assert!(restored.cursor.pending_wrap);
        assert_eq!(SemanticContent::Prompt, restored.cursor.semantic_content);
        assert!(restored.cursor.style.eql(&test_header().cursor_pen));
        assert_ne!(crate::terminal::style::DEFAULT_ID, restored.cursor.style_id);
        assert_eq!(0x0a0b0c0e, restored.cursor.hyperlink_implicit_id);
        let restored_hyperlink = restored.cursor.hyperlink.as_ref().unwrap();
        assert_eq!(b"cursor-uri", &restored_hyperlink.uri[..]);
        match restored_hyperlink.id {
            HyperlinkId::Implicit(id) => assert_eq!(0x0a0b0c0d, id),
            HyperlinkId::Explicit(_) => panic!("expected implicit hyperlink"),
        }

        let restored_saved = restored.saved_cursor.unwrap();
        assert_eq!(7, restored_saved.x);
        assert_eq!(7, restored_saved.y);
        assert!(restored_saved.protected);
        assert!(restored_saved.pending_wrap);
        assert!(restored_saved.origin);
        assert_eq!(charsets::Charset::Ascii, restored.charset.charsets.get(charsets::Slots::G1));
        assert_eq!(ansi::ProtectedMode::Dec, restored.protected_mode);
        assert_eq!(7, restored.kitty_keyboard.idx);
        assert!(restored.semantic_prompt.seen);
        assert_eq!(
            SemanticClick::Cl(osc_sp::Click::SmartVertical),
            restored.semantic_prompt.click
        );

        // Restored page-local cursor IDs are immediately usable by normal writes.
        restored.test_write_string("Z").unwrap();
        let written = restored
            .pages
            .get_cell(Point::Active(Coordinate { x: 0, y: 7 }))
            .unwrap();
        // SAFETY: the cell pointer is valid until the page list changes.
        let written_cell = unsafe { &*written.cell };
        assert_eq!('Z' as u32, written_cell.codepoint());
        assert_eq!(restored.cursor.style_id, written_cell.style_id());
        assert_eq!(
            Some(restored.cursor.hyperlink_id),
            nd(written.node).page().lookup_hyperlink(written_cell)
        );
        assert_eq!(SemanticContent::Prompt, written_cell.semantic_content());
        assert_eq!(Err(Error::EndOfStream), restore_source.take_byte());
    }

    #[test]
    fn screen_encodes_the_minimal_complete_page_active_suffix() {
        let probe = Screen::init(opts(80, 1, Some(0))).unwrap();
        let page_rows = nd(probe.pages.get_top_left(Tag::Screen).node).capacity().rows;
        drop(probe);
        let screen_rows = page_rows + 1;

        let mut screen = Screen::init(opts(80, screen_rows, None)).unwrap();

        // Create one complete historical page followed by a mixed page whose
        // first row is history and whose remaining rows begin the active area.
        screen.cursor_absolute(0, screen_rows - 1);
        for _ in 0..screen_rows {
            screen.test_write_string("\n").unwrap();
        }
        assert_eq!(3, screen.pages.total_pages());

        let active_top = screen.pages.get_top_left(Tag::Active);
        let historical = screen.pages.get_top_left(Tag::Screen).node;
        let mixed = nd(historical).next.unwrap();
        let newest = nd(mixed).next.unwrap();
        assert_eq!(mixed, active_top.node);
        assert_eq!(1, active_top.y);
        assert_eq!(None, nd(newest).next);

        // Mark each native page so the encoded sequence proves both omission and
        // oldest-to-newest ordering. The mixed page's marker is in its incidental
        // history prefix and must survive because complete pages are encoded.
        for (node, ch) in [(historical, 'A'), (mixed, 'B'), (newest, 'C')] {
            let rac = nd(node).page().get_row_and_cell(0, 0);
            // SAFETY: the cell is within the page.
            unsafe { *rac.cell = crate::terminal::page::Cell::init(ch as u32) };
        }

        let destination = encode_screen(&screen, TerminalScreenKey::Primary, b"");

        let mut source = FixedReader::new(&destination);
        {
            let mut screen_record = record::Reader::init(&mut source).unwrap();
            assert_eq!(record::Tag::SCREEN, screen_record.header.tag);

            let payload_reader = screen_record.payload_reader();
            let header = Header::decode(payload_reader).unwrap();
            assert_eq!(TerminalScreenKey::Primary, header.key);
            assert_eq!(2, header.page_count);
            assert_eq!(
                (screen.pages.total_rows - screen.pages.rows as usize) as u64,
                header.history_rows
            );
            assert!(!header.saved_cursor_present);
            assert_eq!(None, decode_cursor_hyperlink(payload_reader).unwrap());
            screen_record.finish().unwrap();
        }

        let h = record::next_header(&mut source).unwrap();
        let decoded_mixed = page::decode(&mut source, h).unwrap();
        // SAFETY: the cell is within the page.
        assert_eq!('B' as u32, unsafe { &*decoded_mixed.get_row_and_cell(0, 0).cell }.codepoint());

        let h = record::next_header(&mut source).unwrap();
        let decoded_newest = page::decode(&mut source, h).unwrap();
        // SAFETY: the cell is within the page.
        assert_eq!('C' as u32, unsafe { &*decoded_newest.get_row_and_cell(0, 0).cell }.codepoint());
        assert_eq!(Err(Error::EndOfStream), source.take_byte());

        // Native restoration keeps the incidental history prefix and positions
        // the active top within the first decoded page.
        let history_rows = (screen.pages.total_rows - screen.pages.rows as usize) as u64;
        let mut restore_source = FixedReader::new(&destination);
        let h = record::next_header(&mut restore_source).unwrap();
        let mut decoded = decode(&mut restore_source, h, Allocator::GLOBAL, &opts(80, screen_rows, None)).unwrap();
        assert_eq!(TerminalScreenKey::Primary, decoded.key);
        assert_eq!(history_rows, decoded.history_rows);
        let restored = &mut decoded.screen;

        assert_eq!(2, restored.pages.total_pages());
        let restored_top = restored.pages.get_top_left(Tag::Active);
        assert_eq!(1, restored_top.y);
        assert_eq!(
            restored_top.y as usize,
            restored.pages.total_rows - restored.pages.rows as usize
        );
        assert!(decoded.history_rows > (restored.pages.total_rows - restored.pages.rows as usize) as u64);
        let first = restored.pages.get_top_left(Tag::Screen).node;
        // SAFETY: cells are within the pages.
        unsafe {
            assert_eq!('B' as u32, (*nd(first).page().get_row_and_cell(0, 0).cell).codepoint());
            assert_eq!(
                'C' as u32,
                (*nd(nd(first).next.unwrap()).page().get_row_and_cell(0, 0).cell).codepoint()
            );
        }

        // The restored PageList resumes ordinary scrolling and row growth.
        restored.test_write_string("\n").unwrap();
        restored.pages.assert_integrity();
        restored.assert_integrity();
        assert_eq!(Err(Error::EndOfStream), restore_source.take_byte());
    }

    #[test]
    fn screen_encoding_preserves_a_compressed_suffix_page() {
        use crate::terminal::compress as compression;
        use crate::terminal::page_list::{NodeData, Storage};

        let screen = Screen::init(opts(8, 2, Some(0))).unwrap();
        let cell = screen.pages.get_cell(Point::Active(Coordinate { x: 0, y: 0 })).unwrap();
        // SAFETY: the cell pointer is valid until the page list changes.
        unsafe { *cell.cell = crate::terminal::page::Cell::init('A' as u32) };

        // Force the active suffix into the representation that current PageList
        // policy normally reserves for history. This models a future policy change
        // and makes pageAssumeResident an invalid tagged-union access.
        let node = screen.pages.get_top_left(Tag::Active).node;
        let compressed = {
            let resident = nd(node).page_assume_resident();
            let mut scratch = vec![0u8; compression::Page::required_scratch(resident.memory.len()).unwrap()];
            let mut table: Box<compression::lz4::HashTable> = Box::new([0; std::mem::size_of::<compression::lz4::HashTable>() / 4]);
            compression::Page::init(Allocator::GLOBAL, resident, &mut scratch, &mut table)
                .unwrap()
                .unwrap()
        };
        let old = std::mem::replace(&mut nd(node).data, NodeData::Compressed(compressed));
        if let NodeData::Resident(mut page) = old {
            page.deinit();
        }
        assert_eq!(Storage::Compressed, nd(node).storage());

        let destination = encode_screen(&screen, TerminalScreenKey::Primary, b"");

        // Encoding borrows or clones through PreservedPage and never changes the
        // source node's storage representation.
        assert_eq!(Storage::Compressed, nd(node).storage());

        let decoded = decode_all(&destination, &opts(8, 2, Some(0))).unwrap();
        let restored = decoded.screen.pages.get_cell(Point::Active(Coordinate { x: 0, y: 0 })).unwrap();
        // SAFETY: valid cell pointer.
        assert_eq!('A' as u32, unsafe { &*restored.cell }.codepoint());
    }

    /// Encode a SCREEN record with `header` and an empty cursor hyperlink,
    /// followed by `page_bytes` as its single PAGE.
    fn screen_with_header(header: &Header, saved: Option<SavedCursor>, native_page: &Page) -> Vec<u8> {
        let mut destination: Vec<u8> = Vec::new();
        {
            let mut stream = record::Writer::init(&mut destination);
            let screen_payload = stream.begin(record::Tag::SCREEN);
            header.encode(screen_payload).unwrap();
            if let Some(saved) = saved {
                saved.encode(screen_payload).unwrap();
            }
            screen_payload.write_byte(0).unwrap();
            stream.finish().unwrap();
            page::encode(native_page, &mut stream).unwrap();
        }
        destination
    }

    #[test]
    fn screen_restoration_normalizes_invalid_cursor_positions() {
        let screen = Screen::init(opts(2, 2, Some(0))).unwrap();

        struct Case {
            x: u16,
            y: u16,
            pending_wrap: bool,
            expected_x: u16,
            expected_y: u16,
            expected_pending_wrap: bool,
        }
        let cases = [
            // Out-of-range coordinates clamp to the bottom-right. Pending wrap is
            // valid there and can be retained.
            Case {
                x: u16::MAX,
                y: u16::MAX,
                pending_wrap: true,
                expected_x: 1,
                expected_y: 1,
                expected_pending_wrap: true,
            },
            // Pending wrap away from the final column would trip native printing
            // assertions, so retain the position and clear only that flag.
            Case {
                x: 0,
                y: 0,
                pending_wrap: true,
                expected_x: 0,
                expected_y: 0,
                expected_pending_wrap: false,
            },
        ];

        for case in cases {
            let mut header = Header::init(&screen, TerminalScreenKey::Primary, 1);
            header.cursor_x = case.x;
            header.cursor_y = case.y;
            header.cursor_flags.pending_wrap = case.pending_wrap;
            header.saved_cursor_present = false;

            let destination = screen_with_header(&header, None, active_page(&screen));
            let decoded = decode_all(&destination, &opts(2, 2, Some(0))).unwrap();

            assert_eq!(case.expected_x, decoded.screen.cursor.x);
            assert_eq!(case.expected_y, decoded.screen.cursor.y);
            assert_eq!(case.expected_pending_wrap, decoded.screen.cursor.pending_wrap);
            decoded.screen.assert_integrity();
        }
    }

    #[test]
    fn screen_validates_pending_wrap_against_a_mixed_width_cursor_page() {
        let screen = Screen::init(opts(8, 1, Some(0))).unwrap();

        // Model a lazily reflowed active page that is narrower than the terminal.
        // Column three is its physical final column even though it is not column
        // seven of the current terminal dimensions.
        let mut narrow_page = Page::init(crate::terminal::page::Capacity {
            cols: 4,
            rows: 1,
            ..Default::default()
        })
        .unwrap();

        let mut header = Header::init(&screen, TerminalScreenKey::Primary, 1);
        header.cursor_x = 3;
        header.cursor_y = 0;
        header.cursor_flags.pending_wrap = true;
        header.saved_cursor_present = false;

        let destination = screen_with_header(&header, None, &narrow_page);
        narrow_page.deinit();

        let decoded = decode_all(&destination, &opts(8, 1, Some(0))).unwrap();

        // SAFETY: the cursor pin is tracked by the restored page list.
        let pin = unsafe { decoded.screen.cursor.page_pin.as_ref() };
        assert_eq!(4, nd(pin.node).cols());
        assert_eq!(3, decoded.screen.cursor.x);
        assert!(decoded.screen.cursor.pending_wrap);
        decoded.screen.assert_integrity();
    }

    #[test]
    fn screen_resizes_a_decoded_max_width_page_to_a_narrow_width() {
        let screen = Screen::init(opts(8, 1, Some(0))).unwrap();

        // A snapshot can restore a page much wider than the terminal. Narrowing
        // this page to 2 columns frees memory for more rows than a page can
        // hold, so the resize must cap the row count. A page with only a couple
        // of rows is too small to trigger this, so it uses 215 rows.
        let mut wide_page = Page::init(crate::terminal::page::Capacity {
            cols: u16::MAX,
            rows: 215,
            ..Default::default()
        })
        .unwrap();

        let mut header = Header::init(&screen, TerminalScreenKey::Primary, 1);
        header.cursor_x = 0;
        header.cursor_y = 0;
        header.saved_cursor_present = false;

        let destination = screen_with_header(&header, None, &wide_page);
        wide_page.deinit();

        let mut decoded = decode_all(&destination, &opts(8, 1, Some(0))).unwrap();

        decoded
            .screen
            .resize(crate::terminal::screen::Resize {
                cols: 2,
                rows: 1,
                ..Default::default()
            })
            .unwrap();
        decoded.screen.assert_integrity();
    }

    #[test]
    fn screen_clamps_a_decoded_saved_cursor_to_terminal_dimensions() {
        let screen = Screen::init(opts(2, 2, Some(0))).unwrap();

        let mut header = Header::init(&screen, TerminalScreenKey::Primary, 1);
        header.saved_cursor_present = true;
        let saved = SavedCursor {
            x: u16::MAX,
            y: u16::MAX,
            pen: TerminalStyle::default(),
            flags: SavedCursorFlags {
                pending_wrap: true,
                ..Default::default()
            },
            charset: CharsetState::default(),
        };
        let destination = screen_with_header(&header, Some(saved), active_page(&screen));

        let decoded = decode_all(&destination, &opts(2, 2, Some(0))).unwrap();

        let saved = decoded.screen.saved_cursor.unwrap();
        assert_eq!(1, saved.x);
        assert_eq!(1, saved.y);
        assert!(saved.pending_wrap);
    }

    #[test]
    fn screen_restoration_rejects_invalid_and_incomplete_sequences() {
        let screen = Screen::init(opts(2, 2, Some(0))).unwrap();
        let destination = encode_screen(&screen, TerminalScreenKey::Primary, b"");

        // Every truncation must fail without leaking a partially restored
        // PageList, cursor pin, or cursor-owned state.
        for fixture_len in 0..destination.len() {
            let mut source = FixedReader::new(&destination[..fixture_len]);
            let Ok(header) = record::next_header(&mut source) else {
                continue;
            };
            if decode(&mut source, header, Allocator::GLOBAL, &opts(2, 2, Some(0))).is_ok() {
                panic!("truncated SCREEN sequence decoded");
            }
        }

        // Native PageList construction is transactional on allocation failure.
        {
            let failing = page_list::testing::FailingAllocator::leak(0);
            let mut source = FixedReader::new(&destination);
            let header = record::next_header(&mut source).unwrap();
            assert_eq!(
                Some(Error::OutOfMemory),
                decode(&mut source, header, failing.allocator(), &opts(2, 2, Some(0))).err()
            );
        }

        // A syntactically valid SCREEN cannot declare an empty PAGE sequence.
        let mut empty_sequence: Vec<u8> = Vec::new();
        {
            let mut empty_stream = record::Writer::init(&mut empty_sequence);
            let payload = empty_stream.begin(record::Tag::SCREEN);
            Header::init(&screen, TerminalScreenKey::Primary, 0).encode(payload).unwrap();
            payload.write_byte(0).unwrap();
            empty_stream.finish().unwrap();
        }
        assert_eq!(
            Some(Error::InvalidPageCount),
            decode_all(&empty_sequence, &opts(2, 2, Some(0))).err()
        );
    }

    // Zig's "SCREEN sequence failure preserves preceding bytes" injects an
    // allocation failure into the record writer's scratch buffer. The port's
    // record scratch uses the global allocator, so that test is not ported;
    // destination-failure behavior is covered by the record writer tests.

    #[test]
    fn screen_decode_ignores_an_invalid_cursor_hyperlink() {
        let screen = Screen::init(opts(1, 1, Some(0))).unwrap();

        let mut header = test_header();
        header.key = TerminalScreenKey::Primary;
        header.page_count = 1;
        header.cursor_x = 0;
        header.cursor_y = 0;
        header.cursor_flags.pending_wrap = false;
        header.saved_cursor_present = false;

        let mut destination: Vec<u8> = Vec::new();
        {
            let mut stream = record::Writer::init(&mut destination);
            let screen_payload = stream.begin(record::Tag::SCREEN);
            header.encode(screen_payload).unwrap();
            screen_payload.write_byte(1).unwrap(); // Implicit hyperlink.
            io::write_int::<u32>(screen_payload, 1).unwrap();
            io::write_int::<u32>(screen_payload, 0).unwrap(); // Empty URI.
            stream.finish().unwrap();

            page::encode(active_page(&screen), &mut stream).unwrap();
        }

        let decoded = decode_all(&destination, &opts(1, 1, None)).unwrap();
        assert!(decoded.screen.cursor.hyperlink.is_none());
    }

    #[test]
    fn screen_decode_ignores_a_page_with_an_empty_hyperlink_uri() {
        let mut header = test_header();
        header.key = TerminalScreenKey::Primary;
        header.page_count = 1;
        header.cursor_x = 0;
        header.cursor_y = 0;
        header.cursor_flags.pending_wrap = false;
        header.saved_cursor_present = false;

        let mut destination: Vec<u8> = Vec::new();
        {
            let mut stream = record::Writer::init(&mut destination);
            let screen_payload = stream.begin(record::Tag::SCREEN);
            header.encode(screen_payload).unwrap();
            screen_payload.write_byte(0).unwrap();
            stream.finish().unwrap();

            let page_header = page::Header {
                columns: 1,
                rows: 1,
                style_count: 0,
                hyperlink_count: 1,
                style_capacity: 16,
                hyperlink_capacity_bytes: 512,
                grapheme_capacity_bytes: 0,
                string_capacity_bytes: 0,
            };
            let page_payload = stream.begin(record::Tag::PAGE);
            page_header.encode(page_payload).unwrap();
            io::write_int::<u16>(page_payload, 1).unwrap();
            page_payload.write_byte(1).unwrap(); // Implicit hyperlink.
            io::write_int::<u32>(page_payload, 1).unwrap();
            io::write_int::<u32>(page_payload, 0).unwrap(); // Empty URI.

            // One narrow codepoint cell refers to the hyperlink table entry above.
            // Since that entry is ignored, the cell must restore without a hyperlink.
            page_payload.write_byte(0x30).unwrap(); // row flags, full cell width
            io::write_int::<u16>(page_payload, 1).unwrap(); // cell count
            io::write_int::<u64>(
                page_payload,
                grid::Cell {
                    content: 'A' as u32,
                    hyperlink: true,
                    hyperlink_id: 1,
                    ..Default::default()
                }
                .to_bits(),
            )
            .unwrap();
            io::write_int::<u32>(page_payload, 0).unwrap(); // grapheme section
            stream.finish().unwrap();
        }

        let mut decoded = decode_all(&destination, &opts(1, 1, None)).unwrap();

        // SAFETY: the cursor cell pointer is valid.
        let cursor_cell = unsafe { &*decoded.screen.cursor.page_cell };
        assert_eq!('A' as u32, cursor_cell.codepoint());
        assert!(!cursor_cell.hyperlink());

        // Resizing copies the restored cells into a wider page. The ignored
        // hyperlink must not leave a zero-length PageEntry to duplicate later.
        decoded
            .screen
            .resize(crate::terminal::screen::Resize {
                cols: 2,
                rows: 1,
                ..Default::default()
            })
            .unwrap();
        // SAFETY: the cursor cell pointer is valid.
        assert!(!unsafe { &*decoded.screen.cursor.page_cell }.hyperlink());
    }
}
