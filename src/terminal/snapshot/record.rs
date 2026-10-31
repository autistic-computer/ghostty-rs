//! Port of `src/terminal/snapshot/record.zig`.
//!
//! STATUS: ported
//!
//! Snapshot record framing.
//!
//! Records begin immediately after the single snapshot envelope and continue
//! back-to-back until FINISH. Each record has one fixed-size header followed
//! by exactly `payload_len` bytes. The payload length limits the record reader
//! so malformed payloads cannot consume bytes from the following record.
//!
//! The CRC covers the first six header bytes (tag and payload length)
//! followed by the payload. The CRC field itself is not included for obvious
//! reasons. All integers are unsigned and little-endian.
//!
//! | Offset | Size          | Field                  |
//! | -----: | ------------: | :--------------------- |
//! |      0 |             2 | Tag (`u16`)            |
//! |      2 |             4 | Payload length (`u32`) |
//! |      6 |             4 | CRC32C (`u32`)         |
//! |     10 | `payload_len` | Payload                |
//!
//! The tags this build understands are listed in `Tag`.
//!
//! ## Unknown Records
//!
//! A decoder skips any record whose tag it does not know, wherever that
//! record appears, and continues with the next record. Skipping still reads
//! the whole record and checks its CRC32C, so a damaged record fails
//! decoding even when its tag is unknown.
//!
//! For example, a decoder sees this sequence as two PAGE records in a row:
//!
//! ```text
//! PAGE | tag 0x4000 (unknown, skipped) | PAGE
//! ```
//!
//! Records with known tags are checked as carefully as ever. Their payloads
//! must be read exactly to the end, and the order and counts that other
//! records declare for them must match.
//!
//! This is how the format grows without changing its version. A newer
//! encoder can add new kinds of records, and older decoders skip them. That
//! only works if every new kind of record is optional, meaning the terminal
//! is still correct when a decoder skips it. Anything the terminal cannot be
//! correct without, and any change to the layout of an existing record,
//! needs a new snapshot version instead.
//!
//! You rarely need to think about this when writing a record decoder.
//! `next_header` does the skipping, and every header is read through it.
//!
//! ## Decoding Records
//!
//! Decoding a record is a two-step job. The caller reads the record's
//! header with `next_header`, then passes that header to the decoder for the
//! record, such as `screen::decode` or `checkpoint::decode`. The decoder
//! reads the payload that follows from the same source.
//!
//! ```text
//! // Read the header, skipping any unknown records before it.
//! let header = record::next_header(source)?;
//!
//! // Decode the record it belongs to.
//! checkpoint::decode(Kind::Ready, source, header)?;
//! ```
//!
//! Splitting the work this way lets the caller look at the tag first. A
//! caller that accepts several kinds of records in one position reads the
//! header, switches on `header.tag`, and calls the matching decoder.
//!
//! A caller that expects exactly one kind of record can pass the header
//! along without checking it. Every decoder checks the tag itself and
//! returns `Error::UnexpectedRecordTag` for a header of any other kind, so
//! that check lives in one place.
//!
//! Decoders that are followed by a sequence of records, such as a SCREEN
//! and its PAGE records, read the headers of the records in that sequence
//! themselves. Only the first header comes from the caller.
//!
//! Notes on the port:
//! - `Tag` is a non-exhaustive `enum(u16)` in Zig; here it is a newtype
//!   over `u16` with associated constants for the named tags.
//! - `Reader.init`/`initHeader` construct a `Reader` that borrows the source
//!   instead of initializing one in place.

use crate::terminal::build_options;

use super::io::{self, Error, Hashed, HashingWriter, Limited, Reader as _, Writer as _};

/// CRC32C as specified by the snapshot format. This is the parameter set
/// Zig's standard library names after its iSCSI use, backed by dedicated
/// CRC32C instructions where the target has them.
pub use crate::crc32c::Crc32c;

/// Identifies the layout and meaning of a record payload.
///
/// A `Tag` can hold any 16-bit value, not just the ones named here, because
/// a snapshot may come from a newer encoder with tags this build has never
/// heard of. Use `known` to tell the two apart. Decoders skip records whose
/// tag is not known (see "Unknown Records" above).
///
/// Any tag added from now on must describe an optional record, one that a
/// decoder can skip without making the terminal incorrect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tag(pub u16);

