//! Port of `src/terminal/osc/parsers/semantic_prompt.zig`.
//!
//! STATUS: ported
//!
//! https://gitlab.freedesktop.org/Per_Bothner/specifications/blob/master/proposals/semantic-prompts.md
//!
//! Rust notes: Zig's `Option` enum is named [`PromptOption`] here (to not
//! shadow `core::option::Option`). Zig's `readOption(comptime option)`
//! returns an option-dependent type; here each option has a marker type in
//! [`options`] implementing [`OptionType`], e.g.
//! `cmd.read_option::<options::Aid>()`.

use crate::lib::parse_int;
use crate::os::string_encoding;
use crate::terminal::osc::{Command as OscCommand, Parser, State};

/// A single semantic prompt command.
///
/// Technically according to the spec, not all commands have options
/// but it is easier to be "liberal in what we accept" here since
/// all except one do and the spec does also say to ignore unknown
/// options. So, I think this is a fair interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command<'a> {
    pub action: Action,
    pub options_unvalidated: &'a [u8],
}

/// The C representation of [`Command`] (unsupported).
pub type CommandC = ();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    FreshLine,                       // 'L'
    FreshLineNewPrompt,              // 'A'
    NewCommand,                      // 'N'
    PromptStart,                     // 'P'
    EndPromptStartInput,             // 'B'
    EndPromptStartInputTerminateEol, // 'I'
    EndInputStartOutput,             // 'C'
    EndCommand,                      // 'D'
}

/// Error from [`Command::write_command_line`].
#[derive(Debug)]
pub enum WriteCommandLineError {
    /// The writer failed.
    WriteFailed(std::io::Error),
    /// The command line could not be decoded.
    DecodeError,
}

impl From<std::io::Error> for WriteCommandLineError {
    fn from(err: std::io::Error) -> Self {
        WriteCommandLineError::WriteFailed(err)
    }
}

impl From<string_encoding::DecodeError> for WriteCommandLineError {
    fn from(err: string_encoding::DecodeError) -> Self {
        match err {
            string_encoding::DecodeError::DecodeError => WriteCommandLineError::DecodeError,
            string_encoding::DecodeError::Write(err) => WriteCommandLineError::WriteFailed(err),
        }
    }
}

impl std::fmt::Display for WriteCommandLineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WriteFailed(err) => write!(f, "WriteFailed: {}", err),
            Self::DecodeError => f.write_str("DecodeError"),
        }
    }
}

impl std::error::Error for WriteCommandLineError {}

impl<'a> Command<'a> {
    pub fn init(action: Action) -> Command<'a> {
        Command {
            action,
            options_unvalidated: b"",
        }
    }

    /// Read an option for this command. Returns null if unset or invalid.
    pub fn read_option<O: OptionType>(&self) -> Option<O::Type<'a>> {
        O::read(self.options_unvalidated)
    }

    /// Write the decoded command line (if any) to the writer. If an error
    /// occurs garbage may have been written to the writer.
    pub fn write_command_line<W: std::io::Write + ?Sized>(
        &self,
        writer: &mut W,
    ) -> Result<(), WriteCommandLineError> {
        if let Some(command_line) = self.read_option::<options::Cmdline>() {
            string_encoding::printf_q_decode(writer, command_line)?;
            return Ok(());
        }
        if let Some(command_line) = self.read_option::<options::CmdlineUrl>() {
            string_encoding::url_percent_decode(writer, command_line)?;
            return Ok(());
        }
        Ok(())
    }
}

/// ClickEvents can either be a click_events=1 or click_events=2.
/// The click_events=1 sends a click event with the absolute coordinates
/// of the click.
/// The click_events=2 sends a click event with the coordinates of the click
/// relative to the prompt area.
/// See https://github.com/ghostty-org/ghostty/issues/10865 and
/// https://github.com/kovidgoyal/kitty/issues/9500
/// for further details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum ClickEvents {
    Absolute,
    Relative,
}

