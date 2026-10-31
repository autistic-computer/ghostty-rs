//! Port of `src/terminal/osc/parsers/mouse_shape.zig`.
//!
//! STATUS: ported

use crate::terminal::osc::{Command, MouseShape, Parser, State};

// Parse OSC 22
pub fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    debug_assert!(parser.state == State::Osc22);
    let Some(cap) = parser.capture.as_mut() else {
        parser.state = State::Invalid;
        return None;
    };
    if cap.writer_write_byte(0).is_err() {
        parser.state = State::Invalid;
        return None;
    }
    let data = cap.trailing();
    Some(Command::MouseShape(MouseShape {
        value: &data[..data.len() - 1],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc_22_pointer_cursor() {
        let mut p = Parser::init(None);

        for &ch in b"22;pointer" {
            p.next(ch);
        }

        let cmd = p.end(None).unwrap();
        assert_eq!(Command::MouseShape(MouseShape { value: b"pointer" }), cmd);
    }
}
