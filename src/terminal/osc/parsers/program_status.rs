//! Port of `src/terminal/osc/parsers/program_status.zig`.
//!
//! STATUS: ported
//!
//! OSC 7501: Program Status Protocol
//!
//! Specification: https://www.superlogical.com/rex/docs/build/program-status
//!
//! A program uses this protocol to tell the terminal what it is doing:
//! idle, working, done, waiting on the user, or failed. Each report is a
//! list of `key=value` pairs separated by colons:
//!
//! ```text
//! ESC ] 7501 ; state=working:app=cargo:progress=40 ESC \
//! ```
//!
//! A terminal keeps one record per id, and each report replaces its
//! record completely. This parser only reads and validates one report at
//! a time. Keeping records is up to the code that handles the command.
//! In libghostty-vt, that is the `program_status` effect of
//! `stream_terminal.Handler`.
//!
//! A program can check whether the terminal supports the protocol by
//! sending `?` as the body. A terminal that supports it replies with `?`
//! and may list the states and kinds it accepts. See `Command::Query` and
//! `QUERY_REPLY`. Checking is optional, so a program may also send
//! reports without asking first.
//!
//! Example:
//!
//! ```ignore
//! let mut p = osc::Parser::init(Some(osc::Allocator::GLOBAL));
//!
//! p.next_slice(b"7501;state=working:id=sync:progress=40");
//! let Some(osc::Command::ProgramStatus(Command::Report(report))) = p.end(Some(0x1b));
//! report.state; // State::Working
//! report.read_option::<options::Id>(); // "sync"
//! report.read_option::<options::Progress>(); // 40
//! report.read_option::<options::App>(); // None, the program didn't send one
//! ```
//!
//! Rust notes: Zig's `Option` enum is named [`ReportOption`] here (to not
//! shadow `core::option::Option`). Zig's `readOption(comptime option)`
//! returns an option-dependent type; here each option has a marker type
//! in [`options`] implementing [`OptionType`].

use crate::terminal::osc::encoding::{self, base64};
use crate::terminal::osc::kitty_metadata::ValueIterator;
use crate::terminal::osc::{Command as OscCommand, Parser, State as ParserState, Terminator};

const LOG_TARGET: &str = "osc_program_status";

// Size limits from the specification. Every limit counts bytes, not
// characters. A report that breaks any limit is discarded as a whole.

/// The longest whole sequence, from `ESC ]` through the terminator.
pub const MAX_SEQUENCE_BYTES: usize = 4096;

/// The longest body, everything after `7501;`. This assumes the
/// longer terminator (`ESC \`), so a sequence ended with BEL is limited
/// to one byte less than the specification allows. The specification
/// lets terminals choose lower limits.
pub const MAX_BODY_BYTES: usize = MAX_SEQUENCE_BYTES - b"\x1b]7501;".len() - b"\x1b\\".len();

/// The longest key, including keys this parser doesn't know.
pub const MAX_KEY_BYTES: usize = 16;

/// The longest `msg` value before and after base64 decoding.
pub const MAX_MSG_ENCODED_BYTES: usize = 2732;
pub const MAX_MSG_BYTES: usize = 2048;

/// The longest `title` value before and after base64 decoding.
pub const MAX_TITLE_ENCODED_BYTES: usize = 256;
pub const MAX_TITLE_BYTES: usize = 192;

/// The longest `app` value.
pub const MAX_APP_BYTES: usize = 32;

/// The longest `id` value, the longest part between two slashes, and the
/// most parts an id can have.
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_ID_SEGMENT_BYTES: usize = 32;
pub const MAX_ID_DEPTH: usize = 8;

/// The body of the reply to a support query.
///
/// A program asks whether the terminal supports the protocol by sending
/// `?` as the body. A terminal that does answers with another OSC 7501
/// whose body is this value, ended with the same terminator the query
/// used:
///
/// ```text
/// program:  ESC ] 7501 ; ? ESC \
/// terminal: ESC ] 7501 ; ?:states=idle,working,done,blocked,error:kinds=permission,question,auth ESC \
/// ```
///
/// The specification only requires the `?`. The rest lists the states
/// and kinds this parser accepts. `clear` is left out because it isn't a
/// state. A program that wants a state or kind from a later revision of
/// the specification checks these lists and falls back to one that is
/// present. The lists come from `State` and `Kind` (Zig builds this at
/// comptime from the enums; keep it in sync with them, which the tests
/// check).
pub const QUERY_REPLY: &str = "?:states=idle,working,done,blocked,error:kinds=permission,question,auth";

