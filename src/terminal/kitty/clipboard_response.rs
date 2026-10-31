//! Port of `src/terminal/kitty/clipboard_response.zig`.
//!
//! STATUS: ported
//!
//! Kitty clipboard protocol (OSC 5522) response encoding.
//!
//! Deviation: `std.Io.Writer` is any `std::io::Write`.

use std::io::Write;

use base64::Engine as _;

use crate::terminal::clipboard;
use crate::terminal::osc::parsers::kitty_clipboard_protocol as protocol;
use crate::terminal::osc::Terminator;

pub use clipboard::Content;
use protocol::{Operation, Status};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Maximum raw (pre-base64) bytes per DATA packet in read responses.
/// This is specified by the protocol.
pub const READ_CHUNK_SIZE: usize = 4096;

/// Maximum requested MIME types served by a single read request.
/// Requests beyond this simply see no DATA packets for the extras,
/// which is how the protocol communicates an unavailable type anyway.
pub const MAX_READ_MIMES: usize = 4;

/// The special MIME type that requests the list of available types.
pub const TARGETS_MIME: &[u8] = b".";

/// Maximum MIME types reported in a paste event's targets listing.
pub const MAX_LISTING_MIMES: usize = 16;

/// The wire name of an operation (Zig `{t}` tag name).
fn operation_name(op: Operation) -> &'static str {
    match op {
        Operation::Read => "read",
        Operation::Walias => "walias",
        Operation::Wdata => "wdata",
        Operation::Write => "write",
    }
}

/// The wire name of a status (Zig `{t}` tag name).
fn status_name(status: Status) -> &'static str {
    match status {
        Status::DATA => "DATA",
        Status::DONE => "DONE",
        Status::EBUSY => "EBUSY",
        Status::EFBIG => "EFBIG",
        Status::EINVAL => "EINVAL",
        Status::EIO => "EIO",
        Status::ENOSYS => "ENOSYS",
        Status::EPERM => "EPERM",
        Status::OK => "OK",
    }
}

/// `std.base64.standard.Encoder.encodeWriter`
fn b64_encode_writer<W: Write + ?Sized>(writer: &mut W, data: &[u8]) -> std::io::Result<()> {
    writer.write_all(B64.encode(data).as_bytes())
}

/// A single response packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Response<'a> {
    pub op: Operation,
    pub status: Status,
    pub primary: bool,
    pub id: &'a [u8],
    pub mime: Option<&'a [u8]>,
    pub pw: Option<&'a [u8]>,
    /// The raw payload; the encoder base64-encodes it. An empty payload
    /// emits no payload section at all (no ';').
    pub payload: &'a [u8],
    pub terminator: Terminator,
}

impl<'a> Response<'a> {
    /// A response with every field but `op` and `status` at its default.
    pub fn new(op: Operation, status: Status) -> Response<'a> {
        Response {
            op,
            status,
            primary: false,
            id: b"",
            mime: None,
            pw: None,
            payload: b"",
            terminator: Terminator::St,
        }
    }

    /// Encode the response. Errors may result in partially written data
    /// so it is up to callers to buffer it if they need to.
    pub fn encode<W: Write + ?Sized>(&self, writer: &mut W) -> std::io::Result<()> {
        self.encode_metadata(writer)?;
        if !self.payload.is_empty() {
            writer.write_all(b";")?;
            b64_encode_writer(writer, self.payload)?;
        }
        writer.write_all(self.terminator.string())
    }

    /// Encode the escape prefix and metadata section only: everything
    /// up to (and not including) the payload section and terminator.
    fn encode_metadata<W: Write + ?Sized>(&self, writer: &mut W) -> std::io::Result<()> {
        // The exact order of fields here matches what Kitty does.
        write!(
            writer,
            "\x1b]5522;type={}:status={}",
            operation_name(self.op),
            status_name(self.status)
        )?;
        if self.primary {
            writer.write_all(b":loc=primary")?;
        }
        if !self.id.is_empty() {
            writer.write_all(b":id=")?;
            writer.write_all(self.id)?;
        }
        if let Some(mime) = self.mime {
            writer.write_all(b":mime=")?;
            b64_encode_writer(writer, mime)?;
        }
        if let Some(pw) = self.pw {
            writer.write_all(b":pw=")?;
            b64_encode_writer(writer, pw)?;
        }
        Ok(())
    }
}