/// The semantic prompt options (Zig: `Option`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PromptOption {
    Aid,
    Cl,
    PromptKind,
    Err,
    Cmdline,
    CmdlineUrl,

    // https://sw.kovidgoyal.net/kitty/shell-integration/#notes-for-shell-developers
    // Kitty supports a "redraw" option for prompt_start. This is extended
    // by Ghostty with the "last" option. See Redraw the type for more details.
    Redraw,

    // Use a special key instead of arrow keys to move the cursor on
    // mouse click. Useful if arrow keys have side-effets like triggering
    // auto-complete. The shell integration script should bind the special
    // key as needed.
    // See: https://sw.kovidgoyal.net/kitty/shell-integration/#notes-for-shell-developers
    SpecialKey,

    // If true, the shell is capable of handling mouse click events.
    // Ghostty will then send a click event to the shell when the user
    // clicks somewhere in the prompt. The shell can then move the cursor
    // to that position or perform some other appropriate action. If false,
    // Ghostty may generate a number of fake key events to move the cursor
    // which is not very robust.
    // See: https://sw.kovidgoyal.net/kitty/shell-integration/#notes-for-shell-developers
    ClickEvents,

    // Not technically an option that can be set with k=v and only
    // present currently with command 'D' but its easier to just
    // parse it into our options.
    ExitCode,
}

impl PromptOption {
    fn key(self) -> &'static [u8] {
        match self {
            PromptOption::Aid => b"aid",
            PromptOption::Cl => b"cl",
            PromptOption::PromptKind => b"k",
            PromptOption::Err => b"err",
            PromptOption::Redraw => b"redraw",
            PromptOption::SpecialKey => b"special_key",
            PromptOption::ClickEvents => b"click_events",
            PromptOption::Cmdline => b"cmdline",
            PromptOption::CmdlineUrl => b"cmdline_url",

            // special case, handled before ever calling key
            PromptOption::ExitCode => unreachable!(),
        }
    }

    /// Find the raw value of the option in the raw options string. For
    /// `ExitCode` this is the first value.
    fn find(self, raw: &[u8]) -> Option<&[u8]> {
        let mut remaining = raw;
        while !remaining.is_empty() {
            // Length of the next value is up to the `;` or the
            // end of the string.
            let len = remaining
                .iter()
                .position(|&c| c == b';')
                .unwrap_or(remaining.len());

            // Grab our full value and move our cursor past the `;`
            let full = &remaining[..len];

            // If we're looking for exit_code we special case it.
            // as the first value.
            if self == PromptOption::ExitCode {
                return Some(full);
            }

            // Parse our key=value and verify our key matches our
            // expectation.
            if let Some(eql_idx) = full.iter().position(|&c| c == b'=') {
                if &full[..eql_idx] == self.key() {
                    return Some(&full[eql_idx + 1..]);
                }
            }

            // No match!
            if len < remaining.len() {
                remaining = &remaining[len + 1..];
                continue;
            }

            break;
        }

        // Not found
        None
    }
}

/// The value type of a [`PromptOption`] (Zig: `Option.Type()`) and how to
/// read it.
pub trait OptionType {
    const OPTION: PromptOption;
    type Type<'a>;
    fn convert(value: &[u8]) -> Option<Self::Type<'_>>;

    /// Read the option value from the raw options string.
    ///
    /// The raw options string is the raw unparsed data after the
    /// OSC 133 command. e.g. for `133;A;aid=14;cl=line`, the
    /// raw options string would be `aid=14;cl=line`.
    ///
    /// Any errors in the raw string will return null since the OSC133
    /// specification says to ignore unknown or malformed options.
    fn read(raw: &[u8]) -> Option<Self::Type<'_>> {
        Self::convert(Self::OPTION.find(raw)?)
    }
}

/// Marker types for each [`PromptOption`].
pub mod options {
    use super::*;

    macro_rules! str_option {
        ($($name:ident),*) => {$(
            pub struct $name;
            impl OptionType for $name {
                const OPTION: PromptOption = PromptOption::$name;
                type Type<'a> = &'a [u8];
                fn convert(value: &[u8]) -> Option<&[u8]> {
                    Some(value)
                }
            }
        )*};
    }