/// The names of the values of `E`, comma separated, leaving out `skip`.
fn names<E: Copy + PartialEq>(all: &[E], name: fn(E) -> &'static str, skip: &[E]) -> String {
    let mut result = String::new();
    for &value in all {
        if skip.contains(&value) {
            continue;
        }
        if !result.is_empty() {
            result.push(',');
        }
        result.push_str(name(value));
    }
    result
}

/// A parsed OSC 7501 sequence: either a support query or a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command<'a> {
    /// The program asked whether the terminal supports the protocol by
    /// sending `OSC 7501 ; ? ST`. A terminal that supports it answers
    /// with `QUERY_REPLY`, ended with this terminator so the program
    /// recognizes it. A program that gets no reply treats the protocol as
    /// unsupported. Asking is optional, so reports may arrive without a
    /// query first.
    Query(Terminator),

    /// A valid status report for one record.
    Report(Report<'a>),
}

/// The C representation of [`Command`] (unsupported).
pub type CommandC = ();

/// The value of the required `state` key.
///
/// - `idle`: at rest, waiting for the user's next instruction.
/// - `working`: running. May carry a progress percentage.
/// - `done`: finished a piece of work the user hasn't looked at yet.
/// - `blocked`: can't continue until the user does something. The
///   report's `kind` says what. May carry a progress percentage.
/// - `error`: failed and stopped.
/// - `clear`: not a real state. Removes the addressed record and every
///   record beneath it. Without an id, it removes every record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    Clear,
}

impl State {
    pub const ALL: [State; 6] = [
        State::Idle,
        State::Working,
        State::Done,
        State::Blocked,
        State::Error,
        State::Clear,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Working => "working",
            State::Done => "done",
            State::Blocked => "blocked",
            State::Error => "error",
            State::Clear => "clear",
        }
    }

    /// Zig `std.meta.stringToEnum`.
    pub fn from_str(s: &[u8]) -> Option<State> {
        Self::ALL.iter().copied().find(|v| v.as_str().as_bytes() == s)
    }
}

/// The value of the `kind` key, which says what a blocked program needs
/// from the user.
///
/// - `permission`: approval to do something.
/// - `question`: an answer the user has to type.
/// - `auth`: a login, token, or other credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum Kind {
    Permission,
    Question,
    Auth,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Permission, Kind::Question, Kind::Auth];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Permission => "permission",
            Kind::Question => "question",
            Kind::Auth => "auth",
        }
    }

    /// Zig `std.meta.stringToEnum`.
    pub fn from_str(s: &[u8]) -> Option<Kind> {
        Self::ALL.iter().copied().find(|v| v.as_str().as_bytes() == s)
    }
}

/// A status report that passed every check in the specification.
///
/// `state` is always present. Read everything else with `read_option`,
/// and the text values with `write_text`.
///
/// The values point into the parser's buffer, so they are only valid
/// until the next call to the parser. Copy anything you want to keep.
///
/// A key the program didn't send reads as null. A report replaces its
/// record completely, so a null value means the record no longer has
/// that value, not that the old value should be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report<'a> {
    /// What the program is doing.
    pub state: State,

    /// Everything after `7501;`, already validated by the parser. Read
    /// it with `read_option` and `write_text`.
    pub data: &'a [u8],
}

