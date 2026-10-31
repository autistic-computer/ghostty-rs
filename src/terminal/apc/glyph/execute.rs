//! Port of `src/terminal/apc/glyph/execute.zig`.
//!
//! STATUS: ported
//!
//! Port note: allocator parameters are dropped.

use super::glossary::{self, Entry, Glossary};
use super::request::{clear_option, query_option, register_option as ro, Request};
use super::response::{self, Response};

const LOG_TARGET: &str = "glyph";

/// Payload formats we support. Hardcoded because the support is
/// fixed.
pub const SUPPORTED_FORMATS: response::Formats = response::Formats {
    glyf: true,
    colrv0: false,
    colrv1: false,
};

/// Execute a Glyph protocol request against the given state.
///
/// This will never fail, but the response may indiciate an error and
/// the terminal state may not be updated to reflect the command. This will
/// never put the terminal in a corrupt or non-recoverable state.
///
/// For example, allocation errors can happen, but they're wrapped up in
/// an out of memory response.
///
/// Query responses only report glossary coverage. Callers that can determine
/// system font coverage must update the returned query response before sending
/// it to the client.
pub fn execute(glossary: &mut Glossary, req: &Request) -> Option<Response<'static>> {
    log::debug!(target: LOG_TARGET, "executing glyph protocol request: {:?}", req);
    match req {
        Request::Support => Some(Response::Support(response::Support {
            fmt: SUPPORTED_FORMATS,
        })),
        Request::Query(qry) => query(glossary, qry),
        Request::Register(reg) => register(glossary, reg),
        Request::Clear(clr) => clear(glossary, clr),
    }
}

fn query(glossary: &mut Glossary, qry: &super::request::Query) -> Option<Response<'static>> {
    let cp = qry.get(query_option::Cp)?;
    Some(Response::Query(response::Query {
        cp,
        status: response::Coverage {
            glossary: glossary.contains(cp),
            ..Default::default()
        },
    }))
}

fn register(glossary: &mut Glossary, reg: &super::request::Register) -> Option<Response<'static>> {
    use super::request::Reply;

    let reply = reg.get(ro::Reply).unwrap_or(Reply::All);
    let cp = match register_fallible(glossary, reg) {
        Ok(cp) => cp,
        Err(err) => {
            return match reply {
                Reply::None => None,
                Reply::All | Reply::Failures => Some(Response::Register(response::Register {
                    cp: reg.get(ro::Cp).unwrap_or(0),
                    status: response::Status::ERR,
                    reason: Some(match err {
                        RegisterFallibleError::OutOfMemory => {
                            response::Reason::Other(b"out_of_memory")
                        }
                        RegisterFallibleError::OutOfNamespace => response::Reason::OutOfNamespace,
                        RegisterFallibleError::PayloadTooLarge => {
                            response::Reason::PayloadTooLarge
                        }
                        RegisterFallibleError::MalformedPayload => {
                            response::Reason::MalformedPayload
                        }
                        RegisterFallibleError::CompositeUnsupported => {
                            response::Reason::CompositeUnsupported
                        }
                        RegisterFallibleError::HintingUnsupported => {
                            response::Reason::HintingUnsupported
                        }
                        RegisterFallibleError::InvalidOptions
                        | RegisterFallibleError::UnsupportedFormat => {
                            response::Reason::MalformedPayload
                        }
                    }),
                })),
            };
        }
    };

    match reply {
        Reply::None | Reply::Failures => None,
        Reply::All => Some(Response::Register(response::Register::ok(cp))),
    }
}

/// `Glossary.Entry.InitError || Glossary.RegisterError`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegisterFallibleError {
    OutOfMemory,
    OutOfNamespace,
    PayloadTooLarge,
    MalformedPayload,
    CompositeUnsupported,
    HintingUnsupported,
    InvalidOptions,
    UnsupportedFormat,
}

