//! Port of `src/terminal/csi.zig`.
//!
//! STATUS: ported

/// Modes for the ED CSI command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EraseDisplay {
    Below = 0,
    Above = 1,
    Complete = 2,
    Scrollback = 3,

    /// This is an extension added by Kitty to move the viewport into the
    /// scrollback and then erase the display.
    ScrollComplete = 22,
}

impl EraseDisplay {
    /// Convert from the integer value (`std.enums.fromInt`).
    pub fn from_int(v: u8) -> Option<EraseDisplay> {
        Some(match v {
            0 => Self::Below,
            1 => Self::Above,
            2 => Self::Complete,
            3 => Self::Scrollback,
            22 => Self::ScrollComplete,
            _ => return None,
        })
    }
}

/// Modes for the EL CSI command.
///
/// Non-exhaustive so that conversion from an int never fails since the
/// inputs are user-generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct EraseLine(pub u8);

impl EraseLine {
    pub const RIGHT: EraseLine = EraseLine(0);
    pub const LEFT: EraseLine = EraseLine(1);
    pub const COMPLETE: EraseLine = EraseLine(2);
    pub const RIGHT_UNLESS_PENDING_WRAP: EraseLine = EraseLine(4);
}

/// Modes for the TBC (tab clear) command.
///
/// Non-exhaustive so that conversion from an int never fails since the
/// inputs are user-generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct TabClear(pub u8);

impl TabClear {
    pub const CURRENT: TabClear = TabClear(0);
    pub const ALL: TabClear = TabClear(3);
}

/// Style formats for terminal size reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SizeReportStyle {
    // XTWINOPS
    Csi14T = 0,
    Csi16T = 1,
    Csi18T = 2,
    Csi19T = 3,
    Csi21T = 4,
}

/// XTWINOPS CSI 22/23
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TitlePushPop {
    pub op: TitlePushPopOp,
    pub index: u16,
}

/// `TitlePushPop.Op`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TitlePushPopOp {
    Push,
    Pop,
}