impl<'a> Report<'a> {
    /// Read the value of a key, or null if the program didn't send it.
    /// If the program sent a key more than once, the last value wins.
    pub fn read_option<O: OptionType>(&self) -> Option<O::Type<'a>> {
        let v = last_value(O::OPTION.key(), self.data)?;
        O::convert(self.state, v)
    }

    /// Write the decoded text of `title` or `msg` to `writer`. Writes
    /// nothing if the program didn't send it.
    ///
    /// The text is valid UTF-8 with no control characters, but it is
    /// still untrusted text from the program. It is at most
    /// `MAX_TITLE_BYTES` or `MAX_MSG_BYTES` long.
    pub fn write_text<O, W>(&self, writer: &mut W) -> std::io::Result<()>
    where
        O: OptionType + for<'x> OptionType<Type<'x> = &'x [u8]>,
        W: std::io::Write + ?Sized,
    {
        assert!(O::OPTION == ReportOption::Title || O::OPTION == ReportOption::Msg);
        let Some(encoded) = self.read_option::<O>() else {
            return Ok(());
        };

        // The parser already checked that this decodes, so it can't fail.
        let mut buf = [0u8; MAX_MSG_BYTES];
        writer.write_all(decode_text(encoded, &mut buf).expect("validated by the parser"))
    }
}

/// The keys of a report, other than `state` (Zig: `Option`).
///
/// - `id`: the record this report is about. Without it, the report is
///   about the root record, the program itself. A program that reports on
///   several things at once, like a deploy to several regions, gives each
///   one an id. A `/` makes one record the child of another, so
///   `deploy/us-east` is a child of `deploy`. The parent doesn't have to
///   exist.
/// - `kind`: what a blocked program needs from the user. Only read when
///   the state is `blocked`. An unknown kind reads as null.
/// - `progress`: how far along the work is, from 0 to 100. Only read when
///   the state is `working` or `blocked`. Null means indeterminate: the
///   program is busy but has no percentage to report. A value outside
///   that range reads as null too.
/// - `app`: a stable name for the program that a machine can match on,
///   such as `cargo`, `terraform`, or `claude-code`. It is a key for
///   grouping, filtering, choosing an icon, and so on, not a label. The
///   label is `title`. A record without `app` takes it from its nearest
///   ancestor that has one.
/// - `title`: a short label for the record, meant for people. Programs
///   that report several records use this to tell them apart.
/// - `msg`: one line of text for people, saying what the record is
///   doing, waiting for, or has finished. Don't try to read meaning into
///   it.
///
/// `read_option` returns `title` and `msg` still encoded as base64. Use
/// `write_text` to read their text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportOption {
    Id,
    Kind,
    Progress,
    App,
    Title,
    Msg,
}

impl ReportOption {
    /// The Zig tag name, which is the key.
    pub fn key(self) -> &'static [u8] {
        match self {
            ReportOption::Id => b"id",
            ReportOption::Kind => b"kind",
            ReportOption::Progress => b"progress",
            ReportOption::App => b"app",
            ReportOption::Title => b"title",
            ReportOption::Msg => b"msg",
        }
    }
}

/// The value type of a [`ReportOption`] (Zig: `Option.Type()`) and how to
/// read it from a report.
pub trait OptionType {
    const OPTION: ReportOption;
    type Type<'a>;
    fn convert(state: State, value: &[u8]) -> Option<Self::Type<'_>>;
}

/// Marker types for each [`ReportOption`].
pub mod options {
    use super::*;

    pub struct Id;
    impl OptionType for Id {
        const OPTION: ReportOption = ReportOption::Id;
        type Type<'a> = &'a [u8];
        fn convert(_: State, v: &[u8]) -> Option<&[u8]> {
            // The parser already discarded reports with an invalid id.
            Some(v)
        }
    }

    pub struct App;
    impl OptionType for App {
        const OPTION: ReportOption = ReportOption::App;
        type Type<'a> = &'a [u8];
        fn convert(_: State, v: &[u8]) -> Option<&[u8]> {
            if is_name(v) {
                Some(v)
            } else {
                None
            }
        }
    }

    pub struct Kind;
    impl OptionType for Kind {
        const OPTION: ReportOption = ReportOption::Kind;
        type Type<'a> = super::Kind;
        fn convert(state: State, v: &[u8]) -> Option<super::Kind> {
            if state == State::Blocked {
                super::Kind::from_str(v)
            } else {
                None
            }
        }
    }

    pub struct Progress;
    impl OptionType for Progress {
        const OPTION: ReportOption = ReportOption::Progress;
        type Type<'a> = u8;
        fn convert(state: State, v: &[u8]) -> Option<u8> {
            match state {
                State::Working | State::Blocked => parse_progress(v),
                _ => None,
            }
        }
    }

