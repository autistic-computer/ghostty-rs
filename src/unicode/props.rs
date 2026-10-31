//! Port of `src/unicode/props.zig`.
//!
//! STATUS: ported
//!
//! Property set per codepoint that Ghostty cares about.
//!
//! Adding to this lets you find new properties but also potentially makes
//! our lookup tables less efficient. Any changes to this should run the
//! benchmarks in src/bench to verify that we haven't regressed.
//!
//! Deviation: the Zig type is a `packed struct(u16)` (with explicit padding
//! as an LLVM codegen workaround). Its bit layout is never relied upon, so
//! this is a plain struct.

use std::fmt;

use super::uucode::types::GraphemeBreakNoControl;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Properties {
    /// Codepoint width. We clamp to [0, 2] since Ghostty handles control
    /// characters and we max out at 2 for wide characters (i.e. 3-em dash
    /// becomes a 2-em dash).
    pub width: u8,

    /// Whether the code point does not contribute to the width of a grapheme
    /// cluster (not used for single code point cells).
    pub width_zero_in_grapheme: bool,

    /// Grapheme break property.
    pub grapheme_break: GraphemeBreakNoControl,

    /// Emoji VS compatibility
    pub emoji_vs_base: bool,
}

impl Properties {
    // Needed for lut::Generator
    pub fn eql(a: Properties, b: Properties) -> bool {
        a.width == b.width
            && a.width_zero_in_grapheme == b.width_zero_in_grapheme
            && a.grapheme_break == b.grapheme_break
            && a.emoji_vs_base == b.emoji_vs_base
    }
}

/// Needed for lut::Tables::write_rust: formats as a Rust struct literal.
impl fmt::Display for Properties {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Properties {{ width: {}, width_zero_in_grapheme: {}, grapheme_break: GraphemeBreakNoControl::{}, emoji_vs_base: {} }}",
            self.width,
            self.width_zero_in_grapheme,
            self.grapheme_break.variant_name(),
            self.emoji_vs_base,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eql_and_format() {
        let a = Properties {
            width: 2,
            width_zero_in_grapheme: false,
            grapheme_break: GraphemeBreakNoControl::ExtendedPictographic,
            emoji_vs_base: true,
        };
        assert!(Properties::eql(a, a));
        assert!(!Properties::eql(a, Properties::default()));
        assert_eq!(
            "Properties { width: 2, width_zero_in_grapheme: false, grapheme_break: GraphemeBreakNoControl::ExtendedPictographic, emoji_vs_base: true }",
            a.to_string()
        );
    }
}
