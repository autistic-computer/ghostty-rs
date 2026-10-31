//! Port of `src/terminal/snapshot/page.zig`.
//!
//! STATUS: ported
//!
//! PAGE record payload encoding.
//!
//! One PAGE record represents a set of rows/columns in the terminal.
//! In libghostty, this happens to map internally to a very specific
//! data structure called a "page" (hence the name), but consumers of
//! the format don't need to reproduce that.
//!
//! The important thing is that one page is fully self-contained: it has
//! a dimension (cols x rows), a set of styles, hyperlinks, cells, etc.
//! and depends on no external state to decode that with some exceptions
//! like assets such as images.
//!
//! A page could have a dimension that doesn't match the terminal
//! dimensions, e.g. for lazy resize/reflow. Callers must be prepared for
//! that.
//!
//! ## Binary Format
//!
//! Every PAGE payload begins with a fixed header, followed by a
//! payload of styles, hyperlinks, and rows and columns.
//!
//! All integers are unsigned and little-endian.
//!
//! ### Header
//!
//! | Offset | Size | Field                              |
//! | -----: | ---: | :--------------------------------- |
//! |      0 |    2 | Logical columns (`u16`)            |
//! |      2 |    2 | Logical rows (`u16`)               |
//! |      4 |    2 | Non-default style count (`u16`)    |
//! |      6 |    2 | Unique hyperlink count (`u16`)     |
//! |      8 |    2 | Style capacity hint (`u16`)        |
//! |     10 |    2 | Hyperlink capacity bytes (`u16`)   |
//! |     12 |    4 | Grapheme capacity bytes (`u32`)    |
//! |     16 |    4 | String capacity bytes (`u32`)      |
//!
//! The first two fields (columns and rows) denote the dimensionality
//! of the page. The payload is guaranteed to have this dimensionality;
//! every row has exactly the columns specified.
//!
//! Next, the style count and hyperlink count denote the number of
//! styles and hyperlinks respectively that are sent with the page.
//! The default style and absence of a hyperlink are implicit at native
//! ID zero and are not included in these counts. Each encoded table entry
//! carries its nonzero native page ID.
//!
//! The final four fields are allocation hints copied from the source page.
//! These represent upper limits on what this page might contain. A decoder
//! can optionally choose to use this for preallocation or it can ignore
//! and decode and allocate dynamically.
//!
//! ### Payload
//!
//! ```text
//! +---------------------------+
//! | Header                    |
//! | 20 bytes                  |
//! +---------------------------+
//! | Style table               |
//! | style_count entries       |
//! +---------------------------+
//! | Hyperlink table           |
//! | hyperlink_count entries   |
//! +---------------------------+
//! | Grid                      |
//! | rows and grapheme section |
//! +---------------------------+
//!
//! Style entry     = encoded ID + style record
//! Hyperlink entry = encoded ID + hyperlink record
//! Grid            = row 0 ... row (rows - 1) + grapheme suffix section
//! Row             = row header + encoded cells
//! ```
//!
//! Following the header, the payload contains exactly `style_count` style
//! records and `hyperlink_count` hyperlink records, followed by exactly
//! `rows` row records and one grapheme suffix section. There is no padding
//! between records.
//!
//! Each style begins with an ID (`u16`) followed by the typical style
//! binary representation (in style.zig). The ID is what cells will use
//! to reference the style. Decoders must maintain some kind of mapping
//! in order to associate these properly.
//!
//! Each hyperlink also begins with an ID (`u16`) with the same semantics
//! as the style ID. The hyperlink and style IDs are separate namespaces,
//! so they may collide. Following the ID, the hyperlink binary format
//! is encoded directly.
//!
//! Both style and hyperlink IDs are not guaranteed to be in any specific
//! order and may contain gaps (e.g. ID 1 and 3 but not 2).
//!
//! Rows and cells use the grid encoding documented in `grid.zig`.

use std::borrow::Cow;

use super::grid;
use super::hyperlink;
use super::io::{self, Error, FixedReader};
use super::record;
use super::style;
use crate::terminal::build_options;
use crate::terminal::hyperlink::{self as terminal_hyperlink, Hyperlink, HyperlinkId, PageEntry, PageEntryId};
use crate::terminal::page::{Capacity, Page, Size};
use crate::terminal::size::MAX_PAGE_SIZE;
use crate::terminal::style as terminal_style;

type TerminalHyperlinkId = terminal_hyperlink::Id;
type TerminalStyleId = terminal_style::Id;

/// Errors possible while encoding a complete PAGE record: hyperlink and grid
/// encode errors and the record finish errors.
pub type EncodeError = Error;

/// Encode one complete PAGE record from a native page.
///
/// The payload is built in the stream's reusable record buffer. If payload
/// encoding fails, no bytes from this record are emitted.
pub fn encode(page: &Page, destination: &mut record::Writer<'_>) -> Result<(), EncodeError> {
    let payload = destination.begin(record::Tag::PAGE);
    if let Err(e) = encode_payload(page, payload) {
        destination.cancel();
        return Err(e);
    }
    destination.finish()
}

/// Errors possible while decoding and validating a complete PAGE record:
/// read errors, `InvalidDimensions`, `PageTooLarge`, grid decode errors,
/// `InvalidKind`, `OutOfMemory`, `InvalidDestinationCapacity`, the record
/// finish errors, `Integrity`, and `UnexpectedRecordTag`.
pub type DecodeError = Error;

/// Decode and validate one complete PAGE record into a fresh native page.
///
/// `record_header` is the record's header, already read from `reader`. A
/// header for any other kind of record returns `UnexpectedRecordTag`.
/// See "Decoding Records" in `record.rs`.
///
/// The record tag, payload boundary, CRC32C, payload contents, and final page
/// integrity are all validated before the page is returned.
pub fn decode(reader: &mut dyn io::Reader, record_header: record::Header) -> Result<Page, DecodeError> {
    let mut decoder = Decoder::init(reader, record_header)?;

    let mut page = Page::init(decoder.capacity()).map_err(|_| Error::OutOfMemory)?;
    decoder.decode(&mut page)?;
    Ok(page)
}

/// PAGE record decoder where the caller is responsible for owning
/// the Page memory. This is particularly useful paired with
/// `PageList::Builder` so you can build up a proper PageList with
/// valid memory ownership.
pub struct Decoder<'a> {
    pub record_reader: record::Reader<'a>,
    pub header: Header,
}

impl<'a> Decoder<'a> {
    /// Largest remaining PAGE payload staged into one contiguous buffer.
    /// This comfortably covers every payload a standard-capacity page can
    /// produce while keeping the allocation an untrusted length can force
    /// far below the record framing's four-byte length limit.
    const MAX_STAGED_PAYLOAD: usize = 8 * 1024 * 1024;

