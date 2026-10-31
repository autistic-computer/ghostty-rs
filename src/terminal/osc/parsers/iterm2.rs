//! Port of `src/terminal/osc/parsers/iterm2.zig`.
//!
//! STATUS: ported

use crate::terminal::osc::{ClipboardContents, Command, Parser, ReportPwd, State, Terminator};

const LOG_TARGET: &str = "osc_iterm2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::enum_variant_names)]
enum Key {
    AddAnnotation,
    AddHiddenAnnotation,
    Block,
    Button,
    ClearCapturedOutput,
    ClearScrollback,
    Copy,
    CopyToClipboard,
    CurrentDir,
    CursorShape,
    Custom,
    Disinter,
    EndCopy,
    File,
    FileEnd,
    FilePart,
    HighlightCursorLine,
    MultipartFile,
    OpenURL,
    PopKeyLabels,
    PushKeyLabels,
    RemoteHost,
    ReportCellSize,
    ReportVariable,
    RequestAttention,
    RequestUpload,
    SetBackgroundImageFile,
    SetBadgeFormat,
    SetColors,
    SetKeyLabel,
    SetMark,
    SetProfile,
    SetUserVar,
    ShellIntegrationVersion,
    StealFocus,
    UnicodeVersion,
}

/// Instead of an exact string-to-enum match, the map uses ASCII
/// case-insensitive lookups.
const MAP: [(&str, Key); 36] = [
    ("AddAnnotation", Key::AddAnnotation),
    ("AddHiddenAnnotation", Key::AddHiddenAnnotation),
    ("Block", Key::Block),
    ("Button", Key::Button),
    ("ClearCapturedOutput", Key::ClearCapturedOutput),
    ("ClearScrollback", Key::ClearScrollback),
    ("Copy", Key::Copy),
    ("CopyToClipboard", Key::CopyToClipboard),
    ("CurrentDir", Key::CurrentDir),
    ("CursorShape", Key::CursorShape),
    ("Custom", Key::Custom),
    ("Disinter", Key::Disinter),
    ("EndCopy", Key::EndCopy),
    ("File", Key::File),
    ("FileEnd", Key::FileEnd),
    ("FilePart", Key::FilePart),
    ("HighlightCursorLine", Key::HighlightCursorLine),
    ("MultipartFile", Key::MultipartFile),
    ("OpenURL", Key::OpenURL),
    ("PopKeyLabels", Key::PopKeyLabels),
    ("PushKeyLabels", Key::PushKeyLabels),
    ("RemoteHost", Key::RemoteHost),
    ("ReportCellSize", Key::ReportCellSize),
    ("ReportVariable", Key::ReportVariable),
    ("RequestAttention", Key::RequestAttention),
    ("RequestUpload", Key::RequestUpload),
    ("SetBackgroundImageFile", Key::SetBackgroundImageFile),
    ("SetBadgeFormat", Key::SetBadgeFormat),
    ("SetColors", Key::SetColors),
    ("SetKeyLabel", Key::SetKeyLabel),
    ("SetMark", Key::SetMark),
    ("SetProfile", Key::SetProfile),
    ("SetUserVar", Key::SetUserVar),
    ("ShellIntegrationVersion", Key::ShellIntegrationVersion),
    ("StealFocus", Key::StealFocus),
    ("UnicodeVersion", Key::UnicodeVersion),
];

fn map_get(key: &[u8]) -> Option<Key> {
    MAP.iter()
        .find(|(name, _)| name.as_bytes().eq_ignore_ascii_case(key))
        .map(|&(_, k)| k)
}

