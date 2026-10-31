//! Port of `src/crash/sentry_envelope.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::io::{self, BufRead, Read, Write};

use serde_json::Value;

/// `std.json.ObjectMap`: an insertion-ordered JSON object (serde_json is
/// built with `preserve_order`).
pub type ObjectMap = serde_json::Map<String, Value>;

/// Same as std.json.ObjectMap but unmanaged. In Rust there is no
/// difference; kept for API parity.
pub type ObjectMapUnmanaged = ObjectMap;

/// Errors from [`Envelope::parse`] and [`Envelope::serialize`].
#[derive(Debug)]
pub enum EnvelopeError {
    EnvelopeMalformedHeaders,
    EnvelopeItemMalformedHeaders,
    EnvelopeItemTypeMissing,
    EnvelopeItemLengthMalformed,
    EnvelopeItemPayloadTooShort,
    EnvelopeItemPayloadNoNewline,
    /// A line exceeded the (1MB) limit.
    StreamTooLong,
    /// Invalid JSON.
    Json(serde_json::Error),
    /// Reading or writing failed.
    Io(io::Error),
}

impl fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnvelopeError::Json(err) => write!(f, "{err}"),
            EnvelopeError::Io(err) => write!(f, "{err}"),
            other => fmt::Debug::fmt(other, f),
        }
    }
}

impl std::error::Error for EnvelopeError {}

impl From<io::Error> for EnvelopeError {
    fn from(err: io::Error) -> Self {
        EnvelopeError::Io(err)
    }
}

impl From<serde_json::Error> for EnvelopeError {
    fn from(err: serde_json::Error) -> Self {
        EnvelopeError::Json(err)
    }
}

/// Line length limit used while parsing. 1MB, arbitrary choice.
const LINE_LIMIT: usize = 1024 * 1024;

/// The Sentry Envelope format: https://develop.sentry.dev/sdk/envelopes/
///
/// The envelope is our primary crash report format since use the Sentry
/// client. It is designed and created by Sentry but is an open format
/// in that it is publicly documented and can be used by any system. This
/// lets us utilize the Sentry client for crash capture but also gives us
/// the opportunity to migrate to another system if we need to, and doesn't
/// force any user or developer to use Sentry the SaaS if they don't want
/// to.
///
/// This struct implements reading the envelope format (writing is not needed
/// currently but can be added later). It is incomplete; I only implemented
/// what I needed at the time.
///
/// The Zig version keeps all memory in an arena; here the envelope owns
/// its data directly.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    /// The headers of the envelope decoded into a json ObjectMap.
    pub headers: ObjectMap,

    /// The items in the envelope in the order they're encoded.
    pub items: Vec<Item>,
}

impl Envelope {
    /// Parse an envelope from a reader.
    ///
    /// The full envelope must fit in memory for this to succeed. This
    /// will always copy the data from the reader into memory, even if the
    /// reader is already in-memory. This simplifies memory lifetimes at the
    /// expense of a copy, but envelope parsing in our use case is not a hot
    /// path.
    pub fn parse<R: BufRead + ?Sized>(reader: &mut R) -> Result<Envelope, EnvelopeError> {
        // Parse our elements.
        let headers = Self::parse_header(reader)?;
        let items = Self::parse_items(reader)?;
        Ok(Envelope { headers, items })
    }

    fn parse_header<R: BufRead + ?Sized>(reader: &mut R) -> Result<ObjectMap, EnvelopeError> {
        let mut buf = Vec::new();
        stream_delimiter_limit(reader, &mut buf, b'\n', LINE_LIMIT)?;
        // It's okay if there isn't a trailing newline
        discard_delimiter_inclusive(reader, b'\n')?;

        let value: Value = serde_json::from_slice(&buf)?;
        match value {
            Value::Object(map) => Ok(map),
            _ => Err(EnvelopeError::EnvelopeMalformedHeaders),
        }
    }

    fn parse_items<R: BufRead + ?Sized>(reader: &mut R) -> Result<Vec<Item>, EnvelopeError> {
        let mut items = Vec::new();
        while let Some(item) = Self::parse_one_item(reader)? {
            items.push(item);
        }
        Ok(items)
    }

