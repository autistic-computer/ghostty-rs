//! Port of `src/terminal/kitty/color.zig`.
//!
//! STATUS: ported

use std::fmt;

use crate::lib::parse_int;
use crate::terminal::color::Rgb;
use crate::terminal::osc::Terminator;

/// Kitty color protocol (OSC 21) command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Osc {
    /// list of requests
    pub list: Vec<Request>,

    /// We must reply with the same string terminator (ST) as used in the
    /// request.
    pub terminator: Terminator,
}

/// A single request in an OSC 21 command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Query(Kind),
    Set { key: Kind, color: Rgb },
    Reset(Kind),
}

/// We don't currently support encoding this to C in any way.
pub type OscC = ();

impl Osc {
    pub fn cval(&self) -> OscC {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Special {
    Foreground,
    Background,
    SelectionForeground,
    SelectionBackground,
    Cursor,
    CursorText,
    VisualBell,
    SecondTransparentBackground,
}

impl Special {
    /// Number of fields in the enum.
    pub const COUNT: usize = 8;

    /// The Zig tag name.
    pub fn as_str(self) -> &'static str {
        match self {
            Special::Foreground => "foreground",
            Special::Background => "background",
            Special::SelectionForeground => "selection_foreground",
            Special::SelectionBackground => "selection_background",
            Special::Cursor => "cursor",
            Special::CursorText => "cursor_text",
            Special::VisualBell => "visual_bell",
            Special::SecondTransparentBackground => "second_transparent_background",
        }
    }

    /// Zig `std.meta.stringToEnum(Special, s)`.
    pub fn from_str(s: &[u8]) -> Option<Special> {
        Some(match s {
            b"foreground" => Special::Foreground,
            b"background" => Special::Background,
            b"selection_foreground" => Special::SelectionForeground,
            b"selection_background" => Special::SelectionBackground,
            b"cursor" => Special::Cursor,
            b"cursor_text" => Special::CursorText,
            b"visual_bell" => Special::VisualBell,
            b"second_transparent_background" => Special::SecondTransparentBackground,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Palette(u8),
    Special(Special),
}

impl Kind {
    pub const MAX: usize = u8::MAX as usize + Special::COUNT;

    pub fn parse(key: &[u8]) -> Option<Kind> {
        if let Some(s) = Special::from_str(key) {
            return Some(Kind::Special(s));
        }
        Some(Kind::Palette(parse_int::<u8>(key, 10).ok()?))
    }

    /// Returns true when a terminal has built-in state for this key.
    ///
    /// Unsupported special colors may still be valid Kitty protocol keys, but
    /// libghostty-vt cannot report them because Terminal does not store them.
    pub fn has_terminal_query_color(self) -> bool {
        match self {
            Kind::Palette(_) => true,
            Kind::Special(special) => matches!(
                special,
                Special::Foreground | Special::Background | Special::Cursor
            ),
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Kind::Palette(p) => write!(f, "{}", p),
            Kind::Special(s) => f.write_str(s.as_str()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc_kitty_color_indexes_reject_digit_separators() {
        assert_eq!(None, Kind::parse(b"1_0"));
        assert_eq!(Kind::Palette(10), Kind::parse(b"10").unwrap());
        assert_eq!(
            Kind::Special(Special::SelectionForeground),
            Kind::parse(b"selection_foreground").unwrap()
        );
    }

    #[test]
    fn osc_kitty_color_protocol_kind_string() {
        assert_eq!("foreground", format!("{}", Kind::Special(Special::Foreground)));
        assert_eq!("42", format!("{}", Kind::Palette(42)));

        assert!(Kind::Palette(42).has_terminal_query_color());
        assert!(Kind::Special(Special::Foreground).has_terminal_query_color());
        assert!(!Kind::Special(Special::SelectionBackground).has_terminal_query_color());
    }
}