/// Parse OSC 1337
/// https://iterm2.com/documentation-escape-codes.html
pub fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    debug_assert!(parser.state == State::Osc1337);

    let Some(cap) = parser.capture.as_mut() else {
        parser.state = State::Invalid;
        return None;
    };
    if cap.writer_write_byte(0).is_err() {
        parser.state = State::Invalid;
        return None;
    }
    let data = cap.trailing_mut();

    let index = data.iter().position(|&c| c == b'=');
    if let Some(index) = index {
        data[index] = 0;
    }
    let data: &[u8] = data;
    let (key_str, value_): (&[u8], Option<&[u8]>) = match index {
        None => (&data[..data.len() - 1], None),
        Some(index) => (&data[..index], Some(&data[index + 1..data.len() - 1])),
    };

    let key = map_get(key_str)?;

    match key {
        Key::Copy => {
            let mut value = value_?;

            // Sending a blank entry to clear the clipboard is an OSC 52-ism,
            // make sure that is invalid here.
            if value.is_empty() {
                return None;
            }

            // base64 value must be prefixed by a colon
            if value[0] != b':' {
                return None;
            }

            value = &value[1..];

            // Sending a blank entry to clear the clipboard is an OSC 52-ism,
            // make sure that is invalid here.
            if value.is_empty() {
                return None;
            }

            // Sending a '?' to query the clipboard is an OSC 52-ism, make sure
            // that is invalid here.
            if value.len() == 1 && value[0] == b'?' {
                return None;
            }

            // It would be better to check for valid base64 data here, but that
            // would mean parsing the base64 data twice in the "normal" case.

            Some(Command::ClipboardContents(ClipboardContents {
                kind: b'c',
                data: value,
                terminator: Terminator::St,
            }))
        }

        Key::CurrentDir => {
            let value = value_?;
            if value.is_empty() {
                return None;
            }
            Some(Command::ReportPwd(ReportPwd { value }))
        }

        Key::AddAnnotation
        | Key::AddHiddenAnnotation
        | Key::Block
        | Key::Button
        | Key::ClearCapturedOutput
        | Key::ClearScrollback
        | Key::CopyToClipboard
        | Key::CursorShape
        | Key::Custom
        | Key::Disinter
        | Key::EndCopy
        | Key::File
        | Key::FileEnd
        | Key::FilePart
        | Key::HighlightCursorLine
        | Key::MultipartFile
        | Key::OpenURL
        | Key::PopKeyLabels
        | Key::PushKeyLabels
        | Key::RemoteHost
        | Key::ReportCellSize
        | Key::ReportVariable
        | Key::RequestAttention
        | Key::RequestUpload
        | Key::SetBackgroundImageFile
        | Key::SetBadgeFormat
        | Key::SetColors
        | Key::SetKeyLabel
        | Key::SetMark
        | Key::SetProfile
        | Key::SetUserVar
        | Key::ShellIntegrationVersion
        | Key::StealFocus
        | Key::UnicodeVersion => {
            log::debug!(target: LOG_TARGET, "unimplemented OSC 1337: {:?}", key);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::osc::Allocator;

    fn run(input: &[u8]) -> Parser {
        let mut p = Parser::init(Some(Allocator::GLOBAL));
        for &ch in input {
            p.next(ch);
        }
        p
    }

    #[test]
    fn osc_1337_test_valid_unimplemented_key_with_no_value() {
        assert!(run(b"1337;SetBadgeFormat").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_valid_unimplemented_key_with_empty_value() {
        assert!(run(b"1337;SetBadgeFormat=").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_valid_unimplemented_key_with_non_empty_value() {
        assert!(run(b"1337;SetBadgeFormat=abc123").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_valid_key_with_lower_case_and_with_no_value() {
        assert!(run(b"1337;setbadgeformat").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_valid_key_with_lower_case_and_with_empty_value() {
        assert!(run(b"1337;setbadgeformat=").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_valid_key_with_lower_case_and_with_non_empty_value() {
        assert!(run(b"1337;setbadgeformat=abc123").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_invalid_key_with_no_value() {
        assert!(run(b"1337;BobrKurwa").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_invalid_key_with_empty_value() {
        assert!(run(b"1337;BobrKurwa=").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_invalid_key_with_non_empty_value() {
        assert!(run(b"1337;BobrKurwa=abc123").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_copy_with_no_value() {
        assert!(run(b"1337;Copy").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_copy_with_empty_value() {
        assert!(run(b"1337;Copy=").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_copy_with_only_prefix_colon() {
        assert!(run(b"1337;Copy=:").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_copy_with_question_mark() {
        assert!(run(b"1337;Copy=:?").end(Some(0x1b)).is_none());
    }

    #[test]
    #[ignore = "For performance reasons, we don't check for valid base64 data right now."]
    fn osc_1337_test_copy_with_non_empty_value_that_is_invalid_base64() {
        assert!(run(b"1337;Copy=:abc123").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_copy_with_non_empty_value_that_is_valid_base64_but_not_prefixed_with_a_colon(
    ) {
        assert!(run(b"1337;Copy=YWJjMTIz").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_copy_with_non_empty_value_that_is_valid_base64() {
        let mut p = run(b"1337;Copy=:YWJjMTIz");
        let Some(Command::ClipboardContents(c)) = p.end(Some(0x1b)) else {
            panic!("expected clipboard_contents");
        };
        assert_eq!(b'c', c.kind);
        assert_eq!(b"YWJjMTIz", c.data);
    }

    #[test]
    fn osc_1337_test_currentdir_with_no_value() {
        assert!(run(b"1337;CurrentDir").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_currentdir_with_empty_value() {
        assert!(run(b"1337;CurrentDir=").end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_1337_test_currentdir_with_non_empty_value() {
        let mut p = run(b"1337;CurrentDir=abc123");
        assert_eq!(
            Some(Command::ReportPwd(ReportPwd { value: b"abc123" })),
            p.end(Some(0x1b))
        );
    }
}
