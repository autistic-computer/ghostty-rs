//! Port of `src/terminal/osc/parsers/hyperlink.zig`.
//!
//! STATUS: ported

use crate::terminal::color::index_of_scalar_pos;
use crate::terminal::osc::{Command, HyperlinkStart, Parser, State};

const LOG_TARGET: &str = "osc_hyperlink";

/// Parse OSC 8 hyperlinks
pub fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    let Some(cap) = parser.capture.as_mut() else {
        parser.state = State::Invalid;
        return None;
    };
    if cap.writer_write_byte(0).is_err() {
        parser.state = State::Invalid;
        return None;
    }
    let data = cap.trailing_mut();
    let Some(s) = data.iter().position(|&c| c == b';') else {
        parser.state = State::Invalid;
        return None;
    };

    data[s] = 0;
    for c in data[..s + 1].iter_mut() {
        if *c == b':' {
            *c = 0;
        }
    }
    let data: &[u8] = data;

    let mut start = HyperlinkStart {
        id: None,
        uri: &data[s + 1..data.len() - 1],
    };

    let kvs = &data[..s + 1];
    let mut kv_start: usize = 0;
    while kv_start < kvs.len() {
        let Some(kv_end) = index_of_scalar_pos(kvs, kv_start + 1, 0) else {
            break;
        };
        let kv = &data[kv_start..kv_end + 1];
        let Some(v) = kv.iter().position(|&c| c == b'=') else {
            break;
        };
        let key = &kv[..v];
        let value = &kv[v + 1..kv.len() - 1];
        if key == b"id" {
            if !value.is_empty() {
                start.id = Some(value);
            }
        } else {
            log::warn!(
                target: LOG_TARGET,
                "unknown hyperlink option: '{}'",
                String::from_utf8_lossy(key)
            );
        }
        kv_start = kv_end + 1;
    }

    if start.uri.is_empty() {
        if start.id.is_some() {
            parser.state = State::Invalid;
            return None;
        }
        return Some(Command::HyperlinkEnd);
    }

    Some(Command::HyperlinkStart(start))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_input(p: &mut Parser, input: &[u8]) {
        for &ch in input {
            p.next(ch);
        }
    }

    #[test]
    fn osc_8_hyperlink() {
        let mut p = Parser::init(None);
        parse_input(&mut p, b"8;;http://example.com");

        let Some(Command::HyperlinkStart(h)) = p.end(Some(0x1b)) else {
            panic!("expected hyperlink_start");
        };
        assert_eq!(h.uri, b"http://example.com");
    }

    #[test]
    fn osc_8_hyperlink_with_id_set() {
        let mut p = Parser::init(None);
        parse_input(&mut p, b"8;id=foo;http://example.com");

        let Some(Command::HyperlinkStart(h)) = p.end(Some(0x1b)) else {
            panic!("expected hyperlink_start");
        };
        assert_eq!(h.id.unwrap(), b"foo");
        assert_eq!(h.uri, b"http://example.com");
    }

    #[test]
    fn osc_8_hyperlink_with_empty_id() {
        let mut p = Parser::init(None);
        parse_input(&mut p, b"8;id=;http://example.com");

        let Some(Command::HyperlinkStart(h)) = p.end(Some(0x1b)) else {
            panic!("expected hyperlink_start");
        };
        assert_eq!(None, h.id);
        assert_eq!(h.uri, b"http://example.com");
    }

    #[test]
    fn osc_8_hyperlink_with_incomplete_key() {
        let mut p = Parser::init(None);
        parse_input(&mut p, b"8;id;http://example.com");

        let Some(Command::HyperlinkStart(h)) = p.end(Some(0x1b)) else {
            panic!("expected hyperlink_start");
        };
        assert_eq!(None, h.id);
        assert_eq!(h.uri, b"http://example.com");
    }

    #[test]
    fn osc_8_hyperlink_with_empty_key() {
        let mut p = Parser::init(None);
        parse_input(&mut p, b"8;=value;http://example.com");

        let Some(Command::HyperlinkStart(h)) = p.end(Some(0x1b)) else {
            panic!("expected hyperlink_start");
        };
        assert_eq!(None, h.id);
        assert_eq!(h.uri, b"http://example.com");
    }

    #[test]
    fn osc_8_hyperlink_with_empty_key_and_id() {
        let mut p = Parser::init(None);
        parse_input(&mut p, b"8;=value:id=foo;http://example.com");

        let Some(Command::HyperlinkStart(h)) = p.end(Some(0x1b)) else {
            panic!("expected hyperlink_start");
        };
        assert_eq!(h.id.unwrap(), b"foo");
        assert_eq!(h.uri, b"http://example.com");
    }

    #[test]
    fn osc_8_hyperlink_with_empty_uri() {
        let mut p = Parser::init(None);
        parse_input(&mut p, b"8;id=foo;");

        assert!(p.end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_8_hyperlink_end() {
        let mut p = Parser::init(None);
        parse_input(&mut p, b"8;;");

        let cmd = p.end(Some(0x1b)).unwrap();
        assert_eq!(Command::HyperlinkEnd, cmd);
    }
}
