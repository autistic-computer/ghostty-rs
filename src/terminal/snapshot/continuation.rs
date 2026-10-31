//! Port of `src/terminal/snapshot/continuation.zig`.
//!
//! STATUS: ported
//!
//! Continuation record type.
//!
//! Continuation uses the standard record header. All trailing bytes
//! are payload bytes for the continuation state.
//!
//! A continuation's payload must be the minimal bytes to go from
//! a ground state to a non-ground state in a VT emulator. Any extra
//! bytes are treated as a validation error.

use super::io::{self, Error};
use super::record;
use crate::terminal::stream_continuation;

/// A borrowed continuation value supplied to encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'a> {
    /// Neither the VT parser nor UTF-8 decoder has unfinished state.
    Ground,

    /// Canonical replay-safe PTY bytes. Empty is equivalent to `Ground`.
    Bytes(&'a [u8]),
}

/// An owned continuation value stored in a decoded complete snapshot
/// (Zig: an allocator-owned `Value`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedValue {
    Ground,
    Bytes(Vec<u8>),
}

impl OwnedValue {
    /// Borrow as a `Value`.
    pub fn as_value(&self) -> Value<'_> {
        match self {
            OwnedValue::Ground => Value::Ground,
            OwnedValue::Bytes(bytes) => Value::Bytes(bytes),
        }
    }
}

/// `stream_continuation.ValidateError`, or `Overflow` when the common
/// record's u32 payload length cannot represent the bytes.
pub type ValidateError = Error;

/// Validate a continuation before any snapshot bytes are emitted.
pub fn validate(value: Value<'_>) -> Result<(), ValidateError> {
    match value {
        Value::Ground => Ok(()),
        Value::Bytes(bytes) => {
            if u32::try_from(bytes.len()).is_err() {
                return Err(Error::Overflow);
            }
            stream_continuation::validate(bytes)?;
            Ok(())
        }
    }
}

pub type EncodeError = Error;

/// Encode one complete CONTINUATION record.
pub fn encode(value: Value<'_>, stream: &mut record::Writer<'_>) -> Result<(), EncodeError> {
    validate(value)?;

    let payload = stream.begin(record::Tag::CONTINUATION);
    if let Value::Bytes(bytes) = value {
        if let Err(e) = io::Writer::write_all(payload, bytes) {
            stream.cancel();
            return Err(e);
        }
    }
    stream.finish()
}

/// Errors from `decode`: allocation, validation, framing and read errors,
/// `UnexpectedRecordTag` when the record is not CONTINUATION, and
/// `ContinuationLimitExceeded` when the declared payload exceeds caller
/// policy.
pub type DecodeError = Error;

