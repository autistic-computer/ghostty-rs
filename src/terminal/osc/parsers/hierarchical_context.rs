//! Port of `src/terminal/osc/parsers/hierarchical_context.zig`.
//!
//! STATUS: ported
//!
//! OSC 3008: Hierarchical Context Signalling (UAPI spec)
//! Specification: https://uapi-group.org/specifications/specs/osc_context/
//!
//! OSC 3008 allows programs to signal context changes to the terminal emulator.
//! Each context has an identifier and metadata fields. Contexts are hierarchical
//! and form a stack.
//!
//! Rust note: Zig's `readOption(comptime field)` returns a field-dependent
//! type. Here each field has a marker type in [`fields`] implementing
//! [`FieldType`], and `read_option::<fields::User>()` returns that field's
//! value type.

use crate::lib::parse_int;
use crate::terminal::osc::{Command as OscCommand, Parser, State};

const LOG_TARGET: &str = "osc_hierarchical_context";

/// Maximum length of a context identifier (per spec).
const MAX_CONTEXT_ID_LEN: usize = 64;

/// A string from a context metadata field that may contain \x3b and \x5c escape sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnescapedString<'a> {
    pub raw: &'a [u8],
}

/// An iterator that decodes `\x3b` (semicolon) and `\x5c` (backslash) escape sequences.
/// Yields one unescaped byte at a time.
#[derive(Debug, Clone)]
pub struct UnescapedStringIterator<'a> {
    pub raw: &'a [u8],
    pub index: usize,
}

impl Iterator for UnescapedStringIterator<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        if self.index >= self.raw.len() {
            return None;
        }

        if self.raw[self.index] == b'\\' && self.index + 3 < self.raw.len() {
            if &self.raw[self.index..self.index + 4] == b"\\x3b" {
                self.index += 4;
                return Some(b';');
            }

            if &self.raw[self.index..self.index + 4] == b"\\x5c" {
                self.index += 4;
                return Some(b'\\');
            }
        }

        let c = self.raw[self.index];
        self.index += 1;
        Some(c)
    }
}

impl<'a> UnescapedString<'a> {
    pub fn iterator(&self) -> UnescapedStringIterator<'a> {
        UnescapedStringIterator {
            raw: self.raw,
            index: 0,
        }
    }

    /// Allocates a new string with the unescaped content.
    pub fn to_allocated(&self) -> Vec<u8> {
        self.iterator().collect()
    }

    /// Returns true if the unescaped string matches `other`.
    pub fn eql(&self, other: &[u8]) -> bool {
        let mut it = self.iterator();

        for &c in other {
            match it.next() {
                Some(next_c) if next_c == c => {}
                _ => return false,
            }
        }

        it.next().is_none()
    }
}

/// A single OSC 3008 hierarchical context command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command<'a> {
    pub action: Action,
    /// The context identifier. Must be 1-64 characters in the 32..126 byte range.
    pub id: &'a [u8],
    /// Raw unparsed metadata fields after the context ID.
    /// Fields are semicolon-separated key=value pairs.
    /// Parsed lazily via `read_option`.
    pub metadata: &'a [u8],
}

/// The C representation of [`Command`] (unsupported).
pub type CommandC = ();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum Action {
    /// OSC 3008;start=<id> — initiates, updates, or returns to a context.
    Start,
    /// OSC 3008;end=<id> — terminates a context.
    End,
}

impl<'a> Command<'a> {
    /// Read a metadata field value from the raw fields string.
    /// Returns null if the field is not present or malformed.
    pub fn read_option<F: FieldType>(&self) -> Option<F::Type<'a>> {
        F::read(self.metadata)
    }
}

/// Context types defined by the specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextType {
    Boot,
    Container,
    Vm,
    Elevate,
    Chpriv,
    Subcontext,
    Remote,
    Shell,
    Command,
    App,
    Service,
    Session,
}

impl ContextType {
    pub fn parse(value: &[u8]) -> Option<ContextType> {
        Some(match value {
            b"boot" => ContextType::Boot,
            b"container" => ContextType::Container,
            b"vm" => ContextType::Vm,
            b"elevate" => ContextType::Elevate,
            b"chpriv" => ContextType::Chpriv,
            b"subcontext" => ContextType::Subcontext,
            b"remote" => ContextType::Remote,
            b"shell" => ContextType::Shell,
            b"command" => ContextType::Command,
            b"app" => ContextType::App,
            b"service" => ContextType::Service,
            b"session" => ContextType::Session,
            _ => return None,
        })
    }
}

