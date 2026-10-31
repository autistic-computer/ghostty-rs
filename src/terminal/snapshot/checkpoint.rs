//! Port of `src/terminal/snapshot/checkpoint.zig`.
//!
//! STATUS: ported
//!
//! READY and FINISH snapshot marker records.
//!
//! READY separates the renderable terminal state from the history sequences.
//! FINISH terminates one snapshot; bytes after it belong to the containing
//! transport. Both markers have empty payloads, independently framed and
//! protected by the same CRC32C as every other record.
//!
//! ## Binary Format
//!
//! READY and FINISH contain only their record headers. Their declared payload
//! length must be zero.

use super::io::{self, Error};
use super::record;

/// Selects one of the two marker positions in a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ready,
    Finish,
}

impl Kind {
    fn tag(self) -> record::Tag {
        match self {
            Kind::Ready => record::Tag::READY,
            Kind::Finish => record::Tag::FINISH,
        }
    }
}

pub type EncodeError = record::FinishError;

/// Append one empty marker record.
pub fn encode(kind: Kind, stream: &mut record::Writer<'_>) -> Result<(), EncodeError> {
    let _ = stream.begin(kind.tag());
    stream.finish()
}

/// Errors from `decode`: the record finish errors, or `UnexpectedRecordTag`
/// when the record is valid but is not the expected marker.
pub type DecodeError = Error;

/// Decode one marker record of the given kind and require its payload to
/// be empty.
///
/// `record_header` is the record's header, already read from `source`. A
/// header for any other kind of record, including the other marker, returns
/// `UnexpectedRecordTag`. See "Decoding Records" in `record.rs`.
pub fn decode(kind: Kind, source: &mut dyn io::Reader, record_header: record::Header) -> Result<(), DecodeError> {
    let mut record_reader = record::Reader::init_header(source, record_header);
    if record_reader.header.tag != kind.tag() {
        return Err(Error::UnexpectedRecordTag);
    }
    record_reader.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::snapshot::fixture;
    use crate::terminal::snapshot::io::{FixedReader, Reader as _, Writer as _};

    #[test]
    fn ready_golden_encoding() {
        let prefix = b"abc";

        let mut snapshot: Vec<u8> = Vec::new();
        {
            let mut stream = record::Writer::init(&mut snapshot);
            stream.writer().write_all(prefix).unwrap();
            encode(Kind::Ready, &mut stream).unwrap();
        }

        let reference = fixture::parse(include_str!("testdata/checkpoint-ready-v1.hex"));
        fixture::expect_equal(
            fixture::Kind::Bytes,
            "src/terminal/snapshot/testdata/checkpoint-ready-v1.hex",
            "snapshot_fixture-checkpoint-ready-v1.hex",
            &reference,
            &snapshot,
        );

        // Decode the checked-in record rather than the generated candidate.
        let mut source = FixedReader::new(&reference);
        source.discard_all(prefix.len()).unwrap();
        let header = record::next_header(&mut source).unwrap();
        decode(Kind::Ready, &mut source, header).unwrap();
    }

    #[test]
    fn ready_and_finish_are_empty_marker_records() {
        let mut snapshot: Vec<u8> = Vec::new();
        let ready_offset;
        let finish_offset;
        {
            let mut stream = record::Writer::init(&mut snapshot);
            stream.writer().write_all(b"prefix").unwrap();
            ready_offset = 6;
            encode(Kind::Ready, &mut stream).unwrap();
            stream.writer().write_all(b"history").unwrap();
            finish_offset = ready_offset + record::Header::LEN + 7;
            encode(Kind::Finish, &mut stream).unwrap();
        }

        assert_eq!(record::Header::LEN, finish_offset - ready_offset - b"history".len());
        assert_eq!(record::Header::LEN, snapshot.len() - finish_offset);
        assert_eq!(b"history", &snapshot[ready_offset + record::Header::LEN..finish_offset]);

        let mut source = FixedReader::new(&snapshot);
        source.discard_all(ready_offset).unwrap();
        let header = record::next_header(&mut source).unwrap();
        decode(Kind::Ready, &mut source, header).unwrap();
        source.discard_all(b"history".len()).unwrap();
        let header = record::next_header(&mut source).unwrap();
        decode(Kind::Finish, &mut source, header).unwrap();
    }

    #[test]
    fn checkpoint_rejects_wrong_tags_and_nonempty_payloads() {
        let mut snapshot: Vec<u8> = Vec::new();
        {
            let mut stream = record::Writer::init(&mut snapshot);
            encode(Kind::Ready, &mut stream).unwrap();
        }

        let mut wrong_tag_source = FixedReader::new(&snapshot);
        let header = record::next_header(&mut wrong_tag_source).unwrap();
        assert_eq!(
            Err(Error::UnexpectedRecordTag),
            decode(Kind::Finish, &mut wrong_tag_source, header)
        );

        let mut invalid: Vec<u8> = Vec::new();
        {
            let mut invalid_stream = record::Writer::init(&mut invalid);
            let invalid_payload = invalid_stream.begin(record::Tag::READY);
            invalid_payload.write_byte(0).unwrap();
            invalid_stream.finish().unwrap();
        }

        let mut invalid_source = FixedReader::new(&invalid);
        let header = record::next_header(&mut invalid_source).unwrap();
        assert_eq!(
            Err(Error::PayloadNotExhausted),
            decode(Kind::Ready, &mut invalid_source, header)
        );
    }

    #[test]
    fn finish_leaves_continuation_bytes_unread() {
        let mut finished: Vec<u8> = Vec::new();
        {
            let mut finished_stream = record::Writer::init(&mut finished);
            encode(Kind::Finish, &mut finished_stream).unwrap();
        }
        finished.write_byte(0).unwrap();

        let mut source = FixedReader::new(&finished);
        let header = record::next_header(&mut source).unwrap();
        decode(Kind::Finish, &mut source, header).unwrap();
        assert_eq!(0u8, source.take_byte().unwrap());
    }
}