impl Tag {
    /// Terminal-wide live state and configuration.
    pub const TERMINAL: Tag = Tag(1);

    /// One live screen and its page sequence.
    pub const SCREEN: Tag = Tag(2);

    /// One complete logical terminal page.
    pub const PAGE: Tag = Tag(3);

    /// One screen's complete history manifest and page sequence.
    pub const HISTORY: Tag = Tag(4);

    /// Marker separating the renderable terminal state from history.
    pub const READY: Tag = Tag(5);

    /// Marker terminating the complete snapshot blob.
    pub const FINISH: Tag = Tag(6);

    /// Canonical unfinished standard TerminalStream input.
    pub const CONTINUATION: Tag = Tag(7);

    /// One screen's Kitty graphics images, placements and in-progress
    /// load. Optional. See `kitty.zig`.
    pub const KITTY_IMAGE_STORAGE: Tag = Tag(8);

    /// The pixels of one Kitty graphics image or animation frame.
    /// Optional. See `kitty.zig`.
    pub const KITTY_IMAGE_PAYLOAD: Tag = Tag(9);

    /// The Kitty graphics placements pinned to the history PAGE right
    /// before this record. Optional. See `kitty.zig`.
    pub const KITTY_IMAGE_PAGE_PLACEMENTS: Tag = Tag(10);

    /// Returns true if this build decodes records with this tag, and false
    /// for a tag it should skip.
    pub fn known(self) -> bool {
        match self {
            Tag::TERMINAL
            | Tag::SCREEN
            | Tag::PAGE
            | Tag::HISTORY
            | Tag::READY
            | Tag::FINISH
            | Tag::CONTINUATION => true,

            // A build without Kitty graphics skips these records and
            // restores the terminal without its images.
            Tag::KITTY_IMAGE_STORAGE | Tag::KITTY_IMAGE_PAYLOAD | Tag::KITTY_IMAGE_PAGE_PLACEMENTS => {
                build_options::KITTY_GRAPHICS
            }

            _ => false,
        }
    }

    /// The Zig tag name of a named tag (Zig: `std.enums.tagName`).
    pub fn name(self) -> Option<&'static str> {
        Some(match self {
            Tag::TERMINAL => "terminal",
            Tag::SCREEN => "screen",
            Tag::PAGE => "page",
            Tag::HISTORY => "history",
            Tag::READY => "ready",
            Tag::FINISH => "finish",
            Tag::CONTINUATION => "continuation",
            Tag::KITTY_IMAGE_STORAGE => "kitty_image_storage",
            Tag::KITTY_IMAGE_PAYLOAD => "kitty_image_payload",
            Tag::KITTY_IMAGE_PAGE_PLACEMENTS => "kitty_image_page_placements",
            _ => return None,
        })
    }
}

/// The fixed framing that precedes every record payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Determines how the payload is decoded.
    pub tag: Tag,

    /// Number of payload bytes immediately following this header.
    pub payload_len: u32,

    /// CRC32C over the encoded tag, payload length, and payload.
    pub crc32c: u32,
}

/// Errors possible while decoding a record header.
pub type HeaderDecodeError = Error;

impl Header {
    /// Number of bytes written by `encode`. This size is part of the wire
    /// format. If it changes, the snapshot version and golden fixtures must
    /// also change.
    pub const LEN: usize = 10;

    /// Encode the fixed record header.
    pub fn encode(&self, writer: &mut dyn io::Writer) -> io::Result<()> {
        encode_checksum_prefix(self.tag, self.payload_len, writer)?;
        io::write_int(writer, self.crc32c)
    }

    /// Decode one fixed record header, whatever its tag. The payload length
    /// and CRC are checked later, while the payload is read.
    ///
    /// Most callers want `next_header` instead, which skips records with
    /// unknown tags and returns the first header this build understands.
    pub fn decode(reader: &mut dyn io::Reader) -> io::Result<Header> {
        Ok(Header {
            tag: Tag(io::read_int::<u16>(reader)?),
            payload_len: io::read_int::<u32>(reader)?,
            crc32c: io::read_int::<u32>(reader)?,
        })
    }
}