/// Exit status for the `exit` end-sequence field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExitStatus {
    Success,
    Failure,
    Crash,
    Interrupt,
}

impl ExitStatus {
    pub fn parse(value: &[u8]) -> Option<ExitStatus> {
        Some(match value {
            b"success" => ExitStatus::Success,
            b"failure" => ExitStatus::Failure,
            b"crash" => ExitStatus::Crash,
            b"interrupt" => ExitStatus::Interrupt,
            _ => return None,
        })
    }
}

/// Metadata fields that can appear in OSC 3008 sequences.
/// Fields are read lazily from the raw string using the `read` method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    // Start sequence fields
    Type,
    User,
    Hostname,
    Machineid,
    Bootid,
    Pid,
    Pidfdid,
    Comm,
    Cwd,
    Cmdline,
    Vm,
    Container,
    Targetuser,
    Targethost,
    Sessionid,

    // End sequence fields
    Exit,
    Status,
    Signal,
}

impl Field {
    fn key(self) -> &'static [u8] {
        match self {
            Field::Type => b"type",
            Field::User => b"user",
            Field::Hostname => b"hostname",
            Field::Machineid => b"machineid",
            Field::Bootid => b"bootid",
            Field::Pid => b"pid",
            Field::Pidfdid => b"pidfdid",
            Field::Comm => b"comm",
            Field::Cwd => b"cwd",
            Field::Cmdline => b"cmdline",
            Field::Vm => b"vm",
            Field::Container => b"container",
            Field::Targetuser => b"targetuser",
            Field::Targethost => b"targethost",
            Field::Sessionid => b"sessionid",
            Field::Exit => b"exit",
            Field::Status => b"status",
            Field::Signal => b"signal",
        }
    }

    /// Find the raw value of this field in the raw fields string.
    ///
    /// The raw fields string contains semicolon-separated key=value pairs
    /// e.g. "type=container;user=lennart;hostname=zeta".
    ///
    /// Unknown or malformed fields are ignored per the specification.
    fn find(self, raw: &[u8]) -> Option<&[u8]> {
        for full in raw.split(|&c| c == b';') {
            // Parse key=value
            if let Some(eql_idx) = full.iter().position(|&c| c == b'=') {
                if &full[..eql_idx] == self.key() {
                    return Some(&full[eql_idx + 1..]);
                }
            }
        }

        // Not found
        None
    }
}

/// The value type of a [`Field`] (Zig: `Field.Type()`), and how to read
/// it from the raw fields string.
pub trait FieldType {
    const FIELD: Field;
    type Type<'a>;
    fn convert(value: &[u8]) -> Option<Self::Type<'_>>;

    /// Read the field value from the raw fields string.
    ///
    /// The raw fields string contains semicolon-separated key=value pairs
    /// e.g. "type=container;user=lennart;hostname=zeta".
    ///
    /// Unknown or malformed fields are ignored per the specification.
    fn read(raw: &[u8]) -> Option<Self::Type<'_>> {
        Self::convert(Self::FIELD.find(raw)?)
    }
}

/// Marker types for each [`Field`].
pub mod fields {
    use super::*;

    macro_rules! string_field {
        ($($name:ident),*) => {$(
            pub struct $name;
            impl FieldType for $name {
                const FIELD: Field = Field::$name;
                type Type<'a> = UnescapedString<'a>;
                fn convert(value: &[u8]) -> Option<UnescapedString<'_>> {
                    if !value.is_empty() {
                        Some(UnescapedString { raw: value })
                    } else {
                        None
                    }
                }
            }
        )*};
    }

    macro_rules! u64_field {
        ($($name:ident),*) => {$(
            pub struct $name;
            impl FieldType for $name {
                const FIELD: Field = Field::$name;
                type Type<'a> = u64;
                fn convert(value: &[u8]) -> Option<u64> {
                    parse_int::<u64>(value, 10).ok()
                }
            }
        )*};
    }

    pub struct Type;
    impl FieldType for Type {
        const FIELD: Field = Field::Type;
        type Type<'a> = ContextType;
        fn convert(value: &[u8]) -> Option<ContextType> {
            ContextType::parse(value)
        }
    }

    pub struct Exit;
    impl FieldType for Exit {
        const FIELD: Field = Field::Exit;
        type Type<'a> = ExitStatus;
        fn convert(value: &[u8]) -> Option<ExitStatus> {
            ExitStatus::parse(value)
        }
    }

    u64_field!(Pid, Pidfdid, Status);

    // All other fields are string values
    string_field!(
        User, Hostname, Machineid, Bootid, Comm, Cwd, Cmdline, Vm, Container, Targetuser,
        Targethost, Sessionid, Signal
    );
}