    str_option!(Aid, Err, Cmdline, CmdlineUrl);

    pub struct Cl;
    impl OptionType for Cl {
        const OPTION: PromptOption = PromptOption::Cl;
        type Type<'a> = Click;
        fn convert(value: &[u8]) -> Option<Click> {
            parse_click(value)
        }
    }

    pub struct PromptKind;
    impl OptionType for PromptKind {
        const OPTION: PromptOption = PromptOption::PromptKind;
        type Type<'a> = super::PromptKind;
        fn convert(value: &[u8]) -> Option<super::PromptKind> {
            if value.len() == 1 {
                super::PromptKind::init(value[0])
            } else {
                None
            }
        }
    }

    pub struct Redraw;
    impl OptionType for Redraw {
        const OPTION: PromptOption = PromptOption::Redraw;
        type Type<'a> = super::Redraw;
        fn convert(value: &[u8]) -> Option<super::Redraw> {
            match value {
                b"0" => Some(super::Redraw::False),
                b"1" => Some(super::Redraw::True),
                b"last" => Some(super::Redraw::Last),
                _ => None,
            }
        }
    }

    pub struct SpecialKey;
    impl OptionType for SpecialKey {
        const OPTION: PromptOption = PromptOption::SpecialKey;
        type Type<'a> = bool;
        fn convert(value: &[u8]) -> Option<bool> {
            match value {
                b"0" => Some(false),
                b"1" => Some(true),
                _ => None,
            }
        }
    }

    pub struct ClickEvents;
    impl OptionType for ClickEvents {
        const OPTION: PromptOption = PromptOption::ClickEvents;
        type Type<'a> = super::ClickEvents;
        fn convert(value: &[u8]) -> Option<super::ClickEvents> {
            match value {
                b"1" => Some(super::ClickEvents::Absolute),
                b"2" => Some(super::ClickEvents::Relative),
                _ => None,
            }
        }
    }

    pub struct ExitCode;
    impl OptionType for ExitCode {
        const OPTION: PromptOption = PromptOption::ExitCode;
        type Type<'a> = i32;
        fn convert(value: &[u8]) -> Option<i32> {
            parse_int::<i32>(value, 10).ok()
        }
    }
}

/// The `cl` option specifies what kind of cursor key sequences are handled
/// by the application for click-to-move-cursor functionality.
///
/// `line` allows movement within one input line. `multiple` allows movement
/// across lines with left/right sequences. The two vertical modes additionally
/// allow up/down sequences, with `smart_vertical` permitting editor-aware
/// column clamping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum Click {
    /// Value: "line". Allows motion within a single input line using
    /// standard left/right arrow escape sequences. Only a single left/right
    /// sequence should be emitted for double-width characters.
    Line,

    /// Value: "m". Allows movement between different lines in the same
    /// group, but only using left/right arrow escape sequences.
    Multiple,

    /// Value: "v". Like `multiple` but cursor up/down should be used. The
    /// terminal should be conservative when moving between lines: move the
    /// cursor left to the start of line, emit the needed up/down sequences,
    /// then move the cursor right to the clicked destination.
    ConservativeVertical,

    /// Value: "w". Like `conservative_vertical` but specifies that there
    /// are no spurious spaces at the end of the line, and the application
    /// editor handles "smart vertical movement" (moving 2 lines up from
    /// position 20, where the intermediate line is 15 chars wide and the
    /// destination is 18 chars wide, ends at position 18).
    SmartVertical,
}