    fn parse_one_item<R: BufRead + ?Sized>(reader: &mut R) -> Result<Option<Item>, EnvelopeError> {
        // Get the next item which must start with a header.
        let mut buf = Vec::new();
        match stream_delimiter_limit(reader, &mut buf, b'\n', LINE_LIMIT) {
            Ok(()) => {}
            Err(EnvelopeError::StreamTooLong) => return Ok(None),
            Err(err) => return Err(err),
        }
        // It's okay if there isn't a trailing newline
        discard_delimiter_inclusive(reader, b'\n')?;

        // Parse the header JSON
        let headers: ObjectMap = {
            let line = trim(&buf, b" \t");
            if line.is_empty() {
                return Ok(None);
            }

            let value: Value = serde_json::from_slice(line)?;
            match value {
                Value::Object(map) => map,
                _ => return Err(EnvelopeError::EnvelopeItemMalformedHeaders),
            }
        };

        // Get the event type
        let typ: ItemType = match headers.get("type") {
            Some(Value::String(str)) => ItemType::from_str(str).unwrap_or(ItemType::Unknown),
            _ => return Err(EnvelopeError::EnvelopeItemTypeMissing),
        };

        // Get the payload length. The length is not required. If the length
        // is not specified then it is the next line ending in `\n`.
        let len_: Option<u64> = match headers.get("length") {
            // std.json parses integers that fit an i64 as `.integer`;
            // anything else (floats, larger numbers) is malformed here.
            Some(Value::Number(n)) => match n.as_i64() {
                Some(int) => match u64::try_from(int) {
                    Ok(v) => Some(v),
                    Err(_) => return Err(EnvelopeError::EnvelopeItemLengthMalformed),
                },
                None => return Err(EnvelopeError::EnvelopeItemLengthMalformed),
            },
            Some(_) => return Err(EnvelopeError::EnvelopeItemLengthMalformed),
            None => None,
        };

        // Get the payload
        let payload: Vec<u8> = if let Some(len) = len_ {
            // The payload length is specified so read the exact length.
            let mut payload = Vec::new();
            let read = io::Read::take(&mut *reader, len).read_to_end(&mut payload)?;
            if (read as u64) < len {
                return Err(EnvelopeError::EnvelopeItemPayloadTooShort);
            }

            // The next byte must be a newline.
            if let Some(byte) = take_byte(reader)? {
                if byte != b'\n' {
                    return Err(EnvelopeError::EnvelopeItemPayloadNoNewline);
                }
            }

            payload
        } else {
            // The payload is the next line ending in `\n`. It is required.
            let mut payload = Vec::new();
            match stream_delimiter_limit(reader, &mut payload, b'\n', LINE_LIMIT) {
                Ok(()) => {}
                Err(EnvelopeError::StreamTooLong) => {
                    return Err(EnvelopeError::EnvelopeItemPayloadTooShort)
                }
                Err(err) => return Err(err),
            }
            // It's okay if there isn't a trailing newline
            discard_delimiter_inclusive(reader, b'\n')?;
            payload
        };

        Ok(Some(Item::Encoded(EncodedItem {
            headers,
            r#type: typ,
            payload,
        })))
    }

    /// Serialize the envelope to the given writer.
    ///
    /// This will convert all decoded items to encoded items.
    pub fn serialize<W: Write + ?Sized>(&mut self, writer: &mut W) -> Result<(), EnvelopeError> {
        // Header line first. serde_json's compact formatter is the
        // equivalent of `.whitespace = .minified`: VERY important for the
        // correctness of the envelope since all JSON headers in the
        // envelope must be on a single line.
        serde_json::to_writer(&mut *writer, &self.headers)?;
        writer.write_all(b"\n")?;

        // Write each item
        for (idx, item) in self.items.iter_mut().enumerate() {
            if idx > 0 {
                writer.write_all(b"\n")?;
            }

            let encoded = item.encode();
            serde_json::to_writer(&mut *writer, &encoded.headers)?;
            writer.write_all(b"\n")?;
            writer.write_all(&encoded.payload)?;
        }

        Ok(())
    }
}

