//! Port of `src/unicode/main.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "grapheme.rs"]
pub mod grapheme;
#[path = "lut.rs"]
pub mod lut;
#[path = "props.rs"]
pub mod props;
#[path = "props_table.rs"]
pub mod props_table;
#[path = "props_uucode.rs"]
pub mod props_uucode;
#[path = "symbols_table.rs"]
pub mod symbols_table;
#[path = "symbols_uucode.rs"]
pub mod symbols_uucode;
// @generated-mods-end

/// Rust stand-in for the external `uucode` Zig package (not a Ghostty
/// source file; see the module docs).
#[path = "uucode.rs"]
pub mod uucode;

pub use grapheme::{
    grapheme_break, grapheme_width, grapheme_width_effect, GraphemeWidth, GraphemeWidthEffect,
};
pub use props::Properties;
/// The Properties lookup table (Zig: `unicode.table`).
pub use props_table::TABLE;

/// Returns the terminal display width of a codepoint in terminal
/// grid cells: 0, 1, or 2.
///
/// This is the same width table the terminal uses when laying out
/// printed text: 0 for zero-width codepoints (controls, combining
/// marks, default-ignorables, surrogates), 2 for wide codepoints
/// (East Asian Wide/Fullwidth, regional indicators, clamped at 2),
/// and 1 otherwise.
///
/// This operates on a single codepoint and cannot account for
/// grapheme-cluster-level width rules (VS16, combining sequences);
/// callers needing cluster-accurate widths should use grapheme_width().
/// Summing per-codepoint widths is only correct when mode 2027 is
/// disabled.
pub fn codepoint_width(cp: u32) -> u8 {
    TABLE.get(cp).width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codepoint_width_test() {
        // Narrow (width 1)
        assert_eq!(1, codepoint_width('a' as u32));
        assert_eq!(1, codepoint_width(' ' as u32));
        assert_eq!(1, codepoint_width(0x10FFFF)); // max codepoint

        // C0/C1 control characters (width 0)
        assert_eq!(0, codepoint_width(0x00)); // NUL
        assert_eq!(0, codepoint_width(0x07)); // BEL
        assert_eq!(0, codepoint_width(0x1B)); // ESC
        assert_eq!(0, codepoint_width(0x7F)); // DEL
        assert_eq!(0, codepoint_width(0x80)); // C1 PAD

        // Zero-width codepoints
        assert_eq!(0, codepoint_width(0x0301)); // combining acute
        assert_eq!(0, codepoint_width(0x200B)); // zero width space
        assert_eq!(0, codepoint_width(0x200D)); // ZWJ
        assert_eq!(0, codepoint_width(0xFE0F)); // VS16
        assert_eq!(0, codepoint_width(0xD800)); // surrogate

        // Wide (width 2)
        assert_eq!(2, codepoint_width(0x4E00)); // CJK ideograph
        assert_eq!(2, codepoint_width(0xFF21)); // fullwidth A
        assert_eq!(2, codepoint_width(0xAC00)); // Hangul syllable
        assert_eq!(2, codepoint_width(0x1F600)); // emoji
        assert_eq!(2, codepoint_width(0x1F1E6)); // regional indicator
        assert_eq!(2, codepoint_width(0x2E3B)); // three-em dash (clamped)
    }
}
