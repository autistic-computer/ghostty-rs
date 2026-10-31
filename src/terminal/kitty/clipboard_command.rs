//! Port of `src/terminal/kitty/clipboard_command.zig`.
//!
//! STATUS: ported
//!
//! Kitty clipboard protocol (OSC 5522) request decoding.
//!
//! OSC 5522 format: `<OSC>5522;metadata;payload<ST>`
//!
//! Per the spec: "metadata is a colon separated list of key-value
//! pairs and payload is base64 encoded data."
//!
//! This contains the logic for parsing this.
//!
//! Deviations: decoded/sanitized metadata values are owned `Vec<u8>`s
//! instead of arena-allocated slices, and `Payload` owns its decoded
//! bytes (exposed via `Payload::data`) instead of aliasing a separate
//! buffer slice.

use std::fmt;

use crate::simd::base64 as simd_base64;
use crate::terminal::clipboard;
use crate::terminal::osc::parsers::kitty_clipboard_protocol as protocol;

use protocol::Operation;

/// Maximum id length, nothing specified but this is the limit used
/// in Kitty's source so we'll match it.
pub const MAX_ID_LEN: usize = 512;

/// Maximum decoded password length. Kitty has no limit. Passwords are
/// UUID-sized in practice so anything longer simply never matches a
/// stored grant anyway.
pub const MAX_PW_LEN: usize = 128;

/// Maximum decoded MIME type length. Kitty has no limit but real MIME
/// types are tiny; anything longer drops the packet.
pub const MAX_MIME_LEN: usize = 256;

/// Maximum decoded name length. Kitty has no limit but names are shown
/// in permission prompts so anything longer drops the packet.
pub const MAX_NAME_LEN: usize = 256;

/// `std.ascii.whitespace`
const ASCII_WHITESPACE: [u8; 6] = [b' ', b'\t', b'\n', b'\r', 0x0B, 0x0C];

/// Error from [`Metadata::parse`] (`error{InvalidValue}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataError {
    InvalidValue,
}

impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InvalidValue")
    }
}

impl std::error::Error for MetadataError {}

/// Error from [`Payload::init`] (`error{Invalid}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadError {
    Invalid,
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invalid")
    }
}

impl std::error::Error for PayloadError {}

/// The decoded, validated metadata of one OSC 5522 sequence.
///
/// All values are owned and sized to their contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub op: Operation,

    /// The clipboard this operation targets. Per the spec: "To read
    /// from the primary selection instead of the clipboard, add the
    /// key `loc=primary` to the metadata section." Any other value
    /// means the clipboard, so only standard and primary are possible
    /// here.
    pub loc: clipboard::Location,

    /// Sanitized id: invalid characters stripped, truncated to
    /// max_id_len. Empty means no id.
    pub id: Vec<u8>,

    /// Decoded mime metadata value. Empty means absent; kitty treats an
    /// empty mime the same as a missing one everywhere it matters (a
    /// wdata packet with either commits the transaction).
    pub mime: Vec<u8>,

    /// Decoded password. Empty means absent. Per the spec:
    /// "Specifying a password without a human friendly name is
    /// equivalent to not specifying a password and the terminal must
    /// treat the request as though it had no password."
    pub pw: Vec<u8>,

    /// Decoded human friendly name of the requesting program, shown in
    /// permission prompts. Empty means absent. Its presence opts into
    /// password grants.
    pub name: Vec<u8>,
}

/// Errors from [`Metadata::decode_value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeValueError {
    Overflow,
    InvalidBase64,
    InvalidUtf8,
}

/// The raw (undecoded) metadata fields (Zig: `Metadata.Raw`).
#[derive(Debug, Default)]
struct Raw<'a> {
    op: Option<&'a [u8]>,
    loc: &'a [u8],
    id: &'a [u8],
    mime: &'a [u8],
    pw: &'a [u8],
    name: &'a [u8],
}

