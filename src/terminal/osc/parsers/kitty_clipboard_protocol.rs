//! Port of `src/terminal/osc/parsers/kitty_clipboard_protocol.zig`.
//!
//! STATUS: ported
//!
//! Kitty's clipboard protocol (OSC 5522)
//! Specification: https://sw.kovidgoyal.net/kitty/clipboard/
//! https://rockorager.dev/misc/bracketed-paste-mime/
//!
//! Rust notes: Zig's `Option` enum is named [`ClipboardOption`] here (to not
//! shadow `core::option::Option`). Zig's `readOption(comptime key)` returns
//! a key-dependent type; here each key has a marker type in [`options`]
//! implementing [`OptionType`], e.g. `osc.read_option::<options::Status>()`.

use crate::terminal::osc::kitty_metadata::ValueIterator;
use crate::terminal::osc::{Command, Parser, State, Terminator};

/// A Kitty clipboard protocol command (OSC 5522).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Osc<'a> {
    /// The raw metadata that was received. It can be parsed by using the `read_option` method.
    pub metadata: &'a [u8],
    /// The raw payload. It may be Base64 encoded, check the `e` option.
    pub payload: Option<&'a [u8]>,
    /// The terminator that was used in case we need to send a response.
    pub terminator: Terminator,
}

/// We don't currently support encoding this to C in any way.
pub type OscC = ();

impl<'a> Osc<'a> {
    pub fn cval(&self) -> OscC {}

    /// Decode an option from the metadata.
    pub fn read_option<O: OptionType>(&self) -> Option<O::Type<'a>> {
        O::read(self.metadata)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Location {
    Primary,
}

impl Location {
    pub fn init(s: &[u8]) -> Option<Location> {
        match s {
            b"primary" => Some(Location::Primary),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    Read,
    Walias,
    Wdata,
    Write,
}

impl Operation {
    pub fn init(s: &[u8]) -> Option<Operation> {
        Some(match s {
            b"read" => Operation::Read,
            b"walias" => Operation::Walias,
            b"wdata" => Operation::Wdata,
            b"write" => Operation::Write,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::upper_case_acronyms)]
pub enum Status {
    DATA,
    DONE,
    EBUSY,
    EFBIG,
    EINVAL,
    EIO,
    ENOSYS,
    EPERM,
    OK,
}

impl Status {
    pub fn init(s: &[u8]) -> Option<Status> {
        Some(match s {
            b"DATA" => Status::DATA,
            b"DONE" => Status::DONE,
            b"EBUSY" => Status::EBUSY,
            b"EFBIG" => Status::EFBIG,
            b"EINVAL" => Status::EINVAL,
            b"EIO" => Status::EIO,
            b"ENOSYS" => Status::ENOSYS,
            b"EPERM" => Status::EPERM,
            b"OK" => Status::OK,
            _ => return None,
        })
    }
}

/// The metadata keys (Zig: `Option`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipboardOption {
    Id,
    Loc,
    Mime,
    Name,
    Password,
    Pw,
    Status,
    Type,
}

impl ClipboardOption {
    /// The Zig tag name, which is the metadata key.
    pub fn key(self) -> &'static [u8] {
        match self {
            ClipboardOption::Id => b"id",
            ClipboardOption::Loc => b"loc",
            ClipboardOption::Mime => b"mime",
            ClipboardOption::Name => b"name",
            ClipboardOption::Password => b"password",
            ClipboardOption::Pw => b"pw",
            ClipboardOption::Status => b"status",
            ClipboardOption::Type => b"type",
        }
    }
}

/// The value type of a [`ClipboardOption`] (Zig: `Option.Type()`) and how
/// to read it.
pub trait OptionType {
    const OPTION: ClipboardOption;
    type Type<'a>;
    fn convert(value: &[u8]) -> Option<Self::Type<'_>>;

    /// Read the option value from the raw metadata string.
    fn read(metadata: &[u8]) -> Option<Self::Type<'_>> {
        let mut it = ValueIterator::init(Self::OPTION.key(), None, metadata);
        let value = it.next()?;

        // return the parsed value
        Self::convert(value)
    }
}

/// Marker types for each [`ClipboardOption`].
pub mod options {
    use super::*;

