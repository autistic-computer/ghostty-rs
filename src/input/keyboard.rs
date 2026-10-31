//! Port of `src/input/keyboard.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::str::FromStr;

use super::config::OptionAsAlt;

/// Keyboard layouts.
///
/// These aren't heavily used in Ghostty and having a fully comprehensive
/// list is not important. We only need to distinguish between a few
/// different layouts for some nice-to-have features, such as setting a default
/// value for "macos-option-as-alt".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layout {
    // Unknown, unmapped layout. Ghostty should not make any assumptions
    // about the layout of the keyboard.
    Unknown,

    // The remaining should be fairly self-explanatory:
    UsStandard,
    UsInternational,
}

impl Layout {
    /// The Zig tag name (`@tagName`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Layout::Unknown => "unknown",
            Layout::UsStandard => "us_standard",
            Layout::UsInternational => "us_international",
        }
    }

    /// Exact tag name match (`std.meta.stringToEnum`).
    pub fn parse(s: &[u8]) -> Option<Layout> {
        match s {
            b"unknown" => Some(Layout::Unknown),
            b"us_standard" => Some(Layout::UsStandard),
            b"us_international" => Some(Layout::UsInternational),
            _ => None,
        }
    }

    /// Map an Apple keyboard layout ID to a value in this enum. The layout
    /// ID can be retrieved using Carbon's TIKeyboardLayoutGetInputSourceProperty
    /// function.
    ///
    /// Even though our layout supports "unknown", we return null if we don't
    /// recognize the layout ID so callers can detect this scenario.
    pub fn map_apple_id(id: &[u8]) -> Option<Layout> {
        if id == b"com.apple.keylayout.US" {
            return Some(Layout::UsStandard);
        } else if id == b"com.apple.keylayout.USInternational" {
            return Some(Layout::UsInternational);
        }

        None
    }

    /// Returns the default macos-option-as-alt value for this layout.
    ///
    /// We apply some heuristics to change the default based on the keyboard
    /// layout if "macos-option-as-alt" is unset. We do this because on some
    /// keyboard layouts such as US standard layouts, users generally expect
    /// an input such as option-b to map to alt-b but macOS by default will
    /// convert it to the codepoint "∫".
    ///
    /// This behavior however is desired on international layout where the
    /// option key is used for important, regularly used inputs.
    pub const fn detect_option_as_alt(self) -> OptionAsAlt {
        match self {
            // On US standard, the option key is typically used as alt
            // and not as a modifier for other codepoints. For example,
            // option-B = ∫ but usually the user wants alt-B.
            Layout::UsStandard | Layout::UsInternational => OptionAsAlt::True,

            Layout::Unknown => OptionAsAlt::False,
        }
    }
}

impl FromStr for Layout {
    type Err = ();

    fn from_str(s: &str) -> Result<Layout, ()> {
        Layout::parse(s.as_bytes()).ok_or(())
    }
}

impl fmt::Display for Layout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
