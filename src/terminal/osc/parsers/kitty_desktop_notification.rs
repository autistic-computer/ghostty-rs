//! Port of `src/terminal/osc/parsers/kitty_desktop_notification.zig`.
//!
//! STATUS: ported
//!
//! Kitty's desktop notification protocol (OSC 99)
//! Specification: https://sw.kovidgoyal.net/kitty/desktop-notifications/
//!
//! Rust notes: Zig's `Option` enum is named [`NotificationOption`] here (to
//! not shadow `core::option::Option`). Zig's `readOption(comptime key)`
//! returns a key-dependent type; here each key has a marker type in
//! [`options`] implementing [`OptionType`], e.g.
//! `osc.read_option::<options::U>()`.

use crate::lib::parse_int;
use crate::terminal::color::trim;
use crate::terminal::osc::encoding;
use crate::terminal::osc::kitty_metadata::{ValueIterator, ASCII_WHITESPACE};
use crate::terminal::osc::{Command, Parser, State, Terminator};

const LOG_TARGET: &str = "kitty_desktop_notification";

pub const MAX_PLAIN_PAYLOAD_BYTES: usize = 2048;
pub const MAX_ENCODED_PAYLOAD_BYTES: usize = 4096;

/// A Kitty desktop notification command (OSC 99).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Osc<'a> {
    /// The raw metadata that was received. It can be parsed by using the `read_option` method.
    pub metadata: &'a [u8],
    /// The raw payload. It may be Base64 encoded, check the `e` option.
    pub payload: &'a [u8],
    /// The terminator that was used in case we need to send a response.
    pub terminator: Terminator,
}

impl<'a> Osc<'a> {
    /// Decode an option from the metadata.
    pub fn read_option<O: OptionType>(&self) -> O::Type<'a> {
        O::read(self.metadata)
    }
}

/// What action(s) should be taken when a notification is clicked (Zig:
/// `packed struct { focus: bool, report: bool }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Action {
    pub focus: bool,
    pub report: bool,
}

impl Default for Action {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Action {
    pub const DEFAULT: Action = Action {
        focus: true,
        report: false,
    };