/// `Reader.streamDelimiterLimit`: append bytes up to (not including) the
/// delimiter to `out`. Reaching the end of the stream is not an error.
/// Errors with `StreamTooLong` if more than `limit` bytes precede the
/// delimiter.
fn stream_delimiter_limit<R: BufRead + ?Sized>(
    reader: &mut R,
    out: &mut Vec<u8>,
    delimiter: u8,
    limit: usize,
) -> Result<(), EnvelopeError> {
    let mut remaining = limit;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let (n, found) = match memchr::memchr(delimiter, available) {
            Some(i) => (i, true),
            None => (available.len(), false),
        };
        if n > remaining {
            out.extend_from_slice(&available[..remaining]);
            reader.consume(remaining);
            return Err(EnvelopeError::StreamTooLong);
        }
        out.extend_from_slice(&available[..n]);
        reader.consume(n);
        remaining -= n;
        if found {
            return Ok(());
        }
    }
}

/// `Reader.discardDelimiterInclusive`, treating end of stream as success.
fn discard_delimiter_inclusive<R: BufRead + ?Sized>(
    reader: &mut R,
    delimiter: u8,
) -> Result<(), EnvelopeError> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        match memchr::memchr(delimiter, available) {
            Some(i) => {
                reader.consume(i + 1);
                return Ok(());
            }
            None => {
                let n = available.len();
                reader.consume(n);
            }
        }
    }
}

/// `Reader.takeByte`, returning `None` at end of stream.
fn take_byte<R: BufRead + ?Sized>(reader: &mut R) -> io::Result<Option<u8>> {
    let available = reader.fill_buf()?;
    let Some(&byte) = available.first() else {
        return Ok(None);
    };
    reader.consume(1);
    Ok(Some(byte))
}

fn trim<'a>(s: &'a [u8], chars: &[u8]) -> &'a [u8] {
    let start = s.iter().position(|c| !chars.contains(c)).unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|c| !chars.contains(c))
        .map_or(start, |i| i + 1);
    &s[start..end]
}

/// The various item types that can be in an envelope. This is a point
/// in time snapshot of the types that are known whenever this is edited.
/// Event types can be introduced at any time and unknown types will
/// take the "unknown" enum value.
///
/// https://develop.sentry.dev/sdk/envelopes/#data-model
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemType {
    /// Special event type for when the item type is unknown.
    Unknown,

    /// Documented event types
    Event,
    Transaction,
    Attachment,
    Session,
    Sessions,
    Statsd,
    MetricMeta,
    UserFeedback,
    ClientReport,
    ReplayEvent,
    ReplayRecording,
    Profile,
    CheckIn,
}

impl ItemType {
    const ALL: [ItemType; 14] = [
        ItemType::Unknown,
        ItemType::Event,
        ItemType::Transaction,
        ItemType::Attachment,
        ItemType::Session,
        ItemType::Sessions,
        ItemType::Statsd,
        ItemType::MetricMeta,
        ItemType::UserFeedback,
        ItemType::ClientReport,
        ItemType::ReplayEvent,
        ItemType::ReplayRecording,
        ItemType::Profile,
        ItemType::CheckIn,
    ];

    /// The wire name of the item type (the Zig tag name).
    pub fn as_str(self) -> &'static str {
        match self {
            ItemType::Unknown => "unknown",
            ItemType::Event => "event",
            ItemType::Transaction => "transaction",
            ItemType::Attachment => "attachment",
            ItemType::Session => "session",
            ItemType::Sessions => "sessions",
            ItemType::Statsd => "statsd",
            ItemType::MetricMeta => "metric_meta",
            ItemType::UserFeedback => "user_feedback",
            ItemType::ClientReport => "client_report",
            ItemType::ReplayEvent => "replay_event",
            ItemType::ReplayRecording => "replay_recording",
            ItemType::Profile => "profile",
            ItemType::CheckIn => "check_in",
        }
    }

    /// `std.meta.stringToEnum(ItemType, str)`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<ItemType> {
        ItemType::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

/// Errors from [`Item::decode`] (`Item.DecodeError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    MissingRequiredField,
    InvalidFieldType,
    UnsupportedType,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for DecodeError {}

