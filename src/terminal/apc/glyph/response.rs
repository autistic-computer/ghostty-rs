//! Port of `src/terminal/apc/glyph/response.zig`.
//!
//! STATUS: ported

use std::io::Write;

/// Query response coverage state for a codepoint.
///
/// `packed struct(u2)`: bit 0 is `system`, bit 1 is `glossary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Coverage {
    /// A system font covers the codepoint.
    pub system: bool,

    /// A session glyph registration covers the codepoint.
    pub glossary: bool,
}

impl Coverage {
    /// No system font or registered glyph covers the codepoint.
    pub const FREE: Coverage = Coverage {
        system: false,
        glossary: false,
    };

    /// Parse the query response coverage list from its comma-separated form.
    /// Unknown coverage names are ignored for forward compatibility.
    pub fn init(value: &[u8]) -> Option<Coverage> {
        let mut result = Coverage::FREE;
        for name in value.split(|&c| c == b',') {
            if name == b"system" {
                result.system = true;
            } else if name == b"glossary" {
                result.glossary = true;
            }
        }

        Some(result)
    }
}

/// Response to a glyph APC request, formatted for the wire protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response<'a> {
    /// Support query response listing supported payload formats.
    Support(Support),

    /// Codepoint coverage query response.
    Query(Query),

    /// Glyph registration response (success or error).
    Register(Register<'a>),

    /// Registration clear response.
    Clear(Clear<'a>),
}

/// Support query response fields (`Response.Support`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Support {
    /// Supported payload formats.
    pub fmt: Formats,
}

/// `Response.Support.Formats`
///
/// `packed struct(u8)`: bit 0 `glyf`, bit 1 `colrv0`, bit 2 `colrv1`,
/// five bits of padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Formats {
    /// TrueType simple glyph outlines (required in v1).
    pub glyf: bool,

    /// COLR v0 layered flat-colour glyphs.
    pub colrv0: bool,

    /// COLR v1 paint-graph glyphs.
    pub colrv1: bool,
}

impl Formats {
    /// The packed u8 value (`@bitCast`).
    pub const fn to_bits(self) -> u8 {
        (self.glyf as u8) | ((self.colrv0 as u8) << 1) | ((self.colrv1 as u8) << 2)
    }
}

/// Codepoint query response fields (`Response.Query`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Query {
    /// The queried codepoint (u21).
    pub cp: u32,

    /// Coverage status for the codepoint.
    pub status: Coverage,
}

/// Register response fields (`Response.Register`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Register<'a> {
    /// The target codepoint of the registration (u21).
    pub cp: u32,

    /// Result status of the registration encoded as a decimal u8.
    pub status: Status,

    /// Optional symbolic error reason.
    pub reason: Option<Reason<'a>>,
}

impl Register<'_> {
    /// A successful registration of `cp` (the Zig field defaults).
    pub fn ok(cp: u32) -> Self {
        Register {
            cp,
            status: Status::OK,
            reason: None,
        }
    }
}

/// Register error reason codes defined by Glyph Protocol §6.2
/// (`Response.Register.Reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason<'a> {
    /// `cp` is not in any PUA range.
    OutOfNamespace,

    /// Payload contains composite glyphs.
    CompositeUnsupported,

    /// Payload contains hinting instructions.
    HintingUnsupported,

    /// Payload failed to parse as the declared `fmt`.
    MalformedPayload,

    /// Payload exceeds 64 KiB after base64 decoding.
    PayloadTooLarge,

    /// A reason code not known by this version of Ghostty.
    Other(&'a [u8]),
}

impl<'a> Reason<'a> {
    /// Return the wire-format reason name.
    pub fn name(&self) -> &'a [u8] {
        match *self {
            Reason::OutOfNamespace => b"out_of_namespace",
            Reason::CompositeUnsupported => b"composite_unsupported",
            Reason::HintingUnsupported => b"hinting_unsupported",
            Reason::MalformedPayload => b"malformed_payload",
            Reason::PayloadTooLarge => b"payload_too_large",
            Reason::Other(value) => value,
        }
    }
}