    pub struct Title;
    impl OptionType for Title {
        const OPTION: ReportOption = ReportOption::Title;
        type Type<'a> = &'a [u8];
        fn convert(_: State, v: &[u8]) -> Option<&[u8]> {
            if !v.is_empty() {
                Some(v)
            } else {
                None
            }
        }
    }

    pub struct Msg;
    impl OptionType for Msg {
        const OPTION: ReportOption = ReportOption::Msg;
        type Type<'a> = &'a [u8];
        fn convert(_: State, v: &[u8]) -> Option<&[u8]> {
            if !v.is_empty() {
                Some(v)
            } else {
                None
            }
        }
    }
}

/// Errors from [`validate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidateError {
    InvalidState,
    InvalidId,
    InvalidText,
    TooLong,
}

/// Parse OSC 7501. The body is everything after `7501;`.
///
/// The parser is forgiving about single pairs and strict about the
/// report as a whole, as the specification requires. It skips a pair
/// that is malformed or has a key it doesn't know, and keeps going:
///
/// ```text
/// state=idle:future=yes:oops   ->  a report with state idle
/// ```
///
/// It returns null, so nothing from the report is applied, when:
///
/// - `state` is missing or isn't a state this parser knows.
/// - Any key or value is longer than its limit, even one that a later
///   pair with the same key replaces.
/// - The id isn't made of valid names joined by single slashes. This
///   throws out the report instead of skipping the pair, so a report
///   meant for one record never falls back to the root record. For
///   `state=clear`, that would remove every record.
/// - `title` or `msg` isn't valid base64, or decodes to text that isn't
///   UTF-8 or contains a control character.
pub fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<OscCommand<'_>> {
    debug_assert!(parser.state == ParserState::Osc7501);

    let Some(cap) = parser.capture.as_ref() else {
        parser.state = ParserState::Invalid;
        return None;
    };

    let data = cap.trailing();
    if data.len() > MAX_BODY_BYTES {
        log::warn!(target: LOG_TARGET, "OSC 7501 sequence too long len={}", data.len());
        parser.state = ParserState::Invalid;
        return None;
    }

    if data == b"?" {
        return Some(OscCommand::ProgramStatus(Command::Query(Terminator::init(
            terminator_ch,
        ))));
    }

    let state = match validate(data) {
        Ok(state) => state,
        Err(err) => {
            log::warn!(target: LOG_TARGET, "invalid OSC 7501 report err={:?}", err);
            parser.state = ParserState::Invalid;
            return None;
        }
    };

    Some(OscCommand::ProgramStatus(Command::Report(Report { state, data })))
}

/// Check a report body against the specification and return its state.
fn validate(data: &[u8]) -> Result<State, ValidateError> {
    // A key that is too long breaks a limit, even a key we don't know.
    for pair in data.split(|&c| c == b':') {
        let Some(eq) = pair.iter().position(|&c| c == b'=') else {
            continue;
        };
        let key = crate::terminal::color::trim(
            &pair[..eq],
            crate::terminal::osc::kitty_metadata::ASCII_WHITESPACE,
        );
        if key.len() > MAX_KEY_BYTES {
            return Err(ValidateError::TooLong);
        }
    }

    // Every value is checked against its limit, even one a later pair
    // replaces, because any limit violation discards the whole report.
    // These iterate the raw values without filtering by the value
    // alphabet. Filtering would skip an id with a disallowed byte, so the
    // report would look like it had no id and fall back to the root record.
    let mut ids = ValueIterator::init(b"id", None, data);
    while let Some(v) = ids.next() {
        validate_id(v)?;
    }
    for (key, limit) in [
        (&b"app"[..], MAX_APP_BYTES),
        (b"title", MAX_TITLE_ENCODED_BYTES),
        (b"msg", MAX_MSG_ENCODED_BYTES),
    ] {
        let mut it = ValueIterator::init(key, None, data);
        while let Some(v) = it.next() {
            if v.len() > limit {
                return Err(ValidateError::TooLong);
            }
        }
    }

    let mut buf = [0u8; MAX_MSG_BYTES];
    if let Some(v) = last_value(b"title", data) {
        if decode_text(v, &mut buf)?.len() > MAX_TITLE_BYTES {
            return Err(ValidateError::TooLong);
        }
    }
    if let Some(v) = last_value(b"msg", data) {
        decode_text(v, &mut buf)?;
    }

    // An unknown state discards the report rather than guessing, so a
    // state added in a later revision never turns into something else.
    State::from_str(last_value(b"state", data).ok_or(ValidateError::InvalidState)?)
        .ok_or(ValidateError::InvalidState)
}

