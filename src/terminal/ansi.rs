//! Port of `src/terminal/ansi.zig`.
//!
//! STATUS: ported

/// C0 (7-bit) control characters from ANSI.
///
/// This is not complete, control characters are only added to this
/// as the terminal emulator handles them.
///
/// Non-exhaustive (any 7-bit value is representable) since the inputs are
/// user-generated, so this is a newtype with named constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct C0(pub u8);

impl C0 {
    /// Null
    pub const NUL: C0 = C0(0x00);
    /// Start of heading
    pub const SOH: C0 = C0(0x01);
    /// Start of text
    pub const STX: C0 = C0(0x02);
    /// Enquiry
    pub const ENQ: C0 = C0(0x05);
    /// Bell
    pub const BEL: C0 = C0(0x07);
    /// Backspace
    pub const BS: C0 = C0(0x08);
    // Horizontal tab
    pub const HT: C0 = C0(0x09);
    /// Line feed
    pub const LF: C0 = C0(0x0A);
    /// Vertical Tab
    pub const VT: C0 = C0(0x0B);
    /// Form feed
    pub const FF: C0 = C0(0x0C);
    /// Carriage return
    pub const CR: C0 = C0(0x0D);
    /// Shift out
    pub const SO: C0 = C0(0x0E);
    /// Shift in
    pub const SI: C0 = C0(0x0F);
}

/// The SGR rendition aspects that can be set, sometimes known as attributes.
/// The value corresponds to the parameter value for the SGR command (ESC [ m).
///
/// Non-exhaustive so that conversion from an int never fails since the
/// inputs are user-generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct RenditionAspect(pub u16);

impl RenditionAspect {
    pub const DEFAULT: RenditionAspect = RenditionAspect(0);
    pub const BOLD: RenditionAspect = RenditionAspect(1);
    pub const DEFAULT_FG: RenditionAspect = RenditionAspect(39);
    pub const DEFAULT_BG: RenditionAspect = RenditionAspect(49);
}

/// Possible cursor styles (ESC [ q)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum CursorStyle {
    #[default]
    Default = 0,
    BlinkingBlock = 1,
    SteadyBlock = 2,
    BlinkingUnderline = 3,
    SteadyUnderline = 4,
    BlinkingBar = 5,
    SteadyBar = 6,
}

/// The status line type for DECSSDT.
///
/// Non-exhaustive so that conversion from an int never fails for
/// unsupported values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct StatusLineType(pub u16);

impl StatusLineType {
    pub const NONE: StatusLineType = StatusLineType(0);
    pub const INDICATOR: StatusLineType = StatusLineType(1);
    pub const HOST_WRITABLE: StatusLineType = StatusLineType(2);
}

/// The display to target for status updates (DECSASD).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StatusDisplay {
    Main = 0,
    StatusLine = 1,
}

/// The possible modify key formats to ESC[>{a};{b}m
/// Note: this is not complete, we should add more as we support more
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ModifyKeyFormat {
    Legacy = 0,
    CursorKeys = 1,
    FunctionKeys = 2,
    OtherKeysNone = 3,
    OtherKeysNumericExcept = 4,
    OtherKeysNumeric = 5,
}

/// The protection modes that can be set for the terminal. See DECSCA and
/// ESC V, W.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum ProtectedMode {
    #[default]
    Off = 0,
    /// ESC V, W
    Iso = 1,
    /// CSI Ps " q
    Dec = 2,
}