/// Errors possible from `next_header`. Besides read errors, a skipped
/// record can fail its checks: `InvalidChecksum` for a damaged record, or
/// `PayloadNotExhausted` if its payload cannot be read to the declared
/// length.
pub type NextHeaderError = Error;

/// Read the next record header whose tag this build knows, skipping any
/// records with unknown tags on the way.
///
/// Each skipped record is read to its end and its CRC32C is checked, so a
/// damaged record fails here even though its contents are ignored. On
/// return, `source` is positioned at the start of the returned header's
/// payload. See "Unknown Records" above.
///
/// Most record decoders call `Reader::init`, which uses this. Call it
/// directly when you need to see the tag before choosing how to decode the
/// record, then hand the header to `Reader::init_header`.
pub fn next_header(source: &mut dyn io::Reader) -> Result<Header, NextHeaderError> {
    loop {
        let header = Header::decode(source)?;
        if header.tag.known() {
            return Ok(header);
        }

        let mut skipped = Reader::init_header(source, header);
        skipped.payload_reader().discard_all(header.payload_len as usize)?;
        skipped.finish()?;
    }
}

/// A writer that calculates a record checksum without retaining payload bytes.
/// The checksum is initialized with the encoded tag and payload length.
#[derive(Debug, Clone, Copy)]
pub struct Checksum {
    pub hashing: HashingWriter,
}

impl Checksum {
    /// Begin a checksum with the encoded tag and payload length already added.
    pub fn init(tag: Tag, payload_len: u32) -> Checksum {
        let mut result = Checksum {
            hashing: HashingWriter { hasher: Crc32c::init() },
        };
        encode_checksum_prefix(tag, payload_len, &mut result.hashing).expect("hashing cannot fail");
        result
    }

    /// Return the writer through which the complete payload must be encoded.
    pub fn writer(&mut self) -> &mut dyn io::Writer {
        &mut self.hashing
    }

    /// Finish and return the checksum stored in the record header.
    pub fn final_(&self) -> u32 {
        self.hashing.hasher.final_()
    }
}

/// Streams complete records while retaining only one payload at a time.
///
/// The scratch allocation is retained between records so a stream's peak
/// memory is the largest record payload rather than the complete snapshot.
pub struct Writer<'a> {
    destination: &'a mut dyn io::Writer,
    pub scratch: Vec<u8>,
    pub active_tag: Option<Tag>,
}

/// Errors from `Writer::finish`: `WriteFailed`, or `PayloadTooLarge` when
/// the payload cannot be represented by the record's `u32` length.
pub type FinishError = Error;

impl<'a> Writer<'a> {
    pub fn init(destination: &'a mut dyn io::Writer) -> Writer<'a> {
        Writer {
            destination,
            scratch: Vec::new(),
            active_tag: None,
        }
    }

    /// Return the destination writer for unframed snapshot bytes.
    pub fn writer(&mut self) -> &mut dyn io::Writer {
        debug_assert!(self.active_tag.is_none());
        &mut *self.destination
    }

    /// Begin one record and return its reusable payload writer.
    pub fn begin(&mut self, tag: Tag) -> &mut Vec<u8> {
        debug_assert!(self.active_tag.is_none());
        debug_assert!(self.scratch.is_empty());
        self.active_tag = Some(tag);
        &mut self.scratch
    }

    /// Finish and emit the active record's header followed by its payload.
    ///
    /// Payload validation failures occur before this function and emit no part
    /// of the record. A destination failure here may have emitted a prefix.
    pub fn finish(&mut self) -> Result<(), FinishError> {
        let tag = self.active_tag.expect("finish without an active record");
        let result = self.emit(tag);
        self.cancel();
        result
    }

    fn emit(&mut self, tag: Tag) -> Result<(), FinishError> {
        let payload = &self.scratch[..];
        let payload_len = u32::try_from(payload.len()).map_err(|_| Error::PayloadTooLarge)?;

        // The CRC prefix contains the payload length, so checksum the retained
        // payload only after its final length is known.
        let mut checksum = Checksum::init(tag, payload_len);
        checksum.writer().write_all(payload).expect("hashing cannot fail");

        let header = Header {
            tag,
            payload_len,
            crc32c: checksum.final_(),
        };
        let mut header_bytes = [0u8; Header::LEN];
        let mut header_writer = io::FixedWriter::new(&mut header_bytes);
        header.encode(&mut header_writer).expect("header fits");

        self.destination.write_all(&header_bytes)?;
        self.destination.write_all(payload)?;
        Ok(())
    }

    /// Discard the active record without emitting any bytes.
    ///
    /// This is idempotent so an outer error cleanup may call it after `finish`
    /// has already cleared state following a destination failure.
    pub fn cancel(&mut self) {
        self.scratch.clear();
        self.active_tag = None;
    }
}

impl Drop for Writer<'_> {
    fn drop(&mut self) {
        debug_assert!(self.active_tag.is_none() || std::thread::panicking());
    }
}