/// An item in the envelope. An item can be either in an encoded
/// or decoded state. The encoded state lets us parse the envelope
/// more cheaply since we can defer the full decoding of the item
/// until we need it.
///
/// The decoded state is more ergonomic to work with and lets us
/// easily build up new items and defer encoding until serialization
/// time.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Encoded(EncodedItem),
    Attachment(Attachment),
}

impl Item {
    /// Convert the item to an encoded item. This modify the item
    /// in place.
    pub fn encode(&mut self) -> &EncodedItem {
        if let Item::Attachment(v) = self {
            let result = v.encode();
            *self = Item::Encoded(result);
        }
        match self {
            Item::Encoded(v) => v,
            Item::Attachment(_) => unreachable!(),
        }
    }

    /// Returns the type of item represented here, whether
    /// it is an encoded item or not.
    pub fn item_type(&self) -> ItemType {
        match self {
            Item::Encoded(v) => v.r#type,
            Item::Attachment(_) => ItemType::Attachment,
        }
    }

    /// Decode the item if it is encoded. This will modify itself.
    /// If the item is already decoded this does nothing.
    ///
    /// If the decoding fails because the item is in an invalid
    /// state (i.e. its missing a required field) then this will
    /// error but the encoded item will remain unmodified. This
    /// allows the caller to handle the error without corrupting the
    /// envelope.
    pub fn decode(&mut self) -> Result<(), DecodeError> {
        // Get our encoded item. If we're not encoded we're done.
        let Item::Encoded(encoded) = self else {
            return Ok(());
        };

        // Decode the item.
        *self = match encoded.r#type {
            ItemType::Attachment => Item::Attachment(Attachment::decode(encoded)?),
            _ => return Err(DecodeError::UnsupportedType),
        };
        Ok(())
    }
}

/// An encoded item. It is "encoded" in the sense that the payload
/// is a byte slice. The headers are "decoded" into a json ObjectMap
/// but that's still a pretty low-level representation.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodedItem {
    pub headers: ObjectMap,
    pub r#type: ItemType,
    pub payload: Vec<u8>,
}

/// An arbitrary file attachment.
///
/// https://develop.sentry.dev/sdk/envelopes/#attachment
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    /// "filename" field is the name of the uploaded file without
    /// a path component.
    pub filename: String,

    /// A special "type" associated with the attachment. This
    /// is documented on the Sentry website. In the future we should
    /// make this an enum.
    pub r#type: Option<String>,

    /// Additional headers for the attachment.
    pub headers_extra: ObjectMapUnmanaged,

    /// The data for the attachment.
    pub payload: Vec<u8>,
}