impl From<glossary::InitError> for RegisterFallibleError {
    fn from(e: glossary::InitError) -> Self {
        use glossary::InitError as E;
        match e {
            E::OutOfMemory => Self::OutOfMemory,
            E::PayloadTooLarge => Self::PayloadTooLarge,
            E::MalformedPayload => Self::MalformedPayload,
            E::CompositeUnsupported => Self::CompositeUnsupported,
            E::HintingUnsupported => Self::HintingUnsupported,
            E::InvalidOptions => Self::InvalidOptions,
            E::UnsupportedFormat => Self::UnsupportedFormat,
        }
    }
}

impl From<glossary::RegisterError> for RegisterFallibleError {
    fn from(e: glossary::RegisterError) -> Self {
        match e {
            glossary::RegisterError::OutOfMemory => Self::OutOfMemory,
            glossary::RegisterError::OutOfNamespace => Self::OutOfNamespace,
        }
    }
}

fn register_fallible(
    glossary: &mut Glossary,
    reg: &super::request::Register,
) -> Result<u32, RegisterFallibleError> {
    let cp = reg
        .get(ro::Cp)
        .ok_or(RegisterFallibleError::MalformedPayload)?;

    let entry = Entry::init(reg)?;

    glossary.register(cp, entry)?;
    Ok(cp)
}

fn clear(glossary: &mut Glossary, clr: &super::request::Clear) -> Option<Response<'static>> {
    if let Some(cp) = clr.get(clear_option::Cp) {
        if let Err(err) = glossary.delete(cp) {
            return Some(Response::Clear(response::Clear {
                status: response::Status::ERR,
                reason: Some(match err {
                    glossary::ClearError::OutOfNamespace => b"out_of_namespace",
                }),
            }));
        }
    } else if clr.has(clear_option::Cp) {
        return Some(Response::Clear(response::Clear {
            status: response::Status::ERR,
            reason: Some(b"malformed_payload"),
        }));
    } else {
        glossary.clear_and_free();
    }

    Some(Response::Clear(response::Clear::default()))
}

#[cfg(test)]
mod tests {
    use super::super::request::CommandParser;
    use super::*;

    fn test_parse(data: &[u8]) -> Request {
        let mut parser = CommandParser::init(1024 * 1024);
        for &byte in data {
            parser.feed(byte).unwrap();
        }
        parser.complete().unwrap()
    }