impl<'a> Raw<'a> {
    fn parse(raw: &'a [u8]) -> Option<Raw<'a>> {
        let mut result = Raw::default();

        // This visits every record even though an empty raw string
        // yields one empty record. Validating the complete structure
        // before decoding also ensures the last duplicate value wins.
        for record in raw.split(|&c| c == b':') {
            let eql_idx = record.iter().position(|&c| c == b'=')?;
            let key = &record[..eql_idx];
            let value = &record[eql_idx + 1..];

            match key {
                b"type" => result.op = Some(value),
                b"loc" => result.loc = value,
                b"id" => result.id = value,
                b"mime" => result.mime = value,
                b"pw" => result.pw = value,
                b"name" => result.name = value,
                // Unknown keys are ignored.
                _ => {}
            }
        }

        Some(result)
    }
}

impl Metadata {
    /// Metadata for `op` with every other field at its default (Zig
    /// field defaults).
    pub fn new(op: Operation) -> Metadata {
        Metadata {
            op,
            loc: clipboard::Location::STANDARD,
            id: Vec::new(),
            mime: Vec::new(),
            pw: Vec::new(),
            name: Vec::new(),
        }
    }

    /// Parse the metadata field. The raw value is expected to be exactly
    /// the metadata (prefix and payload and separators stripped out).
    ///
    /// A None result means the packet should be silently dropped. An
    /// InvalidValue error means a decoded textual value was not valid
    /// UTF-8 or exceeded a local safety limit. Callers use the raw
    /// operation to decide whether that invalid value aborts an in-flight
    /// write transaction.
    pub fn parse(raw: &[u8]) -> Result<Option<Metadata>, MetadataError> {
        let Some(fields) = Raw::parse(raw) else {
            return Ok(None);
        };
        let Some(op) = fields.op.and_then(Operation::init) else {
            return Ok(None);
        };
        let mut result = Metadata {
            loc: if fields.loc == b"primary" {
                clipboard::Location::PRIMARY
            } else {
                clipboard::Location::STANDARD
            },
            id: Self::sanitize_id(fields.id),
            ..Metadata::new(op)
        };

        result.mime = Self::decode_value(fields.mime, MAX_MIME_LEN)
            .map_err(|_| MetadataError::InvalidValue)?;
        result.pw = match Self::decode_value(fields.pw, MAX_PW_LEN) {
            Ok(v) => v,
            Err(DecodeValueError::InvalidBase64 | DecodeValueError::InvalidUtf8) => {
                return Err(MetadataError::InvalidValue)
            }
            // An over-long password behaves as if none was given: it can
            // never match a stored grant.
            Err(DecodeValueError::Overflow) => Vec::new(),
        };
        result.name = Self::decode_value(fields.name, MAX_NAME_LEN)
            .map_err(|_| MetadataError::InvalidValue)?;
        Ok(Some(result))
    }

    /// Return the last recognized operation from syntactically valid raw
    /// metadata. This intentionally does not decode any values, so callers
    /// can still classify an InvalidValue parse error.
    pub fn operation(raw: &[u8]) -> Option<Operation> {
        let fields = Raw::parse(raw)?;
        Operation::init(fields.op?)
    }

    /// Sanitize the ID according to the spec:
    ///
    /// Valid ids must include only characters from the set: [a-zA-Z0-9-_+.].
    /// Any other characters must be stripped out from the id by the terminal
    /// emulator before retransmitting it.
    fn sanitize_id(value: &[u8]) -> Vec<u8> {
        let mut list: Vec<u8> = Vec::new();
        for &c in value {
            match c {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'+' | b'.' => {}
                _ => continue,
            }
            if list.len() >= MAX_ID_LEN {
                break;
            }
            list.push(c);
        }
        list
    }

    /// Base64-decode a metadata value. Per the spec these values "are
    /// UTF-8 strings that are base64 encoded", so the decoded result
    /// must be valid UTF-8, and the encoding is strict RFC 4648 with
    /// required padding per the spec's "Encoding of payloads" section.
    fn decode_value(value: &[u8], max_len: usize) -> Result<Vec<u8>, DecodeValueError> {
        // Avoid hostile large payloads (`Encoder.calcSize(max_len)`).
        if value.len() > max_len.div_ceil(3) * 4 {
            return Err(DecodeValueError::Overflow);
        }

        // Decode
        let mut buf = vec![0u8; simd_base64::max_len(value)];
        let len = simd_base64::decode_strict(value, &mut buf, simd_base64::Padding::Required)
            .map_err(|_| DecodeValueError::InvalidBase64)?
            .len();
        buf.truncate(len);

        // Must be valid UTF-8
        if std::str::from_utf8(&buf).is_err() {
            return Err(DecodeValueError::InvalidUtf8);
        }
        if buf.len() > max_len {
            return Err(DecodeValueError::Overflow);
        }
        Ok(buf)
    }
}

/// A decoded base64 payload of one OSC 5522 sequence, e.g. the MIME
/// type list of a read request or the alias list of a walias packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    /// The decoded bytes (Zig: `buf` sized to `maxLen`, with `data`
    /// aliasing its decoded prefix; here truncated to the decoded data).
    buf: Vec<u8>,
}