fn parse_click(value: &[u8]) -> Option<Click> {
    if value.len() == 1 {
        match value[0] {
            b'm' => Some(Click::Multiple),
            b'v' => Some(Click::ConservativeVertical),
            b'w' => Some(Click::SmartVertical),
            _ => None,
        }
    } else if value == b"line" {
        Some(Click::Line)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PromptKind {
    Initial,
    Right,
    Continuation,
    Secondary,
}

impl PromptKind {
    pub fn init(c: u8) -> Option<PromptKind> {
        match c {
            b'i' => Some(PromptKind::Initial),
            b'r' => Some(PromptKind::Right),
            b'c' => Some(PromptKind::Continuation),
            b's' => Some(PromptKind::Secondary),
            _ => None,
        }
    }
}

/// The values for the `redraw` extension to OSC133. This was
/// started by Kitty[1] and extended by Ghostty (the "last" option).
///
/// [1]: https://sw.kovidgoyal.net/kitty/shell-integration/#notes-for-shell-developers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Redraw {
    /// The shell supports redrawing the full prompt and all continuations.
    /// This is the default value, it does not need to be explicitly set
    /// unless it is to reset a prior other value.
    True,

    /// The shell does NOT support redrawing. In this case, Ghostty will NOT
    /// clear any prompt lines on resize.
    False,

    /// The shell supports redrawing only the LAST line of the prompt.
    /// Ghostty will only clear the last line of the prompt on resize.
    ///
    /// This is specifically introduced because Bash only redraws the last
    /// line. It is literally the only shell that does this and it does this
    /// because its bad and they should feel bad. Don't be like Bash.
    Last,
}

/// Parse OSC 133, semantic prompts
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

    // All valid cases terminate within this block. Any fallthroughs
    // are invalid. This makes some of our parse logic a little less
    // repetitive.
    let action = match data[0] {
        b'A' => Some(Action::FreshLineNewPrompt),
        b'B' => Some(Action::EndPromptStartInput),
        b'I' => Some(Action::EndPromptStartInputTerminateEol),
        b'C' => Some(Action::EndInputStartOutput),
        b'D' => Some(Action::EndCommand),
        b'L' => {
            if data.len() > 1 {
                None
            } else {
                return Some(OscCommand::SemanticPrompt(Command::init(Action::FreshLine)));
            }
        }
        b'N' => Some(Action::NewCommand),
        b'P' => Some(Action::PromptStart),
        _ => None,
    };

    if let Some(action) = action {
        let mut cmd = Command::init(action);
        if data.len() == 1 {
            return Some(OscCommand::SemanticPrompt(cmd));
        }
        if data[1] == b';' {
            cmd.options_unvalidated = &data[2..];
            return Some(OscCommand::SemanticPrompt(cmd));
        }
    }

    // Any fallthroughs are invalid
    parser.state = State::Invalid;
    None
}

#[cfg(test)]
mod tests {
    use super::options as o;
    use super::*;

    fn run(input: &[u8]) -> Parser {
        let mut p = Parser::init(None);
        for &ch in input {
            p.next(ch);
        }
        p
    }