    macro_rules! str_option {
        ($($name:ident),*) => {$(
            pub struct $name;
            impl OptionType for $name {
                const OPTION: ClipboardOption = ClipboardOption::$name;
                type Type<'a> = &'a [u8];
                fn convert(value: &[u8]) -> Option<&[u8]> {
                    Some(value)
                }
            }
        )*};
    }

    str_option!(Mime, Name, Password, Pw);

    pub struct Id;
    impl OptionType for Id {
        const OPTION: ClipboardOption = ClipboardOption::Id;
        type Type<'a> = &'a [u8];
        fn convert(value: &[u8]) -> Option<&[u8]> {
            parse_identifier(value)
        }
    }

    pub struct Loc;
    impl OptionType for Loc {
        const OPTION: ClipboardOption = ClipboardOption::Loc;
        type Type<'a> = Location;
        fn convert(value: &[u8]) -> Option<Location> {
            Location::init(value)
        }
    }

    pub struct Status;
    impl OptionType for Status {
        const OPTION: ClipboardOption = ClipboardOption::Status;
        type Type<'a> = super::Status;
        fn convert(value: &[u8]) -> Option<super::Status> {
            super::Status::init(value)
        }
    }

    pub struct Type;
    impl OptionType for Type {
        const OPTION: ClipboardOption = ClipboardOption::Type;
        type Type<'a> = Operation;
        fn convert(value: &[u8]) -> Option<Operation> {
            Operation::init(value)
        }
    }
}

/// Characters that are valid in identifiers.
const VALID_IDENTIFIER_CHARACTERS: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_+.";

fn is_valid_identifier(s: &[u8]) -> bool {
    if s.is_empty() {
        return false;
    }
    s.iter().all(|c| VALID_IDENTIFIER_CHARACTERS.contains(c))
}

fn parse_identifier(s: &[u8]) -> Option<&[u8]> {
    if is_valid_identifier(s) {
        return Some(s);
    }
    None
}

pub fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    debug_assert!(parser.state == State::Osc5522);

    let Some(cap) = parser.capture.as_ref() else {
        parser.state = State::Invalid;
        return None;
    };

    let data = cap.trailing();

    let (metadata, payload) = match data.iter().position(|&c| c == b';') {
        None => (data, None),
        Some(start) => (&data[..start], Some(&data[start + 1..])),
    };

    Some(Command::KittyClipboardProtocol(Osc {
        metadata,
        payload,
        terminator: Terminator::init(terminator_ch),
    }))
}

#[cfg(test)]
mod tests {
    use super::options as o;
    use super::*;
    use crate::terminal::osc::Allocator;

    /// Expected values of every option, for the repetitive assertions.
    #[derive(Default)]
    struct Expect {
        id: Option<&'static [u8]>,
        loc: Option<Location>,
        mime: Option<&'static [u8]>,
        name: Option<&'static [u8]>,
        password: Option<&'static [u8]>,
        pw: Option<&'static [u8]>,
        status: Option<Status>,
        type_: Option<Operation>,
    }

    fn check(c: &Osc<'_>, e: Expect) {
        assert_eq!(e.id, c.read_option::<o::Id>());
        assert_eq!(e.loc, c.read_option::<o::Loc>());
        assert_eq!(e.mime, c.read_option::<o::Mime>());
        assert_eq!(e.name, c.read_option::<o::Name>());
        assert_eq!(e.password, c.read_option::<o::Password>());
        assert_eq!(e.pw, c.read_option::<o::Pw>());
        assert_eq!(e.status, c.read_option::<o::Status>());
        assert_eq!(e.type_, c.read_option::<o::Type>());
    }

