//! Port of `src/terminal/osc/parsers/kitty_text_sizing.zig`.
//!
//! STATUS: ported
//!
//! Kitty's text sizing protocol (OSC 66)
//! Specification: https://sw.kovidgoyal.net/kitty/text-sizing-protocol/

use crate::lib::parse_int::parse_int_bits_unsigned;
use crate::terminal::osc::encoding;
use crate::terminal::osc::{Command, Parser, State};

const LOG_TARGET: &str = "kitty_text_sizing";

pub const MAX_PAYLOAD_LENGTH: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub enum VAlign {
    #[default]
    Top,
    Bottom,
    Center,
}

impl VAlign {
    /// Zig `std.enums.fromInt`.
    pub fn from_int(v: u64) -> Option<VAlign> {
        Some(match v {
            0 => VAlign::Top,
            1 => VAlign::Bottom,
            2 => VAlign::Center,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub enum HAlign {
    #[default]
    Left,
    Right,
    Center,
}

impl HAlign {
    /// Zig `std.enums.fromInt`.
    pub fn from_int(v: u64) -> Option<HAlign> {
        Some(match v {
            0 => HAlign::Left,
            1 => HAlign::Right,
            2 => HAlign::Center,
            _ => return None,
        })
    }
}

/// A Kitty text sizing command (OSC 66).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Osc<'a> {
    /// 1 - 7 (Zig `u3`)
    pub scale: u8,
    /// 0 - 7 (0 means default) (Zig `u3`)
    pub width: u8,
    /// Zig `u4`
    pub numerator: u8,
    /// Zig `u4`
    pub denominator: u8,
    pub valign: VAlign,
    pub halign: HAlign,
    pub text: &'a [u8],
}

/// We don't currently support encoding this to C in any way.
pub type OscC = ();

/// Errors from [`Osc::update`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateError {
    UnknownKey,
    InvalidValue,
}

impl<'a> Osc<'a> {
    /// A command with default values and the given text.
    pub fn new(text: &'a [u8]) -> Osc<'a> {
        Osc {
            scale: 1,
            width: 0,
            numerator: 0,
            denominator: 0,
            valign: VAlign::Top,
            halign: HAlign::Left,
            text,
        }
    }

    pub fn cval(&self) -> OscC {}

    fn update(&mut self, key: u8, value: &[u8]) -> Result<(), UpdateError> {
        // All values are numeric, so we can do a small hack here
        let v = parse_int_bits_unsigned(value, 10, 4).map_err(|_| UpdateError::InvalidValue)? as u8;

        match key {
            b's' => {
                if v == 0 {
                    return Err(UpdateError::InvalidValue);
                }
                if v > 7 {
                    return Err(UpdateError::InvalidValue);
                }
                self.scale = v;
            }
            b'w' => {
                if v > 7 {
                    return Err(UpdateError::InvalidValue);
                }
                self.width = v;
            }
            b'n' => self.numerator = v,
            b'd' => self.denominator = v,
            b'v' => self.valign = VAlign::from_int(v as u64).ok_or(UpdateError::InvalidValue)?,
            b'h' => self.halign = HAlign::from_int(v as u64).ok_or(UpdateError::InvalidValue)?,
            _ => return Err(UpdateError::UnknownKey),
        }
        Ok(())
    }
}

pub fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    debug_assert!(parser.state == State::Osc66);

    let Some(cap) = parser.capture.as_mut() else {
        parser.state = State::Invalid;
        return None;
    };

    // Write a NUL byte to ensure that `text` is NUL-terminated
    if cap.write_byte(0).is_err() {
        parser.state = State::Invalid;
        return None;
    }
    let data = cap.trailing();

    let Some(payload_start) = data.iter().position(|&c| c == b';') else {
        log::warn!(target: LOG_TARGET, "missing semicolon before payload");
        parser.state = State::Invalid;
        return None;
    };
    let payload = &data[payload_start + 1..data.len() - 1];

    // Payload has to be a URL-safe UTF-8 string,
    // and be under the size limit.
    if payload.len() > MAX_PAYLOAD_LENGTH {
        log::warn!(target: LOG_TARGET, "payload is too long");
        parser.state = State::Invalid;
        return None;
    }
    if !encoding::is_safe_utf8(payload) {
        log::warn!(target: LOG_TARGET, "payload is not escape code safe UTF-8");
        parser.state = State::Invalid;
        return None;
    }

    let mut cmd = Osc::new(payload);

    // Parse any arguments if given
    if payload_start > 0 {
        for kv in data[..payload_start].split(|&c| c == b':') {
            let mut it = kv.split(|&c| c == b'=');
            let Some(k) = it.next() else {
                log::warn!(target: LOG_TARGET, "missing key");
                continue;
            };
            if k.len() != 1 {
                log::warn!(target: LOG_TARGET, "key must be a single character");
                continue;
            }

            let Some(value) = it.next() else {
                log::warn!(target: LOG_TARGET, "missing value");
                continue;
            };

            if let Err(err) = cmd.update(k[0], value) {
                match err {
                    UpdateError::UnknownKey => {
                        log::warn!(target: LOG_TARGET, "unknown key: '{}'", k[0] as char)
                    }
                    UpdateError::InvalidValue => log::warn!(
                        target: LOG_TARGET,
                        "invalid value for key '{}': {:?}",
                        k[0] as char,
                        err
                    ),
                }
                continue;
            }
        }
    }

    Some(Command::KittyTextSizing(cmd))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8]) -> Parser {
        let mut p = Parser::init(None);
        for &ch in input {
            p.next(ch);
        }
        p
    }