    fn sp<'a>(cmd: Option<OscCommand<'a>>) -> Command<'a> {
        match cmd {
            Some(OscCommand::SemanticPrompt(c)) => c,
            other => panic!("expected semantic_prompt, got {:?}", other),
        }
    }

    #[test]
    fn osc_133_end_input_start_output() {
        let mut p = run(b"133;C");
        let cmd = sp(p.end(None));
        assert_eq!(Action::EndInputStartOutput, cmd.action);
        assert!(cmd.read_option::<o::Aid>().is_none());
        assert!(cmd.read_option::<o::Cl>().is_none());
    }

    #[test]
    fn osc_133_end_input_start_output_extra_contents() {
        let mut p = run(b"133;Cextra");
        assert!(p.end(None).is_none());
    }

    #[test]
    fn osc_133_end_input_start_output_with_options() {
        let mut p = run(b"133;C;aid=foo");
        let cmd = sp(p.end(None));
        assert_eq!(Action::EndInputStartOutput, cmd.action);
        assert_eq!(b"foo", cmd.read_option::<o::Aid>().unwrap());
    }

    fn cmdline_ok(input: &[u8], expected: &[u8]) {
        let mut w: Vec<u8> = Vec::new();
        let mut p = run(input);
        let cmd = sp(p.end(None));
        cmd.write_command_line(&mut w).unwrap();
        assert_eq!(expected, &w[..]);
    }

    fn cmdline_err(input: &[u8]) {
        let mut w: Vec<u8> = Vec::new();
        let mut p = run(input);
        let cmd = sp(p.end(None));
        assert_eq!(Action::EndInputStartOutput, cmd.action);
        assert!(matches!(
            cmd.write_command_line(&mut w),
            Err(WriteCommandLineError::DecodeError)
        ));
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline() {
        cmdline_ok(b"133;C;cmdline=echo bobr kurwa", b"echo bobr kurwa");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_3() {
        cmdline_ok(b"133;C;cmdline=echo bobr\\nkurwa", b"echo bobr\nkurwa");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_4() {
        cmdline_ok(b"133;C;cmdline=$'echo bobr kurwa'", b"echo bobr kurwa");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_5() {
        cmdline_ok(b"133;C;cmdline='echo bobr kurwa'", b"echo bobr kurwa");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_6() {
        cmdline_err(b"133;C;cmdline='echo bobr kurwa");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_7() {
        cmdline_err(b"133;C;cmdline=$'echo bobr kurwa");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_8() {
        cmdline_err(b"133;C;cmdline=$'");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_9() {
        cmdline_ok(b"133;C;cmdline=", b"");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_url_1() {
        cmdline_ok(b"133;C;cmdline_url=echo bobr kurwa", b"echo bobr kurwa");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_url_2() {
        cmdline_ok(b"133;C;cmdline_url=echo bobr%20kurwa", b"echo bobr kurwa");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_url_3() {
        cmdline_ok(b"133;C;cmdline_url=echo bobr%3bkurwa", b"echo bobr;kurwa");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_url_4() {
        cmdline_err(b"133;C;cmdline_url=echo bobr%3kurwa");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_url_5() {
        cmdline_err(b"133;C;cmdline_url=echo bobr%kurwa");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_url_6() {
        cmdline_ok(b"133;C;cmdline_url=echo bobr kurwa%20", b"echo bobr kurwa ");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_url_7() {
        cmdline_err(b"133;C;cmdline_url=echo bobr kurwa%2");
    }

    #[test]
    fn osc_133_end_input_start_output_with_cmdline_url_8() {
        cmdline_err(b"133;C;cmdline_url=echo bobr kurwa%");
    }

    #[test]
    fn osc_133_fresh_line() {
        let mut p = run(b"133;L");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLine, cmd.action);
    }

    #[test]
    fn osc_133_fresh_line_extra_contents() {
        // Random
        {
            let mut p = run(b"133;Lol");
            assert!(p.end(None).is_none());
        }

        // Options
        {
            let mut p = run(b"133;L;aid=foo");
            assert!(p.end(None).is_none());
        }
    }

    #[test]
    fn osc_133_fresh_line_new_prompt() {
        let mut p = run(b"133;A");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
        assert!(cmd.read_option::<o::Aid>().is_none());
        assert!(cmd.read_option::<o::Cl>().is_none());
    }

    #[test]
    fn osc_133_fresh_line_new_prompt_with_aid() {
        let mut p = run(b"133;A;aid=14");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
        assert_eq!(b"14", cmd.read_option::<o::Aid>().unwrap());
    }

    #[test]
    fn osc_133_fresh_line_new_prompt_with_equals_in_aid() {
        let mut p = run(b"133;A;aid=a=b");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
        assert_eq!(b"a=b", cmd.read_option::<o::Aid>().unwrap());
    }

    #[test]
    fn osc_133_fresh_line_new_prompt_with_cl_line() {
        let mut p = run(b"133;A;cl=line");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
        assert_eq!(Some(Click::Line), cmd.read_option::<o::Cl>());
    }

    #[test]
    fn osc_133_fresh_line_new_prompt_with_cl_m() {
        let mut p = run(b"133;A;cl=m");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
        assert_eq!(Some(Click::Multiple), cmd.read_option::<o::Cl>());
    }

    #[test]
    fn osc_133_fresh_line_new_prompt_with_invalid_cl() {
        let mut p = run(b"133;A;cl=invalid");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
        assert!(cmd.read_option::<o::Cl>().is_none());
    }

    #[test]
    fn osc_133_fresh_line_new_prompt_with_trailing_semicolon() {
        let mut p = run(b"133;A;");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
    }

    #[test]
    fn osc_133_fresh_line_new_prompt_with_bare_key() {
        let mut p = run(b"133;A;barekey");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
        assert!(cmd.read_option::<o::Aid>().is_none());
        assert!(cmd.read_option::<o::Cl>().is_none());
    }

    #[test]
    fn osc_133_fresh_line_new_prompt_with_multiple_options() {
        let mut p = run(b"133;A;aid=foo;cl=line");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
        assert_eq!(b"foo", cmd.read_option::<o::Aid>().unwrap());
        assert_eq!(Some(Click::Line), cmd.read_option::<o::Cl>());
    }

    #[test]
    fn osc_133_fresh_line_new_prompt_default_redraw() {
        let mut p = run(b"133;A");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
        assert!(cmd.read_option::<o::Redraw>().is_none());
    }

    #[test]
    fn osc_133_fresh_line_new_prompt_with_redraw_0() {
        let mut p = run(b"133;A;redraw=0");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
        assert_eq!(Redraw::False, cmd.read_option::<o::Redraw>().unwrap());
    }

    #[test]
    fn osc_133_fresh_line_new_prompt_with_redraw_1() {
        let mut p = run(b"133;A;redraw=1");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
        assert_eq!(Redraw::True, cmd.read_option::<o::Redraw>().unwrap());
    }

    #[test]
    fn osc_133_fresh_line_new_prompt_with_invalid_redraw() {
        let mut p = run(b"133;A;redraw=x");
        let cmd = sp(p.end(None));
        assert_eq!(Action::FreshLineNewPrompt, cmd.action);
        assert!(cmd.read_option::<o::Redraw>().is_none());
    }

    #[test]
    fn osc_133_prompt_start() {
        let mut p = run(b"133;P");
        let cmd = sp(p.end(None));
        assert_eq!(Action::PromptStart, cmd.action);
        assert!(cmd.read_option::<o::PromptKind>().is_none());
    }

    #[test]
    fn osc_133_prompt_start_with_k() {
        for (input, expected) in [
            (&b"133;P;k=i"[..], PromptKind::Initial),
            (b"133;P;k=r", PromptKind::Right),
            (b"133;P;k=c", PromptKind::Continuation),
            (b"133;P;k=s", PromptKind::Secondary),
        ] {
            let mut p = run(input);
            let cmd = sp(p.end(None));
            assert_eq!(Action::PromptStart, cmd.action);
            assert_eq!(Some(expected), cmd.read_option::<o::PromptKind>());
        }
    }

    #[test]
    fn osc_133_prompt_start_with_invalid_k() {
        let mut p = run(b"133;P;k=x");
        let cmd = sp(p.end(None));
        assert_eq!(Action::PromptStart, cmd.action);
        assert!(cmd.read_option::<o::PromptKind>().is_none());
    }

    #[test]
    fn osc_133_prompt_start_extra_contents() {
        let mut p = run(b"133;Pextra");
        assert!(p.end(None).is_none());
    }

    #[test]
    fn osc_133_new_command() {
        let mut p = run(b"133;N");
        let cmd = sp(p.end(None));
        assert_eq!(Action::NewCommand, cmd.action);
        assert!(cmd.read_option::<o::Aid>().is_none());
        assert!(cmd.read_option::<o::Cl>().is_none());
    }

    #[test]
    fn osc_133_new_command_with_aid() {
        let mut p = run(b"133;N;aid=foo");
        let cmd = sp(p.end(None));
        assert_eq!(Action::NewCommand, cmd.action);
        assert_eq!(b"foo", cmd.read_option::<o::Aid>().unwrap());
    }

    #[test]
    fn osc_133_new_command_with_cl_line() {
        let mut p = run(b"133;N;cl=line");
        let cmd = sp(p.end(None));
        assert_eq!(Action::NewCommand, cmd.action);
        assert_eq!(Some(Click::Line), cmd.read_option::<o::Cl>());
    }

    #[test]
    fn osc_133_new_command_with_multiple_options() {
        let mut p = run(b"133;N;aid=foo;cl=line");
        let cmd = sp(p.end(None));
        assert_eq!(Action::NewCommand, cmd.action);
        assert_eq!(b"foo", cmd.read_option::<o::Aid>().unwrap());
        assert_eq!(Some(Click::Line), cmd.read_option::<o::Cl>());
    }

    #[test]
    fn osc_133_new_command_extra_contents() {
        let mut p = run(b"133;Nextra");
        assert!(p.end(None).is_none());
    }

    #[test]
    fn osc_133_end_prompt_start_input() {
        let mut p = run(b"133;B");
        let cmd = sp(p.end(None));
        assert_eq!(Action::EndPromptStartInput, cmd.action);
    }

    #[test]
    fn osc_133_end_prompt_start_input_extra_contents() {
        let mut p = run(b"133;Bextra");
        assert!(p.end(None).is_none());
    }

    #[test]
    fn osc_133_end_prompt_start_input_with_options() {
        let mut p = run(b"133;B;aid=foo");
        let cmd = sp(p.end(None));
        assert_eq!(Action::EndPromptStartInput, cmd.action);
        assert_eq!(b"foo", cmd.read_option::<o::Aid>().unwrap());
    }

    #[test]
    fn osc_133_end_prompt_start_input_terminate_eol() {
        let mut p = run(b"133;I");
        let cmd = sp(p.end(None));
        assert_eq!(Action::EndPromptStartInputTerminateEol, cmd.action);
    }

    #[test]
    fn osc_133_end_prompt_start_input_terminate_eol_extra_contents() {
        let mut p = run(b"133;Iextra");
        assert!(p.end(None).is_none());
    }

    #[test]
    fn osc_133_end_prompt_start_input_terminate_eol_with_options() {
        let mut p = run(b"133;I;aid=foo");
        let cmd = sp(p.end(None));
        assert_eq!(Action::EndPromptStartInputTerminateEol, cmd.action);
        assert_eq!(b"foo", cmd.read_option::<o::Aid>().unwrap());
    }

    #[test]
    fn osc_133_end_command() {
        let mut p = run(b"133;D");
        let cmd = sp(p.end(None));
        assert_eq!(Action::EndCommand, cmd.action);
        assert!(cmd.read_option::<o::ExitCode>().is_none());
        assert!(cmd.read_option::<o::Aid>().is_none());
        assert!(cmd.read_option::<o::Err>().is_none());
    }

    #[test]
    fn osc_133_end_command_extra_contents() {
        let mut p = run(b"133;Dextra");
        assert!(p.end(None).is_none());
    }

    #[test]
    fn osc_133_end_command_with_exit_code_0() {
        let mut p = run(b"133;D;0");
        let cmd = sp(p.end(None));
        assert_eq!(Action::EndCommand, cmd.action);
        assert_eq!(Some(0), cmd.read_option::<o::ExitCode>());
    }

    #[test]
    fn osc_133_end_command_with_exit_code_and_aid() {
        let mut p = run(b"133;D;12;aid=foo");
        let cmd = sp(p.end(None));
        assert_eq!(Action::EndCommand, cmd.action);
        assert_eq!(b"foo", cmd.read_option::<o::Aid>().unwrap());
        assert_eq!(Some(12), cmd.read_option::<o::ExitCode>());
    }

    #[test]
    fn option_read_aid() {
        assert_eq!(b"test123", o::Aid::read(b"aid=test123").unwrap());
        assert_eq!(b"myaid", o::Aid::read(b"cl=line;aid=myaid;k=i").unwrap());
        assert!(o::Aid::read(b"cl=line;k=i").is_none());
        assert_eq!(b"", o::Aid::read(b"aid=").unwrap());
        assert_eq!(b"last", o::Aid::read(b"k=i;aid=last").unwrap());
        assert_eq!(b"first", o::Aid::read(b"aid=first;k=i").unwrap());
        assert!(o::Aid::read(b"").is_none());
        assert!(o::Aid::read(b"aid").is_none());
        assert_eq!(b"value", o::Aid::read(b";;aid=value;;").unwrap());
    }

    #[test]
    fn option_read_cl() {
        assert_eq!(Click::Line, o::Cl::read(b"cl=line").unwrap());
        assert_eq!(Click::Multiple, o::Cl::read(b"cl=m").unwrap());
        assert_eq!(Click::ConservativeVertical, o::Cl::read(b"cl=v").unwrap());
        assert_eq!(Click::SmartVertical, o::Cl::read(b"cl=w").unwrap());
        assert!(o::Cl::read(b"cl=invalid").is_none());
        assert!(o::Cl::read(b"aid=foo").is_none());
    }

    #[test]
    fn option_read_prompt_kind() {
        assert_eq!(PromptKind::Initial, o::PromptKind::read(b"k=i").unwrap());
        assert_eq!(PromptKind::Right, o::PromptKind::read(b"k=r").unwrap());
        assert_eq!(PromptKind::Continuation, o::PromptKind::read(b"k=c").unwrap());
        assert_eq!(PromptKind::Secondary, o::PromptKind::read(b"k=s").unwrap());
        assert!(o::PromptKind::read(b"k=x").is_none());
        assert!(o::PromptKind::read(b"k=ii").is_none());
        assert!(o::PromptKind::read(b"k=").is_none());
    }

    #[test]
    fn option_read_err() {
        assert_eq!(b"some_error", o::Err::read(b"err=some_error").unwrap());
        assert!(o::Err::read(b"aid=foo").is_none());
    }

    #[test]
    fn option_read_redraw() {
        assert_eq!(Redraw::True, o::Redraw::read(b"redraw=1").unwrap());
        assert_eq!(Redraw::False, o::Redraw::read(b"redraw=0").unwrap());
        assert_eq!(Redraw::Last, o::Redraw::read(b"redraw=last").unwrap());
        assert!(o::Redraw::read(b"redraw=2").is_none());
        assert!(o::Redraw::read(b"redraw=10").is_none());
        assert!(o::Redraw::read(b"redraw=").is_none());
    }

    #[test]
    fn option_read_special_key() {
        assert!(o::SpecialKey::read(b"special_key=1").unwrap());
        assert!(!o::SpecialKey::read(b"special_key=0").unwrap());
        assert!(o::SpecialKey::read(b"special_key=x").is_none());
    }

    #[test]
    fn option_read_click_events() {
        assert!(o::ClickEvents::read(b"click_events=yes").is_none());
        assert!(o::ClickEvents::read(b"click_events=0").is_none());
        assert_eq!(ClickEvents::Absolute, o::ClickEvents::read(b"click_events=1").unwrap());
        assert_eq!(ClickEvents::Relative, o::ClickEvents::read(b"click_events=2").unwrap());
    }

    #[test]
    fn option_read_exit_code() {
        assert_eq!(42, o::ExitCode::read(b"42").unwrap());
        assert_eq!(0, o::ExitCode::read(b"0").unwrap());
        assert_eq!(-1, o::ExitCode::read(b"-1").unwrap());
        assert!(o::ExitCode::read(b"abc").is_none());
        assert_eq!(127, o::ExitCode::read(b"127;aid=foo").unwrap());
    }

    #[test]
    fn osc_133_exit_codes_reject_digit_separators() {
        let mut p = run(b"133;D;-4_2;aid=foo");
        let cmd = sp(p.end(None));
        assert_eq!(None, cmd.read_option::<o::ExitCode>());
    }
}
