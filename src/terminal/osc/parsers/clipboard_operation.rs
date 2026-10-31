//! Port of `src/terminal/osc/parsers/clipboard_operation.zig`.
//!
//! STATUS: ported

use crate::terminal::osc::{ClipboardContents, Command, Parser, State, Terminator};

/// Parse OSC 52
pub fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    debug_assert!(parser.state == State::Osc52);
    let Some(cap) = parser.capture.as_mut() else {
        parser.state = State::Invalid;
        return None;
    };
    if cap.write_byte(0).is_err() {
        parser.state = State::Invalid;
        return None;
    }
    let data = cap.trailing();
    if data.len() == 1 {
        parser.state = State::Invalid;
        return None;
    }
    if data[0] == b';' {
        Some(Command::ClipboardContents(ClipboardContents {
            kind: b'c',
            data: &data[1..data.len() - 1],
            terminator: Terminator::init(terminator_ch),
        }))
    } else {
        if data.len() < 2 {
            parser.state = State::Invalid;
            return None;
        }
        if data[1] != b';' {
            parser.state = State::Invalid;
            return None;
        }
        Some(Command::ClipboardContents(ClipboardContents {
            kind: data[0],
            data: &data[2..data.len() - 1],
            terminator: Terminator::init(terminator_ch),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::osc::Allocator;

    fn clipboard(cmd: Option<Command<'_>>) -> ClipboardContents<'_> {
        match cmd {
            Some(Command::ClipboardContents(c)) => c,
            other => panic!("expected clipboard_contents, got {:?}", other),
        }
    }

    #[test]
    fn osc_52_get_set_clipboard() {
        let mut p = Parser::init(None);

        for &ch in b"52;s;?" {
            p.next(ch);
        }

        let cmd = clipboard(p.end(None));
        assert_eq!(b's', cmd.kind);
        assert_eq!(b"?", cmd.data);
        assert_eq!(Terminator::St, cmd.terminator);
    }

    #[test]
    fn osc_52_get_clipboard_with_bel_terminator() {
        let mut p = Parser::init(None);

        for &ch in b"52;c;?" {
            p.next(ch);
        }

        let cmd = clipboard(p.end(Some(0x07)));
        assert_eq!(Terminator::Bel, cmd.terminator);
    }

    #[test]
    fn osc_52_get_set_clipboard_optional_parameter() {
        let mut p = Parser::init(None);

        for &ch in b"52;;?" {
            p.next(ch);
        }

        let cmd = clipboard(p.end(None));
        assert_eq!(b'c', cmd.kind);
        assert_eq!(b"?", cmd.data);
    }

    #[test]
    fn osc_52_get_set_clipboard_with_allocator() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        for &ch in b"52;s;?" {
            p.next(ch);
        }

        let cmd = clipboard(p.end(None));
        assert_eq!(b's', cmd.kind);
        assert_eq!(b"?", cmd.data);
    }

    #[test]
    fn osc_52_clear_clipboard() {
        let mut p = Parser::init(None);

        for &ch in b"52;;" {
            p.next(ch);
        }

        let cmd = clipboard(p.end(None));
        assert_eq!(b'c', cmd.kind);
        assert_eq!(b"", cmd.data);
    }
}