/// Encode a full successful read response: the OK packet, the targets
/// listing if requested, DATA chunks for each served representation,
/// and the final DONE packet. This is also the shape of an unsolicited
/// paste event (list=true, pw set to the one-time password).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadSuccess<'a> {
    pub primary: bool,
    pub id: &'a [u8],

    /// One-time password echoed in every packet. Only used for
    /// terminal-initiated paste events.
    pub pw: Option<&'a [u8]>,

    /// True when the targets ('.') listing was requested.
    pub list: bool,

    /// The MIME types available on the clipboard, reported by the
    /// targets listing. Kitty reports these space-separated in one
    /// packet with a trailing newline when non-empty.
    pub available: &'a [&'a [u8]],

    /// The representations to serve, in request order. Each entry's
    /// data is chunked into DATA packets under its own MIME type. An
    /// entry with empty data produces no packets, which is how the
    /// protocol communicates an unavailable type.
    pub contents: &'a [Content<'a>],

    pub terminator: Terminator,
}

impl ReadSuccess<'_> {
    pub fn encode<W: Write + ?Sized>(&self, writer: &mut W) -> std::io::Result<()> {
        // Initial read response
        Response {
            primary: self.primary,
            id: self.id,
            pw: self.pw,
            terminator: self.terminator,
            ..Response::new(Operation::Read, Status::OK)
        }
        .encode(writer)?;

        // Listing of mimes if requested
        if self.list {
            self.encode_listing(writer)?;
        }

        // Encoding of each mime-type + content
        for content in self.contents {
            for chunk in content.data.chunks(READ_CHUNK_SIZE) {
                Response {
                    id: self.id,
                    mime: Some(content.mime),
                    pw: self.pw,
                    payload: chunk,
                    terminator: self.terminator,
                    ..Response::new(Operation::Read, Status::DATA)
                }
                .encode(writer)?;
            }
        }

        // Trailing done.
        Response {
            id: self.id,
            pw: self.pw,
            terminator: self.terminator,
            ..Response::new(Operation::Read, Status::DONE)
        }
        .encode(writer)
    }

    /// Encode the targets ('.') listing packet. The listing gets a
    /// trailing newline when non-empty.
    fn encode_listing<W: Write + ?Sized>(&self, writer: &mut W) -> std::io::Result<()> {
        let listing = Response {
            id: self.id,
            mime: Some(TARGETS_MIME),
            pw: self.pw,
            terminator: self.terminator,
            ..Response::new(Operation::Read, Status::DATA)
        };
        listing.encode_metadata(writer)?;
        if !self.available.is_empty() {
            writer.write_all(b";")?;

            // Join the types into one chunk before encoding. The
            // listing is a DATA packet, so it shares the pre-encoding
            // chunk size bound of any other data packet; types that
            // don't fit are dropped (a listing that large doesn't
            // happen in practice).
            let mut raw: Vec<u8> = Vec::with_capacity(READ_CHUNK_SIZE);
            for (i, mime) in self.available.iter().enumerate() {
                let sep: usize = if i > 0 { 1 } else { 0 };
                if raw.len() + sep + mime.len() + b"\n".len() > READ_CHUNK_SIZE {
                    break;
                }
                if i > 0 {
                    raw.push(b' ');
                }
                raw.extend_from_slice(mime);
            }
            raw.push(b'\n');
            b64_encode_writer(writer, &raw)?;
        }
        writer.write_all(self.terminator.string())
    }
}

/// An unsolicited Kitty paste event (mode 5522): a read response that
/// lists the clipboard's available MIME types and carries the one-time
/// password the program uses for its follow-up read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasteEvent<'a> {
    /// True if the paste came from the primary selection, reported as
    /// `loc=primary` on the OK packet.
    pub primary: bool,

    /// The one-time password, echoed in every packet.
    pub pw: &'a [u8],

    /// The MIME types available on the clipboard.
    pub available: &'a [&'a [u8]],

    pub terminator: Terminator,
}

