//! Port of `src/terminal/kitty/dnd_response.zig`.
//!
//! STATUS: ported
//!
//! Deviation: `std.Io.Writer` is any `std::io::Write`.

use std::fmt;
use std::io::Write;

use base64::Engine as _;

use crate::terminal::osc::Terminator;

/// The maximum raw bytes per base64-encoded chunk, chosen by kitty so a
/// chunk is exactly 4096 base64 characters (the protocol's chunk limit).
pub const MAX_CHUNK_RAW: usize = 3072;

/// The maximum bytes per plain-text (non-base64) chunk.
pub const MAX_CHUNK_PLAIN: usize = 4096;

/// Error names used in protocol error payloads. The wire encoding is
/// the tag name itself. This matches kitty's get_errno_name vocabulary,
/// which extends the spec's list with EISDIR, ENOSPC, and OK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::upper_case_acronyms)]
pub enum Errno {
    OK,
    EPERM,
    ENOENT,
    EIO,
    EINVAL,
    EMFILE,
    ENOMEM,
    EFBIG,
    EISDIR,
    ENOSPC,
    EUNKNOWN,
}

impl Errno {
    /// The wire encoding (Zig `@tagName`).
    pub fn as_str(self) -> &'static str {
        match self {
            Errno::OK => "OK",
            Errno::EPERM => "EPERM",
            Errno::ENOENT => "ENOENT",
            Errno::EIO => "EIO",
            Errno::EINVAL => "EINVAL",
            Errno::EMFILE => "EMFILE",
            Errno::ENOMEM => "ENOMEM",
            Errno::EFBIG => "EFBIG",
            Errno::EISDIR => "EISDIR",
            Errno::ENOSPC => "ENOSPC",
            Errno::EUNKNOWN => "EUNKNOWN",
        }
    }
}

/// The payload encoding for a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// Payload bytes are sent as-is (MIME lists, error strings).
    Plain,

    /// Payload bytes are base64-encoded (all binary data).
    Base64,
}

/// The `x`/`y`/`Y` keys of the data request currently being answered,
/// echoed in responses and errors so the client can match them up. Only
/// non-zero keys are written, matching kitty's drop_append_request_keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[allow(non_snake_case)]
pub struct RequestKeys {
    pub x: i32,
    pub y: i32,
    pub Y: i32,
}

impl fmt::Display for RequestKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.x != 0 {
            write!(f, ":x={}", self.x)?;
        }
        if self.y != 0 {
            write!(f, ":y={}", self.y)?;
        }
        if self.Y != 0 {
            write!(f, ":Y={}", self.Y)?;
        }
        Ok(())
    }
}

/// The kind of an error response (Zig: `enum { drop, drag }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Drop,
    Drag,
}

/// Encode a complete protocol message: one bare OSC when `data` is
/// empty, otherwise one complete OSC per chunk of `data`, each
/// repeating the header. The final chunk carries `m=0`, earlier
/// chunks `m=1`.
///
/// `header` is the metadata without the OSC introducer, e.g. "t=q" or
/// "t=m:x=5:y=3". The client ID is appended as `:i=N` when non-zero.
pub fn encode<W: Write + ?Sized>(
    writer: &mut W,
    header: &[u8],
    client_id: u32,
    data: &[u8],
    encoding: Encoding,
    terminator: Terminator,
) -> std::io::Result<()> {
    // The client ID is part of the repeated header.
    let id: String = if client_id != 0 {
        format!(":i={client_id}")
    } else {
        String::new()
    };

    if data.is_empty() {
        writer.write_all(b"\x1b]72;")?;
        writer.write_all(header)?;
        writer.write_all(id.as_bytes())?;
        writer.write_all(terminator.string())?;
        return Ok(());
    }

    let limit: usize = match encoding {
        Encoding::Base64 => MAX_CHUNK_RAW,
        Encoding::Plain => MAX_CHUNK_PLAIN,
    };

    let mut offset: usize = 0;
    while offset < data.len() {
        let chunk_len = (data.len() - offset).min(limit);
        let chunk = &data[offset..offset + chunk_len];
        offset += chunk_len;
        let last: u8 = if offset >= data.len() { b'0' } else { b'1' };

        writer.write_all(b"\x1b]72;")?;
        writer.write_all(header)?;
        writer.write_all(id.as_bytes())?;
        writer.write_all(b":m=")?;
        writer.write_all(&[last, b';'])?;
        match encoding {
            Encoding::Plain => writer.write_all(chunk)?,
            Encoding::Base64 => writer.write_all(
                base64::engine::general_purpose::STANDARD
                    .encode(chunk)
                    .as_bytes(),
            )?,
        }
        writer.write_all(terminator.string())?;
    }
    Ok(())
}

