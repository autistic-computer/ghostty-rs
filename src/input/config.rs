//! Port of `src/input/config.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::str::FromStr;

/// Determines the macOS option key behavior. See the config
/// `macos-option-as-alt` for a lot more details.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OptionAsAlt {
    False,
    True,
    Left,
    Right,
}

impl OptionAsAlt {
    /// The Zig tag name (`@tagName`), used in configuration strings.
    pub const fn as_str(self) -> &'static str {
        match self {
            OptionAsAlt::False => "false",
            OptionAsAlt::True => "true",
            OptionAsAlt::Left => "left",
            OptionAsAlt::Right => "right",
        }
    }

    /// Exact tag name match (`std.meta.stringToEnum`).
    pub fn parse(s: &[u8]) -> Option<OptionAsAlt> {
        match s {
            b"false" => Some(OptionAsAlt::False),
            b"true" => Some(OptionAsAlt::True),
            b"left" => Some(OptionAsAlt::Left),
            b"right" => Some(OptionAsAlt::Right),
            _ => None,
        }
    }
}

impl FromStr for OptionAsAlt {
    type Err = ();

    fn from_str(s: &str) -> Result<OptionAsAlt, ()> {
        OptionAsAlt::parse(s.as_bytes()).ok_or(())
    }
}

impl fmt::Display for OptionAsAlt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