impl<'a> PasteEvent<'a> {
    /// A paste event with `primary` and `terminator` at their defaults.
    pub fn new(pw: &'a [u8], available: &'a [&'a [u8]]) -> PasteEvent<'a> {
        PasteEvent {
            primary: false,
            pw,
            available,
            terminator: Terminator::St,
        }
    }

    pub fn encode<W: Write + ?Sized>(&self, writer: &mut W) -> std::io::Result<()> {
        ReadSuccess {
            primary: self.primary,
            pw: Some(self.pw),
            list: true,
            available: self.available,
            terminator: self.terminator,
            ..Default::default()
        }
        .encode(writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(f: impl FnOnce(&mut Vec<u8>) -> std::io::Result<()>) -> String {
        let mut out: Vec<u8> = Vec::new();
        f(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn response_basic_status_packet() {
        let out = encoded(|w| Response::new(Operation::Write, Status::DONE).encode(w));
        assert_eq!("\x1b]5522;type=write:status=DONE\x1b\\", out);
    }

    #[test]
    fn response_id_echo_and_terminator() {
        let out = encoded(|w| {
            Response {
                id: b"42",
                terminator: Terminator::Bel,
                ..Response::new(Operation::Write, Status::EPERM)
            }
            .encode(w)
        });
        assert_eq!("\x1b]5522;type=write:status=EPERM:id=42\x07", out);
    }

    #[test]
    fn response_key_order_type_status_loc_id_mime_pw_and_payload() {
        let out = encoded(|w| {
            Response {
                primary: true,
                id: b"x",
                mime: Some(b"text/plain"),
                pw: Some(b"otp"),
                payload: b"Ghostty",
                ..Response::new(Operation::Read, Status::DATA)
            }
            .encode(w)
        });
        assert_eq!(
            concat!(
                "\x1b]5522;type=read:status=DATA:loc=primary:id=x",
                ":mime=dGV4dC9wbGFpbg==:pw=b3Rw;R2hvc3R0eQ==\x1b\\"
            ),
            out
        );
    }

    #[test]
    fn read_success_empty_request_is_ok_then_done() {
        let out = encoded(|w| {
            ReadSuccess {
                id: b"7",
                ..Default::default()
            }
            .encode(w)
        });
        assert_eq!(
            concat!(
                "\x1b]5522;type=read:status=OK:id=7\x1b\\",
                "\x1b]5522;type=read:status=DONE:id=7\x1b\\"
            ),
            out
        );
    }

    #[test]
    fn read_success_targets_listing_with_text() {
        let out = encoded(|w| {
            ReadSuccess {
                list: true,
                available: &[b"text/plain"],
                ..Default::default()
            }
            .encode(w)
        });
        // "." => "Lg==", "text/plain\n" => "dGV4dC9wbGFpbgo="
        assert_eq!(
            concat!(
                "\x1b]5522;type=read:status=OK\x1b\\",
                "\x1b]5522;type=read:status=DATA:mime=Lg==;dGV4dC9wbGFpbgo=\x1b\\",
                "\x1b]5522;type=read:status=DONE\x1b\\"
            ),
            out
        );
    }

    #[test]
    fn read_success_targets_listing_joins_multiple_types() {
        let out = encoded(|w| {
            ReadSuccess {
                list: true,
                available: &[b"text/plain", b"image/png"],
                ..Default::default()
            }
            .encode(w)
        });
        // Payload is base64 of "text/plain image/png\n".
        assert_eq!(
            concat!(
                "\x1b]5522;type=read:status=OK\x1b\\",
                "\x1b]5522;type=read:status=DATA:mime=Lg==;dGV4dC9wbGFpbiBpbWFnZS9wbmcK\x1b\\",
                "\x1b]5522;type=read:status=DONE\x1b\\"
            ),
            out
        );
    }

    #[test]
    fn read_success_empty_targets_listing_packet_still_sent() {
        let out = encoded(|w| {
            ReadSuccess {
                list: true,
                ..Default::default()
            }
            .encode(w)
        });
        assert_eq!(
            concat!(
                "\x1b]5522;type=read:status=OK\x1b\\",
                "\x1b]5522;type=read:status=DATA:mime=Lg==\x1b\\",
                "\x1b]5522;type=read:status=DONE\x1b\\"
            ),
            out
        );
    }

    #[test]
    fn read_success_data_chunks_under_requested_mime() {
        let out = encoded(|w| {
            ReadSuccess {
                contents: &[Content {
                    mime: b"text/plain",
                    data: b"Ghostty",
                }],
                ..Default::default()
            }
            .encode(w)
        });
        assert_eq!(
            concat!(
                "\x1b]5522;type=read:status=OK\x1b\\",
                "\x1b]5522;type=read:status=DATA:mime=dGV4dC9wbGFpbg==;R2hvc3R0eQ==\x1b\\",
                "\x1b]5522;type=read:status=DONE\x1b\\"
            ),
            out
        );
    }

    #[test]
    fn read_success_each_representation_carries_its_own_data() {
        let out = encoded(|w| {
            ReadSuccess {
                contents: &[
                    Content {
                        mime: b"text/plain",
                        data: b"hello",
                    },
                    Content {
                        mime: b"image/png",
                        data: b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a",
                    },
                ],
                ..Default::default()
            }
            .encode(w)
        });
        assert_eq!(
            concat!(
                "\x1b]5522;type=read:status=OK\x1b\\",
                "\x1b]5522;type=read:status=DATA:mime=dGV4dC9wbGFpbg==;aGVsbG8=\x1b\\",
                "\x1b]5522;type=read:status=DATA:mime=aW1hZ2UvcG5n;iVBORw0KGgo=\x1b\\",
                "\x1b]5522;type=read:status=DONE\x1b\\"
            ),
            out
        );
    }

    #[test]
    fn read_success_chunking_at_read_chunk_size() {
        let data = [b'z'; READ_CHUNK_SIZE + 1];
        let out = encoded(|w| {
            ReadSuccess {
                contents: &[Content {
                    mime: b"text/plain",
                    data: &data,
                }],
                ..Default::default()
            }
            .encode(w)
        });

        // OK + 2 DATA packets + DONE = 4 packets.
        let count = out.matches("\x1b]5522;").count();
        assert_eq!(4, count);

        // The first chunk is exactly read_chunk_size bytes, base64 encoded
        // with padding.
        let first = B64.encode(&data[..READ_CHUNK_SIZE]);
        assert!(out.contains(&first));
    }

    #[test]
    fn read_success_no_data_packets_for_empty_clipboard() {
        let out = encoded(|w| {
            ReadSuccess {
                contents: &[Content {
                    mime: b"text/plain",
                    data: b"",
                }],
                ..Default::default()
            }
            .encode(w)
        });
        assert_eq!(
            concat!(
                "\x1b]5522;type=read:status=OK\x1b\\",
                "\x1b]5522;type=read:status=DONE\x1b\\"
            ),
            out
        );
    }

    #[test]
    fn read_success_paste_event_carries_pw_in_every_packet() {
        let out = encoded(|w| {
            ReadSuccess {
                list: true,
                pw: Some(b"otp"),
                available: &[b"text/plain"],
                ..Default::default()
            }
            .encode(w)
        });
        assert_eq!(
            concat!(
                "\x1b]5522;type=read:status=OK:pw=b3Rw\x1b\\",
                "\x1b]5522;type=read:status=DATA:mime=Lg==:pw=b3Rw;dGV4dC9wbGFpbgo=\x1b\\",
                "\x1b]5522;type=read:status=DONE:pw=b3Rw\x1b\\"
            ),
            out
        );
    }

    #[test]
    fn paste_event_pw_in_every_packet_listing_of_every_type() {
        let out = encoded(|w| PasteEvent::new(b"otp", &[b"text/plain", b"image/png"]).encode(w));
        // Payload is base64 of "text/plain image/png\n".
        assert_eq!(
            concat!(
                "\x1b]5522;type=read:status=OK:pw=b3Rw\x1b\\",
                "\x1b]5522;type=read:status=DATA:mime=Lg==:pw=b3Rw;dGV4dC9wbGFpbiBpbWFnZS9wbmcK\x1b\\",
                "\x1b]5522;type=read:status=DONE:pw=b3Rw\x1b\\"
            ),
            out
        );
    }

    #[test]
    fn paste_event_primary_is_reported_only_on_the_ok_packet() {
        let out = encoded(|w| {
            PasteEvent {
                primary: true,
                terminator: Terminator::Bel,
                ..PasteEvent::new(b"otp", &[b"text/plain"])
            }
            .encode(w)
        });
        assert_eq!(
            concat!(
                "\x1b]5522;type=read:status=OK:loc=primary:pw=b3Rw\x07",
                "\x1b]5522;type=read:status=DATA:mime=Lg==:pw=b3Rw;dGV4dC9wbGFpbgo=\x07",
                "\x1b]5522;type=read:status=DONE:pw=b3Rw\x07"
            ),
            out
        );
    }

    #[test]
    fn paste_event_empty_listing_packet_is_still_sent() {
        let out = encoded(|w| PasteEvent::new(b"otp", &[]).encode(w));
        assert_eq!(
            concat!(
                "\x1b]5522;type=read:status=OK:pw=b3Rw\x1b\\",
                "\x1b]5522;type=read:status=DATA:mime=Lg==:pw=b3Rw\x1b\\",
                "\x1b]5522;type=read:status=DONE:pw=b3Rw\x1b\\"
            ),
            out
        );
    }
}