    pub fn init(s: &[u8]) -> Action {
        parse_packed_struct(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Occasion {
    Always,
    Invisible,
    Unfocused,
}

impl Default for Occasion {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Occasion {
    pub const DEFAULT: Occasion = Occasion::Always;

    pub fn init(s: &[u8]) -> Occasion {
        match s {
            b"always" => Occasion::Always,
            b"invisible" => Occasion::Invisible,
            b"unfocused" => Occasion::Unfocused,
            _ => Self::DEFAULT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Payload {
    Alive,
    Body,
    Buttons,
    Close,
    Icon,
    Query,
    Title,
    /// This is a special value to indicate that an unknown payload value was
    /// specified and it should be ignored.
    Unknown,
}

impl Default for Payload {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Payload {
    pub const DEFAULT: Payload = Payload::Title;

    pub fn init(s: &[u8]) -> Payload {
        if s.len() == 1 && s[0] == b'?' {
            return Payload::Query;
        }
        // The string `query` is not allowed, it should be a single question
        // mark if you want a query.
        if s == b"query" {
            return Payload::Unknown;
        }
        match s {
            b"alive" => Payload::Alive,
            b"body" => Payload::Body,
            b"buttons" => Payload::Buttons,
            b"close" => Payload::Close,
            b"icon" => Payload::Icon,
            b"title" => Payload::Title,
            b"unknown" => Payload::Unknown,
            _ => Payload::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Urgency {
    Low,
    Normal,
    High,
}

impl Default for Urgency {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Urgency {
    pub const DEFAULT: Urgency = Urgency::Normal;

    pub fn init(s: &[u8]) -> Urgency {
        if s.len() != 1 {
            return Self::DEFAULT;
        }
        match s[0] {
            b'0' => Urgency::Low,
            b'1' => Urgency::Normal,
            b'2' => Urgency::High,
            _ => Self::DEFAULT,
        }
    }
}

/// The metadata keys (Zig: `Option`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationOption {
    /// What action(s) should be taken when a notification is clicked.
    A,
    /// Should a notification be sent to the application when the notification
    /// is closed?
    C,
    /// Are we done with the notification, and it is ready to be sent?
    D,
    /// Is the payload encoded with Base64?
    E,
    /// The name of the application that is sending the notification.
    F,
    /// Identifier for icon data. Only used when the payload is icon data.
    G,
    /// Identifier for the notification.
    I,
    /// Icon name.
    N,
    /// When to honor the notification request.
    O,
    /// Type of the payload.
    P,
    /// The sound name to play with the notification.
    S,
    /// The type of the notification.
    T,
    /// The urgency of the notification.
    U,
    /// When to auto-close the notification.
    W,
}

impl NotificationOption {
    /// The Zig tag name, which is the metadata key.
    pub fn key(self) -> &'static [u8] {
        match self {
            NotificationOption::A => b"a",
            NotificationOption::C => b"c",
            NotificationOption::D => b"d",
            NotificationOption::E => b"e",
            NotificationOption::F => b"f",
            NotificationOption::G => b"g",
            NotificationOption::I => b"i",
            NotificationOption::N => b"n",
            NotificationOption::O => b"o",
            NotificationOption::P => b"p",
            NotificationOption::S => b"s",
            NotificationOption::T => b"t",
            NotificationOption::U => b"u",
            NotificationOption::W => b"w",
        }
    }

    /// An iterator over this key's valid metadata values.
    fn iterator(self, metadata: &[u8]) -> ValueIterator<'_> {
        ValueIterator::init(self.key(), Some(VALID_METADATA_VALUE_CHARACTERS), metadata)
    }
}

/// The value type of a [`NotificationOption`] (Zig: `Option.Type()`), its
/// default, and how to read it.
pub trait OptionType {
    const OPTION: NotificationOption;
    type Type<'a>;

    /// The protocol default (Zig: `Option.default`).
    fn default_value<'a>() -> Self::Type<'a>;

    /// Parse a valid metadata value.
    fn convert(value: &[u8]) -> Self::Type<'_>;

    /// Read the option value from the raw metadata string.
    ///
    /// Unknown and malformed values are ignored. Optional values return null;
    /// all other values return the protocol default.
    fn read(metadata: &[u8]) -> Self::Type<'_> {
        let mut it = Self::OPTION.iterator(metadata);
        match it.next() {
            // the iterator guarantees that it's a valid metadata value
            Some(value) => Self::convert(value),
            None => Self::default_value(),
        }
    }
}

/// Marker types for each [`NotificationOption`].
pub mod options {
    use super::*;

    pub struct A;
    impl OptionType for A {
        const OPTION: NotificationOption = NotificationOption::A;
        type Type<'a> = Action;
        fn default_value<'a>() -> Self::Type<'a> {
            Action::DEFAULT
        }
        fn convert(value: &[u8]) -> Action {
            Action::init(value)
        }
    }

    macro_rules! bool_option {
        ($($name:ident => $default:expr),*) => {$(
            pub struct $name;
            impl OptionType for $name {
                const OPTION: NotificationOption = NotificationOption::$name;
                type Type<'a> = bool;
                fn default_value<'a>() -> Self::Type<'a> {
                    $default
                }
                fn convert(value: &[u8]) -> bool {
                    parse_bool(value).unwrap_or($default)
                }
            }
        )*};
    }

    bool_option!(C => false, D => true, E => false);

    pub struct F;
    impl OptionType for F {
        const OPTION: NotificationOption = NotificationOption::F;
        type Type<'a> = Option<&'a [u8]>;
        fn default_value<'a>() -> Self::Type<'a> {
            None
        }
        fn convert(value: &[u8]) -> Option<&[u8]> {
            Some(value)
        }
    }

    macro_rules! identifier_option {
        ($($name:ident),*) => {$(
            pub struct $name;
            impl OptionType for $name {
                const OPTION: NotificationOption = NotificationOption::$name;
                type Type<'a> = Option<&'a [u8]>;
                fn default_value<'a>() -> Self::Type<'a> {
                    None
                }
                fn convert(value: &[u8]) -> Option<&[u8]> {
                    parse_identifier(value)
                }
            }
        )*};
    }

    identifier_option!(G, I);

    macro_rules! iterator_option {
        ($($name:ident),*) => {$(
            pub struct $name;
            impl OptionType for $name {
                const OPTION: NotificationOption = NotificationOption::$name;
                type Type<'a> = ValueIterator<'a>;
                fn default_value<'a>() -> Self::Type<'a> {
                    unreachable!()
                }
                fn convert(_: &[u8]) -> ValueIterator<'_> {
                    unreachable!()
                }
                fn read(metadata: &[u8]) -> ValueIterator<'_> {
                    Self::OPTION.iterator(metadata)
                }
            }
        )*};
    }

    iterator_option!(N, T);

    pub struct O;
    impl OptionType for O {
        const OPTION: NotificationOption = NotificationOption::O;
        type Type<'a> = Occasion;
        fn default_value<'a>() -> Self::Type<'a> {
            Occasion::DEFAULT
        }
        fn convert(value: &[u8]) -> Occasion {
            Occasion::init(value)
        }
    }

    pub struct P;
    impl OptionType for P {
        const OPTION: NotificationOption = NotificationOption::P;
        type Type<'a> = Payload;
        fn default_value<'a>() -> Self::Type<'a> {
            Payload::DEFAULT
        }
        fn convert(value: &[u8]) -> Payload {
            Payload::init(value)
        }
    }

    pub struct S;
    impl OptionType for S {
        const OPTION: NotificationOption = NotificationOption::S;
        type Type<'a> = &'a [u8];
        fn default_value<'a>() -> Self::Type<'a> {
            b"system"
        }
        fn convert(value: &[u8]) -> &[u8] {
            value
        }
    }

    pub struct U;
    impl OptionType for U {
        const OPTION: NotificationOption = NotificationOption::U;
        type Type<'a> = Urgency;
        fn default_value<'a>() -> Self::Type<'a> {
            Urgency::DEFAULT
        }
        fn convert(value: &[u8]) -> Urgency {
            Urgency::init(value)
        }
    }

    pub struct W;
    impl OptionType for W {
        const OPTION: NotificationOption = NotificationOption::W;
        type Type<'a> = i32;
        fn default_value<'a>() -> Self::Type<'a> {
            -1
        }
        fn convert(value: &[u8]) -> i32 {
            let Ok(tmp) = parse_int::<i32>(value, 10) else {
                return Self::default_value();
            };
            // negative values less than -1 are not allowed
            if tmp < -1 {
                return Self::default_value();
            }
            tmp
        }
    }
}

/// Parse the protocol's booleans
fn parse_bool(s: &[u8]) -> Option<bool> {
    if s.len() != 1 {
        return None;
    }
    match s[0] {
        b'0' => Some(false),
        b'1' => Some(true),
        _ => None,
    }
}

/// This is similar to the packed struct parser used in the configs. The
/// differences are that a literal `true` or `false` value does not turn on/off
/// all the values, and the negation prefix is `-` not `no-`.
fn parse_packed_struct(s: &[u8]) -> Action {
    let mut result = Action::DEFAULT;

    // We split each value by ","
    for raw in s.split(|&c| c == b',') {
        // Determine the field we're looking for and the value. If the
        // field is prefixed with "-" then we set the value to false.
        let (part, value) = {
            let negation_prefix = b"-";
            let trimmed = trim(raw, ASCII_WHITESPACE);
            if trimmed.starts_with(negation_prefix) {
                (&trimmed[negation_prefix.len()..], false)
            } else {
                (trimmed, true)
            }
        };

        match part {
            b"focus" => result.focus = value,
            b"report" => result.report = value,
            // No field matched
            _ => return Action::DEFAULT,
        }
    }

    result
}

/// Characters that are valid in a metadata value. Including `=` is technically
/// against the spec but is needed since Base64 encoded values (with padding)
/// are valid for some options. Including `?` is technically against the spec
/// but is needed since it is a valid value for the `p` option.
const VALID_METADATA_VALUE_CHARACTERS: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_+./,(){}[]*&^%$#@!`~=?";

/// Characters that are valid in identifiers.
const VALID_IDENTIFIER_CHARACTERS: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_+.";

fn is_valid_identifier(s: &[u8]) -> bool {
    s.iter().all(|c| VALID_IDENTIFIER_CHARACTERS.contains(c))
}

fn parse_identifier(s: &[u8]) -> Option<&[u8]> {
    if is_valid_identifier(s) {
        return Some(s);
    }
    None
}

pub fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    debug_assert!(parser.state == State::Osc99);

    let Some(cap) = parser.capture.as_ref() else {
        parser.state = State::Invalid;
        return None;
    };

    let data = cap.trailing();

    let Some(payload_start) = data.iter().position(|&c| c == b';') else {
        log::warn!(target: LOG_TARGET, "missing semicolon before payload");
        parser.state = State::Invalid;
        return None;
    };

    let metadata = &data[..payload_start];
    let payload = &data[payload_start + 1..];

    let max_payload_bytes: usize = if options::E::read(metadata) {
        MAX_ENCODED_PAYLOAD_BYTES
    } else {
        MAX_PLAIN_PAYLOAD_BYTES
    };
    if payload.len() > max_payload_bytes {
        log::warn!(
            target: LOG_TARGET,
            "payload is too large: size={} max={}",
            payload.len(),
            max_payload_bytes
        );
        parser.state = State::Invalid;
        return None;
    }

    // Payload has to be an escape-code-safe UTF-8 string.
    if !encoding::is_safe_utf8(payload) {
        log::warn!(target: LOG_TARGET, "payload is not escape code safe UTF-8");
        parser.state = State::Invalid;
        return None;
    }

    Some(Command::KittyDesktopNotification(Osc {
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

    fn run(input: &[u8], f: impl FnOnce(Osc<'_>)) {
        let mut p = Parser::init(None);
        for &ch in input {
            p.next(ch);
        }
        match p.end(Some(0x1b)) {
            Some(Command::KittyDesktopNotification(c)) => f(c),
            other => panic!("expected kitty_desktop_notification, got {:?}", other),
        }
    }

    fn check_defaults(c: &Osc<'_>) {
        assert_eq!(o::A::default_value(), c.read_option::<o::A>());
        assert!(!c.read_option::<o::C>());
        assert!(c.read_option::<o::D>());
        assert!(!c.read_option::<o::E>());
        assert!(c.read_option::<o::F>().is_none());
        assert!(c.read_option::<o::G>().is_none());
        assert!(c.read_option::<o::I>().is_none());
        {
            let mut it = c.read_option::<o::N>();
            assert!(it.next().is_none());
        }
        assert_eq!(Occasion::Always, c.read_option::<o::O>());
        assert_eq!(Payload::Title, c.read_option::<o::P>());
        assert_eq!(b"system", c.read_option::<o::S>());
        {
            let mut it = c.read_option::<o::T>();
            assert!(it.next().is_none());
        }
        assert_eq!(Urgency::Normal, c.read_option::<o::U>());
        assert_eq!(-1, c.read_option::<o::W>());
    }

    #[test]
    fn osc_99_empty_metadata_and_payload() {
        run(b"99;;", |c| {
            assert_eq!(b"", c.metadata);
            assert_eq!(b"", c.payload);
            check_defaults(&c);
            assert_eq!(Terminator::St, c.terminator);
        });
    }

    #[test]
    fn osc_99_empty_metadata_with_payload() {
        run(b"99;;bobr", |c| {
            assert_eq!(b"", c.metadata);
            assert_eq!(b"bobr", c.payload);
            check_defaults(&c);
        });
    }

    #[test]
    fn osc_99_payload_size_limits() {
        struct Case {
            metadata: &'static [u8],
            payload_size: usize,
            valid: bool,
        }
        let cases = [
            Case {
                metadata: b"",
                payload_size: MAX_PLAIN_PAYLOAD_BYTES,
                valid: true,
            },
            Case {
                metadata: b"",
                payload_size: MAX_PLAIN_PAYLOAD_BYTES + 1,
                valid: false,
            },
            Case {
                metadata: b"e=1",
                payload_size: MAX_ENCODED_PAYLOAD_BYTES,
                valid: true,
            },
            Case {
                metadata: b"e=1",
                payload_size: MAX_ENCODED_PAYLOAD_BYTES + 1,
                valid: false,
            },
        ];

        for case in cases {
            let mut p = Parser::init(Some(Allocator::GLOBAL));

            for &ch in b"99;" {
                p.next(ch);
            }
            for &ch in case.metadata {
                p.next(ch);
            }
            p.next(b';');
            for _ in 0..case.payload_size {
                p.next(b'a');
            }

            assert_eq!(case.valid, p.end(Some(0x1b)).is_some());
        }
    }

    #[test]
    fn osc_99_unknown_prefix_does_not_hide_dotted_identifier() {
        run(b"99;invalid=wrong:i=org.ghostty;payload", |c| {
            assert_eq!(b"org.ghostty", c.read_option::<o::I>().unwrap());
        });
    }

    #[test]
    fn osc_99_single_parameter_i() {
        run(b"99;i=bobr;payload", |c| {
            assert_eq!(b"bobr", c.read_option::<o::I>().unwrap());
            assert_eq!(b"payload", c.payload);
        });
    }

    #[test]
    fn osc_99_repeated_parameter_i() {
        run(b"99;i=bobr:i=foobar;payload", |c| {
            assert_eq!(b"bobr", c.read_option::<o::I>().unwrap());
            assert_eq!(b"payload", c.payload);
        });
    }

    #[test]
    fn osc_99_multiple_types() {
        run(b"99;t=mail: t = chat : t = alert ;notification", |c| {
            assert_eq!(b"notification", c.payload);
            let mut it = c.read_option::<o::T>();
            assert_eq!(b"mail", it.next().unwrap());
            assert_eq!(b"chat", it.next().unwrap());
            assert_eq!(b"alert", it.next().unwrap());
            assert!(it.next().is_none());
        });
    }

    fn check_a(input: &[u8], expected: Action) {
        run(input, |c| {
            assert_eq!(b"foobar", c.payload);
            assert_eq!(expected, c.read_option::<o::A>());
        });
    }

    #[test]
    fn osc_99_a_1() {
        check_a(
            b"99;a=report,focus;foobar",
            Action {
                report: true,
                focus: true,
            },
        );
    }

    #[test]
    fn osc_99_a_2() {
        check_a(
            b"99;a=report,-focus;foobar",
            Action {
                report: true,
                focus: false,
            },
        );
    }

    #[test]
    fn osc_99_a_3() {
        check_a(
            b"99;a=-report,focus;foobar",
            Action {
                report: false,
                focus: true,
            },
        );
    }

    #[test]
    fn osc_99_a_4() {
        check_a(
            b"99;a=-report,-focus;foobar",
            Action {
                report: false,
                focus: false,
            },
        );
    }

    fn check<F: FnOnce(&Osc<'_>)>(input: &[u8], f: F) {
        run(input, |c| {
            assert_eq!(b"foobar", c.payload);
            f(&c);
        });
    }

    #[test]
    fn osc_99_c_1() {
        check(b"99;c=0;foobar", |c| assert!(!c.read_option::<o::C>()));
    }

    #[test]
    fn osc_99_c_2() {
        check(b"99;c=1;foobar", |c| assert!(c.read_option::<o::C>()));
    }

    #[test]
    fn osc_99_c_3() {
        check(b"99;c=bobr;foobar", |c| assert!(!c.read_option::<o::C>()));
    }

    #[test]
    fn osc_99_d_1() {
        check(b"99;d=0;foobar", |c| assert!(!c.read_option::<o::D>()));
    }

    #[test]
    fn osc_99_d_2() {
        check(b"99;d=1;foobar", |c| assert!(c.read_option::<o::D>()));
    }

    #[test]
    fn osc_99_d_3() {
        check(b"99;d=bobr;foobar", |c| assert!(c.read_option::<o::D>()));
    }

    #[test]
    fn osc_99_e_1() {
        check(b"99;e=0;foobar", |c| assert!(!c.read_option::<o::E>()));
    }

    #[test]
    fn osc_99_e_2() {
        check(b"99;e=1;foobar", |c| assert!(c.read_option::<o::E>()));
    }

    #[test]
    fn osc_99_e_3() {
        check(b"99;e=bobr;foobar", |c| assert!(!c.read_option::<o::E>()));
    }

    #[test]
    fn osc_99_f_1() {
        check(b"99;f=R2hvc3R0eQ==;foobar", |c| {
            assert_eq!(b"R2hvc3R0eQ==", c.read_option::<o::F>().unwrap())
        });
    }

    #[test]
    fn osc_99_f_2() {
        check(b"99;c=0:f= R2hvc3R0eQ== ;foobar", |c| {
            assert_eq!(b"R2hvc3R0eQ==", c.read_option::<o::F>().unwrap())
        });
    }

    #[test]
    fn osc_99_g_1() {
        check(b"99;c=0:g=7f8a9129-a35d-4e9f-8043-ce2700e15e2c;foobar", |c| {
            assert_eq!(
                b"7f8a9129-a35d-4e9f-8043-ce2700e15e2c",
                c.read_option::<o::G>().unwrap()
            )
        });
    }

    #[test]
    fn osc_99_g_2() {
        check(b"99;c=0:g=aaa*bbb;foobar", |c| {
            assert!(c.read_option::<o::G>().is_none())
        });
    }

    #[test]
    fn osc_99_i_1() {
        check(b"99;;foobar", |c| assert!(c.read_option::<o::I>().is_none()));
    }

    #[test]
    fn osc_99_i_2() {
        check(b"99;i=bobr;foobar", |c| {
            assert_eq!(b"bobr", c.read_option::<o::I>().unwrap())
        });
    }

    #[test]
    fn osc_99_i_3() {
        check(b"99;i=;foobar", |c| {
            assert_eq!(b"", c.read_option::<o::I>().unwrap())
        });
    }

    #[test]
    fn osc_99_i_4() {
        check(b"99;i= :;foobar", |c| {
            assert_eq!(b"", c.read_option::<o::I>().unwrap())
        });
    }

    #[test]
    fn osc_99_i_5() {
        check(b"99;i= bobr ;foobar", |c| {
            assert_eq!(b"bobr", c.read_option::<o::I>().unwrap())
        });
    }

    #[test]
    fn osc_99_i_6() {
        check(b"99;i= bobr : i=kurwa ;foobar", |c| {
            assert_eq!(b"bobr", c.read_option::<o::I>().unwrap())
        });
    }

    #[test]
    fn osc_99_n_1() {
        check(b"99;n=R2hvc3R0eQ==;foobar", |c| {
            let mut it = c.read_option::<o::N>();
            assert_eq!(b"R2hvc3R0eQ==", it.next().unwrap());
            assert!(it.next().is_none());
        });
    }

    #[test]
    fn osc_99_n_2() {
        check(b"99;n=R2hvc3R0eQ==:n=R2hvc3R0eQ==;foobar", |c| {
            let mut it = c.read_option::<o::N>();
            assert_eq!(b"R2hvc3R0eQ==", it.next().unwrap());
            assert_eq!(b"R2hvc3R0eQ==", it.next().unwrap());
            assert!(it.next().is_none());
        });
    }

    #[test]
    fn osc_99_o_1() {
        check(b"99;o= ;foobar", |c| {
            assert_eq!(Occasion::Always, c.read_option::<o::O>())
        });
    }

    #[test]
    fn osc_99_o_2() {
        check(b"99;o=always;foobar", |c| {
            assert_eq!(Occasion::Always, c.read_option::<o::O>())
        });
    }

    #[test]
    fn osc_99_o_3() {
        check(b"99;o=unfocused;foobar", |c| {
            assert_eq!(Occasion::Unfocused, c.read_option::<o::O>())
        });
    }

    #[test]
    fn osc_99_o_4() {
        check(b"99;o=invisible;foobar", |c| {
            assert_eq!(Occasion::Invisible, c.read_option::<o::O>())
        });
    }

    #[test]
    fn osc_99_o_5() {
        check(b"99;o=bobr;foobar", |c| {
            assert_eq!(Occasion::Always, c.read_option::<o::O>())
        });
    }

    #[test]
    fn osc_99_p() {
        for (input, expected) in [
            (&b"99;p=alive;foobar"[..], Payload::Alive),
            (b"99;p=body;foobar", Payload::Body),
            (b"99;p=buttons;foobar", Payload::Buttons),
            (b"99;p=close;foobar", Payload::Close),
            (b"99;p=icon;foobar", Payload::Icon),
            (b"99;p=?;foobar", Payload::Query),
            (b"99;p=title;foobar", Payload::Title),
            (b"99;p=query;foobar", Payload::Unknown),
            (b"99;p=bobr;foobar", Payload::Unknown),
        ] {
            check(input, |c| assert_eq!(expected, c.read_option::<o::P>()));
        }
    }

    #[test]
    fn osc_99_s_1() {
        check(b"99;s=R2hvc3R0eQ==;foobar", |c| {
            assert_eq!(b"R2hvc3R0eQ==", c.read_option::<o::S>())
        });
    }

    #[test]
    fn osc_99_t_1() {
        check(b"99;t=R2hvc3R0eQ==;foobar", |c| {
            let mut it = c.read_option::<o::T>();
            assert_eq!(b"R2hvc3R0eQ==", it.next().unwrap());
            assert!(it.next().is_none());
        });
    }

    #[test]
    fn osc_99_t_2() {
        check(b"99;t=R2hvc3R0eQ==:t=R2hvc3R0eQ==;foobar", |c| {
            let mut it = c.read_option::<o::T>();
            assert_eq!(b"R2hvc3R0eQ==", it.next().unwrap());
            assert_eq!(b"R2hvc3R0eQ==", it.next().unwrap());
            assert!(it.next().is_none());
        });
    }

    #[test]
    fn osc_99_u() {
        for (input, expected) in [
            (&b"99;u=0;foobar"[..], Urgency::Low),
            (b"99;u=1;foobar", Urgency::Normal),
            (b"99;u=2;foobar", Urgency::High),
            (b"99;u=bobr;foobar", Urgency::Normal),
        ] {
            check(input, |c| assert_eq!(expected, c.read_option::<o::U>()));
        }
    }

    #[test]
    fn osc_99_w() {
        for (input, expected) in [
            (&b"99;w=0;foobar"[..], 0),
            (b"99;w=-1;foobar", -1),
            (b"99;w=-42;foobar", -1),
            (b"99;w=4294967296;foobar", -1),
            (b"99;w=4_2;foobar", -1),
        ] {
            check(input, |c| assert_eq!(expected, c.read_option::<o::W>()));
        }
    }
}