    fn run(input: &[u8], f: impl FnOnce(Osc<'_>)) {
        let mut p = Parser::init(Some(Allocator::GLOBAL));
        for &ch in input {
            p.next(ch);
        }
        match p.end(Some(0x1b)) {
            Some(Command::KittyClipboardProtocol(c)) => f(c),
            other => panic!("expected kitty_clipboard_protocol, got {:?}", other),
        }
    }

    #[test]
    fn osc_5522_empty_metadata_and_missing_payload() {
        run(b"5522;", |c| {
            assert_eq!(b"", c.metadata);
            assert!(c.payload.is_none());
            check(&c, Expect::default());
        });
    }

    #[test]
    fn osc_5522_empty_metadata_and_empty_payload() {
        run(b"5522;;", |c| {
            assert_eq!(b"", c.metadata);
            assert_eq!(b"", c.payload.unwrap());
            check(&c, Expect::default());
        });
    }

    #[test]
    fn osc_5522_non_empty_metadata_and_payload() {
        run(b"5522;type=read;dGV4dC9wbGFpbg==", |c| {
            assert_eq!(b"type=read", c.metadata);
            assert_eq!(b"dGV4dC9wbGFpbg==", c.payload.unwrap());
            check(
                &c,
                Expect {
                    type_: Some(Operation::Read),
                    ..Default::default()
                },
            );
        });
    }

    #[test]
    fn osc_5522_empty_id() {
        run(b"5522;id=", |c| {
            assert!(c.read_option::<o::Id>().is_none());
        });
    }

    #[test]
    fn osc_5522_valid_id() {
        run(b"5522;id=5c076ad9-d36f-4705-847b-d4dbf356cc0d", |c| {
            assert_eq!(
                b"5c076ad9-d36f-4705-847b-d4dbf356cc0d",
                c.read_option::<o::Id>().unwrap()
            );
        });
    }

    #[test]
    fn osc_5522_invalid_id() {
        run(b"5522;id=*42*", |c| {
            assert!(c.read_option::<o::Id>().is_none());
        });
    }

    #[test]
    fn osc_5522_invalid_status() {
        run(b"5522;status=BOBR", |c| {
            assert!(c.read_option::<o::Status>().is_none());
        });
    }

    #[test]
    fn osc_5522_valid_status() {
        run(b"5522;status=DONE", |c| {
            assert_eq!(Status::DONE, c.read_option::<o::Status>().unwrap());
        });
    }

    #[test]
    fn osc_5522_invalid_location() {
        run(b"5522;loc=bobr", |c| {
            assert!(c.read_option::<o::Loc>().is_none());
        });
    }

    #[test]
    fn osc_5522_valid_location() {
        run(b"5522;loc=primary", |c| {
            assert_eq!(Location::Primary, c.read_option::<o::Loc>().unwrap());
        });
    }

    #[test]
    fn osc_5522_password_1() {
        run(b"5522;pw=R2hvc3R0eQ==:name=Qk9CUiBLVVJXQQ==", |c| {
            assert_eq!(b"R2hvc3R0eQ==", c.read_option::<o::Pw>().unwrap());
            assert_eq!(b"Qk9CUiBLVVJXQQ==", c.read_option::<o::Name>().unwrap());
        });
    }

    #[test]
    fn osc_5522_password_2() {
        run(b"5522;password=R2hvc3R0eQ==", |c| {
            assert_eq!(b"R2hvc3R0eQ==", c.read_option::<o::Password>().unwrap());
        });
    }

    #[test]
    fn osc_5522_example_1() {
        run(b"5522;type=read:status=OK", |c| {
            assert!(c.payload.is_none());
            check(
                &c,
                Expect {
                    status: Some(Status::OK),
                    type_: Some(Operation::Read),
                    ..Default::default()
                },
            );
        });
    }

    #[test]
    fn osc_5522_example_2() {
        run(b"5522;type=read:mime=dGV4dC9wbGFpbg==;R2hvc3R0eQ==", |c| {
            assert_eq!(b"R2hvc3R0eQ==", c.payload.unwrap());
            check(
                &c,
                Expect {
                    mime: Some(b"dGV4dC9wbGFpbg=="),
                    type_: Some(Operation::Read),
                    ..Default::default()
                },
            );
        });
    }

    #[test]
    fn osc_5522_example_3() {
        run(b"5522;type=read:status=OK", |c| {
            assert!(c.payload.is_none());
            check(
                &c,
                Expect {
                    status: Some(Status::OK),
                    type_: Some(Operation::Read),
                    ..Default::default()
                },
            );
        });
    }

    #[test]
    fn osc_5522_example_4() {
        run(b"5522;type=write", |c| {
            assert!(c.payload.is_none());
            check(
                &c,
                Expect {
                    type_: Some(Operation::Write),
                    ..Default::default()
                },
            );
        });
    }

    #[test]
    fn osc_5522_example_5() {
        run(b"5522;type=wdata:mime=dGV4dC9wbGFpbg==;R2hvc3R0eQ==", |c| {
            assert_eq!(b"R2hvc3R0eQ==", c.payload.unwrap());
            check(
                &c,
                Expect {
                    mime: Some(b"dGV4dC9wbGFpbg=="),
                    type_: Some(Operation::Wdata),
                    ..Default::default()
                },
            );
        });
    }

    #[test]
    fn osc_5522_example_6() {
        run(b"5522;type=wdata", |c| {
            assert!(c.payload.is_none());
            check(
                &c,
                Expect {
                    type_: Some(Operation::Wdata),
                    ..Default::default()
                },
            );
        });
    }

    #[test]
    fn osc_5522_example_7() {
        run(b"5522;type=write:status=DONE", |c| {
            assert!(c.payload.is_none());
            check(
                &c,
                Expect {
                    status: Some(Status::DONE),
                    type_: Some(Operation::Write),
                    ..Default::default()
                },
            );
        });
    }

    #[test]
    fn osc_5522_example_8() {
        run(b"5522;type=write:status=EPERM", |c| {
            assert!(c.payload.is_none());
            check(
                &c,
                Expect {
                    status: Some(Status::EPERM),
                    type_: Some(Operation::Write),
                    ..Default::default()
                },
            );
        });
    }

    #[test]
    fn osc_5522_example_9() {
        run(
            b"5522;type=walias:mime=dGV4dC9wbGFpbg==;dGV4dC9odG1sIGFwcGxpY2F0aW9uL2pzb24=",
            |c| {
                assert_eq!(
                    b"dGV4dC9odG1sIGFwcGxpY2F0aW9uL2pzb24=",
                    c.payload.unwrap()
                );
                check(
                    &c,
                    Expect {
                        mime: Some(b"dGV4dC9wbGFpbg=="),
                        type_: Some(Operation::Walias),
                        ..Default::default()
                    },
                );
            },
        );
    }

    #[test]
    fn osc_5522_example_10() {
        run(b"5522;type=read:status=OK:password=Qk9CUiBLVVJXQQ==", |c| {
            assert!(c.payload.is_none());
            check(
                &c,
                Expect {
                    password: Some(b"Qk9CUiBLVVJXQQ=="),
                    status: Some(Status::OK),
                    type_: Some(Operation::Read),
                    ..Default::default()
                },
            );
        });
    }

    #[test]
    fn osc_5522_example_11() {
        run(b"5522;type=read:status=DATA:mime=dGV4dC9wbGFpbg==", |c| {
            assert!(c.payload.is_none());
            check(
                &c,
                Expect {
                    mime: Some(b"dGV4dC9wbGFpbg=="),
                    status: Some(Status::DATA),
                    type_: Some(Operation::Read),
                    ..Default::default()
                },
            );
        });
    }

    #[test]
    fn osc_5522_example_12() {
        run(
            b"5522;type=read:mime=dGV4dC9wbGFpbg==:password=Qk9CUiBLVVJXQQ==",
            |c| {
                assert!(c.payload.is_none());
                check(
                    &c,
                    Expect {
                        mime: Some(b"dGV4dC9wbGFpbg=="),
                        password: Some(b"Qk9CUiBLVVJXQQ=="),
                        type_: Some(Operation::Read),
                        ..Default::default()
                    },
                );
            },
        );
    }

    #[test]
    fn osc_5522_example_13() {
        run(b"5522;type=read:status=OK", |c| {
            assert!(c.payload.is_none());
            check(
                &c,
                Expect {
                    status: Some(Status::OK),
                    type_: Some(Operation::Read),
                    ..Default::default()
                },
            );
        });
    }

    #[test]
    fn osc_5522_example_14() {
        run(
            b"5522;type=read:status=DATA:mime=dGV4dC9wbGFpbg==;Qk9CUiBLVVJXQQ==",
            |c| {
                assert_eq!(b"Qk9CUiBLVVJXQQ==", c.payload.unwrap());
                check(
                    &c,
                    Expect {
                        mime: Some(b"dGV4dC9wbGFpbg=="),
                        status: Some(Status::DATA),
                        type_: Some(Operation::Read),
                        ..Default::default()
                    },
                );
            },
        );
    }

    #[test]
    fn osc_5522_example_15() {
        run(b"5522;type=read:status=OK", |c| {
            assert!(c.payload.is_none());
            check(
                &c,
                Expect {
                    status: Some(Status::OK),
                    type_: Some(Operation::Read),
                    ..Default::default()
                },
            );
        });
    }
}
