//! Port of `src/terminal/osc/parsers/change_window_icon.zig`.
//!
//! STATUS: ported

use crate::terminal::osc::{Command, Parser, State};

/// Parse OSC 1
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
    Some(Command::ChangeWindowIcon(&data[..data.len() - 1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc_1_change_window_icon() {
        let mut p = Parser::init(None);
        p.next(b'1');
        p.next(b';');
        p.next(b'a');
        p.next(b'b');
        let cmd = p.end(None).unwrap();
        assert_eq!(Command::ChangeWindowIcon(b"ab"), cmd);
    }
}
