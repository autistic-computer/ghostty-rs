//! Port of `src/terminal/snapshot/history.zig`.
//!
//! STATUS: ported
//!
//! HISTORY record payload encoding.
//!
//! One HISTORY record describes the complete history for a previously encoded
//! screen. It is followed immediately by the number of complete PAGE records
//! declared by `page_count`. Those pages contain the history older than the
//! first page in the corresponding SCREEN sequence and are ordered from newest
//! to oldest.
//!
//! Every encoded SCREEN has one corresponding HISTORY record. A screen with no
//! older pages uses a zero page count and has no following PAGE records.
//!
//! The first SCREEN page may begin above the active area. Those incidental
//! history rows are already present at READY and are not repeated after
//! HISTORY. The corresponding SCREEN header declares the complete logical
//! history extent, including both that resident overlap and the following PAGE
//! records.
//!
//! All integers are unsigned and little-endian.
//!
//! ## Binary Format
//!
//! A HISTORY record is followed immediately by its declared PAGE records:
//!
//! ```text
//! +----------------------+
//! | HISTORY record       |
//! +----------------------+
//! | PAGE record 0        | newest
//! +----------------------+
//! | ...                  |
//! +----------------------+
//! | PAGE record (n - 1)  | oldest
//! +----------------------+
//!
//! n = page_count
//! ```
//!
//! Newest-to-oldest order lets a reader start showing most-recent
//! history as soon as possible which is more useful to a user.
//!
//! The PAGE sequence may be empty when all history is already included in the
//! first SCREEN page or when the screen has no history.
//!
//! ### Header
//!
//! The HISTORY payload consists only of this fixed header:
//!
//! ```text
//!  0 +--------------------------------+
//!    | Screen key (u16)               |
//!  2 +--------------------------------+
//!    | Following page count (u32)     |
//!  6 +--------------------------------+
//! ```
//!
//! A decoder uses `page_count`, rather than another record tag, to find the end
//! of the page sequence.

use super::io::{self, Error};
use super::kitty;
use super::page;
use super::record;
use crate::terminal::build_options;
use crate::terminal::page::{Page as TerminalPage, SemanticContent, SemanticPrompt};
use crate::terminal::page_list::{Allocator, FinalizeOptions, Location, Node, NodePtr};
use crate::terminal::point;
use crate::terminal::screen::Screen as TerminalScreen;
use crate::terminal::screen_set::Key as TerminalScreenKey;

/// The complete fixed payload of one HISTORY record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Identifies the previously encoded screen that owns this history.
    pub key: TerminalScreenKey,

    /// Number of complete PAGE records immediately following this record.
    pub page_count: u32,
}

/// Errors from `Header::decode`: read errors and `InvalidKey`.
pub type HeaderDecodeError = Error;

impl Header {
    /// Number of encoded bytes in the fixed payload. This size is part of the
    /// wire format. If it changes, the snapshot version must also change.
    pub const LEN: usize = 6;

    /// Encode the fixed HISTORY payload.
    pub fn encode(&self, writer: &mut dyn io::Writer) -> io::Result<()> {
        io::write_int::<u16>(writer, self.key as u16)?;
        io::write_int::<u32>(writer, self.page_count)
    }

    /// Decode and validate the fixed HISTORY payload.
    pub fn decode(reader: &mut dyn io::Reader) -> Result<Header, HeaderDecodeError> {
        let raw = io::read_int::<u16>(reader)?;
        let key = TerminalScreenKey::ALL
            .get(raw as usize)
            .copied()
            .ok_or(Error::InvalidKey)?;
        Ok(Header {
            key,
            page_count: io::read_int::<u32>(reader)?,
        })
    }
}

/// Errors possible while encoding one HISTORY and its complete PAGE sequence,
/// including `PageCountOverflow` when the complete historical prefix has more
/// pages than the header fits.
pub type EncodeError = Error;

#[inline(always)]
fn nd<'a>(node: NodePtr) -> &'a mut Node {
    // SAFETY: node pointers come from the live PageList being walked.
    unsafe { &mut *node.as_ptr() }
}