impl Payload {
    /// Decode a base64 payload into freshly allocated memory. The
    /// encoding is strict RFC 4648 with required padding per the
    /// spec's "Encoding of payloads" section; how an invalid payload
    /// is reported (or not) depends on the packet type.
    pub fn init(payload: &[u8]) -> Result<Payload, PayloadError> {
        let mut buf = vec![0u8; simd_base64::max_len(payload)];
        let len = simd_base64::decode_strict(payload, &mut buf, simd_base64::Padding::Required)
            .map_err(|_| PayloadError::Invalid)?
            .len();
        buf.truncate(len);
        Ok(Payload { buf })
    }

    /// The decoded data.
    pub fn data(&self) -> &[u8] {
        &self.buf
    }

    pub fn is_valid_utf8(&self) -> bool {
        std::str::from_utf8(self.data()).is_ok()
    }

    /// Iterate the whitespace-separated MIME types of the payload.
    /// Matches Python str.split() used by kitty.
    pub fn mime_iterator(&self) -> impl Iterator<Item = &[u8]> {
        self.data()
            .split(|c| ASCII_WHITESPACE.contains(c))
            .filter(|s| !s.is_empty())
    }
}

/// Whether a read request with this many data MIME types (the targets
/// type '.' excluded) is exempt from the user permission prompt.
pub fn read_prompt_exempt(data_mimes: usize) -> bool {
    data_mimes == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn parse(raw: &[u8]) -> Option<Metadata> {
        Metadata::parse(raw).unwrap()
    }

    #[test]
    fn metadata_empty_is_dropped() {
        assert!(parse(b"").is_none());
    }

    #[test]
    fn metadata_record_without_eq_is_dropped() {
        assert!(parse(b"type=read:bare").is_none());
        assert!(parse(b"bare:type=read").is_none());
    }

    #[test]
    fn metadata_missing_or_unknown_type_is_dropped() {
        assert!(parse(b"loc=primary").is_none());
        assert!(parse(b"type=bobr").is_none());
        assert!(parse(b"type=").is_none());
    }

    #[test]
    fn metadata_duplicate_keys_keep_the_last_occurrence() {
        assert_eq!(Operation::Read, parse(b"type=bobr:type=read").unwrap().op);
        assert!(parse(b"type=read:type=bobr").is_none());
    }

    #[test]
    fn metadata_basic_read() {
        let meta = parse(b"type=read").unwrap();
        assert_eq!(Operation::Read, meta.op);
        assert!(meta.loc == clipboard::Location::STANDARD);
        assert_eq!(0, meta.id.len());
    }

    #[test]
    fn metadata_unknown_keys_ignored() {
        let meta = parse(b"type=read:bobr=kurwa").unwrap();
        assert_eq!(Operation::Read, meta.op);
    }

    #[test]
    fn metadata_loc() {
        assert!(parse(b"type=read:loc=primary").unwrap().loc == clipboard::Location::PRIMARY);
        // Anything other than "primary" means the clipboard; it is not an
        // error.
        assert!(parse(b"type=read:loc=bobr").unwrap().loc == clipboard::Location::STANDARD);
    }

    #[test]
    fn metadata_id_sanitized() {
        {
            let meta = parse(b"type=read:id=abc-123_x.Y+z").unwrap();
            assert_eq!(b"abc-123_x.Y+z".as_slice(), meta.id.as_slice());
        }
        {
            // Invalid characters are stripped, not rejected.
            let meta = parse(b"type=read:id=*4 2*").unwrap();
            assert_eq!(b"42".as_slice(), meta.id.as_slice());
        }
    }

    #[test]
    fn metadata_id_truncated_to_max() {
        let mut raw = b"type=read:id=".to_vec();
        raw.extend(std::iter::repeat_n(b'a', MAX_ID_LEN + 100));
        let meta = parse(&raw).unwrap();
        assert_eq!(MAX_ID_LEN, meta.id.len());
    }

    #[test]
    fn metadata_mime_decoded() {
        // "text/plain"
        let meta = parse(b"type=wdata:mime=dGV4dC9wbGFpbg==").unwrap();
        assert_eq!(b"text/plain".as_slice(), meta.mime.as_slice());
    }

    #[test]
    fn metadata_invalid_mime_base64_reported() {
        // Invalid base64 in a metadata value aborts an in-flight write
        // per the spec, so like invalid UTF-8 it is reported rather than
        // silently dropping the packet. Unpadded or whitespace-laced
        // values are invalid too: metadata values use the same strict
        // encoding as payloads.
        let cases: [&[u8]; 4] = [
            b"type=wdata:mime=!!!",
            // "text/plain" without its padding.
            b"type=wdata:mime=dGV4dC9wbGFpbg",
            // "text/plain" with its final byte replaced by '!'.
            b"type=wdata:mime=dGV4dC9wbGFpbg=!",
            // A newline inside otherwise valid base64.
            b"type=wdata:mime=dGV4dC9w\nbGFpbg==",
        ];
        for case in cases {
            assert_eq!(Err(MetadataError::InvalidValue), Metadata::parse(case));
            assert_eq!(Operation::Wdata, Metadata::operation(case).unwrap());
        }
    }

    #[test]
    fn metadata_invalid_mime_utf8_reported() {
        // base64 of 0xff 0xfe
        assert_eq!(
            Err(MetadataError::InvalidValue),
            Metadata::parse(b"type=wdata:mime=//4=")
        );
        assert_eq!(
            Operation::Wdata,
            Metadata::operation(b"type=wdata:mime=//4=").unwrap()
        );
    }

    #[test]
    fn metadata_pw_and_name() {
        // pw="secret", name="app"
        let meta = parse(b"type=read:pw=c2VjcmV0:name=YXBw").unwrap();
        assert_eq!(b"secret".as_slice(), meta.pw.as_slice());
        assert_eq!(b"app".as_slice(), meta.name.as_slice());
    }

    #[test]
    fn metadata_over_long_name_reported() {
        let long = vec![b'n'; MAX_NAME_LEN + 1];
        let mut raw = b"type=read:name=".to_vec();
        raw.extend_from_slice(
            base64::engine::general_purpose::STANDARD
                .encode(&long)
                .as_bytes(),
        );
        assert_eq!(Err(MetadataError::InvalidValue), Metadata::parse(&raw));
    }

    #[test]
    fn metadata_empty_name() {
        let meta = parse(b"type=read:pw=c2VjcmV0:name=").unwrap();
        assert_eq!(0, meta.name.len());
    }

    #[test]
    fn read_prompt_exemption_only_requests_without_data_types() {
        // A targets-only ('.') read parses to zero data MIME types and is
        // served without a prompt; any data type, even alongside the
        // targets listing, still prompts.
        assert!(read_prompt_exempt(0));
        assert!(!read_prompt_exempt(1));
    }

    #[test]
    fn payload_mime_iterator() {
        // base64 of "text/plain  text/html\ntext/uri-list"
        let payload =
            Payload::init(b"dGV4dC9wbGFpbiAgdGV4dC9odG1sCnRleHQvdXJpLWxpc3Q=").unwrap();
        let mut it = payload.mime_iterator();
        assert_eq!(b"text/plain".as_slice(), it.next().unwrap());
        assert_eq!(b"text/html".as_slice(), it.next().unwrap());
        assert_eq!(b"text/uri-list".as_slice(), it.next().unwrap());
        assert!(it.next().is_none());
    }

    #[test]
    fn payload_invalid_base64() {
        let cases: [&[u8]; 3] = [
            b"!!!",
            // "text/plain" without its padding: payloads use the strict
            // encoding, so missing padding is rejected.
            b"dGV4dC9wbGFpbg",
            // A newline inside otherwise valid base64.
            b"dGV4dC9w\nbGFpbg==",
        ];
        for case in cases {
            assert_eq!(Err(PayloadError::Invalid), Payload::init(case));
        }
    }

    #[test]
    fn payload_decoded_text_must_be_valid_utf8() {
        // Valid base64 encoding of a single 0xff byte.
        let payload = Payload::init(b"/w==").unwrap();
        assert!(!payload.is_valid_utf8());
    }
}
