//! Port of `src/terminal/osc/parsers/kitty_color.zig`.
//!
//! STATUS: ported

use crate::terminal::color::{trim, Rgb};
use crate::terminal::kitty::color as kitty_color;
use crate::terminal::osc::{Command, Parser, State, Terminator};

const LOG_TARGET: &str = "osc_kitty_color";

/// Parse OSC 21, the Kitty Color Protocol.
pub fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    debug_assert!(parser.state == State::Osc21);

    if parser.alloc.is_none() {
        parser.state = State::Invalid;
        return None;
    }
    let Some(cap) = parser.capture.as_ref() else {
        parser.state = State::Invalid;
        return None;
    };
    let mut result = kitty_color::Osc {
        list: Vec::new(),
        terminator: Terminator::init(terminator_ch),
    };
    let list = &mut result.list;
    let data = cap.trailing();
    for kv in data.split(|&c| c == b';') {
        if list.len() >= kitty_color::Kind::MAX * 2 {
            log::warn!(
                target: LOG_TARGET,
                "exceeded limit for number of keys in kitty color protocol, ignoring"
            );
            parser.state = State::Invalid;
            return None;
        }
        let mut it = kv.splitn(2, |&c| c == b'=');
        let Some(k) = it.next() else {
            continue;
        };
        if k.is_empty() {
            log::warn!(target: LOG_TARGET, "zero length key in kitty color protocol");
            continue;
        }
        let Some(key) = kitty_color::Kind::parse(k) else {
            log::warn!(
                target: LOG_TARGET,
                "unknown key in kitty color protocol: {}",
                String::from_utf8_lossy(k)
            );
            continue;
        };
        let value = trim(it.next().unwrap_or(b""), b" ");
        if value.is_empty() {
            list.push(kitty_color::Request::Reset(key));
        } else if value == b"?" {
            list.push(kitty_color::Request::Query(key));
        } else {
            let color = match Rgb::parse(value) {
                Ok(c) => c,
                Err(_) => {
                    log::warn!(
                        target: LOG_TARGET,
                        "invalid color format in kitty color protocol: {}",
                        String::from_utf8_lossy(value)
                    );
                    continue;
                }
            };
            list.push(kitty_color::Request::Set { key, color });
        }
    }
    Some(Command::KittyColorProtocol(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::kitty::color::{Kind, Request, Special};
    use crate::terminal::osc::Allocator;

    const INPUT: &[u8] = b"21;foreground=?;background=rgb:f0/f8/ff;cursor=aliceblue;cursor_text;visual_bell=;selection_foreground=#xxxyyzz;selection_background=?;selection_background=#aabbcc;2=?;3=rgbi:1.0/1.0/1.0";

    fn run(alloc: Option<Allocator>, input: &[u8]) -> Parser {
        let mut p = Parser::init(alloc);
        for &ch in input {
            p.next(ch);
        }
        p
    }

    #[test]
    fn osc_21_kitty_color_protocol() {
        let mut p = run(Some(Allocator::GLOBAL), INPUT);

        let Some(Command::KittyColorProtocol(cmd)) = p.end(Some(0x1b)) else {
            panic!("expected kitty_color_protocol");
        };
        assert_eq!(9, cmd.list.len());
        assert_eq!(Request::Query(Kind::Special(Special::Foreground)), cmd.list[0]);
        assert_eq!(
            Request::Set {
                key: Kind::Special(Special::Background),
                color: Rgb { r: 0xf0, g: 0xf8, b: 0xff }
            },
            cmd.list[1]
        );
        assert_eq!(
            Request::Set {
                key: Kind::Special(Special::Cursor),
                color: Rgb { r: 0xf0, g: 0xf8, b: 0xff }
            },
            cmd.list[2]
        );
        assert_eq!(Request::Reset(Kind::Special(Special::CursorText)), cmd.list[3]);
        assert_eq!(Request::Reset(Kind::Special(Special::VisualBell)), cmd.list[4]);
        assert_eq!(
            Request::Query(Kind::Special(Special::SelectionBackground)),
            cmd.list[5]
        );
        assert_eq!(
            Request::Set {
                key: Kind::Special(Special::SelectionBackground),
                color: Rgb { r: 0xaa, g: 0xbb, b: 0xcc }
            },
            cmd.list[6]
        );
        assert_eq!(Request::Query(Kind::Palette(2)), cmd.list[7]);
        assert_eq!(
            Request::Set {
                key: Kind::Palette(3),
                color: Rgb { r: 0xff, g: 0xff, b: 0xff }
            },
            cmd.list[8]
        );
    }

    #[test]
    fn osc_21_kitty_color_protocol_without_allocator() {
        let mut p = run(None, b"21;foreground=?");
        assert!(p.end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_21_kitty_color_protocol_double_reset() {
        let mut p = run(Some(Allocator::GLOBAL), INPUT);

        let cmd = p.end(Some(0x1b));
        assert!(matches!(cmd, Some(Command::KittyColorProtocol(_))));

        p.reset();
        p.reset();
    }

    #[test]
    fn osc_21_kitty_color_protocol_reset_after_invalid() {
        let mut p = run(Some(Allocator::GLOBAL), INPUT);

        let cmd = p.end(Some(0x1b));
        assert!(matches!(cmd, Some(Command::KittyColorProtocol(_))));

        p.reset();

        assert_eq!(State::Start, p.state);
        p.next(b'X');
        assert_eq!(State::Invalid, p.state);

        p.reset();
    }

    #[test]
    fn osc_21_kitty_color_protocol_no_key() {
        let mut p = run(Some(Allocator::GLOBAL), b"21;");

        let Some(Command::KittyColorProtocol(cmd)) = p.end(Some(0x1b)) else {
            panic!("expected kitty_color_protocol");
        };
        assert_eq!(0, cmd.list.len());
    }
}