    /// Begin decoding one PAGE record and expose its required capacity.
    /// After this, call `capacity` to get the capacity to allocate
    /// the page properly, then `decode` into it.
    ///
    /// `record_header` is the record's header, already read from `reader`.
    /// A header for any other kind of record returns
    /// `UnexpectedRecordTag`. See "Decoding Records" in `record.rs`.
    pub fn init(reader: &'a mut dyn io::Reader, record_header: record::Header) -> Result<Decoder<'a>, DecodeError> {
        let mut record_reader = record::Reader::init_header(reader, record_header);
        if record_reader.header.tag != record::Tag::PAGE {
            return Err(Error::UnexpectedRecordTag);
        }

        let header = Header::decode(record_reader.payload_reader())?;
        header.page_capacity()?;
        Ok(Decoder { record_reader, header })
    }

    /// Return the exact native capacity advertised by the PAGE header.
    pub fn capacity(&self) -> Capacity {
        self.header.page_capacity().expect("validated by init")
    }

    /// Decode the remaining payload into caller-owned native page storage.
    ///
    /// The destination must be freshly initialized with `capacity`.
    pub fn decode(&mut self, destination: &mut Page) -> Result<(), DecodeError> {
        if destination.capacity != self.capacity() {
            return Err(Error::InvalidDestinationCapacity);
        }

        destination.size = Size {
            cols: self.header.columns,
            rows: self.header.rows,
        };

        // `init` consumed exactly the fixed header from the declared
        // payload, so this is the byte count of the tables and grid.
        let remaining = self.record_reader.header.payload_len as usize - Header::LEN;

        if self.record_reader.payload_reader().buffered_len() >= remaining {
            // The complete payload is already buffered, e.g. borrowed from
            // an in-memory snapshot. Parse it in place with no staging
            // copy; `finish` still enforces the CRC and exact exhaustion.
            decode_payload_body(self.record_reader.payload_reader(), destination, self.header)?;
        } else if remaining <= Self::MAX_STAGED_PAYLOAD {
            // Stage the payload with one bulk read. The whole payload passes
            // through the checksum hasher as one update and the payload
            // decoders then parse a flat buffer, which keeps per-row work free
            // of stream adapters. The CRC and exact-length checks in `finish`
            // are unaffected.
            let mut staged = Vec::new();
            staged.try_reserve_exact(remaining).map_err(|_| Error::OutOfMemory)?;
            staged.resize(remaining, 0);
            self.record_reader.payload_reader().read_slice_all(&mut staged)?;

            let mut staged_reader = FixedReader::new(&staged);
            decode_payload_body(&mut staged_reader, destination, self.header)?;
            if io::Reader::buffered_len(&staged_reader) != 0 {
                return Err(Error::PayloadNotExhausted);
            }
        } else {
            // A payload this large is either hostile or a page far beyond
            // native capacities. Decode it through the streaming payload
            // reader so its declared length cannot force an allocation.
            decode_payload_body(self.record_reader.payload_reader(), destination, self.header)?;
        }
        self.record_reader.finish()?;

        // The decoder normalizes every semantic value, so a complete decode
        // upholds native page invariants by construction. Verifying them
        // again is a defense against decoder bugs and follows the native
        // page policy: full integrity verification only when slow runtime
        // safety is enabled.
        if build_options::SLOW_RUNTIME_SAFETY {
            destination.verify_integrity()?;
        }
        Ok(())
    }
}

/// Errors possible while discarding one PAGE record without decoding it:
/// the record finish errors, read errors, and `UnexpectedRecordTag`.
pub type DiscardError = Error;

/// Consume exactly one complete PAGE record, validating its framing and
/// CRC32C while discarding the payload without structural validation.
///
/// `record_header` is the record's header, already read from `source`. A
/// header for any other kind of record returns `UnexpectedRecordTag`.
/// See "Decoding Records" in `record.rs`.
///
/// This lets a decoder stay aligned with the record sequence while dropping
/// page content it can no longer apply.
pub fn discard(source: &mut dyn io::Reader, record_header: record::Header) -> Result<(), DiscardError> {
    let mut record_reader = record::Reader::init_header(source, record_header);
    if record_reader.header.tag != record::Tag::PAGE {
        return Err(Error::UnexpectedRecordTag);
    }
    let len = record_reader.header.payload_len as usize;
    record_reader.payload_reader().discard_all(len)?;
    record_reader.finish()
}

/// Encode a PAGE payload directly from a native page.
fn encode_payload(page: &Page, writer: &mut dyn io::Writer) -> Result<(), Error> {
    // Write header
    Header::init(page).encode(writer)?;

    let mem = page.memory;

    // Sparse styles
    // SAFETY: `mem` is the page base.
    for entry in unsafe { page.styles.iterator(mem) } {
        io::write_int::<TerminalStyleId>(writer, entry.id)?;
        // SAFETY: the iterator yields live entries.
        style::encode(unsafe { *entry.value_ptr }, writer)?;
    }

    // Sparse hyperlinks
    // SAFETY: `mem` is the page base.
    for entry in unsafe { page.hyperlink_set.iterator(mem) } {
        io::write_int::<TerminalHyperlinkId>(writer, entry.id)?;
        // SAFETY: the iterator yields live entries.
        hyperlink::encode(&page_hyperlink(page, unsafe { &*entry.value_ptr }), writer)?;
    }

    // Rows and cells
    grid::encode(page, writer)
}

/// Decode a PAGE payload directly into a fresh native page.
///
/// Temporary native-ID remap tables use the global allocator. Styles,
/// hyperlinks, strings, graphemes, rows, and cells are stored in the page.
fn decode_payload(reader: &mut dyn io::Reader) -> Result<Page, Error> {
    let header = Header::decode(reader)?;
    let capacity = header.page_capacity()?;
    let mut page = Page::init(capacity).map_err(|_| Error::OutOfMemory)?;
    decode_payload_body(reader, &mut page, header)?;
    Ok(page)
}

/// Decode PAGE tables and grid after the header into initialized native page
/// storage with the exact advertised capacity and dimensions.
fn decode_payload_body(reader: &mut dyn io::Reader, page: &mut Page, header: Header) -> Result<(), Error> {
    page.pause_integrity_checks(true);
    let result = decode_payload_body_inner(reader, page, header);
    page.pause_integrity_checks(false);
    result
}

fn decode_payload_body_inner(reader: &mut dyn io::Reader, page: &mut Page, header: Header) -> Result<(), Error> {
    // Pages without styles or hyperlinks, the common case for plain
    // scrollback, skip the remap tables entirely: every encoded cell ID
    // resolves to the default through the empty remap.
    let mut style_remap: grid::StyleRemap = if header.style_count > 0 {
        grid::StyleRemap::init()?
    } else {
        grid::StyleRemap::EMPTY
    };

    let mut hyperlink_remap: grid::HyperlinkRemap = if header.hyperlink_count > 0 {
        grid::HyperlinkRemap::init()?
    } else {
        grid::HyperlinkRemap::EMPTY
    };

    let mem = page.memory;

    // Styles. The complete fixed-size entry is parsed from the buffered
    // payload when possible so each entry costs no reader calls.
    const STYLE_ENTRY_LEN: usize = std::mem::size_of::<TerminalStyleId>() + style::LEN;
    for _ in 0..header.style_count {
        let (native_id, value): (TerminalStyleId, Option<terminal_style::Style>) =
            if reader.buffered_len() >= STYLE_ENTRY_LEN {
                let bytes = &reader.buffered()[..STYLE_ENTRY_LEN];
                let entry = (
                    u16::from_le_bytes([bytes[0], bytes[1]]),
                    style::parse_or_null(bytes[2..2 + style::LEN].try_into().unwrap()),
                );
                reader.toss(STYLE_ENTRY_LEN);
                entry
            } else {
                (io::read_int::<TerminalStyleId>(reader)?, style::decode_or_null(reader)?)
            };

        // Zero is reserved for the implicit default. For a duplicate encoded
        // ID, the first entry wins and this complete entry is simply ignored.
        if native_id == 0 || style_remap.contains(native_id) {
            continue;
        }

        // Invalid/default styles map to the native default. `add` returns
        // the existing entry for a repeated concrete value, taking one
        // reference either way, while capacity failure degrades only this
        // style. The references are surrendered through the remap below
        // once every cell reference is installed.
        let decoded_id: TerminalStyleId = match value {
            Some(valid) => {
                if valid.is_default() {
                    0
                } else {
                    // SAFETY: `mem` is the page base.
                    unsafe { page.styles.add(mem, valid) }.unwrap_or(0)
                }
            }
            None => 0,
        };
        style_remap.put(native_id, decoded_id);
    }

    // Hyperlinks
    for _ in 0..header.hyperlink_count {
        let native_id = io::read_int::<TerminalHyperlinkId>(reader)?;
        let decoded_id = hyperlink::decode_page(page, reader)?;

        // As with styles, zero is reserved and the first duplicate encoded ID
        // wins. Release any native entry reference created for an ignored ID.
        if native_id == 0 || hyperlink_remap.contains(native_id) {
            if decoded_id != 0 {
                // SAFETY: `mem` is the page base and the ID is live.
                unsafe { page.hyperlink_set.release(mem, decoded_id) };
            }
            continue;
        }

        // Zero records an ignored table entry. Grid decoding treats every
        // reference to it as no hyperlink.
        hyperlink_remap.put(native_id, decoded_id);
    }

    // Rows and cells
    grid::decode(page, reader, &style_remap, &hyperlink_remap)?;

    // Every accepted table entry took one reference through `add` so grid
    // decoding can safely attach its style to any number of cells. Unlike
    // organically built pages, those references do not themselves represent
    // cells. Release through the encoded-ID remap, so duplicate values
    // which deduplicated to the same native ID each surrender their own
    // reference; unused entries then become dead and disappear from
    // canonical re-encoding.
    for encoded_id in style_remap.seen_iter() {
        let id = style_remap.entries[encoded_id];
        if id != 0 {
            // SAFETY: `mem` is the page base and the ID is live.
            unsafe { page.styles.release(mem, id) };
        }
    }

    // Hyperlink insertion likewise creates one temporary reference for every
    // accepted wire table entry. Release through the encoded-ID remap rather
    // than the native set so duplicate values which deduplicated to the same
    // native ID each surrender their own reference.
    for encoded_id in hyperlink_remap.seen_iter() {
        let id = hyperlink_remap.entries[encoded_id];
        if id != 0 {
            // SAFETY: `mem` is the page base and the ID is live.
            unsafe { page.hyperlink_set.release(mem, id) };
        }
    }

    Ok(())
}

/// The fixed logical dimensions, table counts, and allocation hints at the
/// start of PAGE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Number of logical cells in every encoded row.
    pub columns: u16,