/// Encode one screen's HISTORY and its complete historical pages.
///
/// Pages are encoded newest-to-oldest. Compressed source pages are inspected
/// without changing their storage state. Completed records may already be
/// emitted if a later page fails.
///
/// With a `kitty_plan` for this screen, each page with Kitty graphics
/// placements on it is followed by its KITTY_IMAGE_PAGE_PLACEMENTS record. Pass
/// `None` for a screen without images.
pub fn encode(
    terminal_screen: &TerminalScreen,
    key: TerminalScreenKey,
    destination: &mut record::Writer<'_>,
    mut kitty_plan: Option<&mut kitty::Plan<'_>>,
) -> Result<(), EncodeError> {
    // SCREEN begins at the page containing the active area's first row. Its
    // leading rows are already resident; every previous complete page belongs
    // to this HISTORY sequence.
    let first = nd(terminal_screen.pages.get_top_left(point::Tag::Active).node).prev;
    let page_count: usize = {
        let mut page_count = 0usize;
        let mut node = first;
        while let Some(current) = node {
            page_count += 1;
            node = nd(current).prev;
        }
        page_count
    };

    let header = Header {
        key,
        page_count: u32::try_from(page_count).map_err(|_| Error::PageCountOverflow)?,
    };

    // HISTORY declares exactly how many PAGE records follow.
    {
        let payload = destination.begin(record::Tag::HISTORY);
        if let Err(e) = header.encode(payload) {
            destination.cancel();
            return Err(e);
        }
        destination.finish()?;
    }

    // Walk backward so each page can be prepended by the decoder immediately.
    // PreservedPage borrows resident pages and clones compressed pages without
    // changing the source node's representation.
    let mut node = first;
    while let Some(current) = node {
        let mut preserved = nd(current).page_preserving_state(Allocator::GLOBAL)?;
        let result = page::encode(preserved.page(), destination);
        preserved.deinit();
        result?;

        // The Kitty graphics placements pinned to this page follow it, so
        // a decoder that drops the page drops them too.
        if build_options::KITTY_GRAPHICS {
            if let Some(plan) = kitty_plan.as_deref_mut() {
                plan.encode_page_placements(current, destination)?;
            }
        }
        node = nd(current).prev;
    }
    Ok(())
}

/// Errors possible while restoring one HISTORY and its PAGE sequence,
/// including `UnexpectedScreenKey` when the HISTORY key does not match the
/// caller-selected screen.
pub type DecodeError = Error;

/// Options for restoring history PAGE records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecodeOptions {
    /// Compress each page right after it is added to the screen, so that
    /// at most one restored history page is uncompressed at a time. A page
    /// that is visible in the viewport stays uncompressed. This is the
    /// same as `PageList.PageAllocation.FinalizeOptions.compress`.
    pub compress: bool,
}

/// Errors possible while restoring one history PAGE into a native Screen.
pub type DecodePageError = Error;

/// Restore the next history PAGE record and prepend it to the native Screen.
///
/// Returns the number of rows added above the screen's existing content.
/// The page is decoded directly into a detached PageList-pooled allocation
/// and prepended only after its record validates, so a failure leaves the
/// screen unchanged. A limit failure from `finalize` occurs after the record
/// bytes were fully consumed, leaving `source` aligned on the next record.
///
/// With `options.compress`, the page is compressed after it is added. A
/// page that cannot be compressed is kept uncompressed and is not an error.
///
/// `record_header` is the PAGE record's header, already read from `source`.
/// A header for any other kind of record returns `UnexpectedRecordTag`. See
/// "Decoding Records" in `record.rs`.
pub fn decode_page(
    source: &mut dyn io::Reader,
    record_header: record::Header,
    terminal_screen: &mut TerminalScreen,
    options: DecodeOptions,
) -> Result<usize, DecodePageError> {
    // PAGE exposes its exact capacity before decoding the payload, allowing
    // the destination PageList to allocate the final backing memory once.
    let mut decoder = page::Decoder::init(source, record_header)?;
    let mut allocation = terminal_screen.pages.allocate_page(decoder.capacity())?;
    let result = (|| -> Result<usize, Error> {
        decoder.decode(allocation.page())?;

        let rows = allocation.page().size.rows as usize;
        let contains_prompt = has_semantic_prompt(allocation.page());
        allocation.finalize(
            Location::Prepend,
            FinalizeOptions {
                compress: options.compress,
            },
        )?;
        if contains_prompt {
            terminal_screen.semantic_prompt.seen = true;
        }
        Ok(rows)
    })();
    allocation.deinit();
    result
}

