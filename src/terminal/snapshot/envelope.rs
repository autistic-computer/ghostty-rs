//! Port of `src/terminal/snapshot/envelope.zig`.
//!
//! STATUS: ported
//!
//! Snapshot envelope.
//!
//! Every snapshot binary blob begins with exactly one envelope at byte zero.
//! It is followed by a set of records. Each record provides their own
//! tag, payload length, CRC, etc.
//!
//! The envelope identifies the bytes as a terminal snapshot and selects the
//! single version governing the entire blob. A decoder validates both fields
//! before reading any records.
//!
//! The envelope is exactly ten bytes. All integers are unsigned and
//! little-endian.
//!
//! | Offset | Size | Field                |
//! | -----: | ---: | :------------------- |
//! |      0 |    8 | Magic (`GHOSTSNP`)   |
//! |      8 |    2 | Version (`u16`)      |

use super::io::{self, Error};

/// Identifies a Ghostty terminal snapshot and rejects unrelated input before
/// any record decoding begins. All eight bytes are part of the wire value.
pub const MAGIC: &[u8; 8] = b"GHOSTSNP";

/// The complete compatibility boundary for snapshot layout and behavior.
/// Version 1 readers require this value to match exactly.
pub const VERSION: u16 = 1;

/// Number of bytes in the fixed envelope: magic followed by version.
pub const ENCODED_LEN: usize = MAGIC.len() + 2;

// We expect this so if it changes we should think carefully.
const _: () = assert!(ENCODED_LEN == 10);

/// Errors from `decode`: read errors, `InvalidMagic`, `UnsupportedVersion`.
pub type DecodeError = Error;

/// Encode the envelope.
pub fn encode(writer: &mut dyn io::Writer) -> io::Result<()> {
    writer.write_all(MAGIC)?;
    io::write_int(writer, VERSION)
}

/// Decode and validate the envelope.
pub fn decode(reader: &mut dyn io::Reader) -> Result<(), DecodeError> {
    let mut actual_magic = [0u8; 8];
    reader.read_slice_all(&mut actual_magic)?;
    if &actual_magic != MAGIC {
        return Err(Error::InvalidMagic);
    }

    let actual_version = io::read_int::<u16>(reader)?;
    if actual_version != VERSION {
        return Err(Error::UnsupportedVersion);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::snapshot::fixture;
    use crate::terminal::snapshot::io::{FixedReader, FixedWriter};

    fn golden() -> Vec<u8> {
        fixture::parse(include_str!("testdata/envelope-v1.hex"))
    }

    #[test]
    fn golden_encoding() {
        let mut buf = [0u8; ENCODED_LEN];
        let mut writer = FixedWriter::new(&mut buf);
        encode(&mut writer).unwrap();

        let reference = golden();
        fixture::expect_equal(
            fixture::Kind::Bytes,
            "src/terminal/snapshot/testdata/envelope-v1.hex",
            "snapshot_fixture-envelope-v1.hex",
            &reference,
            writer.buffered(),
        );

        let mut reader = FixedReader::new(&reference);
        decode(&mut reader).unwrap();
    }

    #[test]
    fn reject_invalid_magic_and_version() {
        let mut invalid_magic = FixedReader::new(b"GHOSTSNX\x01\x00");
        assert_eq!(Err(Error::InvalidMagic), decode(&mut invalid_magic));

        let mut invalid_version = FixedReader::new(b"GHOSTSNP\x00\x00");
        assert_eq!(Err(Error::UnsupportedVersion), decode(&mut invalid_version));
    }

    #[test]
    fn reject_every_truncation() {
        let reference = golden();
        for len in 0..ENCODED_LEN {
            let mut reader = FixedReader::new(&reference[..len]);
            assert_eq!(Err(Error::EndOfStream), decode(&mut reader));
        }
    }
}