/// The last value of `key` in a report body. Pairs without an `=` and
/// values with bytes outside the value alphabet are skipped.
fn last_value<'a>(key: &'static [u8], data: &'a [u8]) -> Option<&'a [u8]> {
    let mut it = ValueIterator::init(key, Some(VALUE_BYTES), data);
    let mut last: Option<&[u8]> = None;
    while let Some(v) = it.next() {
        last = Some(v);
    }
    last
}

/// The bytes allowed in a name and a value. A name is the form of an
/// `app` value and of each part of an `id`.
const NAME_BYTES: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.+-";
const VALUE_BYTES: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.+-,/=";

fn is_name(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|c| NAME_BYTES.contains(c))
}

/// Validate an id: names joined by single slashes, within the limits.
fn validate_id(value: &[u8]) -> Result<(), ValidateError> {
    if value.len() > MAX_ID_BYTES {
        return Err(ValidateError::TooLong);
    }
    let mut depth: usize = 0;
    for segment in value.split(|&c| c == b'/') {
        depth += 1;
        if depth > MAX_ID_DEPTH {
            return Err(ValidateError::TooLong);
        }
        if segment.len() > MAX_ID_SEGMENT_BYTES {
            return Err(ValidateError::TooLong);
        }
        if !is_name(segment) {
            return Err(ValidateError::InvalidId);
        }
    }
    Ok(())
}

/// Parse a whole number from 0 to 100. Anything else is treated as
/// absent. This doesn't use a std parser because it accepts underscores.
fn parse_progress(value: &[u8]) -> Option<u8> {
    if value.is_empty() {
        return None;
    }
    let mut n: u16 = 0;
    for &c in value {
        if !c.is_ascii_digit() {
            return None;
        }
        let digit = (c - b'0') as u16;
        n = n * 10 + digit;
        if n > 100 {
            return None;
        }
    }
    Some(n as u8)
}