/// Encode an error response. `kind` selects the header: t=R for drop
/// data request errors, t=E for drag offer errors. The payload is
/// "NAME" or "NAME:description", sent plain (not base64).
pub fn encode_error<W: Write + ?Sized>(
    writer: &mut W,
    kind: ErrorKind,
    keys: RequestKeys,
    client_id: u32,
    errno: Errno,
    desc: &[u8],
    terminator: Terminator,
) -> std::io::Result<()> {
    let header = format!(
        "t={}{}",
        match kind {
            ErrorKind::Drop => 'R',
            ErrorKind::Drag => 'E',
        },
        keys
    );

    let mut payload: Vec<u8> = errno.as_str().as_bytes().to_vec();
    if !desc.is_empty() {
        payload.push(b':');
        payload.extend_from_slice(desc);
    }

    encode(
        writer,
        header.as_bytes(),
        client_id,
        &payload,
        Encoding::Plain,
        terminator,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out_str(out: &[u8]) -> &str {
        std::str::from_utf8(out).unwrap()
    }

    #[test]
    fn encode_bare_message() {
        let mut out: Vec<u8> = Vec::new();
        encode(&mut out, b"t=q", 0, b"", Encoding::Plain, Terminator::St).unwrap();
        assert_eq!("\x1b]72;t=q\x1b\\", out_str(&out));
    }

    #[test]
    fn encode_bare_message_with_client_id() {
        let mut out: Vec<u8> = Vec::new();
        encode(&mut out, b"t=q", 7, b"", Encoding::Plain, Terminator::St).unwrap();
        assert_eq!("\x1b]72;t=q:i=7\x1b\\", out_str(&out));
    }

    #[test]
    fn encode_plain_payload_single_chunk() {
        let mut out: Vec<u8> = Vec::new();
        encode(
            &mut out,
            b"t=m:x=1:y=2",
            0,
            b"text/plain ",
            Encoding::Plain,
            Terminator::St,
        )
        .unwrap();
        assert_eq!("\x1b]72;t=m:x=1:y=2:m=0;text/plain \x1b\\", out_str(&out));
    }

    #[test]
    fn encode_base64_payload_chunking() {
        let mut out: Vec<u8> = Vec::new();

        // Exactly one byte more than a chunk to force two chunks.
        let data = [b'A'; MAX_CHUNK_RAW + 1];
        encode(&mut out, b"t=r:x=1", 3, &data, Encoding::Base64, Terminator::St).unwrap();

        let out = out_str(&out);

        // First chunk: full header, m=1, 4096 base64 chars.
        let prefix = "\x1b]72;t=r:x=1:i=3:m=1;";
        assert!(out.starts_with(prefix));
        let first_end = out.find("\x1b\\").unwrap();
        assert_eq!(prefix.len() + 4096, first_end);

        // Second chunk: m=0 with the single remaining byte.
        let rest = &out[first_end + 2..];
        assert!(rest.starts_with("\x1b]72;t=r:x=1:i=3:m=0;"));

        // Decodes back to the original data.
        let mut decoded: Vec<u8> = Vec::new();
        for osc in out.split("\x1b\\") {
            if osc.is_empty() {
                continue;
            }
            let payload_start = osc.find(';').unwrap();
            let payload = &osc[payload_start + 1 + osc[payload_start + 1..].find(';').unwrap() + 1..];
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .unwrap();
            decoded.extend_from_slice(&bytes);
        }
        assert_eq!(data.as_slice(), decoded.as_slice());
    }

    #[test]
    fn encode_error_with_and_without_description() {
        {
            let mut out: Vec<u8> = Vec::new();
            encode_error(
                &mut out,
                ErrorKind::Drop,
                RequestKeys {
                    x: 2,
                    ..Default::default()
                },
                0,
                Errno::ENOENT,
                b"drop data request index out of bounds",
                Terminator::St,
            )
            .unwrap();
            assert_eq!(
                "\x1b]72;t=R:x=2:m=0;ENOENT:drop data request index out of bounds\x1b\\",
                out_str(&out)
            );
        }
        {
            let mut out: Vec<u8> = Vec::new();
            encode_error(
                &mut out,
                ErrorKind::Drag,
                RequestKeys::default(),
                5,
                Errno::EPERM,
                b"",
                Terminator::St,
            )
            .unwrap();
            assert_eq!("\x1b]72;t=E:i=5:m=0;EPERM\x1b\\", out_str(&out));
        }
    }

    #[test]
    fn request_keys_only_non_zero_keys_written() {
        assert_eq!("", RequestKeys::default().to_string());
        assert_eq!(
            ":x=1:y=2:Y=3",
            RequestKeys { x: 1, y: 2, Y: 3 }.to_string()
        );
        assert_eq!(
            ":Y=4",
            RequestKeys {
                Y: 4,
                ..Default::default()
            }
            .to_string()
        );
    }
}