    /// Number of logical rows encoded after the tables.
    pub rows: u16,

    /// Number of non-default style entries following the header.
    pub style_count: u16,

    /// Number of unique hyperlink entries following the style table.
    pub hyperlink_count: u16,

    /// Suggested capacity for the native non-default style set.
    pub style_capacity: u16,

    /// Suggested native hyperlink storage capacity in bytes.
    pub hyperlink_capacity_bytes: u16,

    /// Suggested native grapheme storage capacity in bytes.
    pub grapheme_capacity_bytes: u32,

    /// Suggested native string storage capacity in bytes.
    pub string_capacity_bytes: u32,
}

/// `InvalidDimensions` or `PageTooLarge`.
pub type CapacityError = Error;

impl Header {
    /// Number of bytes written by `encode`. This size is part of the wire
    /// format. If it changes, the snapshot version and golden fixtures must
    /// also change.
    pub const LEN: usize = 20;

    /// Encode the fixed PAGE payload header.
    pub fn encode(&self, writer: &mut dyn io::Writer) -> io::Result<()> {
        io::write_int::<u16>(writer, self.columns)?;
        io::write_int::<u16>(writer, self.rows)?;
        io::write_int::<u16>(writer, self.style_count)?;
        io::write_int::<u16>(writer, self.hyperlink_count)?;
        io::write_int::<u16>(writer, self.style_capacity)?;
        io::write_int::<u16>(writer, self.hyperlink_capacity_bytes)?;
        io::write_int::<u32>(writer, self.grapheme_capacity_bytes)?;
        io::write_int::<u32>(writer, self.string_capacity_bytes)
    }

    /// Decode the fixed PAGE payload header.
    ///
    /// This reads field values only. The complete PAGE decoder is responsible
    /// for applying configured limits before using any capacity hint.
    pub fn decode(reader: &mut dyn io::Reader) -> io::Result<Header> {
        Ok(Header {
            columns: io::read_int(reader)?,
            rows: io::read_int(reader)?,
            style_count: io::read_int(reader)?,
            hyperlink_count: io::read_int(reader)?,
            style_capacity: io::read_int(reader)?,
            hyperlink_capacity_bytes: io::read_int(reader)?,
            grapheme_capacity_bytes: io::read_int(reader)?,
            string_capacity_bytes: io::read_int(reader)?,
        })
    }

    /// Initialize a header from the current contents of a native page.
    ///
    /// This copies the page's existing allocation capacities. It does not
    /// scan cells, allocate, or encode any bytes.
    fn init(page: &Page) -> Header {
        Header {
            columns: page.size.cols,
            rows: page.size.rows,
            style_count: page.styles.count() as u16,
            hyperlink_count: page.hyperlink_set.count() as u16,
            style_capacity: page.capacity.styles,
            hyperlink_capacity_bytes: page.capacity.hyperlink_bytes,
            grapheme_capacity_bytes: page.capacity.grapheme_bytes,
            string_capacity_bytes: page.capacity.string_bytes,
        }
    }

    /// Validate native allocation requirements and produce the page capacity.
    fn page_capacity(&self) -> Result<Capacity, CapacityError> {
        if self.columns == 0 || self.rows == 0 {
            return Err(Error::InvalidDimensions);
        }

        let cap = Capacity {
            cols: self.columns,
            rows: self.rows,
            styles: self.style_capacity,
            hyperlink_bytes: self.hyperlink_capacity_bytes,
            grapheme_bytes: self.grapheme_capacity_bytes,
            string_bytes: self.string_capacity_bytes,
        };

        let page_layout = Page::layout(cap);
        if page_layout.total_size > MAX_PAGE_SIZE {
            return Err(Error::PageTooLarge);
        }

        Ok(cap)
    }
}

const _: () = assert!(Header::LEN == 20);