/// Decode a base64 title or msg into `buf` and check that the text is
/// safe to show. Padding is optional. The caller must have checked the
/// encoded length against `MAX_MSG_ENCODED_BYTES`.
fn decode_text<'b>(
    encoded: &[u8],
    buf: &'b mut [u8; MAX_MSG_BYTES],
) -> Result<&'b [u8], ValidateError> {
    let decoder = if encoded.ends_with(b"=") {
        base64::STANDARD
    } else {
        base64::STANDARD_NO_PAD
    };

    let len = decoder
        .calc_size_for_slice(encoded)
        .map_err(|_| ValidateError::InvalidText)?;
    if len > buf.len() {
        return Err(ValidateError::TooLong);
    }

    let decoded = &mut buf[..len];
    decoder
        .decode(decoded, encoded)
        .map_err(|_| ValidateError::InvalidText)?;
    if !encoding::is_safe_utf8(decoded) {
        return Err(ValidateError::InvalidText);
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::options as o;
    use super::*;
    use crate::terminal::osc::Allocator;

    fn report<'a>(cmd: Option<OscCommand<'a>>) -> Report<'a> {
        match cmd {
            Some(OscCommand::ProgramStatus(Command::Report(r))) => r,
            other => panic!("expected program_status report, got {:?}", other),
        }
    }

    fn text<O>(r: &Report<'_>) -> Vec<u8>
    where
        O: OptionType + for<'x> OptionType<Type<'x> = &'x [u8]>,
    {
        let mut buf: Vec<u8> = Vec::new();
        r.write_text::<O, _>(&mut buf).unwrap();
        buf
    }

    #[test]
    fn osc_7501_query() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        p.next_slice(b"7501;?");
        assert_eq!(
            Some(OscCommand::ProgramStatus(Command::Query(Terminator::St))),
            p.end(Some(0x1b))
        );

        p.reset();
        p.next_slice(b"7501;?");
        assert_eq!(
            Some(OscCommand::ProgramStatus(Command::Query(Terminator::Bel))),
            p.end(Some(0x07))
        );
    }

    #[test]
    fn osc_7501_query_reply_lists_every_state_and_kind() {
        // The specification fixes these lists for the states and kinds it
        // defines. A new state or kind must be added to the specification
        // before it can appear here.
        assert_eq!(
            "?:states=idle,working,done,blocked,error:kinds=permission,question,auth",
            QUERY_REPLY
        );

        // The reply is built from the enums (Zig does this at comptime).
        let built = format!(
            "?:states={}:kinds={}",
            names(&State::ALL, State::as_str, &[State::Clear]),
            names(&Kind::ALL, Kind::as_str, &[])
        );
        assert_eq!(QUERY_REPLY, built);
    }

    #[test]
    fn osc_7501_minimal_report() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        p.next_slice(b"7501;state=idle");
        let r = report(p.end(Some(0x1b)));
        assert_eq!(State::Idle, r.state);
        assert!(r.read_option::<o::Kind>().is_none());
        assert!(r.read_option::<o::Progress>().is_none());
        assert!(r.read_option::<o::Id>().is_none());
        assert!(r.read_option::<o::App>().is_none());
        assert!(r.read_option::<o::Title>().is_none());
        assert!(r.read_option::<o::Msg>().is_none());
    }

    #[test]
    fn osc_7501_all_keys() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        // "Plan" and "Apply 3 to add, 1 to change, 0 to destroy?"
        p.next_slice(
            b"7501;state=blocked:kind=permission:progress=42:id=tf/plan:app=terraform:title=UGxhbg==:msg=QXBwbHkgMyB0byBhZGQsIDEgdG8gY2hhbmdlLCAwIHRvIGRlc3Ryb3k/",
        );
        let r = report(p.end(Some(0x1b)));
        assert_eq!(State::Blocked, r.state);
        assert_eq!(Kind::Permission, r.read_option::<o::Kind>().unwrap());
        assert_eq!(42u8, r.read_option::<o::Progress>().unwrap());
        assert_eq!(b"tf/plan", r.read_option::<o::Id>().unwrap());
        assert_eq!(b"terraform", r.read_option::<o::App>().unwrap());

        assert_eq!(b"Plan".to_vec(), text::<o::Title>(&r));
        assert_eq!(
            b"Apply 3 to add, 1 to change, 0 to destroy?".to_vec(),
            text::<o::Msg>(&r)
        );
    }

    #[test]
    fn osc_7501_base64_padding_is_optional() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        p.next_slice(b"7501;state=done:msg=UGxhbg");
        let r = report(p.end(Some(0x1b)));
        assert_eq!(b"Plan".to_vec(), text::<o::Msg>(&r));
    }

    #[test]
    fn osc_7501_whitespace_around_keys_and_values() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        p.next_slice(b"7501; state = working :\tapp=cargo\t");
        let r = report(p.end(Some(0x1b)));
        assert_eq!(State::Working, r.state);
        assert_eq!(b"cargo", r.read_option::<o::App>().unwrap());
    }

    #[test]
    fn osc_7501_last_value_wins() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        p.next_slice(b"7501;state=idle:app=a:state=done:app=b");
        let r = report(p.end(Some(0x1b)));
        assert_eq!(State::Done, r.state);
        assert_eq!(b"b", r.read_option::<o::App>().unwrap());
    }

    #[test]
    fn osc_7501_malformed_pairs_and_unknown_keys_are_skipped() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        p.next_slice(b"7501;garbage:=x:Bad=1:app=b@d:future=yes:state=working:app=ok:::");
        let r = report(p.end(Some(0x1b)));
        assert_eq!(State::Working, r.state);
        assert_eq!(b"ok", r.read_option::<o::App>().unwrap());
    }

    #[test]
    fn osc_7501_kind_and_progress_only_apply_to_some_states() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        p.next_slice(b"7501;state=working:kind=auth:progress=10");
        {
            let r = report(p.end(Some(0x1b)));
            assert!(r.read_option::<o::Kind>().is_none());
            assert_eq!(10u8, r.read_option::<o::Progress>().unwrap());
        }

        p.reset();
        p.next_slice(b"7501;state=done:kind=auth:progress=10");
        {
            let r = report(p.end(Some(0x1b)));
            assert!(r.read_option::<o::Kind>().is_none());
            assert!(r.read_option::<o::Progress>().is_none());
        }

        p.reset();
        p.next_slice(b"7501;state=blocked:kind=dance");
        {
            let r = report(p.end(Some(0x1b)));
            assert!(r.read_option::<o::Kind>().is_none());
        }
    }

    #[test]
    fn osc_7501_out_of_range_progress_is_absent() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        let cases: [(&[u8], Option<u8>); 11] = [
            (b"0", Some(0)),
            (b"100", Some(100)),
            (b"101", None),
            (b"256", None),
            (b"99999999999999999999", None),
            (b"-1", None),
            (b"+1", None),
            (b"4.5", None),
            (b"1_0", None),
            (b"007", Some(7)),
            (b"", None),
        ];

        for (input, expected) in cases {
            p.reset();
            p.next_slice(b"7501;state=working:progress=");
            p.next_slice(input);
            let r = report(p.end(Some(0x1b)));
            assert_eq!(expected, r.read_option::<o::Progress>());
        }
    }

    #[test]
    fn osc_7501_invalid_app_is_ignored() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        p.next_slice(b"7501;state=idle:app=a/b");
        let r = report(p.end(Some(0x1b)));
        assert!(r.read_option::<o::App>().is_none());
    }

    #[test]
    fn osc_7501_ids() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        let max_segment = [b'a'; MAX_ID_SEGMENT_BYTES];
        let valid: [&[u8]; 5] = [
            b"a",
            b"build/test",
            b"A-Z_0.9+",
            &max_segment,
            b"a/b/c/d/e/f/g/h",
        ];
        for id in valid {
            p.reset();
            p.next_slice(b"7501;state=clear:id=");
            p.next_slice(id);
            let r = report(p.end(Some(0x1b)));
            assert_eq!(id, r.read_option::<o::Id>().unwrap());
        }

        let mut long_segment = max_segment.to_vec();
        long_segment.push(b'a');
        // 128 bytes of "aaa...a/" repeated, then "a".
        let mut longs: Vec<u8> = Vec::new();
        while longs.len() < 128 {
            longs.extend_from_slice(&[b'a'; 31]);
            longs.push(b'/');
        }
        longs.truncate(128);
        longs.push(b'a');

        // An invalid id discards the report so that it can never fall back
        // to the root record.
        let invalid: [&[u8]; 13] = [
            b"",
            b"/",
            b"a/",
            b"/a",
            b"a//b",
            b"a,b",
            b"a=b",
            b"a@b",
            b"a b",
            b"a%20b",
            &long_segment,
            b"a/b/c/d/e/f/g/h/i",
            &longs,
        ];
        for id in invalid {
            for terminator in [0x1bu8, 0x07] {
                p.reset();
                p.next_slice(b"7501;state=clear:id=");
                p.next_slice(id);
                assert!(p.end(Some(terminator)).is_none());
            }

            // An invalid id is never replaced by a later valid one.
            p.reset();
            p.next_slice(b"7501;state=clear:id=");
            p.next_slice(id);
            p.next_slice(b":id=a");
            assert!(p.end(Some(0x1b)).is_none());
        }
    }

    #[test]
    fn osc_7501_limits_apply_to_values_with_disallowed_bytes() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        let long_app = [b'a'; MAX_APP_BYTES];
        p.next_slice(b"7501;state=idle:app=@");
        p.next_slice(&long_app);
        assert!(p.end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_7501_text_must_decode_to_safe_utf8() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        let invalid: [&[u8]; 6] = [
            // Not base64.
            b"7501;state=done:msg=a",
            b"7501;state=done:msg=QQ=",
            b"7501;state=done:title=a",
            // "a\nb", a control character.
            b"7501;state=done:msg=YQpi",
            // "\x9b", a C1 control character as UTF-8.
            b"7501;state=done:msg=wps=",
            // Invalid UTF-8.
            b"7501;state=done:msg=/w==",
        ];
        for input in invalid {
            p.reset();
            p.next_slice(input);
            assert!(p.end(Some(0x1b)).is_none());
        }

        // Unicode is fine. "安全"
        p.reset();
        p.next_slice(b"7501;state=done:msg=5a6J5YWo");
        let r = report(p.end(Some(0x1b)));
        assert_eq!("安全".as_bytes().to_vec(), text::<o::Msg>(&r));
    }

    #[test]
    fn osc_7501_empty_text_is_absent() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        p.next_slice(b"7501;state=done:msg=:title=");
        let r = report(p.end(Some(0x1b)));
        assert!(r.read_option::<o::Msg>().is_none());
        assert!(r.read_option::<o::Title>().is_none());
    }

    #[test]
    fn osc_7501_discarded_reports() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        // 192 bytes of title and 2048 bytes of message are the most allowed.
        // "QUFB" decodes to "AAA" and "QUE" to "AA".
        let title: Vec<u8> = b"QUFB".repeat(MAX_TITLE_ENCODED_BYTES / 4);
        let main: Vec<u8> = b"QUFB".repeat(MAX_MSG_BYTES / 3);
        let mut msg = main.clone();
        msg.extend_from_slice(b"QUE");
        let fill = MAX_SEQUENCE_BYTES - b"\x1b]7501;state=idle:".len() - b"\x1b\\".len();

        let cat = |parts: &[&[u8]]| -> Vec<u8> { parts.concat() };

        let cases: Vec<(Vec<u8>, bool)> = vec![
            // A missing or unknown state.
            (b"7501;".to_vec(), false),
            (b"7501;app=cargo".to_vec(), false),
            (b"7501;state=sleeping".to_vec(), false),
            (b"7501;state=".to_vec(), false),
            (b"7501;state=IDLE".to_vec(), false),
            // A key that is too long, even one we don't know.
            (
                cat(&[b"7501;state=idle:", &[b'a'; MAX_KEY_BYTES], b"=1"]),
                true,
            ),
            (
                cat(&[b"7501;state=idle:", &[b'a'; MAX_KEY_BYTES + 1], b"=1"]),
                false,
            ),
            // An app that is too long, even if a later pair replaces it.
            (
                cat(&[b"7501;state=idle:app=", &[b'a'; MAX_APP_BYTES + 1]]),
                false,
            ),
            (
                cat(&[
                    b"7501;state=idle:app=",
                    &[b'a'; MAX_APP_BYTES + 1],
                    b":app=ok",
                ]),
                false,
            ),
            // Text at its limits and one step over.
            (cat(&[b"7501;state=idle:title=", &title]), true),
            (cat(&[b"7501;state=idle:title=", &title, b"QUFB"]), false),
            (cat(&[b"7501;state=idle:msg=", &msg]), true),
            (cat(&[b"7501;state=idle:msg=", &main, b"QUFB"]), false),
            // The whole sequence, including the terminator.
            (cat(&[b"7501;state=idle:", &vec![b'x'; fill]]), true),
            (cat(&[b"7501;state=idle:", &vec![b'x'; fill + 1]]), false),
            // Numbers that only start like 7501.
            (b"75;state=idle".to_vec(), false),
            (b"750;state=idle".to_vec(), false),
            (b"75010;state=idle".to_vec(), false),
            (b"7501".to_vec(), false),
        ];
        for (input, valid) in cases {
            p.reset();
            p.next_slice(&input);
            assert_eq!(
                valid,
                p.end(Some(0x1b)).is_some(),
                "{}",
                String::from_utf8_lossy(&input[..input.len().min(40)])
            );
        }
    }

    #[test]
    fn osc_7501_no_allocator() {
        let mut p = Parser::init(None);

        p.next_slice(b"7501;state=working:app=cargo");
        let r = report(p.end(Some(0x1b)));
        assert_eq!(State::Working, r.state);
        assert_eq!(b"cargo", r.read_option::<o::App>().unwrap());
    }
}