/// Decode and validate one CONTINUATION record.
///
/// `record_header` is the record's header, already read from `source`. A
/// header for any other kind of record returns `UnexpectedRecordTag`.
/// See "Decoding Records" in `record.rs`.
///
/// A zero-length payload returns `Ground` without allocating. A nonempty
/// payload returns owned bytes.
pub fn decode(
    source: &mut dyn io::Reader,
    record_header: record::Header,
    max_bytes: usize,
) -> Result<OwnedValue, DecodeError> {
    let mut record_reader = record::Reader::init_header(source, record_header);
    if record_reader.header.tag != record::Tag::CONTINUATION {
        return Err(Error::UnexpectedRecordTag);
    }

    // Validate the header payload vs our max size since the header
    // enforces it.
    let len = record_reader.header.payload_len as usize;
    if len > max_bytes {
        return Err(Error::ContinuationLimitExceeded);
    }

    if len == 0 {
        // Empty is the ground representation, but finish is still required:
        // its CRC covers the tag and zero length even though there is no body.
        record_reader.finish()?;
        return Ok(OwnedValue::Ground);
    }

    // Consume the bytes
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|_| Error::OutOfMemory)?;
    bytes.resize(len, 0);
    record_reader.payload_reader().read_slice_all(&mut bytes)?;
    record_reader.finish()?;

    // Framing is valid, so it is now safe to classify the bytes as replay
    // state. Validation requires a minimal, unfinished, side-effect-free tail.
    stream_continuation::validate(&bytes)?;

    // Ownership transfers to the returned Value only after all validation.
    Ok(OwnedValue::Bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::snapshot::fixture;
    use crate::terminal::snapshot::io::FixedReader;

    fn ground_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/continuation-ground-v1.hex"))
    }
    fn csi_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/continuation-csi-v1.hex"))
    }

    fn encode_to_vec(value: Value<'_>) -> Result<Vec<u8>, Error> {
        let mut encoded = Vec::new();
        {
            let mut stream = record::Writer::init(&mut encoded);
            encode(value, &mut stream)?;
        }
        Ok(encoded)
    }

    #[test]
    fn continuation_golden_records() {
        struct Golden {
            path: &'static str,
            candidate: &'static str,
            value: Value<'static>,
            expected: Vec<u8>,
        }
        let values = [
            Golden {
                path: "src/terminal/snapshot/testdata/continuation-ground-v1.hex",
                candidate: "snapshot_fixture-continuation-ground-v1.hex",
                value: Value::Ground,
                expected: ground_fixture(),
            },
            Golden {
                path: "src/terminal/snapshot/testdata/continuation-utf8-v1.hex",
                candidate: "snapshot_fixture-continuation-utf8-v1.hex",
                value: Value::Bytes(b"\xF0\x9F\x98"),
                expected: fixture::parse(include_str!("testdata/continuation-utf8-v1.hex")),
            },
            Golden {
                path: "src/terminal/snapshot/testdata/continuation-esc-v1.hex",
                candidate: "snapshot_fixture-continuation-esc-v1.hex",
                value: Value::Bytes(b"\x1b"),
                expected: fixture::parse(include_str!("testdata/continuation-esc-v1.hex")),
            },
            Golden {
                path: "src/terminal/snapshot/testdata/continuation-csi-v1.hex",
                candidate: "snapshot_fixture-continuation-csi-v1.hex",
                value: Value::Bytes(b"\x1b[31"),
                expected: csi_fixture(),
            },
            Golden {
                path: "src/terminal/snapshot/testdata/continuation-osc-v1.hex",
                candidate: "snapshot_fixture-continuation-osc-v1.hex",
                value: Value::Bytes(b"\x1b]2;title"),
                expected: fixture::parse(include_str!("testdata/continuation-osc-v1.hex")),
            },
            Golden {
                path: "src/terminal/snapshot/testdata/continuation-dcs-v1.hex",
                candidate: "snapshot_fixture-continuation-dcs-v1.hex",
                value: Value::Bytes(b"\x1bPqdata"),
                expected: fixture::parse(include_str!("testdata/continuation-dcs-v1.hex")),
            },
            Golden {
                path: "src/terminal/snapshot/testdata/continuation-apc-v1.hex",
                candidate: "snapshot_fixture-continuation-apc-v1.hex",
                value: Value::Bytes(b"\x1b_Gdata"),
                expected: fixture::parse(include_str!("testdata/continuation-apc-v1.hex")),
            },
        ];

        for golden in &values {
            let encoded = encode_to_vec(golden.value).unwrap();
            fixture::expect_equal(fixture::Kind::Bytes, golden.path, golden.candidate, &golden.expected, &encoded);
        }
    }

    #[test]
    fn continuation_validates_supported_unfinished_states() {
        let values: [&[u8]; 6] = [b"\x1b", b"\x1b[31", b"\x1b]2;title", b"\x1bPqdata", b"\x1b_Gdata", b"\xF0\x9F\x98"];
        for bytes in values {
            validate(Value::Bytes(bytes)).unwrap();
        }
    }

    #[test]
    fn empty_continuation_bytes_encode_and_decode_as_ground() {
        validate(Value::Bytes(b"")).unwrap();

        let encoded = encode_to_vec(Value::Bytes(b"")).unwrap();
        assert_eq!(ground_fixture(), encoded);

        let mut source = FixedReader::new(&encoded);
        let header = record::next_header(&mut source).unwrap();
        let decoded = decode(&mut source, header, 0).unwrap();
        assert_eq!(OwnedValue::Ground, decoded);
    }

    #[test]
    fn continuation_rejects_invalid_semantic_shapes() {
        assert_eq!(Err(Error::NoPendingState), validate(Value::Bytes(b"\x1b[31m")));
        assert_eq!(Err(Error::ReplayWouldCommit), validate(Value::Bytes(b"\x1b[31\x07")));
        assert_eq!(Err(Error::NonCanonicalContinuation), validate(Value::Bytes(b"prefix\x1b[31")));
        assert_eq!(Err(Error::NonCanonicalContinuation), validate(Value::Bytes(b"\x1b[31\x1b[4")));
    }

    #[test]
    fn continuation_record_round_trip_and_cap() {
        let values = [Value::Ground, Value::Bytes(b"\x1b[31"), Value::Bytes(b"\xF0\x9F")];

        for value in values {
            let encoded = encode_to_vec(value).unwrap();
            let mut source = FixedReader::new(&encoded);
            let header = record::next_header(&mut source).unwrap();
            let decoded = decode(&mut source, header, 1024).unwrap();
            match value {
                Value::Ground => assert_eq!(OwnedValue::Ground, decoded),
                Value::Bytes(expected) => assert_eq!(OwnedValue::Bytes(expected.to_vec()), decoded),
            }
        }

        let encoded = encode_to_vec(Value::Bytes(b"\x1b[31")).unwrap();

        // Zig decodes this with an allocator that fails on its first
        // allocation to prove the limit is checked before allocating; the
        // port uses the global allocator, so only the result and the source
        // position are checked.
        let mut capped_source = FixedReader::new(&encoded);
        let header = record::next_header(&mut capped_source).unwrap();
        assert_eq!(
            Err(Error::ContinuationLimitExceeded),
            decode(&mut capped_source, header, 3)
        );
        assert_eq!(record::Header::LEN, capped_source.seek);
    }

    #[test]
    fn continuation_record_rejects_truncation_checksum_and_tag() {
        let csi = csi_fixture();

        // A cut inside the header fails while reading the header, and a cut
        // inside the payload fails while decoding it.
        for len in 0..csi.len() {
            let mut source = FixedReader::new(&csi[..len]);
            let result = match record::next_header(&mut source) {
                Ok(header) => decode(&mut source, header, 1024).map(|_| ()),
                Err(err) => Err(err),
            };
            assert_eq!(Err(Error::EndOfStream), result);
        }

        let mut invalid_checksum = csi.clone();
        invalid_checksum[6] ^= 1;
        let mut checksum_source = FixedReader::new(&invalid_checksum);
        let header = record::next_header(&mut checksum_source).unwrap();
        assert_eq!(
            Err(Error::InvalidChecksum),
            decode(&mut checksum_source, header, 1024)
        );

        let mut wrong_tag = ground_fixture();
        wrong_tag[0..2].copy_from_slice(&record::Tag::READY.0.to_le_bytes());
        let mut tag_source = FixedReader::new(&wrong_tag);
        let header = record::next_header(&mut tag_source).unwrap();
        assert_eq!(Err(Error::UnexpectedRecordTag), decode(&mut tag_source, header, 1024));
    }
}