/// A decoded HISTORY manifest ready to restore its following PAGE records.
///
/// Keeping manifest decoding separate lets the full snapshot wrapper route a
/// HISTORY sequence by its encoded key before any pages mutate a Screen.
pub struct Decoder<'a> {
    pub source: &'a mut dyn io::Reader,
    pub header: Header,
}

/// Errors from `Decoder::init`: header decode errors, the record finish
/// errors, and `UnexpectedRecordTag`.
pub type DecoderInitError = Error;

/// Errors from `Decoder::decode`: page decode errors, `next_header` errors,
/// and `ExistingHistory` when the Screen already contains complete pages
/// before its active page.
pub type DecoderRestoreError = Error;

impl<'a> Decoder<'a> {
    /// Decode and finish the self-contained HISTORY manifest.
    ///
    /// `record_header` is the HISTORY record's header, already read from
    /// `source`. A header for any other kind of record returns
    /// `UnexpectedRecordTag`. See "Decoding Records" in `record.rs`.
    ///
    /// The PAGE records that follow are not read here. Restore them with
    /// `decode`, or one at a time with `history::decode_page`.
    pub fn init(source: &'a mut dyn io::Reader, record_header: record::Header) -> Result<Decoder<'a>, DecoderInitError> {
        let header = {
            let mut record_reader = record::Reader::init_header(&mut *source, record_header);
            if record_reader.header.tag != record::Tag::HISTORY {
                return Err(Error::UnexpectedRecordTag);
            }
            let header = Header::decode(record_reader.payload_reader())?;
            record_reader.finish()?;
            header
        };
        Ok(Decoder { source, header })
    }

    /// Restore the declared PAGE records into the selected native Screen.
    pub fn decode(&mut self, terminal_screen: &mut TerminalScreen, options: DecodeOptions) -> Result<(), DecoderRestoreError> {
        // A freshly restored SCREEN may carry overlap inside its first page,
        // but cannot already contain a complete historical page before that
        // boundary.
        let active_top = terminal_screen.pages.get_top_left(point::Tag::Active);
        if terminal_screen.pages.get_top_left(point::Tag::Screen).node != active_top.node {
            return Err(Error::ExistingHistory);
        }

        // Native row totals remain derived from the actual PAGE dimensions.
        for _ in 0..self.header.page_count {
            let header = record::next_header(&mut *self.source)?;
            decode_page(&mut *self.source, header, terminal_screen, options)?;
        }

        terminal_screen.pages.assert_integrity();
        terminal_screen.assert_integrity();
        Ok(())
    }
}

/// Restore one HISTORY and its declared PAGE records into a native Screen.
///
/// Each PAGE is decoded directly into a detached PageList-pooled allocation and
/// prepended only after its record and native integrity are validated. If a
/// later PAGE fails, earlier successful pages remain as a contiguous recent
/// history prefix.
///
/// `record_header` is the HISTORY record's header, already read from
/// `source`. A header for any other kind of record returns
/// `UnexpectedRecordTag`. This function reads the headers of the PAGE
/// records that follow itself. See "Decoding Records" in `record.rs`.
pub fn decode(
    source: &mut dyn io::Reader,
    record_header: record::Header,
    expected_key: TerminalScreenKey,
    terminal_screen: &mut TerminalScreen,
    options: DecodeOptions,
) -> Result<(), DecodeError> {
    let mut decoder = Decoder::init(source, record_header)?;
    if decoder.header.key != expected_key {
        return Err(Error::UnexpectedScreenKey);
    }
    decoder.decode(terminal_screen, options)
}

fn has_semantic_prompt(terminal_page: &TerminalPage) -> bool {
    for y in 0..terminal_page.size.rows as usize {
        // SAFETY: `y` is within the page; its cells are live.
        let row = unsafe { &*terminal_page.get_row(y) };
        if row.semantic_prompt() != SemanticPrompt::None {
            return true;
        }

        // SAFETY: as above.
        let cells = unsafe { &*terminal_page.get_cells(row) };
        for cell in cells {
            if cell.semantic_content() == SemanticContent::Prompt {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::page::Cell;
    use crate::terminal::page_list::{CompressMode, Storage};
    use crate::terminal::point::{Coordinate, Point, Tag};
    use crate::terminal::screen::Options;
    use crate::terminal::snapshot::fixture;
    use crate::terminal::snapshot::io::{FixedReader, FixedWriter, Reader as _};
    use crate::terminal::snapshot::screen;

    fn opts(cols: u16, rows: u16, max_scrollback_bytes: Option<usize>) -> Options {
        Options {
            cols,
            rows,
            max_scrollback_bytes,
            ..Options::default()
        }
    }

    fn header_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/history-header-v1.hex"))
    }

    fn cell_at(node: NodePtr, x: usize, y: usize) -> &'static mut Cell {
        let rac = nd(node).page().get_row_and_cell(x, y);
        // SAFETY: the cell is within a live page.
        unsafe { &mut *rac.cell }
    }

    fn screen_decode(source: &mut FixedReader<'_>, options: &Options) -> screen::Decoded {
        let header = record::next_header(source).unwrap();
        screen::decode(source, header, Allocator::GLOBAL, options).unwrap()
    }

    #[test]
    fn history_header_golden_encoding_and_decoding() {
        let expected = Header {
            key: TerminalScreenKey::Alternate,
            page_count: 0x01020304,
        };
        let mut encoded = [0u8; Header::LEN];
        let mut writer = FixedWriter::new(&mut encoded);
        expected.encode(&mut writer).unwrap();
        let reference = header_fixture();
        fixture::expect_equal(
            fixture::Kind::Bytes,
            "src/terminal/snapshot/testdata/history-header-v1.hex",
            "snapshot_fixture-history-header-v1.hex",
            &reference,
            writer.buffered(),
        );
        assert_eq!(Header::LEN, reference.len());

        let mut reader = FixedReader::new(&reference);
        assert_eq!(expected, Header::decode(&mut reader).unwrap());
        for fixture_len in 0..Header::LEN {
            let mut truncated = FixedReader::new(&reference[..fixture_len]);
            assert_eq!(Err(Error::EndOfStream), Header::decode(&mut truncated));
        }

        let mut invalid_key = reference.clone();
        invalid_key[0] = 2;
        let mut invalid_key_reader = FixedReader::new(&invalid_key);
        assert_eq!(Err(Error::InvalidKey), Header::decode(&mut invalid_key_reader));
    }

    #[test]
    fn history_encodes_newest_first_and_restores_complete_history() {
        // Choose an active height which spans two native pages, then grow until
        // exactly two additional complete pages precede the active boundary.
        let probe = TerminalScreen::init(opts(80, 1, Some(0))).unwrap();
        let page_rows = nd(probe.pages.get_top_left(Tag::Screen).node).capacity().rows;
        drop(probe);
        let screen_rows = page_rows + 1;

        let mut source_screen = TerminalScreen::init(opts(80, screen_rows, None)).unwrap();
        source_screen.cursor_absolute(0, screen_rows - 1);
        while source_screen.pages.total_pages() < 4 {
            source_screen.test_write_string("\n").unwrap();
        }

        let active_top = source_screen.pages.get_top_left(Tag::Active);
        let newest_history = nd(active_top.node).prev.unwrap();
        let oldest_history = nd(newest_history).prev.unwrap();
        assert_eq!(source_screen.pages.get_top_left(Tag::Screen).node, oldest_history);
        assert_eq!(None, nd(oldest_history).prev);

        // Mark the two complete historical pages so the wire sequence and final
        // native order are visible. The oldest prompt also verifies derived state
        // is updated only when that page is restored.
        *cell_at(oldest_history, 0, 0) = Cell::init('A' as u32);
        cell_at(oldest_history, 0, 0).set_semantic_content(SemanticContent::Prompt);
        *cell_at(newest_history, 0, 0) = Cell::init('B' as u32);

        // Encode SCREEN before history compression, then compress eligible history
        // and require HISTORY encoding to preserve each source storage state.
        let mut destination: Vec<u8> = Vec::new();
        {
            let mut stream = record::Writer::init(&mut destination);
            screen::encode(&source_screen, TerminalScreenKey::Primary, &mut stream).unwrap();
        }
        let history_offset = destination.len();

        let _ = source_screen.pages.compress(CompressMode::Full);
        let oldest_storage = nd(oldest_history).storage();
        let newest_storage = nd(newest_history).storage();
        {
            let mut stream = record::Writer::init(&mut destination);
            encode(&source_screen, TerminalScreenKey::Primary, &mut stream, None).unwrap();
        }
        assert_eq!(oldest_storage, nd(oldest_history).storage());
        assert_eq!(newest_storage, nd(newest_history).storage());

        // Inspect the manifest and PAGE records independently. Newest history is
        // sent first even though native PageList order is oldest-to-newest.
        let mut history_source = FixedReader::new(&destination[history_offset..]);
        {
            let mut history_record = record::Reader::init(&mut history_source).unwrap();
            assert_eq!(record::Tag::HISTORY, history_record.header.tag);
            let header = Header::decode(history_record.payload_reader()).unwrap();
            history_record.finish().unwrap();
            assert_eq!(TerminalScreenKey::Primary, header.key);
            assert_eq!(2, header.page_count);
        }

        let h = record::next_header(&mut history_source).unwrap();
        let decoded_newest = page::decode(&mut history_source, h).unwrap();
        // SAFETY: the cell is within the page.
        assert_eq!('B' as u32, unsafe { &*decoded_newest.get_row_and_cell(0, 0).cell }.codepoint());
        let h = record::next_header(&mut history_source).unwrap();
        let decoded_oldest = page::decode(&mut history_source, h).unwrap();
        // SAFETY: the cell is within the page.
        assert_eq!('A' as u32, unsafe { &*decoded_oldest.get_row_and_cell(0, 0).cell }.codepoint());
        assert_eq!(Err(Error::EndOfStream), history_source.take_byte());

        // Restore SCREEN first, then prepend HISTORY directly into that PageList.
        let mut restore_source = FixedReader::new(&destination);
        let mut decoded_screen = screen_decode(&mut restore_source, &opts(80, screen_rows, None));
        assert_eq!(TerminalScreenKey::Primary, decoded_screen.key);
        assert_eq!(
            (source_screen.pages.total_rows - screen_rows as usize) as u64,
            decoded_screen.history_rows
        );
        let restored = &mut decoded_screen.screen;
        assert!(!restored.semantic_prompt.seen);
        let h = record::next_header(&mut restore_source).unwrap();
        decode(
            &mut restore_source,
            h,
            TerminalScreenKey::Primary,
            restored,
            DecodeOptions::default(),
        )
        .unwrap();

        assert_eq!(source_screen.pages.total_pages(), restored.pages.total_pages());
        let restored_oldest = restored.pages.get_top_left(Tag::Screen).node;
        assert_eq!('A' as u32, cell_at(restored_oldest, 0, 0).codepoint());
        assert_eq!('B' as u32, cell_at(nd(restored_oldest).next.unwrap(), 0, 0).codepoint());
        assert!(restored.semantic_prompt.seen);
        assert_eq!(Err(Error::EndOfStream), restore_source.take_byte());

        // Reuse a writable copy of the sequence for failure-path fixtures.
        let mut encoded = destination.clone();
        let first_page_offset = history_offset + record::Header::LEN + Header::LEN;
        let first_payload_len = u32::from_le_bytes(
            encoded[first_page_offset + 2..first_page_offset + 6]
                .try_into()
                .unwrap(),
        ) as usize;
        let second_page_offset = first_page_offset + record::Header::LEN + first_payload_len;

        // Truncate the first PAGE only after its header has exposed a capacity.
        // Its detached allocation is discarded and the live SCREEN list remains
        // completely unchanged.
        {
            let mut truncated_source = FixedReader::new(&encoded[..second_page_offset - 1]);
            let mut decoded_truncated = screen_decode(&mut truncated_source, &opts(80, screen_rows, None));
            let truncated = &mut decoded_truncated.screen;
            let truncated_screen_first = truncated.pages.get_top_left(Tag::Screen).node;
            let truncated_screen_page_count = truncated.pages.total_pages();
            let h = record::next_header(&mut truncated_source).unwrap();
            assert_eq!(
                Err(Error::EndOfStream),
                decode(
                    &mut truncated_source,
                    h,
                    TerminalScreenKey::Primary,
                    truncated,
                    DecodeOptions::default(),
                )
            );
            assert_eq!(truncated_screen_page_count, truncated.pages.total_pages());
            assert_eq!(truncated_screen_first, truncated.pages.get_top_left(Tag::Screen).node);
            truncated.pages.assert_integrity();
            truncated.assert_integrity();
        }

        // Corrupt only the older PAGE tag. A failure in a later PAGE keeps only
        // the successfully prepended newer pages, contiguous with SCREEN.
        encoded[second_page_offset..second_page_offset + 2].copy_from_slice(&record::Tag::SCREEN.0.to_le_bytes());

        let mut partial_source = FixedReader::new(&encoded);
        let mut decoded_partial = screen_decode(&mut partial_source, &opts(80, screen_rows, None));
        let partial = &mut decoded_partial.screen;
        let screen_page_count = partial.pages.total_pages();
        let h = record::next_header(&mut partial_source).unwrap();
        assert_eq!(
            Err(Error::UnexpectedRecordTag),
            decode(
                &mut partial_source,
                h,
                TerminalScreenKey::Primary,
                partial,
                DecodeOptions::default(),
            )
        );
        assert_eq!(screen_page_count + 1, partial.pages.total_pages());
        assert_eq!(
            'B' as u32,
            cell_at(partial.pages.get_top_left(Tag::Screen).node, 0, 0).codepoint()
        );
        assert!(!partial.semantic_prompt.seen);
        partial.pages.assert_integrity();
        partial.assert_integrity();
    }

    #[test]
    fn history_decode_compresses_history_pages_when_requested() {
        let screen_options = opts(80, 24, None);

        // Write numbered lines until there are several pages of history, so the
        // content comparison also checks that pages are restored in order.
        let mut source_screen = TerminalScreen::init(screen_options.clone()).unwrap();
        let mut line = 0usize;
        while source_screen.pages.total_pages() < 5 {
            source_screen.test_write_string(&format!("line {line}\n")).unwrap();
            line += 1;
        }
        let expected = source_screen
            .dump_string_alloc(Point::Screen(Coordinate::default()))
            .unwrap();

        let mut destination: Vec<u8> = Vec::new();
        {
            let mut stream = record::Writer::init(&mut destination);
            screen::encode(&source_screen, TerminalScreenKey::Primary, &mut stream).unwrap();
            encode(&source_screen, TerminalScreenKey::Primary, &mut stream, None).unwrap();
        }

        for compress in [false, true] {
            let mut restore_source = FixedReader::new(&destination);
            let mut decoded_screen = screen_decode(&mut restore_source, &screen_options);
            let restored = &mut decoded_screen.screen;
            let h = record::next_header(&mut restore_source).unwrap();
            decode(
                &mut restore_source,
                h,
                TerminalScreenKey::Primary,
                restored,
                DecodeOptions { compress },
            )
            .unwrap();
            assert_eq!(source_screen.pages.total_pages(), restored.pages.total_pages());

            // The viewport shows the active area, so every history page is off
            // screen and can be compressed. Pages in the active area are never
            // compressed.
            let active = restored.pages.get_top_left(Tag::Active).node;
            let mut history_pages = 0usize;
            let mut current = restored.pages.pages.first;
            while let Some(node) = current {
                if node == active {
                    break;
                }
                history_pages += 1;
                let expected_storage = if compress {
                    Storage::Compressed
                } else {
                    Storage::Resident
                };
                assert_eq!(expected_storage, nd(node).storage());
                current = nd(node).next;
            }
            assert!(history_pages >= 3);
            current = Some(active);
            while let Some(node) = current {
                assert_eq!(Storage::Resident, nd(node).storage());
                current = nd(node).next;
            }
            assert_eq!(
                if compress { history_pages } else { 0 },
                restored.pages.memory_stats().compressed_pages
            );

            // Formatting restores compressed pages on access, so compare
            // contents only after the storage checks.
            let actual = restored
                .dump_string_alloc(Point::Screen(Coordinate::default()))
                .unwrap();
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn history_encodes_and_restores_an_empty_sequence() {
        let mut terminal_screen = TerminalScreen::init(opts(2, 2, None)).unwrap();

        let mut destination: Vec<u8> = Vec::new();
        {
            let mut stream = record::Writer::init(&mut destination);
            encode(&terminal_screen, TerminalScreenKey::Primary, &mut stream, None).unwrap();
        }

        let mut inspect_source = FixedReader::new(&destination);
        {
            let mut history_record = record::Reader::init(&mut inspect_source).unwrap();
            let header = Header::decode(history_record.payload_reader()).unwrap();
            history_record.finish().unwrap();
            assert_eq!(0, header.page_count);
        }
        assert_eq!(Err(Error::EndOfStream), inspect_source.take_byte());

        let mut decode_source = FixedReader::new(&destination);
        let initial_first = terminal_screen.pages.get_top_left(Tag::Screen).node;
        let h = record::next_header(&mut decode_source).unwrap();
        decode(
            &mut decode_source,
            h,
            TerminalScreenKey::Primary,
            &mut terminal_screen,
            DecodeOptions { compress: true },
        )
        .unwrap();
        assert_eq!(0, terminal_screen.pages.memory_stats().compressed_pages);
        assert_eq!(initial_first, terminal_screen.pages.get_top_left(Tag::Screen).node);
        assert_eq!(Err(Error::EndOfStream), decode_source.take_byte());
    }

    #[test]
    fn history_rejects_invalid_routing_and_incomplete_sequences() {
        let mut terminal_screen = TerminalScreen::init(opts(2, 2, None)).unwrap();

        // Routing and sequence length still determine which native screen is
        // mutated and where the following record begins, so they remain strict.
        let cases = [
            (
                Header {
                    key: TerminalScreenKey::Alternate,
                    page_count: 0,
                },
                Error::UnexpectedScreenKey,
            ),
            (
                Header {
                    key: TerminalScreenKey::Primary,
                    page_count: 1,
                },
                Error::EndOfStream,
            ),
        ];
        for (header, expected) in cases {
            let mut destination: Vec<u8> = Vec::new();
            {
                let mut stream = record::Writer::init(&mut destination);
                let payload = stream.begin(record::Tag::HISTORY);
                header.encode(payload).unwrap();
                stream.finish().unwrap();
            }

            let mut source = FixedReader::new(&destination);
            let h = record::next_header(&mut source).unwrap();
            assert_eq!(
                Err(expected),
                decode(
                    &mut source,
                    h,
                    TerminalScreenKey::Primary,
                    &mut terminal_screen,
                    DecodeOptions::default(),
                )
            );
            terminal_screen.pages.assert_integrity();
            terminal_screen.assert_integrity();
        }
    }
}