    fn test_execute(glossary: &mut Glossary, req: &Request) -> Option<Response<'static>> {
        execute(glossary, req)
    }

    #[test]
    fn execute_register_stores_glyph_and_returns_success() {
        let mut glossary = Glossary::empty();

        let req = test_parse(b"r;cp=e0a0;AAAAAAAAAAAAAA==");

        assert_eq!(
            Response::Register(response::Register::ok(0xE0A0)),
            test_execute(&mut glossary, &req).unwrap()
        );
        assert!(glossary.contains(0xE0A0));
    }

    #[test]
    fn execute_register_reply_failures_suppresses_success() {
        let mut glossary = Glossary::empty();

        let req = test_parse(b"r;cp=e0a0;reply=2;AAAAAAAAAAAAAA==");

        assert!(test_execute(&mut glossary, &req).is_none());
        assert!(glossary.contains(0xE0A0));
    }

    #[test]
    fn execute_register_reply_none_suppresses_failure() {
        let mut glossary = Glossary::empty();

        let req = test_parse(b"r;cp=41;reply=0;%%%not-base64%%%");

        assert!(test_execute(&mut glossary, &req).is_none());
        assert!(!glossary.contains('A' as u32));
    }

    #[test]
    fn execute_register_rejects_non_pua() {
        let mut glossary = Glossary::empty();

        let req = test_parse(b"r;cp=41;AAAAAAAAAAAAAA==");

        assert_eq!(
            Response::Register(response::Register {
                cp: 'A' as u32,
                status: response::Status::ERR,
                reason: Some(response::Reason::OutOfNamespace),
            }),
            test_execute(&mut glossary, &req).unwrap()
        );
        assert!(!glossary.contains('A' as u32));
    }

    #[test]
    fn execute_register_reports_malformed_payload() {
        let mut glossary = Glossary::empty();

        let req = test_parse(b"r;cp=e0a0;%%%not-base64%%%");

        assert_eq!(
            Response::Register(response::Register {
                cp: 0xE0A0,
                status: response::Status::ERR,
                reason: Some(response::Reason::MalformedPayload),
            }),
            test_execute(&mut glossary, &req).unwrap()
        );
        assert!(!glossary.contains(0xE0A0));
    }

    #[test]
    fn execute_clear_removes_all_glyphs() {
        let mut glossary = Glossary::empty();

        let reg1 = test_parse(b"r;cp=e0a0;AAAAAAAAAAAAAA==");
        let _ = test_execute(&mut glossary, &reg1);

        let reg2 = test_parse(b"r;cp=e0a1;AAAAAAAAAAAAAA==");
        let _ = test_execute(&mut glossary, &reg2);

        let req = test_parse(b"c");

        assert_eq!(
            Response::Clear(response::Clear::default()),
            test_execute(&mut glossary, &req).unwrap()
        );
        assert!(!glossary.contains(0xE0A0));
        assert!(!glossary.contains(0xE0A1));
    }

    #[test]
    fn execute_clear_removes_one_glyph() {
        let mut glossary = Glossary::empty();

        let reg1 = test_parse(b"r;cp=e0a0;AAAAAAAAAAAAAA==");
        let _ = test_execute(&mut glossary, &reg1);

        let reg2 = test_parse(b"r;cp=e0a1;AAAAAAAAAAAAAA==");
        let _ = test_execute(&mut glossary, &reg2);

        let req = test_parse(b"c;cp=e0a0");

        assert_eq!(
            Response::Clear(response::Clear::default()),
            test_execute(&mut glossary, &req).unwrap()
        );
        assert!(!glossary.contains(0xE0A0));
        assert!(glossary.contains(0xE0A1));
    }

    #[test]
    fn execute_clear_rejects_non_pua() {
        let mut glossary = Glossary::empty();

        let req = test_parse(b"c;cp=41");

        assert_eq!(
            Response::Clear(response::Clear {
                status: response::Status::ERR,
                reason: Some(b"out_of_namespace"),
            }),
            test_execute(&mut glossary, &req).unwrap()
        );
    }

    #[test]
    fn execute_clear_rejects_malformed_cp_without_clearing_glossary() {
        let mut glossary = Glossary::empty();

        let reg1 = test_parse(b"r;cp=e0a0;AAAAAAAAAAAAAA==");
        let _ = test_execute(&mut glossary, &reg1);

        let reg2 = test_parse(b"r;cp=e0a1;AAAAAAAAAAAAAA==");
        let _ = test_execute(&mut glossary, &reg2);

        for data in [b"c;cp=zz".as_slice(), b"c;cp=", b"c;cp=200000"] {
            let req = test_parse(data);

            assert_eq!(
                Response::Clear(response::Clear {
                    status: response::Status::ERR,
                    reason: Some(b"malformed_payload"),
                }),
                test_execute(&mut glossary, &req).unwrap()
            );
            assert!(glossary.contains(0xE0A0));
            assert!(glossary.contains(0xE0A1));
        }
    }

    #[test]
    fn execute_query_reports_no_coverage() {
        let mut glossary = Glossary::empty();

        let req = test_parse(b"q;cp=e0a0");

        assert_eq!(
            Response::Query(response::Query {
                cp: 0xE0A0,
                status: response::Coverage::default(),
            }),
            test_execute(&mut glossary, &req).unwrap()
        );
    }

    #[test]
    fn execute_query_reports_glossary_coverage() {
        let mut glossary = Glossary::empty();

        let reg = test_parse(b"r;cp=e0a0;AAAAAAAAAAAAAA==");
        let _ = test_execute(&mut glossary, &reg);

        let req = test_parse(b"q;cp=e0a0");

        assert_eq!(
            Response::Query(response::Query {
                cp: 0xE0A0,
                status: response::Coverage {
                    glossary: true,
                    ..Default::default()
                },
            }),
            test_execute(&mut glossary, &req).unwrap()
        );
    }

    #[test]
    fn execute_query_without_cp_returns_no_response() {
        let mut glossary = Glossary::empty();

        let req = test_parse(b"q;foo=bar");

        assert!(test_execute(&mut glossary, &req).is_none());
    }
}