/// Reads one complete record.
///
/// `init` decodes the header, `payload_reader` returns a reader limited to the
/// declared payload, and `finish` verifies exact exhaustion and the CRC32C.
pub struct Reader<'a> {
    pub header: Header,
    mode: Mode<'a>,
}

enum Mode<'a> {
    /// When the source already has the complete payload buffered, for
    /// example an in-memory snapshot, the payload is borrowed straight
    /// from the source buffer instead of streaming through the limited
    /// and hashing adapters. The checksum is then verified with one bulk
    /// update in `finish`, and the source is not advanced until `finish`.
    Borrowed(Borrowed<'a>),

    /// The limited reader streams into the checksum reader's buffer.
    Streaming(Hashed<Limited<&'a mut dyn io::Reader>>),
}

/// A payload borrowed in place from the source's buffer.
struct Borrowed<'a> {
    source: &'a mut dyn io::Reader,
    seek: usize,
    len: usize,
}

impl io::Reader for Borrowed<'_> {
    fn buffered(&self) -> &[u8] {
        &self.source.buffered()[self.seek..self.len]
    }
    fn toss(&mut self, n: usize) {
        debug_assert!(n <= self.len - self.seek);
        self.seek += n;
    }
    fn fill_more(&mut self) -> io::Result<()> {
        Err(Error::EndOfStream)
    }
}

/// Errors possible from `init`, which reads the header with `next_header`.
pub type InitError = NextHeaderError;

/// Errors detected after a payload decoder returns: `InvalidChecksum` when
/// the payload bytes do not match the CRC in the record header, or
/// `PayloadNotExhausted` when the decoder did not consume exactly
/// `payload_len` bytes.
pub type ReaderFinishError = Error;