    fn ts<'a>(cmd: Option<Command<'a>>) -> Osc<'a> {
        match cmd {
            Some(Command::KittyTextSizing(c)) => c,
            other => panic!("expected kitty_text_sizing, got {:?}", other),
        }
    }

    #[test]
    fn osc_66_numeric_fields_reject_digit_separators() {
        let mut p = run(b"66;s=0_2;text");
        let cmd = ts(p.end(None));
        assert_eq!(Osc::new(b"text"), cmd);
    }

    #[test]
    fn osc_66_empty_parameters() {
        let mut p = run(b"66;;bobr");
        let cmd = ts(p.end(Some(0x1b)));
        assert_eq!(1, cmd.scale);
        assert_eq!(b"bobr", cmd.text);
    }

    #[test]
    fn osc_66_single_parameter() {
        let mut p = run(b"66;s=2;kurwa");
        let cmd = ts(p.end(Some(0x1b)));
        assert_eq!(2, cmd.scale);
        assert_eq!(b"kurwa", cmd.text);
    }

    #[test]
    fn osc_66_multiple_parameters() {
        let mut p = run(b"66;s=2:w=7:n=13:d=15:v=1:h=2;long");
        let cmd = ts(p.end(Some(0x1b)));
        assert_eq!(2, cmd.scale);
        assert_eq!(7, cmd.width);
        assert_eq!(13, cmd.numerator);
        assert_eq!(15, cmd.denominator);
        assert_eq!(VAlign::Bottom, cmd.valign);
        assert_eq!(HAlign::Center, cmd.halign);
        assert_eq!(b"long", cmd.text);
    }

    #[test]
    fn osc_66_scale_is_zero() {
        let mut p = run(b"66;s=0;nope");
        let cmd = ts(p.end(Some(0x1b)));
        assert_eq!(1, cmd.scale);
    }

    #[test]
    fn osc_66_invalid_parameters() {
        let mut p = run(b"66;w=8:v=3:n=16;");
        let cmd = ts(p.end(Some(0x1b)));
        assert_eq!(0, cmd.width);
        assert_eq!(VAlign::Top, cmd.valign);
        assert_eq!(0, cmd.numerator);
    }

    #[test]
    fn osc_66_utf8() {
        let mut p = run("66;;👻魑魅魍魉ゴースッティ".as_bytes());
        let cmd = ts(p.end(Some(0x1b)));
        assert_eq!("👻魑魅魍魉ゴースッティ".as_bytes(), cmd.text);
    }

    #[test]
    fn osc_66_unsafe_utf8() {
        let mut p = run(b"66;;\n");
        assert!(p.end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_66_overlong_utf8() {
        let mut p = Parser::init(None);
        for &ch in b"66;;" {
            p.next(ch);
        }
        for _ in 0..1025 {
            for &ch in b"bobr" {
                p.next(ch);
            }
        }

        assert!(p.end(Some(0x1b)).is_none());
    }
}