/// Parse OSC 3008: hierarchical context signalling.
///
/// Expected data format (after "3008;" prefix has been consumed by the state machine):
///   start=<id>[;<field>=<value>]*
///   end=<id>[;<field>=<value>]*
pub fn parse(parser: &mut Parser, _: Option<u8>) -> Option<OscCommand<'_>> {
    let Some(cap) = parser.capture.as_ref() else {
        parser.state = State::Invalid;
        return None;
    };
    let data = cap.trailing();
    if data.is_empty() {
        parser.state = State::Invalid;
        return None;
    }

    // Determine the action (start= or end=)
    let action = if data.starts_with(b"start=") {
        Action::Start
    } else if data.starts_with(b"end=") {
        Action::End
    } else {
        log::warn!(
            target: LOG_TARGET,
            "OSC 3008: expected 'start=' or 'end=' prefix, got: {}",
            String::from_utf8_lossy(&data[..data.len().min(10)])
        );
        parser.state = State::Invalid;
        return None;
    };

    // Skip past the "start=" or "end=" prefix
    let prefix_len: usize = match action {
        Action::Start => b"start=".len(),
        Action::End => b"end=".len(),
    };
    let rest = &data[prefix_len..];

    if rest.is_empty() {
        log::warn!(target: LOG_TARGET, "OSC 3008: missing context ID");
        parser.state = State::Invalid;
        return None;
    }

    // Extract the context ID (up to the first semicolon or end of data)
    let id_end = rest.iter().position(|&c| c == b';').unwrap_or(rest.len());
    let id = &rest[..id_end];

    // Validate context ID length (1-64 chars per spec)
    if id.is_empty() || id.len() > MAX_CONTEXT_ID_LEN {
        log::warn!(
            target: LOG_TARGET,
            "OSC 3008: context ID length {} out of range (1-{})",
            id.len(),
            MAX_CONTEXT_ID_LEN
        );
        parser.state = State::Invalid;
        return None;
    }

    // Validate context ID characters (32..126 byte range per spec)
    for &c in id {
        if !(0x20..=0x7e).contains(&c) {
            log::warn!(
                target: LOG_TARGET,
                "OSC 3008: invalid character 0x{:02x} in context ID",
                c
            );
            parser.state = State::Invalid;
            return None;
        }
    }

    // Extract raw metadata fields (everything after the ID)
    let metadata: &[u8] = if id_end < rest.len() {
        &rest[id_end + 1..]
    } else {
        b""
    };

    Some(OscCommand::HierarchicalContext(Command {
        action,
        id,
        metadata,
    }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::fields as f;
    use super::*;

    fn run(input: &[u8]) -> Parser {
        let mut p = Parser::init(None);
        for &ch in input {
            p.next(ch);
        }
        p
    }

    fn hc<'a>(cmd: Option<OscCommand<'a>>) -> Command<'a> {
        match cmd {
            Some(OscCommand::HierarchicalContext(c)) => c,
            other => panic!("expected hierarchical_context, got {:?}", other),
        }
    }

    #[test]
    fn osc_3008_basic_start_command() {
        let mut p = run(b"3008;start=abc123");
        let cmd = hc(p.end(None));
        assert_eq!(Action::Start, cmd.action);
        assert_eq!(b"abc123", cmd.id);
        assert_eq!(b"", cmd.metadata);
    }

    #[test]
    fn osc_3008_basic_end_command() {
        let mut p = run(b"3008;end=abc123");
        let cmd = hc(p.end(None));
        assert_eq!(Action::End, cmd.action);
        assert_eq!(b"abc123", cmd.id);
        assert_eq!(b"", cmd.metadata);
    }

    #[test]
    fn osc_3008_start_with_metadata_fields() {
        let mut p = run(b"3008;start=bed86fab93af4328bbed0a1224af6d40;type=container;user=lennart;hostname=zeta");
        let cmd = hc(p.end(None));
        assert_eq!(Action::Start, cmd.action);
        assert_eq!(b"bed86fab93af4328bbed0a1224af6d40", cmd.id);

        // Read individual fields
        assert_eq!(ContextType::Container, cmd.read_option::<f::Type>().unwrap());
        assert!(cmd.read_option::<f::User>().unwrap().eql(b"lennart"));
        assert!(cmd.read_option::<f::Hostname>().unwrap().eql(b"zeta"));
    }

    #[test]
    fn osc_3008_start_with_all_common_fields() {
        let mut p = run(b"3008;start=myctx;type=shell;user=root;hostname=myhost;machineid=3deb5353d3ba43d08201c136a47ead7b;bootid=d4a3d0fdf2e24fdea6d971ce73f4fbf2;pid=1062862;pidfdid=1063162;comm=bash");
        let cmd = hc(p.end(None));
        assert_eq!(ContextType::Shell, cmd.read_option::<f::Type>().unwrap());
        assert!(cmd.read_option::<f::User>().unwrap().eql(b"root"));
        assert!(cmd.read_option::<f::Hostname>().unwrap().eql(b"myhost"));
        assert!(cmd
            .read_option::<f::Machineid>()
            .unwrap()
            .eql(b"3deb5353d3ba43d08201c136a47ead7b"));
        assert!(cmd
            .read_option::<f::Bootid>()
            .unwrap()
            .eql(b"d4a3d0fdf2e24fdea6d971ce73f4fbf2"));
        assert_eq!(1062862u64, cmd.read_option::<f::Pid>().unwrap());
        assert_eq!(1063162u64, cmd.read_option::<f::Pidfdid>().unwrap());
        assert!(cmd.read_option::<f::Comm>().unwrap().eql(b"bash"));
    }

    #[test]
    fn osc_3008_end_with_exit_metadata() {
        let mut p = run(b"3008;end=myctx;exit=success;status=0");
        let cmd = hc(p.end(None));
        assert_eq!(Action::End, cmd.action);
        assert_eq!(b"myctx", cmd.id);
        assert_eq!(ExitStatus::Success, cmd.read_option::<f::Exit>().unwrap());
        assert_eq!(0u64, cmd.read_option::<f::Status>().unwrap());
    }

    #[test]
    fn osc_3008_end_with_failure_exit() {
        let mut p = run(b"3008;end=myctx;exit=failure;status=1;signal=SIGKILL");
        let cmd = hc(p.end(None));
        assert_eq!(ExitStatus::Failure, cmd.read_option::<f::Exit>().unwrap());
        assert_eq!(1u64, cmd.read_option::<f::Status>().unwrap());
        assert!(cmd.read_option::<f::Signal>().unwrap().eql(b"SIGKILL"));
    }

    #[test]
    fn osc_3008_numeric_fields_reject_non_digits() {
        let mut p = run(b"3008;start=myctx;pid=4_2;pidfdid=+42;status=-1");
        let cmd = hc(p.end(None));
        assert!(cmd.read_option::<f::Pid>().is_none());
        assert!(cmd.read_option::<f::Pidfdid>().is_none());
        assert!(cmd.read_option::<f::Status>().is_none());
    }

    #[test]
    fn osc_3008_unknown_fields_are_ignored() {
        let mut p = run(b"3008;start=myctx;type=shell;unknownfield=value;user=root");
        let cmd = hc(p.end(None));
        assert_eq!(ContextType::Shell, cmd.read_option::<f::Type>().unwrap());
        assert!(cmd.read_option::<f::User>().unwrap().eql(b"root"));
    }

    #[test]
    fn osc_3008_missing_field_returns_null() {
        let mut p = run(b"3008;start=myctx;user=lennart");
        let cmd = hc(p.end(None));
        assert!(cmd.read_option::<f::Type>().is_none());
        assert!(cmd.read_option::<f::Hostname>().is_none());
        assert!(cmd.read_option::<f::Pid>().is_none());
    }

    #[test]
    fn osc_3008_invalid_prefix() {
        let mut p = run(b"3008;bogus=abc123");
        assert!(p.end(None).is_none());
    }

    #[test]
    fn osc_3008_empty_data() {
        // Can't really produce empty data after "3008;" because the state machine
        // won't write a writer for that case, but we test the edge case where
        // only "start=" is present with no ID.
        let mut p = run(b"3008;start=");
        assert!(p.end(None).is_none());
    }

    #[test]
    fn osc_3008_max_length_context_id() {
        let id = [b'a'; 64];
        let mut input = b"3008;start=".to_vec();
        input.extend_from_slice(&id);
        let mut p = run(&input);
        let cmd = hc(p.end(None));
        assert_eq!(&id[..], cmd.id);
    }

    #[test]
    fn osc_3008_over_length_context_id() {
        let id = [b'a'; 65];
        let mut input = b"3008;start=".to_vec();
        input.extend_from_slice(&id);
        let mut p = run(&input);
        assert!(p.end(None).is_none());
    }

    #[test]
    fn osc_3008_context_type_enum_coverage() {
        let types: [(&[u8], ContextType); 12] = [
            (b"boot", ContextType::Boot),
            (b"container", ContextType::Container),
            (b"vm", ContextType::Vm),
            (b"elevate", ContextType::Elevate),
            (b"chpriv", ContextType::Chpriv),
            (b"subcontext", ContextType::Subcontext),
            (b"remote", ContextType::Remote),
            (b"shell", ContextType::Shell),
            (b"command", ContextType::Command),
            (b"app", ContextType::App),
            (b"service", ContextType::Service),
            (b"session", ContextType::Session),
        ];

        for (s, expected) in types {
            assert_eq!(expected, ContextType::parse(s).unwrap());
        }

        assert!(ContextType::parse(b"invalid").is_none());
    }

    #[test]
    fn osc_3008_exit_status_enum_coverage() {
        assert_eq!(ExitStatus::Success, ExitStatus::parse(b"success").unwrap());
        assert_eq!(ExitStatus::Failure, ExitStatus::parse(b"failure").unwrap());
        assert_eq!(ExitStatus::Crash, ExitStatus::parse(b"crash").unwrap());
        assert_eq!(ExitStatus::Interrupt, ExitStatus::parse(b"interrupt").unwrap());
        assert!(ExitStatus::parse(b"invalid").is_none());
    }

    #[test]
    fn osc_3008_spec_example_container_start() {
        // From the spec: a new container "foobar" invoked by user "lennart" on host "zeta"
        let mut p = run(b"3008;start=bed86fab93af4328bbed0a1224af6d40;type=container;user=lennart;hostname=zeta;machineid=3deb5353d3ba43d08201c136a47ead7b;bootid=d4a3d0fdf2e24fdea6d971ce73f4fbf2;pid=1062862;pidfdid=1063162;comm=systemd-nspawn;container=foobar");
        let cmd = hc(p.end(None));
        assert_eq!(Action::Start, cmd.action);
        assert_eq!(b"bed86fab93af4328bbed0a1224af6d40", cmd.id);
        assert_eq!(ContextType::Container, cmd.read_option::<f::Type>().unwrap());
        assert!(cmd.read_option::<f::User>().unwrap().eql(b"lennart"));
        assert!(cmd.read_option::<f::Hostname>().unwrap().eql(b"zeta"));
        assert!(cmd.read_option::<f::Comm>().unwrap().eql(b"systemd-nspawn"));
        assert!(cmd.read_option::<f::Container>().unwrap().eql(b"foobar"));
        assert_eq!(1062862u64, cmd.read_option::<f::Pid>().unwrap());
    }

    #[test]
    fn osc_3008_spec_example_context_end() {
        // From the spec: context end
        let mut p = run(b"3008;end=bed86fab93af4328bbed0a1224af6d40");
        let cmd = hc(p.end(None));
        assert_eq!(Action::End, cmd.action);
        assert_eq!(b"bed86fab93af4328bbed0a1224af6d40", cmd.id);
    }

    #[test]
    fn osc_3008_cwd_and_cmdline_fields() {
        let mut p = run(b"3008;start=myctx;type=command;cwd=/home/user;cmdline=ls -la");
        let cmd = hc(p.end(None));
        assert!(cmd.read_option::<f::Cwd>().unwrap().eql(b"/home/user"));
        assert!(cmd.read_option::<f::Cmdline>().unwrap().eql(b"ls -la"));
    }

    #[test]
    fn osc_3008_start_command_with_no_fields() {
        let mut p = run(b"3008;start=simpleid");
        let cmd = hc(p.end(None));
        assert_eq!(Action::Start, cmd.action);
        assert_eq!(b"simpleid", cmd.id);
        assert!(cmd.read_option::<f::Type>().is_none());
        assert!(cmd.read_option::<f::User>().is_none());
        assert!(cmd.read_option::<f::Exit>().is_none());
    }

    #[test]
    fn osc_3008_string_unescaping() {
        let mut p = run(b"3008;start=myctx;cmdline=echo \\x3b \\x5c;cwd=\\x5csystem07\\x5cC$\\x5c");
        let cmd = hc(p.end(None));
        assert!(cmd.read_option::<f::Cmdline>().unwrap().eql(b"echo ; \\"));
        assert!(cmd.read_option::<f::Cwd>().unwrap().eql(b"\\system07\\C$\\"));
        assert_eq!(
            b"echo ; \\".to_vec(),
            cmd.read_option::<f::Cmdline>().unwrap().to_allocated()
        );
    }
}