impl<'a> Reader<'a> {
    /// Read the next record header with a known tag and get ready to read
    /// its payload. Records with unknown tags before it are skipped, as
    /// described in `next_header`.
    ///
    /// Decode the payload only through the reader returned by
    /// `payload_reader`.
    pub fn init(source: &'a mut dyn io::Reader) -> Result<Reader<'a>, InitError> {
        let header = next_header(source)?;
        Ok(Reader::init_header(source, header))
    }

    /// Get ready to read the payload of a header that was already read
    /// from `source`, usually with `next_header`. `source` must be
    /// positioned at the start of that payload.
    ///
    /// Use this when you need to look at the tag before deciding how to
    /// decode the record. Otherwise use `init`. The same rules apply
    /// afterward: read the payload through `payload_reader`, then call
    /// `finish`.
    pub fn init_header(source: &'a mut dyn io::Reader, header: Header) -> Reader<'a> {
        let len = header.payload_len as usize;

        // The complete payload is already sitting in the source buffer:
        // borrow it in place. Payload decoders read from a fixed reader
        // over the borrowed bytes and `finish` checksums them in one pass.
        if source.buffered_len() >= len {
            return Reader {
                header,
                mode: Mode::Borrowed(Borrowed { source, seek: 0, len }),
            };
        }

        // The limited reader normally streams directly into the checksum
        // reader's buffer. One byte is enough for operations that require it
        // to buffer, such as peek and discard.
        let limited = Limited::new(source, Some(len), 1);
        let checksum = Checksum::init(header.tag, header.payload_len);
        Reader {
            header,
            mode: Mode::Streaming(Hashed::new(limited, checksum.hashing.hasher)),
        }
    }

    /// Return the length-limited, checksum-updating payload reader.
    pub fn payload_reader(&mut self) -> &mut dyn io::Reader {
        match &mut self.mode {
            Mode::Borrowed(borrowed) => borrowed,
            Mode::Streaming(hashing) => hashing,
        }
    }

    /// Require exact payload exhaustion and validate its CRC32C.
    pub fn finish(&mut self) -> Result<(), ReaderFinishError> {
        match &mut self.mode {
            Mode::Borrowed(borrowed) => {
                if borrowed.seek != borrowed.len {
                    return Err(Error::PayloadNotExhausted);
                }

                let mut checksum = Checksum::init(self.header.tag, self.header.payload_len);
                checksum
                    .writer()
                    .write_all(&borrowed.source.buffered()[..borrowed.len])
                    .expect("hashing cannot fail");
                if checksum.final_() != self.header.crc32c {
                    return Err(Error::InvalidChecksum);
                }

                // The borrowed bytes validated, so consume them from the
                // source only now, leaving it positioned at the next record.
                borrowed.source.toss(borrowed.len);
                // Make a repeated finish a no-op exhaustion failure rather than
                // a double toss.
                borrowed.len = 0;
                borrowed.seek = 0;
                Ok(())
            }
            Mode::Streaming(hashing) => {
                if hashing.buffered_len() != 0
                    || hashing.source().buffered_len() != 0
                    || hashing.source().remaining != Some(0)
                {
                    return Err(Error::PayloadNotExhausted);
                }

                if hashing.hasher.final_() != self.header.crc32c {
                    return Err(Error::InvalidChecksum);
                }
                Ok(())
            }
        }
    }
}

/// Encode the portion of a record header covered by CRC32C.
fn encode_checksum_prefix(tag: Tag, payload_len: u32, writer: &mut dyn io::Writer) -> io::Result<()> {
    io::write_int(writer, tag.0)?;
    io::write_int(writer, payload_len)
}

/// Test helper that appends one complete record with any raw tag,
/// including tags that `Tag` does not name.
pub fn test_write_raw(writer: &mut dyn io::Writer, tag: u16, payload: &[u8]) -> io::Result<()> {
    let len = payload.len() as u32;
    let mut checksum = Checksum::init(Tag(tag), len);
    checksum.writer().write_all(payload)?;
    Header {
        tag: Tag(tag),
        payload_len: len,
        crc32c: checksum.final_(),
    }
    .encode(writer)?;
    writer.write_all(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::snapshot::fixture;
    use crate::terminal::snapshot::io::{FixedReader, FixedWriter, Reader as _};

    fn page_header_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/record-page-header-v1.hex"))
    }

    #[test]
    fn golden_page_record_header_and_checksum() {
        let page_header: &[u8] = b"\x50\x00\x18\x00\x00\x00\x00\x00\
            \x00\x00\x00\x00\x00\x00\x00\x00\
            \x00\x00\x00\x00\x00\x00\x00\x00";

        let mut checksum = Checksum::init(Tag::PAGE, page_header.len() as u32);
        checksum.writer().write_all(page_header).unwrap();
        assert_eq!(0x7178441bu32, checksum.final_());

        let header = Header {
            tag: Tag::PAGE,
            payload_len: page_header.len() as u32,
            crc32c: 0x7178441b,
        };
        let mut buf = [0u8; Header::LEN];
        let mut writer = FixedWriter::new(&mut buf);
        header.encode(&mut writer).unwrap();

        let reference = page_header_fixture();
        fixture::expect_equal(
            fixture::Kind::Bytes,
            "src/terminal/snapshot/testdata/record-page-header-v1.hex",
            "snapshot_fixture-record-page-header-v1.hex",
            &reference,
            writer.buffered(),
        );

        let mut reader = FixedReader::new(&reference);
        assert_eq!(header, Header::decode(&mut reader).unwrap());
    }

    #[test]
    fn headers_decode_unknown_tags() {
        for tag in [0u16, 12, u16::MAX] {
            let mut fixture = [0u8; Header::LEN];
            fixture[0..2].copy_from_slice(&tag.to_le_bytes());
            let mut reader = FixedReader::new(&fixture);
            let header = Header::decode(&mut reader).unwrap();
            assert_eq!(tag, header.tag.0);
            assert!(!header.tag.known());
        }
    }

    #[test]
    fn record_reader_skips_unknown_records() {
        let mut encoded: Vec<u8> = Vec::new();
        test_write_raw(&mut encoded, 0, b"").unwrap();
        test_write_raw(&mut encoded, 0x7fff, b"unknown payload").unwrap();
        test_write_raw(&mut encoded, Tag::PAGE.0, b"page").unwrap();

        // Skip both from a fully buffered source and through a one-byte buffer,
        // which forces the streaming payload path.
        for streaming in 0..2 {
            let mut fixed = FixedReader::new(&encoded);
            let mut limited = Limited::new(FixedReader::new(&encoded), None, 1);
            let source: &mut dyn io::Reader = if streaming == 1 { &mut limited } else { &mut fixed };

            let mut record_reader = Reader::init(source).unwrap();
            assert_eq!(Tag::PAGE, record_reader.header.tag);
            let mut payload = [0u8; 4];
            record_reader.payload_reader().read_slice_all(&mut payload).unwrap();
            record_reader.finish().unwrap();
            assert_eq!(b"page", &payload);
        }
    }

    #[test]
    fn record_reader_rejects_a_corrupted_unknown_record() {
        let mut encoded: Vec<u8> = Vec::new();
        test_write_raw(&mut encoded, 0x7fff, b"unknown payload").unwrap();
        test_write_raw(&mut encoded, Tag::PAGE.0, b"page").unwrap();
        encoded[Header::LEN] ^= 1;

        let mut source = FixedReader::new(&encoded);
        assert_eq!(Some(Error::InvalidChecksum), Reader::init(&mut source).err());
    }

    #[test]
    fn reject_every_header_truncation() {
        let reference = page_header_fixture();
        for len in 0..Header::LEN {
            let mut reader = FixedReader::new(&reference[..len]);
            assert_eq!(Err(Error::EndOfStream), Header::decode(&mut reader));
        }
    }

    #[test]
    fn record_reader_verifies_exhaustion_and_checksum() {
        let payload = b"snapshot payload";
        let mut checksum = Checksum::init(Tag::PAGE, payload.len() as u32);
        checksum.writer().write_all(payload).unwrap();
        let header = Header {
            tag: Tag::PAGE,
            payload_len: payload.len() as u32,
            crc32c: checksum.final_(),
        };

        let mut encoded = [0u8; Header::LEN + 16 + 4];
        let mut writer = FixedWriter::new(&mut encoded);
        header.encode(&mut writer).unwrap();
        writer.write_all(payload).unwrap();
        writer.write_all(b"next").unwrap();
        let written = writer.buffered().to_vec();

        let mut source = FixedReader::new(&written);
        let mut record_reader = Reader::init(&mut source).unwrap();
        let mut decoded = [0u8; 16];
        record_reader.payload_reader().read_slice_all(&mut decoded).unwrap();
        record_reader.finish().unwrap();
        assert_eq!(payload, &decoded);
        assert_eq!(b"next".to_vec(), source.take(4).unwrap());
    }

    #[test]
    fn record_reader_rejects_remaining_bytes_and_invalid_checksum() {
        let payload = b"payload";
        let header = Header {
            tag: Tag::PAGE,
            payload_len: payload.len() as u32,
            crc32c: 0,
        };

        let mut encoded = [0u8; Header::LEN + 7];
        let mut writer = FixedWriter::new(&mut encoded);
        header.encode(&mut writer).unwrap();
        writer.write_all(payload).unwrap();
        let written = writer.buffered().to_vec();

        {
            let mut source = FixedReader::new(&written);
            let mut record_reader = Reader::init(&mut source).unwrap();
            record_reader.payload_reader().take_byte().unwrap();
            assert_eq!(Err(Error::PayloadNotExhausted), record_reader.finish());
        }

        {
            let mut source = FixedReader::new(&written);
            let mut record_reader = Reader::init(&mut source).unwrap();
            record_reader.payload_reader().discard_all(payload.len()).unwrap();
            assert_eq!(Err(Error::InvalidChecksum), record_reader.finish());
        }
    }

    #[test]
    fn payload_limit_does_not_consume_the_next_record() {
        let payload = b"ab";
        let mut checksum = Checksum::init(Tag::PAGE, payload.len() as u32);
        checksum.writer().write_all(payload).unwrap();
        let header = Header {
            tag: Tag::PAGE,
            payload_len: payload.len() as u32,
            crc32c: checksum.final_(),
        };

        let mut encoded = [0u8; Header::LEN + 2 + 4];
        let mut writer = FixedWriter::new(&mut encoded);
        header.encode(&mut writer).unwrap();
        writer.write_all(payload).unwrap();
        writer.write_all(b"next").unwrap();
        let written = writer.buffered().to_vec();

        let mut source = FixedReader::new(&written);
        let mut record_reader = Reader::init(&mut source).unwrap();
        let mut too_long = [0u8; 3];
        assert_eq!(
            Err(Error::EndOfStream),
            record_reader.payload_reader().read_slice_all(&mut too_long)
        );
        record_reader.finish().unwrap();
        assert_eq!(b"next".to_vec(), source.take(4).unwrap());
    }

    #[test]
    fn record_writer_buffers_a_payload_and_appends_framing() {
        let mut destination: Vec<u8> = Vec::new();
        destination.write_all(b"prefix").unwrap();
        {
            let mut stream = Writer::init(&mut destination);
            let payload = stream.begin(Tag::PAGE);
            payload.write_all(b"payload").unwrap();
            stream.finish().unwrap();
        }

        let encoded = &destination;
        assert_eq!(b"prefix", &encoded[0..6]);

        let mut source = FixedReader::new(&encoded[6..]);
        let mut record_reader = Reader::init(&mut source).unwrap();
        assert_eq!(Tag::PAGE, record_reader.header.tag);
        assert_eq!(7u32, record_reader.header.payload_len);
        assert_eq!(b"payload".to_vec(), record_reader.payload_reader().take(7).unwrap());
        record_reader.finish().unwrap();
    }

    #[test]
    fn record_writer_cancel_preserves_preceding_bytes() {
        let mut destination: Vec<u8> = Vec::new();
        destination.write_all(b"prefix").unwrap();
        {
            let mut stream = Writer::init(&mut destination);

            let partial = stream.begin(Tag::PAGE);
            partial.write_all(b"partial").unwrap();
            let scratch_capacity = stream.scratch.capacity();
            stream.cancel();

            assert_eq!(scratch_capacity, stream.scratch.capacity());

            // Cancel retains the allocation but leaves it ready for the next record.
            let next = stream.begin(Tag::PAGE);
            next.write_all(b"next").unwrap();
            stream.finish().unwrap();
        }
        assert_eq!(b"prefix", &destination[..6]);

        let mut source = FixedReader::new(&destination[6..]);
        let mut record_reader = Reader::init(&mut source).unwrap();
        assert_eq!(b"next".to_vec(), record_reader.payload_reader().take(4).unwrap());
        record_reader.finish().unwrap();
    }

    #[test]
    fn record_writer_cancel_emits_nothing() {
        // Part of "record writer cancel preserves preceding bytes" in Zig:
        // the destination still holds only the prefix after a cancel.
        let mut destination: Vec<u8> = Vec::new();
        destination.write_all(b"prefix").unwrap();
        {
            let mut stream = Writer::init(&mut destination);
            stream.begin(Tag::PAGE).write_all(b"partial").unwrap();
            stream.cancel();
        }
        assert_eq!(b"prefix", &destination[..]);
    }

    #[test]
    fn record_writer_clears_active_state_after_destination_failure() {
        // The header fits but the payload does not, leaving a permitted partial
        // record in the destination while the reusable writer becomes idle again.
        let mut destination_bytes = [0u8; Header::LEN];
        let mut destination = FixedWriter::new(&mut destination_bytes);
        let mut stream = Writer::init(&mut destination);

        let payload = stream.begin(Tag::PAGE);
        payload.write_byte(0).unwrap();
        assert_eq!(Err(Error::WriteFailed), stream.finish());
        assert_eq!(None, stream.active_tag);
        assert_eq!(0, stream.scratch.len());
    }
}
