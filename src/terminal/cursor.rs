//! Port of `src/terminal/cursor.zig`.
//!
//! STATUS: ported

/// The visual style of the cursor. Whether or not it blinks
/// is determined by mode 12 (modes.zig). This mode is synchronized
/// with CSI q, the same as xterm.
///
/// Bar, block, and underline correspond to DECSCUSR 5/6, 1/2, and 3/4.
/// Hollow block is Ghostty-specific and is reported as DECSCUSR 1 or 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Style {
    /// DECSCUSR 5, 6
    Bar = 0,

    /// DECSCUSR 1, 2
    Block = 1,

    /// DECSCUSR 3, 4
    Underline = 2,

    // The cursor styles below aren't known by DESCUSR and are custom
    // implemented in Ghostty. They are reported as some standard style
    // if requested, though.
    /// Hollow block cursor. This is a block cursor with the center empty.
    /// Reported as DECSCUSR 1 or 2 (block).
    BlockHollow = 3,
}

impl Style {
    /// The Zig tag name.
    pub fn as_str(self) -> &'static str {
        match self {
            Style::Bar => "bar",
            Style::Block => "block",
            Style::Underline => "underline",
            Style::BlockHollow => "block_hollow",
        }
    }
}
