//! Port of `src/terminal/osc/parsers/change_window_title.zig`.
//!
//! STATUS: ported

use crate::terminal::osc::{Command, Parser, State};

/// Parse OSC 0 and OSC 2
pub fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    let Some(cap) = parser.capture.as_mut() else {
        parser.state = State::Invalid;
        return None;
    };
    if cap.writer_write_byte(0).is_err() {
        parser.state = State::Invalid;
        return None;
    }
    let data = cap.trailing();
    Some(Command::ChangeWindowTitle(&data[..data.len() - 1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc_0_change_window_title() {
        let mut p = Parser::init(None);
        p.next(b'0');
        p.next(b';');
        p.next(b'a');
        p.next(b'b');
        let cmd = p.end(None).unwrap();
        assert_eq!(Command::ChangeWindowTitle(b"ab"), cmd);
    }

    #[test]
    fn osc_0_longer_than_buffer() {
        let mut p = Parser::init(None);

        for &ch in b"0;" {
            p.next(ch);
        }
        for _ in 0..Parser::MAX_BUF + 2 {
            p.next(b'a');
        }

        assert!(p.end(None).is_none());
    }

    #[test]
    fn osc_0_one_shorter_than_buffer_length() {
        let mut p = Parser::init(None);

        let title = [b'a'; Parser::MAX_BUF - 1];
        for &ch in b"0;" {
            p.next(ch);
        }
        for &ch in &title {
            p.next(ch);
        }

        let cmd = p.end(None).unwrap();
        assert_eq!(Command::ChangeWindowTitle(&title), cmd);
    }

    #[test]
    fn osc_0_exactly_at_buffer_length() {
        let mut p = Parser::init(None);

        for &ch in b"0;" {
            p.next(ch);
        }
        for _ in 0..Parser::MAX_BUF {
            p.next(b'a');
        }

        // This should be null because we always reserve space for a null terminator.
        assert!(p.end(None).is_none());
    }

    #[test]
    fn osc_2_change_window_title_with_2() {
        let mut p = Parser::init(None);
        p.next(b'2');
        p.next(b';');
        p.next(b'a');
        p.next(b'b');
        let cmd = p.end(None).unwrap();
        assert_eq!(Command::ChangeWindowTitle(b"ab"), cmd);
    }

    #[test]
    fn osc_2_change_window_title_with_utf8() {
        let mut p = Parser::init(None);
        p.next(b'2');
        p.next(b';');
        // '—' EM DASH U+2014 (E2 80 94)
        p.next(0xE2);
        p.next(0x80);
        p.next(0x94);

        p.next(b' ');
        // '‐' HYPHEN U+2010 (E2 80 90)
        // Intententionally chosen to conflict with the 0x90 C1 control
        p.next(0xE2);
        p.next(0x80);
        p.next(0x90);
        let cmd = p.end(None).unwrap();
        assert_eq!(Command::ChangeWindowTitle("— ‐".as_bytes()), cmd);
    }

    #[test]
    fn osc_2_change_window_title_empty() {
        let mut p = Parser::init(None);
        p.next(b'2');
        p.next(b';');
        let cmd = p.end(None).unwrap();
        assert_eq!(Command::ChangeWindowTitle(b""), cmd);
    }
}