fn page_hyperlink<'p>(page: &'p Page, entry: &PageEntry) -> Hyperlink<'p> {
    // SAFETY: the entry's strings live in this page's memory.
    unsafe {
        Hyperlink {
            id: match entry.id {
                PageEntryId::Implicit(value) => HyperlinkId::Implicit(value),
                PageEntryId::Explicit(value) => HyperlinkId::Explicit(Cow::Borrowed(value.slice(page.memory))),
            },
            uri: Cow::Borrowed(entry.uri.slice(page.memory)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::page::{Cell, ContentTag, Row, SemanticContent, SemanticPrompt, Wide};
    use crate::terminal::snapshot::fixture;
    use crate::terminal::snapshot::io::{Discarding, FixedWriter, Limited, Reader as _, Writer as _};
    use crate::terminal::style::{Color, Flags, Style};

    fn rac(page: &Page, x: usize, y: usize) -> (&'static mut Row, &'static mut Cell) {
        let rac = page.get_row_and_cell(x, y);
        // SAFETY: test pages outlive every use of these references.
        unsafe { (&mut *rac.row, &mut *rac.cell) }
    }

    fn page_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/page-v1.hex"))
    }
    fn header_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/page-header-v1.hex"))
    }
    fn empty_framed_page_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/page-empty-record-v1.hex"))
    }

    fn bold() -> Style {
        Style {
            flags: Flags {
                bold: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }
    fn italic() -> Style {
        Style {
            flags: Flags {
                italic: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn implicit(id: u32, uri: &'static [u8]) -> Hyperlink<'static> {
        Hyperlink {
            id: HyperlinkId::Implicit(id),
            uri: Cow::Borrowed(uri),
        }
    }
    fn explicit(id: &'static [u8], uri: &'static [u8]) -> Hyperlink<'static> {
        Hyperlink {
            id: HyperlinkId::Explicit(Cow::Borrowed(id)),
            uri: Cow::Borrowed(uri),
        }
    }

    fn cap(cols: u16, rows: u16) -> Capacity {
        Capacity {
            cols,
            rows,
            ..Default::default()
        }
    }

    fn header(
        columns: u16,
        rows: u16,
        style_count: u16,
        hyperlink_count: u16,
        style_capacity: u16,
        hyperlink_capacity_bytes: u16,
        grapheme_capacity_bytes: u32,
        string_capacity_bytes: u32,
    ) -> Header {
        Header {
            columns,
            rows,
            style_count,
            hyperlink_count,
            style_capacity,
            hyperlink_capacity_bytes,
            grapheme_capacity_bytes,
            string_capacity_bytes,
        }
    }

    fn encode_to_vec(page: &Page) -> Result<Vec<u8>, Error> {
        let mut destination = Vec::new();
        {
            let mut stream = record::Writer::init(&mut destination);
            encode(page, &mut stream)?;
        }
        Ok(destination)
    }

    #[test]
    fn page_header_golden_encoding_and_decoding() {
        let h = header(0x0102, 0x0304, 0x0506, 0x0708, 0x090a, 0x0b0c, 0x0d0e0f10, 0x11121314);
        let mut buf = [0u8; Header::LEN];
        let mut writer = FixedWriter::new(&mut buf);
        h.encode(&mut writer).unwrap();
        let reference = header_fixture();
        fixture::expect_equal(
            fixture::Kind::Bytes,
            "src/terminal/snapshot/testdata/page-header-v1.hex",
            "snapshot_fixture-page-header-v1.hex",
            &reference,
            writer.buffered(),
        );

        let mut limited = Limited::new(FixedReader::new(&reference), None, 1);
        assert_eq!(h, Header::decode(&mut limited).unwrap());
    }

    #[test]
    fn reject_every_truncation() {
        let reference = header_fixture();
        for len in 0..Header::LEN {
            let mut reader = FixedReader::new(&reference[..len]);
            assert_eq!(Err(Error::EndOfStream), Header::decode(&mut reader));
        }
    }

    #[test]
    fn framed_page_encode_and_decode_a_sparse_native_page() {
        let capacity = Capacity {
            cols: 3,
            rows: 2,
            styles: 8,
            hyperlink_bytes: 512,
            grapheme_bytes: 128,
            string_bytes: 256,
        };
        let mut page = Page::init(capacity).unwrap();
        let mem = page.memory;

        // Create holes in both source tables so the encoded IDs exercise sparse
        // table remapping rather than coincidentally matching decoded IDs.
        unsafe {
            let style_a = page.styles.add(mem, bold()).unwrap();
            let dead_style = page.styles.add(mem, italic()).unwrap();
            let style_b = page
                .styles
                .add(
                    mem,
                    Style {
                        bg_color: Color::Palette(42),
                        ..Default::default()
                    },
                )
                .unwrap();
            page.styles.release(mem, dead_style);

            let (first_row, first) = rac(&page, 0, 0);
            first.set_style_id(style_a);
            first_row.set_styled(true);

            let (second_row, second) = rac(&page, 1, 0);
            second.set_style_id(style_b);
            second_row.set_styled(true);

            let (third_row, third) = rac(&page, 2, 0);
            page.styles.use_(mem, style_a);
            third.set_style_id(style_a);
            third_row.set_styled(true);

            let hyperlink_a = page.insert_hyperlink(&explicit(b"a", b"alpha")).unwrap();
            let dead_hyperlink = page.insert_hyperlink(&explicit(b"dead-id", b"dead-uri")).unwrap();
            let hyperlink_b = page.insert_hyperlink(&implicit(0x01020304, b"beta")).unwrap();
            page.hyperlink_set.release(mem, dead_hyperlink);

            page.hyperlink_set.use_(mem, hyperlink_a);
            page.set_hyperlink(first_row, first, hyperlink_a).unwrap();
            page.set_hyperlink(second_row, second, hyperlink_b).unwrap();
            page.set_hyperlink(third_row, third, hyperlink_a).unwrap();

            // Cover graphemes, wide-cell pairs, colors, protection, and semantic
            // metadata in one compact two-row page.
            let (grow, gcell) = rac(&page, 0, 1);
            *gcell = Cell::init('x' as u32);
            page.set_graphemes(grow, gcell, &[0x0301, 0x0302]).unwrap();

            first.set_content_codepoint('A' as u32);
            first.set_wide(Wide::Wide);
            first.set_protected(true);
            first.set_semantic_content(SemanticContent::Prompt);
            first_row.set_semantic_prompt(SemanticPrompt::Prompt);

            second.set_wide(Wide::SpacerTail);
            second.set_semantic_content(SemanticContent::Input);

            third.set_content_tag(ContentTag::BgColorPalette);
            third.set_content_color_palette(7);

            let (_, rgb) = rac(&page, 1, 1);
            rgb.set_content_tag(ContentTag::BgColorRgb);
            rgb.set_content_color_rgb(crate::terminal::page::Rgb {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc,
            });
            rgb.set_protected(true);

            let (head_row, head) = rac(&page, 2, 1);
            head.set_wide(Wide::SpacerHead);
            head_row.set_wrap(true);
            head_row.set_wrap_continuation(true);
            head_row.set_semantic_prompt(SemanticPrompt::PromptContinuation);
        }

        let h = header(3, 2, 2, 2, 8, 512, 128, 256);
        assert_eq!(h, Header::init(&page));

        // Lock the payload bytes and independently verify the counting writer sees
        // the same encoded length.
        let mut counter = Discarding::default();
        encode_payload(&page, &mut counter).unwrap();

        let mut encoded = [0u8; 512];
        let mut writer = FixedWriter::new(&mut encoded);
        encode_payload(&page, &mut writer).unwrap();
        let written = writer.buffered().to_vec();

        let reference = page_fixture();
        fixture::expect_equal(
            fixture::Kind::Page,
            "src/terminal/snapshot/testdata/page-v1.hex",
            "snapshot_fixture-page-v1.hex",
            &reference,
            &written,
        );
        assert_eq!(reference.len() as u64, counter.count);

        // Decode the checked-in reference rather than the just-generated bytes.
        // A one-byte backing buffer exercises streaming reads across every field.
        let mut limited = Limited::new(FixedReader::new(&reference), None, 1);
        let decoded = decode_payload(&mut limited).unwrap();

        assert_eq!(h, Header::init(&decoded));
        decoded.verify_integrity().unwrap();

        // Decoding compacts the sparse source IDs while preserving table values
        // and all cell references through the remap tables.
        let dmem = decoded.memory;
        unsafe {
            let mut style_it = decoded.styles.iterator(dmem);
            let decoded_style_a = style_it.next().unwrap();
            assert_eq!(1, decoded_style_a.id);
            assert!(bold().eql(&*decoded_style_a.value_ptr));
            let decoded_style_b = style_it.next().unwrap();
            assert_eq!(2, decoded_style_b.id);
            assert!(Style {
                bg_color: Color::Palette(42),
                ..Default::default()
            }
            .eql(&*decoded_style_b.value_ptr));
            assert!(style_it.next().is_none());
            assert_eq!(2, decoded.styles.ref_count(dmem, decoded_style_a.id));
            assert_eq!(1, decoded.styles.ref_count(dmem, decoded_style_b.id));

            let mut hyperlink_it = decoded.hyperlink_set.iterator(dmem);
            let decoded_link_a = hyperlink_it.next().unwrap();
            assert_eq!(1, decoded_link_a.id);
            let decoded_hyperlink_a = page_hyperlink(&decoded, &*decoded_link_a.value_ptr);
            assert_eq!(HyperlinkId::Explicit(Cow::Borrowed(b"a")), decoded_hyperlink_a.id);
            assert_eq!(b"alpha", &decoded_hyperlink_a.uri[..]);
            let decoded_link_b = hyperlink_it.next().unwrap();
            assert_eq!(2, decoded_link_b.id);
            let decoded_hyperlink_b = page_hyperlink(&decoded, &*decoded_link_b.value_ptr);
            assert_eq!(HyperlinkId::Implicit(0x01020304), decoded_hyperlink_b.id);
            assert_eq!(b"beta", &decoded_hyperlink_b.uri[..]);
            assert!(hyperlink_it.next().is_none());
        }

        let decoded_first = rac(&decoded, 0, 0).1;
        assert_eq!(1, decoded_first.style_id());
        assert_eq!(Some(1), decoded.lookup_hyperlink(decoded_first));

        let decoded_second = rac(&decoded, 1, 0).1;
        assert_eq!(2, decoded_second.style_id());
        assert_eq!(Some(2), decoded.lookup_hyperlink(decoded_second));

        // The first re-encode reflects compacted IDs; subsequent round trips must
        // stabilize byte-for-byte.
        let mut reencoded = [0u8; 512];
        let mut rewriter = FixedWriter::new(&mut reencoded);
        encode_payload(&decoded, &mut rewriter).unwrap();
        let rewritten = rewriter.buffered().to_vec();
        assert_ne!(written, rewritten);

        let mut reencoded_reader = FixedReader::new(&rewritten);
        let decoded_again = decode_payload(&mut reencoded_reader).unwrap();
        decoded_again.verify_integrity().unwrap();

        let mut reencoded_again = [0u8; 512];
        let mut rewriter_again = FixedWriter::new(&mut reencoded_again);
        encode_payload(&decoded_again, &mut rewriter_again).unwrap();
        assert_eq!(&rewritten[..], rewriter_again.buffered());
    }

    #[test]
    fn framed_page_golden_empty_record() {
        let capacity = Capacity {
            cols: 1,
            rows: 1,
            styles: 0,
            hyperlink_bytes: 0,
            grapheme_bytes: 0,
            string_bytes: 0,
        };
        let page = Page::init(capacity).unwrap();

        let destination = encode_to_vec(&page).unwrap();
        let reference = empty_framed_page_fixture();
        fixture::expect_equal(
            fixture::Kind::Bytes,
            "src/terminal/snapshot/testdata/page-empty-record-v1.hex",
            "snapshot_fixture-page-empty-record-v1.hex",
            &reference,
            &destination,
        );

        let mut source = FixedReader::new(&reference);
        let h = record::next_header(&mut source).unwrap();
        let decoded = decode(&mut source, h).unwrap();
        assert_eq!(Header::init(&page), Header::init(&decoded));
    }

    #[test]
    fn framed_page_rejects_incomplete_wide_cells_transactionally() {
        // One column puts the wide cell at the row end. Two columns put an
        // ordinary narrow cell after it. Neither case supplies a spacer tail.
        for columns in [1u16, 2] {
            let page = Page::init(cap(columns, 1)).unwrap();
            let first = rac(&page, 0, 0).1;
            *first = Cell::init('A' as u32);
            first.set_wide(Wide::Wide);
            if columns > 1 {
                *rac(&page, 1, 0).1 = Cell::init('B' as u32);
            }

            let mut destination: Vec<u8> = Vec::new();
            destination.write_all(b"prefix").unwrap();
            {
                let mut stream = record::Writer::init(&mut destination);
                assert_eq!(Err(Error::InvalidWideCell), encode(&page, &mut stream));
            }
            assert_eq!(b"prefix", &destination[..]);
        }
    }

    #[test]
    fn framed_page_rejects_a_different_record_tag() {
        let mut wrong_tag = empty_framed_page_fixture();
        wrong_tag[0..2].copy_from_slice(&record::Tag::SCREEN.0.to_le_bytes());
        let mut reader = FixedReader::new(&wrong_tag);
        let h = record::next_header(&mut reader).unwrap();
        assert_eq!(Some(Error::UnexpectedRecordTag), decode(&mut reader, h).err());
    }

    #[test]
    fn framed_page_preserves_following_bytes() {
        let mut bytes = empty_framed_page_fixture();
        bytes.extend_from_slice(b"next");
        let mut source = FixedReader::new(&bytes);
        let h = record::next_header(&mut source).unwrap();
        let _decoded = decode(&mut source, h).unwrap();
        assert_eq!(b"next".to_vec(), source.take(4).unwrap());
    }

    #[test]
    fn decode_sparse_page_rejects_every_truncation() {
        let reference = page_fixture();
        for len in 0..reference.len() {
            let mut reader = FixedReader::new(&reference[..len]);
            assert_eq!(Some(Error::EndOfStream), decode_payload(&mut reader).err());
        }
    }

    #[test]
    fn decode_accepts_unordered_sparse_style_ids_and_ignores_zero() {
        let h = header(2, 1, 2, 0, 8, 0, 0, 0);

        let mut descending: Vec<u8> = Vec::new();
        h.encode(&mut descending).unwrap();
        io::write_int::<TerminalStyleId>(&mut descending, 3).unwrap();
        style::encode(bold(), &mut descending).unwrap();
        io::write_int::<TerminalStyleId>(&mut descending, 2).unwrap();
        style::encode(italic(), &mut descending).unwrap();
        descending
            .write_byte(
                grid::Row {
                    cell_width: grid::EncodedWidth::Eight,
                    ..Default::default()
                }
                .to_bits(),
            )
            .unwrap();
        io::write_int::<u16>(&mut descending, 2).unwrap(); // cell count
        io::write_int::<u64>(
            &mut descending,
            grid::Cell {
                content: 'A' as u32,
                style_id: 3,
                ..Default::default()
            }
            .to_bits(),
        )
        .unwrap();
        io::write_int::<u64>(
            &mut descending,
            grid::Cell {
                content: 'B' as u32,
                style_id: 2,
                ..Default::default()
            }
            .to_bits(),
        )
        .unwrap();
        io::write_int::<u32>(&mut descending, 0).unwrap(); // grapheme section

        let mut descending_reader = FixedReader::new(&descending);
        let decoded_descending = decode_payload(&mut descending_reader).unwrap();
        assert_eq!(2, decoded_descending.styles.count());
        assert_eq!(1, rac(&decoded_descending, 0, 0).1.style_id());
        assert_eq!(2, rac(&decoded_descending, 1, 0).1.style_id());

        let one_header = header(1, 1, 1, 0, 8, 0, 0, 0);

        let mut zero: Vec<u8> = Vec::new();
        one_header.encode(&mut zero).unwrap();
        io::write_int::<TerminalStyleId>(&mut zero, 0).unwrap();
        style::encode(bold(), &mut zero).unwrap();

        let zero_grid = Page::init(cap(1, 1)).unwrap();
        grid::encode(&zero_grid, &mut zero).unwrap();
        assert_eq!(Header::LEN + 2 + style::LEN + 7, zero.len());

        let mut zero_reader = FixedReader::new(&zero);
        let decoded_zero = decode_payload(&mut zero_reader).unwrap();
        assert_eq!(0, decoded_zero.styles.count());
    }

    #[test]
    fn decode_accepts_unordered_sparse_hyperlink_ids() {
        let h = header(2, 1, 0, 2, 0, 512, 0, 6);

        let first = implicit(1, b"one");
        let second = implicit(2, b"two");

        let mut encoded: Vec<u8> = Vec::new();
        h.encode(&mut encoded).unwrap();
        io::write_int::<TerminalHyperlinkId>(&mut encoded, 3).unwrap();
        hyperlink::encode(&first, &mut encoded).unwrap();
        io::write_int::<TerminalHyperlinkId>(&mut encoded, 2).unwrap();
        hyperlink::encode(&second, &mut encoded).unwrap();
        encoded
            .write_byte(
                grid::Row {
                    cell_width: grid::EncodedWidth::Eight,
                    ..Default::default()
                }
                .to_bits(),
            )
            .unwrap();
        io::write_int::<u16>(&mut encoded, 2).unwrap(); // cell count
        io::write_int::<u64>(
            &mut encoded,
            grid::Cell {
                content: 'A' as u32,
                hyperlink: true,
                hyperlink_id: 3,
                ..Default::default()
            }
            .to_bits(),
        )
        .unwrap();
        io::write_int::<u64>(
            &mut encoded,
            grid::Cell {
                content: 'B' as u32,
                hyperlink: true,
                hyperlink_id: 2,
                ..Default::default()
            }
            .to_bits(),
        )
        .unwrap();
        io::write_int::<u32>(&mut encoded, 0).unwrap(); // grapheme section
        assert_eq!(Header::LEN + 2 * 14 + 3 + 2 * 8 + 4, encoded.len());

        let mut reader = FixedReader::new(&encoded);
        let decoded = decode_payload(&mut reader).unwrap();
        assert_eq!(2, decoded.hyperlink_set.count());
        let first_id = decoded.lookup_hyperlink(rac(&decoded, 0, 0).1).unwrap();
        let second_id = decoded.lookup_hyperlink(rac(&decoded, 1, 0).1).unwrap();
        let mem = decoded.memory;
        unsafe {
            assert_eq!(1, decoded.hyperlink_set.ref_count(mem, first_id));
            assert_eq!(1, decoded.hyperlink_set.ref_count(mem, second_id));
            assert_eq!(b"one", (*decoded.hyperlink_set.get(mem, first_id)).uri.slice(mem));
            assert_eq!(b"two", (*decoded.hyperlink_set.get(mem, second_id)).uri.slice(mem));
        }
    }

    #[test]
    fn decode_defaults_missing_sparse_cell_references() {
        // An unknown style ID falls back to the default style.
        let style_header = header(1, 1, 0, 0, 8, 0, 0, 0);

        let mut style_encoded: Vec<u8> = Vec::new();
        style_header.encode(&mut style_encoded).unwrap();
        style_encoded.write_byte(0x30).unwrap(); // row flags, full cell width
        io::write_int::<u16>(&mut style_encoded, 1).unwrap(); // cell count
        io::write_int::<u64>(
            &mut style_encoded,
            grid::Cell {
                style_id: 1,
                ..Default::default()
            }
            .to_bits(),
        )
        .unwrap();
        io::write_int::<u32>(&mut style_encoded, 0).unwrap(); // grapheme section

        let mut style_reader = FixedReader::new(&style_encoded);
        let style_page = decode_payload(&mut style_reader).unwrap();
        assert_eq!(0, rac(&style_page, 0, 0).1.style_id());

        // An unknown hyperlink ID likewise falls back to no hyperlink.
        let hyperlink_header = header(1, 1, 0, 0, 0, 512, 0, 0);

        let mut hyperlink_encoded: Vec<u8> = Vec::new();
        hyperlink_header.encode(&mut hyperlink_encoded).unwrap();
        hyperlink_encoded.write_byte(0x30).unwrap(); // row flags, full cell width
        io::write_int::<u16>(&mut hyperlink_encoded, 1).unwrap(); // cell count
        io::write_int::<u64>(
            &mut hyperlink_encoded,
            grid::Cell {
                hyperlink: true,
                hyperlink_id: 1,
                ..Default::default()
            }
            .to_bits(),
        )
        .unwrap();
        io::write_int::<u32>(&mut hyperlink_encoded, 0).unwrap(); // grapheme section

        let mut hyperlink_reader = FixedReader::new(&hyperlink_encoded);
        let hyperlink_page = decode_payload(&mut hyperlink_reader).unwrap();
        let hyperlink_cell = rac(&hyperlink_page, 0, 0).1;
        assert!(!hyperlink_cell.hyperlink());
        assert_eq!(None, hyperlink_page.lookup_hyperlink(hyperlink_cell));
    }

    #[test]
    fn decode_normalizes_invalid_grid_semantics() {
        let h = header(3, 1, 0, 0, 0, 0, 0, 0);

        let mut encoded: Vec<u8> = Vec::new();
        h.encode(&mut encoded).unwrap();

        // Preserve wrap while degrading the unknown semantic-prompt value to none
        // and ignoring all reserved row bits.
        encoded.write_byte(0xFD).unwrap();
        io::write_int::<u16>(&mut encoded, 3).unwrap();

        // A text cell replaces an invalid Unicode scalar with U+FFFD while its
        // reserved semantic content degrades to output.
        io::write_int::<u64>(
            &mut encoded,
            grid::Cell {
                content: 0xD800,
                semantic_content: 3,
                ..Default::default()
            }
            .to_bits(),
        )
        .unwrap();

        // A declared grapheme suffix cannot fit the advertised zero capacity,
        // so decoding preserves the base character and drops the suffix.
        io::write_int::<u64>(
            &mut encoded,
            grid::Cell {
                kind: 1,
                content: 'x' as u32,
                ..Default::default()
            }
            .to_bits(),
        )
        .unwrap();

        // Reserved palette content bits do not obscure the valid low palette
        // byte.
        io::write_int::<u64>(
            &mut encoded,
            grid::Cell {
                kind: 2,
                content: 0xFFFF07,
                ..Default::default()
            }
            .to_bits(),
        )
        .unwrap();

        // The grapheme section carries the suffix for the second cell.
        io::write_int::<u32>(&mut encoded, 1).unwrap();
        io::write_int::<u16>(&mut encoded, 0).unwrap();
        io::write_int::<u16>(&mut encoded, 1).unwrap();
        io::write_int::<u16>(&mut encoded, 1).unwrap();
        io::write_int::<u32>(&mut encoded, 0x0301).unwrap();
        assert_eq!(Header::LEN + 3 + 3 * 8 + 4 + 10, encoded.len());

        let mut reader = FixedReader::new(&encoded);
        let page = decode_payload(&mut reader).unwrap();
        page.verify_integrity().unwrap();

        let (first_row, first) = rac(&page, 0, 0);
        assert!(first_row.wrap());
        assert_eq!(SemanticPrompt::None, first_row.semantic_prompt());
        assert_eq!(0xFFFD, first.codepoint());
        assert_eq!(Wide::Narrow, first.wide());
        assert_eq!(SemanticContent::Output, first.semantic_content());
        assert!(!first.protected());
        assert!(!first.has_grapheme());

        let second = rac(&page, 1, 0).1;
        assert_eq!('x' as u32, second.codepoint());
        assert!(!second.has_grapheme());

        let third = rac(&page, 2, 0).1;
        assert_eq!(ContentTag::BgColorPalette, third.content_tag());
        assert_eq!(7, third.content_color_palette());
        assert!(!third.has_grapheme());
    }

    #[test]
    fn decode_rejects_capacity_exceeding_maximum_page_size() {
        let cases = [
            header(80, 24, 0, 0, 0, 0, u32::MAX, 0),
            header(80, 24, 0, 0, 0, 0, 0, u32::MAX),
            header(23169, 23170, 0, 0, 0, u16::MAX, 0, 0),
            header(u16::MAX, u16::MAX, 0, 0, 0, 0, 0, 0),
        ];

        for h in cases {
            let mut encoded = [0u8; Header::LEN];
            let mut writer = FixedWriter::new(&mut encoded);
            h.encode(&mut writer).unwrap();

            let mut reader = FixedReader::new(writer.buffered());
            assert_eq!(Some(Error::PageTooLarge), decode_payload(&mut reader).err());
        }
    }

    #[test]
    fn decode_validates_dimensions() {
        let cases = [header(0, 24, 0, 0, 0, 0, 0, 0), header(80, 0, 0, 0, 0, 0, 0, 0)];

        for h in cases {
            let mut encoded = [0u8; Header::LEN];
            let mut writer = FixedWriter::new(&mut encoded);
            h.encode(&mut writer).unwrap();

            let mut reader = FixedReader::new(writer.buffered());
            assert_eq!(Some(Error::InvalidDimensions), decode_payload(&mut reader).err());
        }
    }

    /// A reader with no buffer that fails exactly once after
    /// `bytes_before_failure` bytes.
    struct FailOnceReader<'a> {
        source: FixedReader<'a>,
        bytes_before_failure: usize,
        failed: bool,
    }

    impl io::Reader for FailOnceReader<'_> {
        fn buffered(&self) -> &[u8] {
            &[]
        }
        fn toss(&mut self, n: usize) {
            assert_eq!(0, n);
        }
        fn fill_more(&mut self) -> io::Result<()> {
            Err(Error::ReadFailed)
        }
        fn read_direct(&mut self, dest: &mut [u8]) -> io::Result<usize> {
            if self.bytes_before_failure == 0 && !self.failed {
                self.failed = true;
                return Err(Error::ReadFailed);
            }

            let limit = if self.failed {
                dest.len()
            } else {
                dest.len().min(self.bytes_before_failure)
            };
            let n = self.source.read_direct(&mut dest[..limit])?;
            if !self.failed {
                self.bytes_before_failure -= n;
            }
            Ok(n)
        }
    }

    #[test]
    fn decode_propagates_a_style_table_read_failure() {
        let h = header(1, 1, 1, 0, 8, 0, 0, 0);

        // If the style read error is swallowed, its sixteen zero bytes are then
        // misread as a valid empty grid and the unframed payload appears to decode.
        let mut encoded = [0u8; Header::LEN + 2 + style::LEN];
        let mut writer = FixedWriter::new(&mut encoded);
        h.encode(&mut writer).unwrap();
        io::write_int::<TerminalStyleId>(&mut writer, 1).unwrap();
        writer.splat_byte_all(0, style::LEN).unwrap();

        let mut source = FailOnceReader {
            source: FixedReader::new(writer.buffered()),
            bytes_before_failure: Header::LEN + 2,
            failed: false,
        };
        match decode_payload(&mut source) {
            Err(err) => assert_eq!(Error::ReadFailed, err),
            Ok(_) => panic!("expected ReadFailed"),
        }
    }

    #[test]
    fn decode_normalizes_duplicate_default_and_invalid_style_entries() {
        let h = header(1, 1, 2, 0, 16, 512, 0, 256);
        let duplicate_style = bold();

        // Generate a valid grid whose cell references a sparse style ID. The PAGE
        // table below can then give that ID duplicate, default, or invalid contents
        // without hand-authoring the cell wire format.
        let grid_page = Page::init(Capacity {
            cols: 1,
            rows: 1,
            styles: 8,
            ..Default::default()
        })
        .unwrap();
        let gmem = grid_page.memory;
        let grid_page = {
            let mut grid_page = grid_page;
            unsafe {
                let released_style_a = grid_page.styles.add(gmem, italic()).unwrap();
                let released_style_b = grid_page
                    .styles
                    .add(
                        gmem,
                        Style {
                            bg_color: Color::Palette(1),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                let encoded_style_id = grid_page.styles.add(gmem, duplicate_style).unwrap();
                grid_page.styles.release(gmem, released_style_a);
                grid_page.styles.release(gmem, released_style_b);
                let (grow, gcell) = rac(&grid_page, 0, 0);
                *gcell = Cell::init('A' as u32);
                gcell.set_style_id(encoded_style_id);
                grow.set_styled(true);
                (grid_page, encoded_style_id)
            }
        };
        let (grid_page, encoded_style_id) = grid_page;

        let mut grid_bytes: Vec<u8> = Vec::new();
        grid::encode(&grid_page, &mut grid_bytes).unwrap();

        let mut encoded: Vec<u8> = Vec::new();
        h.encode(&mut encoded).unwrap();
        io::write_int::<TerminalStyleId>(&mut encoded, 1).unwrap();
        style::encode(duplicate_style, &mut encoded).unwrap();
        io::write_int::<TerminalStyleId>(&mut encoded, encoded_style_id).unwrap();
        style::encode(duplicate_style, &mut encoded).unwrap();
        encoded.write_all(&grid_bytes).unwrap();

        let mut reader = FixedReader::new(&encoded);
        let duplicate_decoded = decode_payload(&mut reader).unwrap();
        assert_eq!(1, duplicate_decoded.styles.count());
        let duplicate_cell = rac(&duplicate_decoded, 0, 0).1;
        assert_ne!(0, duplicate_cell.style_id());
        assert!(unsafe {
            (*duplicate_decoded
                .styles
                .get(duplicate_decoded.memory, duplicate_cell.style_id()))
            .flags
            .bold
        });

        let default_header = header(1, 1, 1, 0, 16, 0, 0, 0);
        let mut default_encoded: Vec<u8> = Vec::new();
        default_header.encode(&mut default_encoded).unwrap();
        io::write_int::<TerminalStyleId>(&mut default_encoded, encoded_style_id).unwrap();
        style::encode(Style::default(), &mut default_encoded).unwrap();
        default_encoded.write_all(&grid_bytes).unwrap();

        let mut default_reader = FixedReader::new(&default_encoded);
        let default_decoded = decode_payload(&mut default_reader).unwrap();
        assert_eq!(0, rac(&default_decoded, 0, 0).1.style_id());

        // The strict style codec rejects this kind, but PAGE owns the fixed entry
        // boundary and can safely map the encoded ID to the default style. A zero
        // capacity hint also remains advisory.
        let mut invalid_header = default_header;
        invalid_header.style_capacity = 0;
        let mut invalid_encoded: Vec<u8> = Vec::new();
        invalid_header.encode(&mut invalid_encoded).unwrap();
        io::write_int::<TerminalStyleId>(&mut invalid_encoded, encoded_style_id).unwrap();
        invalid_encoded.write_byte(3).unwrap();
        invalid_encoded.splat_byte_all(0, style::LEN - 1).unwrap();
        invalid_encoded.write_all(&grid_bytes).unwrap();

        let mut invalid_reader = FixedReader::new(&invalid_encoded);
        let invalid_decoded = decode_payload(&mut invalid_reader).unwrap();
        assert_eq!(0, rac(&invalid_decoded, 0, 0).1.style_id());
    }

    #[test]
    fn decode_releases_unused_style_table_references() {
        let h = header(1, 1, 1, 0, 8, 0, 0, 0);

        let empty_page = Page::init(cap(1, 1)).unwrap();
        let mut grid_bytes: Vec<u8> = Vec::new();
        grid::encode(&empty_page, &mut grid_bytes).unwrap();

        let mut encoded: Vec<u8> = Vec::new();
        h.encode(&mut encoded).unwrap();
        io::write_int::<TerminalStyleId>(&mut encoded, 7).unwrap();
        style::encode(bold(), &mut encoded).unwrap();
        encoded.write_all(&grid_bytes).unwrap();

        let mut reader = FixedReader::new(&encoded);
        let decoded = decode_payload(&mut reader).unwrap();
        assert_eq!(0, decoded.styles.count());

        // The dead table-only entry is absent from the very first re-encode.
        let mut canonical = [0u8; 64];
        let mut canonical_writer = FixedWriter::new(&mut canonical);
        encode_payload(&decoded, &mut canonical_writer).unwrap();
        let mut canonical_reader = FixedReader::new(canonical_writer.buffered());
        let canonical_header = Header::decode(&mut canonical_reader).unwrap();
        assert_eq!(0, canonical_header.style_count);
    }

    #[test]
    fn decode_reuses_duplicate_hyperlinks() {
        let h = header(1, 1, 0, 2, 0, 512, 0, 16);
        let duplicate = explicit(b"id", b"uri");

        // As with styles above, use a sparse native ID to make grid.encode produce
        // the cell reference whose table value this test deliberately duplicates.
        let mut grid_page = Page::init(Capacity {
            cols: 1,
            rows: 1,
            hyperlink_bytes: 512,
            string_bytes: 64,
            ..Default::default()
        })
        .unwrap();
        let gmem = grid_page.memory;
        let released_hyperlink_a = grid_page.insert_hyperlink(&implicit(1, b"one")).unwrap();
        let released_hyperlink_b = grid_page.insert_hyperlink(&implicit(2, b"two")).unwrap();
        let encoded_hyperlink_id = grid_page.insert_hyperlink(&implicit(3, b"three")).unwrap();
        unsafe {
            grid_page.hyperlink_set.release(gmem, released_hyperlink_a);
            grid_page.hyperlink_set.release(gmem, released_hyperlink_b);
        }
        let (grow, gcell) = rac(&grid_page, 0, 0);
        *gcell = Cell::init('A' as u32);
        grid_page.set_hyperlink(grow, gcell, encoded_hyperlink_id).unwrap();

        let mut grid_bytes: Vec<u8> = Vec::new();
        grid::encode(&grid_page, &mut grid_bytes).unwrap();

        let mut encoded: Vec<u8> = Vec::new();
        h.encode(&mut encoded).unwrap();
        io::write_int::<TerminalHyperlinkId>(&mut encoded, 1).unwrap();
        hyperlink::encode(&duplicate, &mut encoded).unwrap();
        io::write_int::<TerminalHyperlinkId>(&mut encoded, encoded_hyperlink_id).unwrap();
        hyperlink::encode(&duplicate, &mut encoded).unwrap();
        encoded.write_all(&grid_bytes).unwrap();

        let mut reader = FixedReader::new(&encoded);
        let mut decoded = decode_payload(&mut reader).unwrap();
        assert_eq!(1, decoded.hyperlink_set.count());
        let cell = rac(&decoded, 0, 0).1;
        assert!(cell.hyperlink());
        let id = decoded.lookup_hyperlink(cell).unwrap();
        let mem = decoded.memory;
        unsafe {
            let entry = &*decoded.hyperlink_set.get(mem, id);
            assert_eq!(1, decoded.hyperlink_set.ref_count(mem, id));
            assert_eq!(b"uri", entry.uri.slice(mem));
        }

        // Overwriting the sole linked cell must make the deduplicated entry dead.
        // Any surviving reference belongs to the decode-time wire table, not the
        // native page.
        let row = unsafe { &mut *decoded.get_row(0) };
        decoded.clear_cells(row, 0, 1);
        assert_eq!(0, decoded.hyperlink_set.count());
    }

    #[test]
    fn discard_consumes_exactly_one_page_record() {
        // Discard validates framing only, so an arbitrary payload keeps this test
        // focused on record consumption rather than page structure.
        let mut encoded: Vec<u8> = Vec::new();
        {
            let mut stream = record::Writer::init(&mut encoded);
            let payload = stream.begin(record::Tag::PAGE);
            payload.write_all(b"undecodable page payload").unwrap();
            stream.finish().unwrap();
        }
        let record_len = encoded.len();
        encoded.write_all(b"next").unwrap();

        let mut source = FixedReader::new(&encoded);
        let h = record::next_header(&mut source).unwrap();
        discard(&mut source, h).unwrap();
        assert_eq!(b"next".to_vec(), source.take(4).unwrap());

        // Only PAGE records may be discarded; the tag remains strict.
        let mut wrong_tag: Vec<u8> = Vec::new();
        {
            let mut wrong_tag_stream = record::Writer::init(&mut wrong_tag);
            let payload = wrong_tag_stream.begin(record::Tag::HISTORY);
            payload.write_all(b"payload").unwrap();
            wrong_tag_stream.finish().unwrap();
        }
        let mut wrong_tag_source = FixedReader::new(&wrong_tag);
        let h = record::next_header(&mut wrong_tag_source).unwrap();
        assert_eq!(Err(Error::UnexpectedRecordTag), discard(&mut wrong_tag_source, h));

        // Discarded bytes are still covered by the record CRC.
        let mut corrupted = encoded[..record_len].to_vec();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 1;
        let mut corrupted_source = FixedReader::new(&corrupted);
        let h = record::next_header(&mut corrupted_source).unwrap();
        assert_eq!(Err(Error::InvalidChecksum), discard(&mut corrupted_source, h));

        // A truncated payload is detected before `finish`.
        let mut truncated_source = FixedReader::new(&encoded[..record_len - 1]);
        let h = record::next_header(&mut truncated_source).unwrap();
        assert_eq!(Err(Error::EndOfStream), discard(&mut truncated_source, h));
    }

    #[test]
    fn decode_ignores_empty_hyperlink_strings() {
        let h = header(1, 1, 0, 1, 0, 0, 0, 16);
        let fixtures: [&[u8]; 4] = [
            // Empty implicit URI.
            b"\x01\x01\x00\x00\x00\x00\x00\x00\x00",
            // Empty explicit ID.
            b"\x02\x00\x00\x00\x00\x03\x00\x00\x00uri",
            // Empty explicit URI.
            b"\x02\x02\x00\x00\x00id\x00\x00\x00\x00",
            // Valid contents that cannot fit the zero-capacity native set.
            b"\x01\x01\x00\x00\x00\x03\x00\x00\x00uri",
        ];

        // Only the invalid hyperlink entry itself is hand-authored above. Generate
        // its cell reference through the native grid encoder.
        let mut grid_page = Page::init(Capacity {
            cols: 1,
            rows: 1,
            hyperlink_bytes: 256,
            string_bytes: 16,
            ..Default::default()
        })
        .unwrap();
        let encoded_hyperlink_id = grid_page.insert_hyperlink(&implicit(1, b"uri")).unwrap();
        let (grow, gcell) = rac(&grid_page, 0, 0);
        *gcell = Cell::init('A' as u32);
        grid_page.set_hyperlink(grow, gcell, encoded_hyperlink_id).unwrap();
        let mut grid_bytes: Vec<u8> = Vec::new();
        grid::encode(&grid_page, &mut grid_bytes).unwrap();

        for fixture in fixtures {
            let mut encoded: Vec<u8> = Vec::new();
            h.encode(&mut encoded).unwrap();
            io::write_int::<TerminalHyperlinkId>(&mut encoded, encoded_hyperlink_id).unwrap();
            encoded.write_all(fixture).unwrap();

            // The cell refers to the ignored table entry. Its absent native
            // remapping must degrade to no hyperlink while preserving the cell.
            encoded.write_all(&grid_bytes).unwrap();

            let mut reader = FixedReader::new(&encoded);
            let decoded = decode_payload(&mut reader).unwrap();

            assert_eq!(0, decoded.hyperlink_set.count());
            let cell = rac(&decoded, 0, 0).1;
            assert_eq!('A' as u32, cell.codepoint());
            assert!(!cell.hyperlink());
            assert_eq!(None, decoded.lookup_hyperlink(cell));
        }
    }
}