/// Clear response fields (`Response.Clear`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Clear<'a> {
    /// Result status of the clear operation encoded as a decimal u8.
    pub status: Status,

    /// Optional symbolic error reason.
    pub reason: Option<&'a [u8]>,
}

/// Status code for register and clear responses (`Response.Status`).
///
/// Non-exhaustive: any u8 value is representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct Status(pub u8);

impl Status {
    /// The operation completed successfully.
    pub const OK: Status = Status(0);

    /// A generic or unspecified error occurred.
    pub const ERR: Status = Status(1);
}

impl Response<'_> {
    /// Recommended fixed buffer size for formatting a Glyph Protocol response.
    ///
    /// Glyph Protocol responses contain only framing plus bounded scalar fields:
    /// a u21 codepoint as hex, a decimal u8 status, a small fixed set of
    /// supported format names, coverage names, and the reason names produced by
    /// the executor. 1024 bytes is therefore far above the longest response we
    /// can emit today, while still being small enough for stack allocation in
    /// stream handlers. If callers construct responses with arbitrary `.other`
    /// or clear reason strings, they must ensure those strings fit or handle the
    /// writer error from `format_wire`.
    pub const MAX_WIRE_BYTES: usize = 1024;

    /// Write the response in the glyph APC wire format to `writer`.
    ///
    /// The framing is: `ESC _ 25a1 ; <verb> ; <key=value>* ESC \`
    pub fn format_wire<W: Write + ?Sized>(&self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(b"\x1b_25a1;")?;
        match self {
            Response::Support(r) => {
                // From the spec:
                // Order is not significant; clients MUST treat the value as a
                // set. An empty fmt= value means the terminal recognises
                // Glyph Protocol but currently advertises no payload formats
                // — every r will be rejected. Clients MUST ignore names they
                // do not recognise rather than failing the reply, so future
                // format names are forward- compatible.
                writer.write_all(b"s;fmt=")?;
                let mut first = true;
                if r.fmt.glyf {
                    first = false;
                    writer.write_all(b"glyf")?;
                }
                if r.fmt.colrv0 {
                    if !first {
                        writer.write_all(b",")?;
                    }
                    first = false;
                    writer.write_all(b"colrv0")?;
                }
                if r.fmt.colrv1 {
                    if !first {
                        writer.write_all(b",")?;
                    }
                    writer.write_all(b"colrv1")?;
                }
            }
            Response::Query(r) => {
                // status is a comma-separated list of coverage names — the
                // set of sources that can render cp in this session. Order is
                // not significant; clients MUST treat the value as a set.
                write!(writer, "q;cp={:x};status=", r.cp)?;
                let mut first = true;
                if r.status.system {
                    first = false;
                    writer.write_all(b"system")?;
                }
                if r.status.glossary {
                    if !first {
                        writer.write_all(b",")?;
                    }
                    writer.write_all(b"glossary")?;
                }
            }
            Response::Register(r) => {
                write!(writer, "r;cp={:x};status={}", r.cp, r.status.0)?;
                if let Some(reason) = &r.reason {
                    writer.write_all(b";reason=")?;
                    writer.write_all(reason.name())?;
                }
            }
            Response::Clear(r) => {
                write!(writer, "c;status={}", r.status.0)?;
                if let Some(reason) = r.reason {
                    writer.write_all(b";reason=")?;
                    writer.write_all(reason)?;
                }
            }
        }
        writer.write_all(b"\x1b\\")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(resp: Response<'_>) -> Vec<u8> {
        let mut buf = [0u8; 256];
        let mut writer: &mut [u8] = &mut buf;
        resp.format_wire(&mut writer).unwrap();
        let n = 256 - writer.len();
        buf[..n].to_vec()
    }

    #[test]
    fn support_formats_default_to_no_advertised_formats() {
        assert_eq!(0u8, Formats::default().to_bits());
    }

    #[test]
    fn response_support_format_wire() {
        let resp = Response::Support(Support {
            fmt: Formats {
                glyf: true,
                colrv0: true,
                ..Default::default()
            },
        });
        assert_eq!(
            b"\x1b_25a1;s;fmt=glyf,colrv0\x1b\\".as_slice(),
            wire(resp).as_slice()
        );
    }

    #[test]
    fn response_support_format_wire_with_no_formats() {
        let resp = Response::Support(Support {
            fmt: Formats::default(),
        });
        assert_eq!(b"\x1b_25a1;s;fmt=\x1b\\".as_slice(), wire(resp).as_slice());
    }

    #[test]
    fn response_query_format_wire() {
        let resp = Response::Query(Query {
            cp: 0xE0A0,
            status: Coverage {
                system: true,
                glossary: true,
            },
        });
        assert_eq!(
            b"\x1b_25a1;q;cp=e0a0;status=system,glossary\x1b\\".as_slice(),
            wire(resp).as_slice()
        );
    }

    #[test]
    fn response_query_format_wire_with_no_coverage() {
        let resp = Response::Query(Query {
            cp: 0xE0A0,
            status: Coverage::default(),
        });
        assert_eq!(
            b"\x1b_25a1;q;cp=e0a0;status=\x1b\\".as_slice(),
            wire(resp).as_slice()
        );
    }

    #[test]
    fn coverage_parses_comma_separated_names() {
        assert_eq!(Coverage::default(), Coverage::init(b"").unwrap());
        assert_eq!(
            Coverage {
                system: true,
                ..Default::default()
            },
            Coverage::init(b"system").unwrap()
        );
        assert_eq!(
            Coverage {
                glossary: true,
                ..Default::default()
            },
            Coverage::init(b"glossary").unwrap()
        );
        assert_eq!(
            Coverage {
                system: true,
                glossary: true
            },
            Coverage::init(b"system,glossary").unwrap()
        );
        assert_eq!(
            Coverage {
                system: true,
                glossary: true
            },
            Coverage::init(b"glossary,system").unwrap()
        );
        assert_eq!(
            Coverage {
                system: true,
                ..Default::default()
            },
            Coverage::init(b"system,future").unwrap()
        );
        assert_eq!(Coverage::default(), Coverage::init(b"future").unwrap());
    }

    #[test]
    fn response_register_success_format_wire() {
        let resp = Response::Register(Register::ok(0xE0A0));
        assert_eq!(
            b"\x1b_25a1;r;cp=e0a0;status=0\x1b\\".as_slice(),
            wire(resp).as_slice()
        );
    }

    #[test]
    fn response_register_error_format_wire() {
        let resp = Response::Register(Register {
            cp: 0xE0A0,
            status: Status::ERR,
            reason: Some(Reason::OutOfNamespace),
        });
        assert_eq!(
            b"\x1b_25a1;r;cp=e0a0;status=1;reason=out_of_namespace\x1b\\".as_slice(),
            wire(resp).as_slice()
        );
    }

    #[test]
    fn response_register_arbitrary_status_format_wire() {
        let resp = Response::Register(Register {
            cp: 0xE0A0,
            status: Status(37),
            reason: Some(Reason::PayloadTooLarge),
        });
        assert_eq!(
            b"\x1b_25a1;r;cp=e0a0;status=37;reason=payload_too_large\x1b\\".as_slice(),
            wire(resp).as_slice()
        );
    }

    #[test]
    fn register_reason_names() {
        assert_eq!(b"out_of_namespace", Reason::OutOfNamespace.name());
        assert_eq!(b"composite_unsupported", Reason::CompositeUnsupported.name());
        assert_eq!(b"hinting_unsupported", Reason::HintingUnsupported.name());
        assert_eq!(b"malformed_payload", Reason::MalformedPayload.name());
        assert_eq!(b"payload_too_large", Reason::PayloadTooLarge.name());
        assert_eq!(b"future_reason", Reason::Other(b"future_reason").name());
    }

    #[test]
    fn response_clear_format_wire() {
        let resp = Response::Clear(Clear::default());
        assert_eq!(
            b"\x1b_25a1;c;status=0\x1b\\".as_slice(),
            wire(resp).as_slice()
        );
    }

    #[test]
    fn response_clear_error_format_wire() {
        let resp = Response::Clear(Clear {
            status: Status::ERR,
            reason: Some(b"out_of_namespace"),
        });
        assert_eq!(
            b"\x1b_25a1;c;status=1;reason=out_of_namespace\x1b\\".as_slice(),
            wire(resp).as_slice()
        );
    }
}
