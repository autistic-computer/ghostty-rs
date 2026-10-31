//! Port of `src/font/sprite.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "sprite/canvas.rs"]
pub mod canvas;
#[path = "sprite/draw/mod.rs"]
pub mod draw;
#[path = "sprite/face.rs"]
pub mod face;
// @generated-mods-end

pub use self::canvas::{Box, Canvas, Color, Point};
pub use self::face::Face;

/// Sprites are represented as special codepoints outside of the Unicode
/// codepoint range. Unicode maxes out at U+10FFFF (21 bits), and we use the
/// high 11 bits to hide our special characters.
///
/// These characters are ONLY used for rendering and NEVER used written to
/// text files or any other exported format, so we don't use the Private Use
/// Area of Unicode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Sprite {
    // Start 1 above the maximum Unicode codepoint.
    Underline = Sprite::START,
    UnderlineDouble,
    UnderlineDotted,
    UnderlineDashed,
    UnderlineCurly,

    Strikethrough,

    Overline,

    CursorRect,
    CursorHollowRect,
    CursorBar,
    CursorUnderline,
}

impl Sprite {
    pub const START: u32 = (1 << 21) - 1 + 1;
    pub const END: u32 = u32::MAX;

    /// All sprites, in declaration order.
    pub const ALL: [Sprite; 11] = [
        Sprite::Underline,
        Sprite::UnderlineDouble,
        Sprite::UnderlineDotted,
        Sprite::UnderlineDashed,
        Sprite::UnderlineCurly,
        Sprite::Strikethrough,
        Sprite::Overline,
        Sprite::CursorRect,
        Sprite::CursorHollowRect,
        Sprite::CursorBar,
        Sprite::CursorUnderline,
    ];

    /// The codepoint for this sprite (Zig `@backingInt`).
    pub const fn cp(self) -> u32 {
        self as u32
    }

    /// The sprite for a codepoint (Zig `@fromBackingInt`), or `None` if
    /// the value isn't a sprite.
    pub fn from_cp(cp: u32) -> Option<Sprite> {
        if cp < Sprite::START {
            return None;
        }
        Sprite::ALL.get((cp - Sprite::START) as usize).copied()
    }

    /// The Zig field name of this sprite.
    pub const fn as_str(self) -> &'static str {
        match self {
            Sprite::Underline => "underline",
            Sprite::UnderlineDouble => "underline_double",
            Sprite::UnderlineDotted => "underline_dotted",
            Sprite::UnderlineDashed => "underline_dashed",
            Sprite::UnderlineCurly => "underline_curly",
            Sprite::Strikethrough => "strikethrough",
            Sprite::Overline => "overline",
            Sprite::CursorRect => "cursor_rect",
            Sprite::CursorHollowRect => "cursor_hollow_rect",
            Sprite::CursorBar => "cursor_bar",
            Sprite::CursorUnderline => "cursor_underline",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_start() {
        assert_eq!(Sprite::START, Sprite::Underline as u32);
        for (i, s) in Sprite::ALL.iter().enumerate() {
            assert_eq!(Sprite::START + i as u32, s.cp());
            assert_eq!(Some(*s), Sprite::from_cp(s.cp()));
        }
        assert_eq!(None, Sprite::from_cp(Sprite::START - 1));
        assert_eq!(None, Sprite::from_cp(Sprite::START + 11));
    }
}