impl Attachment {
    pub fn decode(item: &EncodedItem) -> Result<Attachment, DecodeError> {
        Ok(Attachment {
            filename: match item.headers.get("filename") {
                Some(Value::String(str)) => str.clone(),
                Some(_) => return Err(DecodeError::InvalidFieldType),
                None => return Err(DecodeError::MissingRequiredField),
            },

            r#type: match item.headers.get("attachment_type") {
                Some(Value::String(str)) => Some(str.clone()),
                Some(_) => return Err(DecodeError::InvalidFieldType),
                None => None,
            },

            headers_extra: item.headers.clone(),
            payload: item.payload.clone(),
        })
    }

    pub fn encode(&mut self) -> EncodedItem {
        self.headers_extra
            .insert("filename".to_owned(), Value::String(self.filename.clone()));

        if let Some(v) = &self.r#type {
            self.headers_extra
                .insert("attachment_type".to_owned(), Value::String(v.clone()));
        } else {
            self.headers_extra.swap_remove("attachment_type");
        }

        EncodedItem {
            headers: self.headers_extra.clone(),
            r#type: ItemType::Attachment,
            payload: self.payload.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = concat!(
        "{}\n",
        "{\"type\":\"session\",\"length\":218}\n",
        "{\"init\":true,\"sid\":\"c148cc2f-5f9f-4231-575c-2e85504d6434\",\"status\":\"abnormal\",\"errors\":0,\"started\":\"2024-08-29T02:38:57.607016Z\",\"duration\":0.000343,\"attrs\":{\"release\":\"0.1.0-HEAD+d37b7d09\",\"environment\":\"production\"}}",
    );

    fn trim_nl(s: &[u8]) -> &[u8] {
        trim(s, b"\n")
    }

    #[test]
    fn envelope_parse() {
        let mut reader: &[u8] = b"{}";
        let _v = Envelope::parse(&mut reader).unwrap();
    }

    #[test]
    fn envelope_parse_session() {
        let mut reader: &[u8] = SESSION.as_bytes();
        let v = Envelope::parse(&mut reader).unwrap();

        assert_eq!(1usize, v.items.len());
        assert_eq!(ItemType::Session, v.items[0].item_type());
        assert!(matches!(v.items[0], Item::Encoded(_)));
    }

    #[test]
    fn envelope_parse_multiple() {
        let input = format!(
            "{SESSION}\n{}\n{}",
            "{\"type\":\"attachment\",\"length\":4,\"filename\":\"test.txt\"}", "ABCD"
        );
        let mut reader: &[u8] = input.as_bytes();
        let v = Envelope::parse(&mut reader).unwrap();

        assert_eq!(2usize, v.items.len());
        let Item::Encoded(a) = &v.items[0] else {
            panic!()
        };
        let Item::Encoded(b) = &v.items[1] else {
            panic!()
        };
        assert_eq!(ItemType::Session, a.r#type);
        assert_eq!(ItemType::Attachment, b.r#type);
    }

    #[test]
    fn envelope_parse_multiple_no_length() {
        let input = concat!(
            "{}\n",
            "{\"type\":\"session\"}\n",
            "{}\n",
            "{\"type\":\"attachment\",\"length\":4,\"filename\":\"test.txt\"}\n",
            "ABCD",
        );
        let mut reader: &[u8] = input.as_bytes();
        let v = Envelope::parse(&mut reader).unwrap();

        assert_eq!(2usize, v.items.len());
        let Item::Encoded(a) = &v.items[0] else {
            panic!()
        };
        let Item::Encoded(b) = &v.items[1] else {
            panic!()
        };
        assert_eq!(ItemType::Session, a.r#type);
        assert_eq!(ItemType::Attachment, b.r#type);
    }

    #[test]
    fn envelope_parse_end_in_new_line() {
        let input = format!("{SESSION}\n");
        let mut reader: &[u8] = input.as_bytes();
        let v = Envelope::parse(&mut reader).unwrap();

        assert_eq!(1usize, v.items.len());
        let Item::Encoded(a) = &v.items[0] else {
            panic!()
        };
        assert_eq!(ItemType::Session, a.r#type);
    }

    #[test]
    fn envelope_parse_attachment() {
        let input = concat!(
            "{}\n",
            "{\"type\":\"attachment\",\"length\":4,\"filename\":\"test.txt\"}\n",
            "ABCD",
        );
        let mut reader: &[u8] = input.as_bytes();
        let mut v = Envelope::parse(&mut reader).unwrap();

        assert_eq!(1usize, v.items.len());

        let item = &mut v.items[0];
        let Item::Encoded(enc) = &*item else { panic!() };
        assert_eq!(ItemType::Attachment, enc.r#type);
        item.decode().unwrap();
        let Item::Attachment(att) = &*item else {
            panic!()
        };
        assert_eq!("test.txt", att.filename);

        // Serialization test
        {
            let mut output: Vec<u8> = Vec::new();
            v.serialize(&mut output).unwrap();
            assert_eq!(input.as_bytes(), trim_nl(&output));
        }
    }

    #[test]
    fn envelope_serialize_empty() {
        let mut reader: &[u8] = b"{}";
        let mut v = Envelope::parse(&mut reader).unwrap();

        let mut output: Vec<u8> = Vec::new();
        v.serialize(&mut output).unwrap();

        assert_eq!(b"{}", trim_nl(&output));
    }

    #[test]
    fn envelope_serialize_session() {
        let mut reader: &[u8] = SESSION.as_bytes();
        let mut v = Envelope::parse(&mut reader).unwrap();

        let mut output: Vec<u8> = Vec::new();
        v.serialize(&mut output).unwrap();

        assert_eq!(SESSION.as_bytes(), trim_nl(&output));
    }
}
