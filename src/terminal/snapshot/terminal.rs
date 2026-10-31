//! Port of `src/terminal/snapshot/terminal.zig`.
//!
//! STATUS: ported
//!
//! TERMINAL record payload encoding.
//!
//! One TERMINAL record contains terminal-wide state shared by every screen. It
//! is the first record in a snapshot and declares which SCREEN sequences follow
//! it. Screen contents, cursors, and history are encoded by `screen.zig` and
//! `history.zig`.
//!
//! Snapshot version 1 supports the primary screen and an optional alternate
//! screen. `screen_count` is therefore one or two. SCREEN records identify
//! their destination by key and may appear in either order. Canonical encoders
//! write primary first, then alternate when present. `active_screen_key` must
//! name one of those declared screens.
//!
//! All integers are unsigned and little-endian.
//!
//! ## Binary Format
//!
//! The payload begins with a fixed header followed by terminal-wide variable
//! state. The enclosing record supplies the payload boundary.
//!
//! ```text
//!   0 +--------------------------+
//!     | Header                   |
//! 103 +--------------------------+
//!     | Tab stops                |
//!     | ceil(columns / 8) bytes  |
//!     +--------------------------+
//!     | Original palette         |
//!     | 256 * RGB                |
//!     +--------------------------+
//!     | Palette override mask    |
//!     | 32 bytes                 |
//!     +--------------------------+
//!     | Palette overrides        |
//!     | 3 * popcount(mask) bytes |
//!     +--------------------------+
//!     | PWD length (u32)         |
//!     +--------------------------+
//!     | PWD bytes                |
//!     +--------------------------+
//!     | Title length (u32)       |
//!     +--------------------------+
//!     | Title bytes              |
//! end +--------------------------+
//! ```
//!
//! ### Header
//!
//! ```text
//!   0 +-----------------------------------+
//!     | Columns (u16)                     |
//!   2 +-----------------------------------+
//!     | Rows (u16)                        |
//!   4 +-----------------------------------+
//!     | Pixel width (u32)                 |
//!   8 +-----------------------------------+
//!     | Pixel height (u32)                |
//!  12 +-----------------------------------+
//!     | Scrolling-region top (u16)        |
//!  14 +-----------------------------------+
//!     | Scrolling-region bottom (u16)     |
//!  16 +-----------------------------------+
//!     | Scrolling-region left (u16)       |
//!  18 +-----------------------------------+
//!     | Scrolling-region right (u16)      |
//!  20 +-----------------------------------+
//!     | Status display (u8)               |
//!  21 +-----------------------------------+
//!     | Active screen key (u16)           |
//!  23 +-----------------------------------+
//!     | Screen count (u16)                |
//!  25 +-----------------------------------+
//!     | Previous codepoint (u32)          |
//!  29 +-----------------------------------+
//!     | Cursor is-default (u8)            |
//!  30 +-----------------------------------+
//!     | Cursor default visual style (u8)  |
//!  31 +-----------------------------------+
//!     | Cursor default blink policy (u8)  |
//!  32 +-----------------------------------+
//!     | Shell-redraw policy (u8)          |
//!  33 +-----------------------------------+
//!     | Modify-other-keys level 2 (u8)    |
//!  34 +-----------------------------------+
//!     | Mouse event (u8)                  |
//!  35 +-----------------------------------+
//!     | Mouse format (u8)                 |
//!  36 +-----------------------------------+
//!     | Mouse shift-capture policy (u8)   |
//!  37 +-----------------------------------+
//!     | Mouse shape (u8)                  |
//!  38 +-----------------------------------+
//!     | Password-input flag (u8)          |
//!  39 +-----------------------------------+
//!     | Current modes (ModePacked)        |
//!  47 +-----------------------------------+
//!     | Saved modes (ModePacked)          |
//!  55 +-----------------------------------+
//!     | Default modes (ModePacked)        |
//!  63 +-----------------------------------+
//!     | Background color (DynamicRGB)     |
//!  71 +-----------------------------------+
//!     | Foreground color (DynamicRGB)     |
//!  79 +-----------------------------------+
//!     | Cursor color (DynamicRGB)         |
//!  87 +-----------------------------------+
//!     | Maximum scrollback bytes (u64)    |
//!  95 +-----------------------------------+
//!     | Maximum scrollback rows (u64)     |
//! 103 +-----------------------------------+
//! ```
//!
//! The previous codepoint is a Unicode scalar value or `0xffffffff` when there
//! is no previous character. Boolean fields are zero or one.
//! Cursor blink and mouse shift-capture policies encode null as zero, false as
//! one, and true as two.
//! Canonical encoders use only the documented enum and boolean values.
//! Decoders replace unknown semantic values with neutral native defaults,
//! discard invalid previous codepoints, and reset each invalid scrolling axis
//! to the terminal's full extent. Dimensions and screen count remain
//! structural.
//!
//! A scrollback limit of `0xffffffffffffffff` means unlimited. Every other
//! value is finite, including zero. The values are the source primary
//! PageList's explicit policies, not its dimension-adjusted effective limits.
//! Alternate-screen no-scrollback behavior is derived from the screen key.
//!
//! ### Tab stops
//!
//! One bit is encoded for each column, least-significant bit first within each
//! byte. Bit zero of the first byte is column zero. Unused high bits in the last
//! byte are canonically zero and ignored by decoders.
//!
//! ### Dynamic RGB
//!
//! The three terminal dynamic colors share this layout:
//!
//! ```text
//! 0 +----------------------------+
//!   | Default present (u8)       |
//! 1 +----------------------------+
//!   | Default RGB                |
//! 4 +----------------------------+
//!   | Override present (u8)      |
//! 5 +----------------------------+
//!   | Override RGB               |
//! 8 +----------------------------+
//! ```
//!
//! Presence values are zero or one. The corresponding RGB bytes must be zero
//! when a value is absent. Decoders treat every other presence value as absent
//! and ignore RGB bytes belonging to an absent value.
//!
//! ### Palette
//!
//! The complete original 256-color palette is encoded first in index order.
//! Each mask bit then states whether the current palette entry overrides that
//! original value. Bit zero of the first mask byte is palette index zero.
//! Override RGB values follow in increasing index order with no index or count
//! fields.
//!
//! ### Modes
//!
//! Current, saved, and default modes use the same stable bit registry:
//!
//! ```text
//! bit  0  disable_keyboard
//! bit  1  insert
//! bit  2  send_receive_mode
//! bit  3  linefeed
//! bit  4  cursor_keys
//! bit  5  132_column
//! bit  6  slow_scroll
//! bit  7  reverse_colors
//! bit  8  origin
//! bit  9  wraparound
//! bit 10  autorepeat
//! bit 11  mouse_event_x10
//! bit 12  cursor_blinking
//! bit 13  cursor_visible
//! bit 14  enable_mode_3
//! bit 15  reverse_wrap
//! bit 16  alt_screen_legacy
//! bit 17  keypad_keys
//! bit 18  backarrow_key_mode
//! bit 19  enable_left_and_right_margin
//! bit 20  mouse_event_normal
//! bit 21  mouse_event_button
//! bit 22  mouse_event_any
//! bit 23  focus_event
//! bit 24  mouse_format_utf8
//! bit 25  mouse_format_sgr
//! bit 26  mouse_alternate_scroll
//! bit 27  mouse_format_urxvt
//! bit 28  mouse_format_sgr_pixels
//! bit 29  ignore_keypad_with_numlock
//! bit 30  alt_esc_prefix
//! bit 31  alt_sends_escape
//! bit 32  reverse_wrap_extended
//! bit 33  alt_screen
//! bit 34  save_cursor
//! bit 35  alt_screen_save_cursor_clear_enter
//! bit 36  bracketed_paste
//! bit 37  synchronized_output
//! bit 38  grapheme_cluster
//! bit 39  report_color_scheme
//! bit 40  report_visibility
//! bit 41  in_band_size_reports
//! bit 42  kitty_paste_events
//! bits 43-63  reserved, zero
//! ```
//!
//! This is the packed field order of native `ModePacked`. Its layout is
//! well-defined and is the snapshot registry. Moving or adding a native mode
//! therefore requires a snapshot version bump. Decoders ignore reserved bits.
//!
//! ## Field classification
//!
//! The TERMINAL payload encodes terminal dimensions, pixel dimensions,
//! scrolling region, status display, tab stops, PWD, title, color state,
//! previous character, all three mode sets, cursor defaults, mouse behavior,
//! password-input state, source scrollback policies, the active screen key, and
//! the set of declared screens.
//!
//! SCREEN and HISTORY sequences encode each Screen's pages, cursor, saved
//! cursor, charset, protected mode, Kitty keyboard stack, semantic-click state,
//! and complete history. The semantic-prompt `seen` bit is derived from
//! restored resident content.
//!
//! Native allocators, I/O implementations, pools, pointers, page IDs, byte
//! accounting, tracked pins, and ScreenSet generations are reconstructed.
//! PageList row totals and the active viewport are derived from decoded pages.
//! The destination surface supplies its own focus state. Selections, scrolled
//! viewports, selection-scroll activity, dirty flags, search flags, hyperlink
//! hover state, and incremental compression state are presentation or cache
//! state and reset during restore.
//!
//! Kitty image state and glyph glossary registrations are unsupported by this
//! snapshot version and are ignored during capture. Unicode virtual placeholder
//! cells are preserved as grid content, but their image and placement state is
//! not restored. Build-time terminal behavior, Unicode width policy, parser
//! continuation, and external callbacks are version-level or caller-local
//! requirements.
//!
//! Native enum declaration indices and mode bit positions used by this format
//! are snapshot-version registries. Changing any of them requires a snapshot
//! version bump.

use super::io::{self, Error};
use super::record;
use crate::terminal::ansi;
use crate::terminal::color;
use crate::terminal::cursor::Style as CursorStyle;
use crate::terminal::modes::{self as terminal_modes, ModePacked, ModeState};
use crate::terminal::mouse;
use crate::terminal::osc::semantic_prompt as osc_sp;
use crate::terminal::screen;
use crate::terminal::screen_set::Key as TerminalScreenKey;
use crate::terminal::tabstops::Tabstops;
use crate::terminal::terminal::{self as terminal_mod, Colors, MouseShiftCapture, ScrollingRegion, Terminal};

/// Reuse the terminal's semantic RGB value without adopting its memory layout
/// as a wire encoding.
pub type Rgb = color::Rgb;

/// Reuse the terminal's semantic dynamic color without adopting its memory
/// layout as a wire encoding.
pub type DynamicRgb = color::DynamicRgb;

/// The number of bits in native `ModePacked` (Zig: its backing integer).
fn mode_bits() -> u32 {
    terminal_modes::Mode::all().count() as u32
}

/// The mask of the native `ModePacked` bits.
fn mode_mask() -> u64 {
    let bits = mode_bits();
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// Errors possible while encoding a TERMINAL payload: `WriteFailed`, the
/// validation errors (`InvalidDimensions`, `InvalidScrollingRegion`,
/// `InvalidScreenCount`, `InvalidActiveScreenKey`,
/// `InvalidPreviousCodepoint`, `InvalidScrollbackLimit`), `InvalidTabStops`,
/// `InvalidPalette`, `StringTooLong`.
type PayloadEncodeError = Error;

/// Errors possible while decoding a TERMINAL payload: read errors,
/// `OutOfMemory`, `InvalidDimensions` (terminal dimensions control every
/// following native allocation), `InvalidScreenCount` (the count determines
/// how many SCREEN and HISTORY sequences follow).
type PayloadDecodeError = Error;

/// The fixed fields at the start of every TERMINAL payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Header {
    // Terminal geometry and its current scrolling region.
    pub columns: u16,
    pub rows: u16,
    pub width_px: u32,
    pub height_px: u32,

    pub scrolling_region_top: u16,
    pub scrolling_region_bottom: u16,
    pub scrolling_region_left: u16,
    pub scrolling_region_right: u16,

    // Status display, screen routing, and character context.
    pub status_display: ansi::StatusDisplay,
    pub active_screen_key: TerminalScreenKey,
    pub screen_count: u16,
    pub previous_codepoint: Option<u32>,

    // Cursor presentation defaults shared by the screens.
    pub cursor_is_default: bool,
    pub cursor_default_style: CursorStyle,
    pub cursor_default_blink: Option<bool>,

    // Terminal input, semantic redraw, and pointer behavior.
    pub shell_redraw: osc_sp::Redraw,
    pub modify_other_keys_2: bool,
    pub mouse_event: mouse::Event,
    pub mouse_format: mouse::Format,
    pub mouse_shift_capture: Option<bool>,
    pub mouse_shape: mouse::Shape,
    pub password_input: bool,

    // Runtime, saved, and reset mode sets.
    pub current_modes: ModePacked,
    pub saved_modes: ModePacked,
    pub default_modes: ModePacked,

    // Terminal-wide dynamic color state.
    pub background: DynamicRgb,
    pub foreground: DynamicRgb,
    pub cursor_color: DynamicRgb,

    // Explicit primary-screen scrollback policies.
    pub max_scrollback_bytes: Option<u64>,
    pub max_scrollback_rows: Option<u64>,
}

impl Header {
    /// Number of bytes written by `encode`.
    pub const LEN: usize = 103;

    /// Capture the terminal-wide native state represented by this header.
    fn init(terminal: &Terminal) -> Result<Header, Error> {
        let primary = terminal.screens.get(TerminalScreenKey::Primary).expect("primary screen");

        Ok(Header {
            // Terminal geometry and its current scrolling region.
            columns: terminal.cols,
            rows: terminal.rows,
            width_px: terminal.width_px,
            height_px: terminal.height_px,
            scrolling_region_top: terminal.scrolling_region.top,
            scrolling_region_bottom: terminal.scrolling_region.bottom,
            scrolling_region_left: terminal.scrolling_region.left,
            scrolling_region_right: terminal.scrolling_region.right,

            // Status display, screen routing, and character context.
            status_display: terminal.status_display,
            active_screen_key: terminal.screens.active_key,
            screen_count: if terminal.screens.get(TerminalScreenKey::Alternate).is_none() {
                1
            } else {
                2
            },
            previous_codepoint: terminal.previous_char,

            // Cursor presentation defaults shared by the screens.
            cursor_is_default: terminal.cursor.is_default,
            cursor_default_style: terminal.cursor.default_style,
            cursor_default_blink: terminal.cursor.default_blink,

            // Terminal input, semantic redraw, and pointer behavior.
            shell_redraw: terminal.flags.shell_redraws_prompt,
            modify_other_keys_2: terminal.flags.modify_other_keys_2,
            mouse_event: terminal.flags.mouse_event,
            mouse_format: terminal.flags.mouse_format,
            mouse_shift_capture: match terminal.flags.mouse_shift_capture {
                MouseShiftCapture::Null => None,
                MouseShiftCapture::False => Some(false),
                MouseShiftCapture::True => Some(true),
            },
            mouse_shape: terminal.mouse_shape,
            password_input: terminal.flags.password_input,

            // Runtime, saved, and reset mode sets.
            current_modes: terminal.modes.values,
            saved_modes: terminal.modes.saved,
            default_modes: terminal.modes.default,

            // Terminal-wide dynamic color state.
            background: terminal.colors.background,
            foreground: terminal.colors.foreground,
            cursor_color: terminal.colors.cursor,

            // Explicit primary-screen scrollback policies.
            max_scrollback_bytes: encode_scrollback_limit(primary.pages.limits.bytes.explicit)?,
            max_scrollback_rows: encode_scrollback_limit(primary.pages.limits.lines.explicit)?,
        })
    }

    /// Encode the fixed TERMINAL payload header field by field.
    pub fn encode(&self, writer: &mut dyn io::Writer) -> Result<(), PayloadEncodeError> {
        self.validate()?;

        // Terminal geometry and its current scrolling region.
        io::write_int::<u16>(writer, self.columns)?;
        io::write_int::<u16>(writer, self.rows)?;
        io::write_int::<u32>(writer, self.width_px)?;
        io::write_int::<u32>(writer, self.height_px)?;
        io::write_int::<u16>(writer, self.scrolling_region_top)?;
        io::write_int::<u16>(writer, self.scrolling_region_bottom)?;
        io::write_int::<u16>(writer, self.scrolling_region_left)?;
        io::write_int::<u16>(writer, self.scrolling_region_right)?;

        // Status display, screen routing, and character context.
        writer.write_byte(self.status_display as u8)?;
        io::write_int::<u16>(writer, self.active_screen_key as u16)?;
        io::write_int::<u16>(writer, self.screen_count)?;
        io::write_int::<u32>(writer, self.previous_codepoint.unwrap_or(u32::MAX))?;

        // Cursor presentation defaults shared by the screens.
        writer.write_byte(self.cursor_is_default as u8)?;
        writer.write_byte(self.cursor_default_style as u8)?;
        writer.write_byte(encode_optional_bool(self.cursor_default_blink))?;

        // Terminal input, semantic redraw, and pointer behavior.
        writer.write_byte(self.shell_redraw as u8)?;
        writer.write_byte(self.modify_other_keys_2 as u8)?;
        writer.write_byte(self.mouse_event as u8)?;
        writer.write_byte(self.mouse_format as u8)?;
        writer.write_byte(encode_optional_bool(self.mouse_shift_capture))?;
        writer.write_byte(self.mouse_shape as u8)?;
        writer.write_byte(self.password_input as u8)?;

        // Runtime, saved, and reset mode sets. ModePacked occupies 43 bits;
        // its eight-byte wire slots zero-extend the native packed value.
        for value in [self.current_modes, self.saved_modes, self.default_modes] {
            io::write_int::<u64>(writer, value.bits & mode_mask())?;
        }

        // Terminal-wide dynamic color state.
        encode_dynamic_rgb(self.background, writer)?;
        encode_dynamic_rgb(self.foreground, writer)?;
        encode_dynamic_rgb(self.cursor_color, writer)?;

        // Explicit primary-screen scrollback policies.
        io::write_int::<u64>(writer, self.max_scrollback_bytes.unwrap_or(u64::MAX))?;
        io::write_int::<u64>(writer, self.max_scrollback_rows.unwrap_or(u64::MAX))?;
        Ok(())
    }

    /// Decode the fixed TERMINAL payload header, normalizing semantic values.
    pub fn decode(reader: &mut dyn io::Reader) -> Result<Header, PayloadDecodeError> {
        // Terminal geometry and its current scrolling region.
        let columns = io::read_int::<u16>(reader)?;
        let rows = io::read_int::<u16>(reader)?;
        let width_px = io::read_int::<u32>(reader)?;
        let height_px = io::read_int::<u32>(reader)?;
        let scrolling_region_top = io::read_int::<u16>(reader)?;
        let scrolling_region_bottom = io::read_int::<u16>(reader)?;
        let scrolling_region_left = io::read_int::<u16>(reader)?;
        let scrolling_region_right = io::read_int::<u16>(reader)?;

        // Status display, screen routing, and character context.
        let status_display = status_display_from_int(reader.take_byte()?).unwrap_or(ansi::StatusDisplay::Main);
        let active_screen_key_raw = io::read_int::<u16>(reader)?;
        let screen_count = io::read_int::<u16>(reader)?;
        let previous_raw = io::read_int::<u32>(reader)?;
        let previous_codepoint: Option<u32> = 'previous: {
            if previous_raw == u32::MAX {
                break 'previous None;
            }
            // Zig: `std.math.cast(u21, ...)`.
            if previous_raw >= 1 << 21 {
                break 'previous None;
            }
            let value = previous_raw;
            if value > 0x10FFFF || (0xD800..=0xDFFF).contains(&value) {
                break 'previous None;
            }
            Some(value)
        };

        // Cursor presentation defaults shared by the screens.
        let cursor_is_default = !matches!(reader.take_byte()?, 0);
        let cursor_default_style = cursor_style_from_int(reader.take_byte()?).unwrap_or(CursorStyle::Block);
        let cursor_default_blink: Option<bool> = match reader.take_byte()? {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        };

        // Terminal input, semantic redraw, and pointer behavior.
        let shell_redraw = redraw_from_int(reader.take_byte()?).unwrap_or(osc_sp::Redraw::True);
        let modify_other_keys_2 = matches!(reader.take_byte()?, 1);
        let mouse_event = mouse_event_from_int(reader.take_byte()?).unwrap_or(mouse::Event::None);
        let mouse_format = mouse_format_from_int(reader.take_byte()?).unwrap_or(mouse::Format::X10);
        let mouse_shift_capture: Option<bool> = match reader.take_byte()? {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        };
        let mouse_shape = mouse_shape_from_int(reader.take_byte()?).unwrap_or(mouse::Shape::Text);
        let password_input = matches!(reader.take_byte()?, 1);

        // Runtime, saved, and reset mode sets.
        let mut mode_values = [ModePacked::default(); 3];
        for value in &mut mode_values {
            let raw = io::read_int::<u64>(reader)?;
            *value = ModePacked { bits: raw & mode_mask() };
        }

        // Terminal-wide dynamic color state.
        let background = decode_dynamic_rgb(reader)?;
        let foreground = decode_dynamic_rgb(reader)?;
        let cursor_color = decode_dynamic_rgb(reader)?;

        // Explicit primary-screen scrollback policies.
        let max_scrollback_bytes = decode_optional_limit(io::read_int::<u64>(reader)?);
        let max_scrollback_rows = decode_optional_limit(io::read_int::<u64>(reader)?);

        // Dimensions and sequence count are the only header fields that define
        // following allocation or record boundaries.
        if columns == 0 || rows == 0 {
            return Err(Error::InvalidDimensions);
        }
        if screen_count != 1 && screen_count != 2 {
            return Err(Error::InvalidScreenCount);
        }

        let scrolling_region_vertical_valid =
            scrolling_region_top <= scrolling_region_bottom && scrolling_region_bottom < rows;
        let scrolling_region_horizontal_valid =
            scrolling_region_left <= scrolling_region_right && scrolling_region_right < columns;
        let active_screen_key = match TerminalScreenKey::ALL.get(active_screen_key_raw as usize).copied() {
            None => TerminalScreenKey::Primary,
            Some(TerminalScreenKey::Alternate) if screen_count != 2 => TerminalScreenKey::Primary,
            Some(decoded) => decoded,
        };

        Ok(Header {
            columns,
            rows,
            width_px,
            height_px,

            scrolling_region_top: if scrolling_region_vertical_valid {
                scrolling_region_top
            } else {
                0
            },
            scrolling_region_bottom: if scrolling_region_vertical_valid {
                scrolling_region_bottom
            } else {
                rows - 1
            },
            scrolling_region_left: if scrolling_region_horizontal_valid {
                scrolling_region_left
            } else {
                0
            },
            scrolling_region_right: if scrolling_region_horizontal_valid {
                scrolling_region_right
            } else {
                columns - 1
            },

            status_display,
            active_screen_key,
            screen_count,
            previous_codepoint,

            cursor_is_default,
            cursor_default_style,
            cursor_default_blink,

            shell_redraw,
            modify_other_keys_2,
            mouse_event,
            mouse_format,
            mouse_shift_capture,
            mouse_shape,
            password_input,

            current_modes: mode_values[0],
            saved_modes: mode_values[1],
            default_modes: mode_values[2],

            background,
            foreground,
            cursor_color,

            max_scrollback_bytes,
            max_scrollback_rows,
        })
    }

    fn validate(&self) -> Result<(), Error> {
        if self.columns == 0 || self.rows == 0 {
            return Err(Error::InvalidDimensions);
        }
        if self.scrolling_region_top > self.scrolling_region_bottom
            || self.scrolling_region_bottom >= self.rows
            || self.scrolling_region_left > self.scrolling_region_right
            || self.scrolling_region_right >= self.columns
        {
            return Err(Error::InvalidScrollingRegion);
        }

        if self.screen_count != 1 && self.screen_count != 2 {
            return Err(Error::InvalidScreenCount);
        }
        if self.active_screen_key == TerminalScreenKey::Alternate && self.screen_count != 2 {
            return Err(Error::InvalidActiveScreenKey);
        }

        if let Some(value) = self.previous_codepoint {
            if value > 0x10FFFF || (0xD800..=0xDFFF).contains(&value) {
                return Err(Error::InvalidPreviousCodepoint);
            }
        }

        if self.max_scrollback_bytes == Some(u64::MAX) || self.max_scrollback_rows == Some(u64::MAX) {
            return Err(Error::InvalidScrollbackLimit);
        }
        Ok(())
    }
}

/// Owned semantic state decoded from one TERMINAL payload.
struct DecodedPayload {
    header: Header,
    tabstops: Tabstops,
    palette: color::DynamicPalette,
    pwd: Vec<u8>,
    title: Vec<u8>,
}

/// Encode one complete TERMINAL payload without record framing.
fn encode_payload(
    header: &Header,
    tabstops: &Tabstops,
    palette: &color::DynamicPalette,
    pwd: &[u8],
    title: &[u8],
    writer: &mut dyn io::Writer,
) -> Result<(), PayloadEncodeError> {
    // Validate the complete semantic value before writing any bytes.
    header.validate()?;
    if tabstops.cols != header.columns as usize {
        return Err(Error::InvalidTabStops);
    }
    if pwd.len() > u32::MAX as usize || title.len() > u32::MAX as usize {
        return Err(Error::StringTooLong);
    }
    for (index, (original, current)) in palette.original().iter().zip(palette.current.iter()).enumerate() {
        if !palette.mask.is_set(index) && original != current {
            return Err(Error::InvalidPalette);
        }
    }

    header.encode(writer)?;

    // Tab stops are packed least-significant bit first.
    let tabstop_len = (header.columns as usize).div_ceil(8);
    for byte_index in 0..tabstop_len {
        let mut raw: u8 = 0;
        for bit in 0..8 {
            let column = byte_index * 8 + bit;
            if column >= header.columns as usize {
                break;
            }
            if tabstops.get(column) {
                raw |= 1u8 << bit;
            }
        }
        writer.write_byte(raw)?;
    }

    // Encode the original palette, then a compact mask and only the active
    // overrides in increasing palette-index order.
    for value in palette.original() {
        encode_rgb(*value, writer)?;
    }
    for byte_index in 0..32 {
        let mut raw: u8 = 0;
        for bit in 0..8 {
            if palette.mask.is_set(byte_index * 8 + bit) {
                raw |= 1u8 << bit;
            }
        }
        writer.write_byte(raw)?;
    }
    for (index, value) in palette.current.iter().enumerate() {
        if palette.mask.is_set(index) {
            encode_rgb(*value, writer)?;
        }
    }

    // Length-prefixed terminal strings end the payload.
    io::write_int::<u32>(writer, pwd.len() as u32)?;
    writer.write_all(pwd)?;
    io::write_int::<u32>(writer, title.len() as u32)?;
    writer.write_all(title)?;
    Ok(())
}

fn alloc_bytes(len: usize) -> Result<Vec<u8>, Error> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(len).map_err(|_| Error::OutOfMemory)?;
    buf.resize(len, 0);
    Ok(buf)
}

/// Decode one complete owned TERMINAL payload.
///
/// The enclosing record reader remains responsible for verifying exact payload
/// exhaustion.
fn decode_payload(reader: &mut dyn io::Reader) -> Result<DecodedPayload, PayloadDecodeError> {
    let header = Header::decode(reader)?;

    // Restore the native tab-stop representation from its packed wire bits.
    let mut tabstops = Tabstops::init(header.columns as usize, 0).map_err(|_| Error::OutOfMemory)?;
    let tabstop_len = (header.columns as usize).div_ceil(8);
    for byte_index in 0..tabstop_len {
        let raw = reader.take_byte()?;
        for bit in 0..8 {
            let mask = 1u8 << bit;
            let column = byte_index * 8 + bit;
            if column < header.columns as usize && raw & mask != 0 {
                tabstops.set(column);
            }
        }
    }

    // Reconstruct the dynamic palette from its original values and sparse
    // current-value overrides.
    let mut original: color::Palette = [Rgb::default(); 256];
    for value in &mut original {
        *value = decode_rgb(reader)?;
    }
    let mut override_mask = [0u8; 32];
    reader.read_slice_all(&mut override_mask)?;

    let mut palette = color::DynamicPalette::init(original);
    for index in 0..256usize {
        let mask = 1u8 << (index % 8);
        if override_mask[index / 8] & mask != 0 {
            palette.set(index as u8, decode_rgb(reader)?);
        }
    }

    // Read the two owned terminal strings last so all earlier fixed-size
    // validation happens before allocating them.
    let pwd_len = io::read_int::<u32>(reader)? as usize;
    let mut pwd = alloc_bytes(pwd_len)?;
    reader.read_slice_all(&mut pwd)?;

    let title_len = io::read_int::<u32>(reader)? as usize;
    let mut title = alloc_bytes(title_len)?;
    reader.read_slice_all(&mut title)?;

    Ok(DecodedPayload {
        header,
        tabstops,
        palette,
        pwd,
        title,
    })
}

/// Errors possible while encoding one native TERMINAL record:
/// `ScrollbackLimitOverflow`, payload encode errors, record finish errors.
pub type EncodeError = Error;

/// Encode terminal-wide native state as one framed TERMINAL record.
///
/// State not represented by this snapshot version is ignored. Payload failures
/// emit no part of the TERMINAL record.
pub fn encode(terminal: &Terminal, destination: &mut record::Writer<'_>) -> Result<(), EncodeError> {
    let header = Header::init(terminal)?;

    let payload = destination.begin(record::Tag::TERMINAL);
    if let Err(e) = encode_payload(
        &header,
        &terminal.tabstops,
        &terminal.colors.palette,
        terminal.get_pwd().unwrap_or(b""),
        terminal.get_title().unwrap_or(b""),
        payload,
    ) {
        destination.cancel();
        return Err(e);
    }
    destination.finish()
}

/// Errors possible while decoding one native TERMINAL record: payload decode
/// errors, record finish errors, and `UnexpectedRecordTag`.
pub type DecodeError = Error;

/// Decode one framed TERMINAL record directly into native terminal state.
///
/// The returned terminal owns every decoded allocation and contains empty
/// primary and optional alternate screens with the declared routing. Later
/// SCREEN records replace those screens in place. On failure, all decoded and
/// partially initialized native state is released.
///
/// `record_header` is the record's header, already read from `source`. See
/// "Decoding Records" in `record.rs`.
pub fn decode(source: &mut dyn io::Reader, record_header: record::Header) -> Result<Terminal, DecodeError> {
    // Finish the complete record before constructing native terminal state.
    let payload: DecodedPayload = {
        let mut record_reader = record::Reader::init_header(source, record_header);
        if record_reader.header.tag != record::Tag::TERMINAL {
            return Err(Error::UnexpectedRecordTag);
        }

        let payload = decode_payload(record_reader.payload_reader())?;
        record_reader.finish()?;
        payload
    };

    let header = payload.header;
    let max_scrollback_bytes = native_scrollback_limit(header.max_scrollback_bytes);
    let max_scrollback_lines = native_scrollback_limit(header.max_scrollback_rows);

    // Terminal.init establishes native pools, pins, defaults, and ownership.
    // The terminal takes ownership of the decoded palette.
    let mut result = Terminal::init(terminal_mod::Options {
        cols: header.columns,
        rows: header.rows,
        max_scrollback_bytes,
        max_scrollback_lines,
        colors: Colors {
            background: header.background,
            foreground: header.foreground,
            cursor: header.cursor_color,
            palette: payload.palette,
        },
        default_modes: header.default_modes,
        default_cursor_style: header.cursor_default_style,
        default_cursor_blink: header.cursor_default_blink,
        ..Default::default()
    })
    .map_err(|err| match err {
        // Header::decode already rejects zero dimensions, so this should
        // not happen. Map it to our own error to keep DecodeError exact.
        terminal_mod::InitError::InvalidValue => Error::InvalidDimensions,
        terminal_mod::InitError::OutOfMemory => Error::OutOfMemory,
    })?;

    // Recreate the optional screen now so ScreenSet routing is complete before
    // its empty screens are replaced by the following SCREEN records.
    if header.screen_count == 2 {
        result.screens.get_init(
            TerminalScreenKey::Alternate,
            screen::Options {
                cols: header.columns,
                rows: header.rows,
                max_scrollback_bytes: Some(0),
                ..Default::default()
            },
        )?;
    }
    result.screens.switch_to(header.active_screen_key);

    // Restore terminal-wide runtime state that is not an initialization
    // policy. Presentation-only flags retain Terminal.init defaults.
    result.width_px = header.width_px;
    result.height_px = header.height_px;
    result.scrolling_region = ScrollingRegion {
        top: header.scrolling_region_top,
        bottom: header.scrolling_region_bottom,
        left: header.scrolling_region_left,
        right: header.scrolling_region_right,
    };
    result.status_display = header.status_display;
    result.previous_char = header.previous_codepoint;
    result.cursor = terminal_mod::Cursor {
        is_default: header.cursor_is_default,
        default_style: header.cursor_default_style,
        default_blink: header.cursor_default_blink,
    };
    result.modes = ModeState {
        values: header.current_modes,
        saved: header.saved_modes,
        default: header.default_modes,
    };
    result.flags.shell_redraws_prompt = header.shell_redraw;
    result.flags.modify_other_keys_2 = header.modify_other_keys_2;
    result.flags.mouse_event = header.mouse_event;
    result.flags.mouse_format = header.mouse_format;
    result.flags.mouse_shift_capture = match header.mouse_shift_capture {
        Some(true) => MouseShiftCapture::True,
        Some(false) => MouseShiftCapture::False,
        None => MouseShiftCapture::Null,
    };
    result.flags.password_input = header.password_input;
    result.mouse_shape = header.mouse_shape;

    // Native strings retain trailing NUL sentinels. Setters construct those
    // without exposing that detail in the wire format.
    result.set_pwd(&payload.pwd)?;
    result.set_title(&payload.title)?;

    // Transfer decoded tab-stop ownership last.
    result.tabstops = payload.tabstops;

    result.screens.active.pages.assert_integrity();
    Ok(result)
}

fn status_display_from_int(v: u8) -> Option<ansi::StatusDisplay> {
    match v {
        0 => Some(ansi::StatusDisplay::Main),
        1 => Some(ansi::StatusDisplay::StatusLine),
        _ => None,
    }
}

fn cursor_style_from_int(v: u8) -> Option<CursorStyle> {
    match v {
        0 => Some(CursorStyle::Bar),
        1 => Some(CursorStyle::Block),
        2 => Some(CursorStyle::Underline),
        3 => Some(CursorStyle::BlockHollow),
        _ => None,
    }
}

fn redraw_from_int(v: u8) -> Option<osc_sp::Redraw> {
    match v {
        0 => Some(osc_sp::Redraw::True),
        1 => Some(osc_sp::Redraw::False),
        2 => Some(osc_sp::Redraw::Last),
        _ => None,
    }
}

fn mouse_event_from_int(v: u8) -> Option<mouse::Event> {
    match v {
        0 => Some(mouse::Event::None),
        1 => Some(mouse::Event::X10),
        2 => Some(mouse::Event::Normal),
        3 => Some(mouse::Event::Button),
        4 => Some(mouse::Event::Any),
        _ => None,
    }
}

fn mouse_format_from_int(v: u8) -> Option<mouse::Format> {
    match v {
        0 => Some(mouse::Format::X10),
        1 => Some(mouse::Format::Utf8),
        2 => Some(mouse::Format::Sgr),
        3 => Some(mouse::Format::Urxvt),
        4 => Some(mouse::Format::SgrPixels),
        _ => None,
    }
}

/// `mouse::Shape` in declaration order (Zig: `std.enums.fromInt`).
const MOUSE_SHAPES: [mouse::Shape; 34] = {
    use mouse::Shape::*;
    [
        Default,
        ContextMenu,
        Help,
        Pointer,
        Progress,
        Wait,
        Cell,
        Crosshair,
        Text,
        VerticalText,
        Alias,
        Copy,
        Move,
        NoDrop,
        NotAllowed,
        Grab,
        Grabbing,
        AllScroll,
        ColResize,
        RowResize,
        NResize,
        EResize,
        SResize,
        WResize,
        NeResize,
        NwResize,
        SeResize,
        SwResize,
        EwResize,
        NsResize,
        NeswResize,
        NwseResize,
        ZoomIn,
        ZoomOut,
    ]
};

fn mouse_shape_from_int(v: u8) -> Option<mouse::Shape> {
    MOUSE_SHAPES.get(v as usize).copied()
}

fn encode_optional_bool(value: Option<bool>) -> u8 {
    match value {
        Some(true) => 2,
        Some(false) => 1,
        None => 0,
    }
}

fn encode_rgb(value: Rgb, writer: &mut dyn io::Writer) -> io::Result<()> {
    writer.write_byte(value.r)?;
    writer.write_byte(value.g)?;
    writer.write_byte(value.b)
}

fn decode_rgb(reader: &mut dyn io::Reader) -> io::Result<Rgb> {
    Ok(Rgb {
        r: reader.take_byte()?,
        g: reader.take_byte()?,
        b: reader.take_byte()?,
    })
}

fn encode_dynamic_rgb(value: DynamicRgb, writer: &mut dyn io::Writer) -> io::Result<()> {
    match value.default {
        Some(rgb) => {
            writer.write_byte(1)?;
            encode_rgb(rgb, writer)?;
        }
        None => {
            writer.write_byte(0)?;
            writer.splat_byte_all(0, 3)?;
        }
    }

    match value.r#override {
        Some(rgb) => {
            writer.write_byte(1)?;
            encode_rgb(rgb, writer)?;
        }
        None => {
            writer.write_byte(0)?;
            writer.splat_byte_all(0, 3)?;
        }
    }
    Ok(())
}

fn decode_dynamic_rgb(reader: &mut dyn io::Reader) -> io::Result<DynamicRgb> {
    let default_present = reader.take_byte()?;
    let default_rgb = decode_rgb(reader)?;
    let default = if default_present == 1 { Some(default_rgb) } else { None };

    let override_present = reader.take_byte()?;
    let override_rgb = decode_rgb(reader)?;
    let r#override = if override_present == 1 { Some(override_rgb) } else { None };

    Ok(DynamicRgb { default, r#override })
}

fn decode_optional_limit(raw: u64) -> Option<u64> {
    if raw == u64::MAX {
        None
    } else {
        Some(raw)
    }
}

fn encode_scrollback_limit(value: usize) -> Result<Option<u64>, Error> {
    if value == usize::MAX {
        return Ok(None);
    }
    let result = u64::try_from(value).map_err(|_| Error::ScrollbackLimitOverflow)?;
    if result == u64::MAX {
        return Err(Error::ScrollbackLimitOverflow);
    }
    Ok(Some(result))
}

fn native_scrollback_limit(value: Option<u64>) -> Option<usize> {
    let present = value?;
    Some(present.min(usize::MAX as u64 - 1) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::snapshot::fixture;
    use crate::terminal::snapshot::io::{FixedReader, FixedWriter, Limited};

    fn test_header() -> Header {
        let mut current_modes = ModePacked { bits: 0 };
        current_modes.set_disable_keyboard(true);
        let mut saved_modes = ModePacked { bits: 0 };
        saved_modes.set_in_band_size_reports(true);
        let mut default_modes = current_modes;
        default_modes.set_in_band_size_reports(true);

        Header {
            columns: 0x0102,
            rows: 0x0304,
            width_px: 0x05060708,
            height_px: 0x090a0b0c,

            scrolling_region_top: 1,
            scrolling_region_bottom: 2,
            scrolling_region_left: 3,
            scrolling_region_right: 4,

            status_display: ansi::StatusDisplay::StatusLine,
            active_screen_key: TerminalScreenKey::Alternate,
            screen_count: 2,
            previous_codepoint: Some('A' as u32),

            cursor_is_default: true,
            cursor_default_style: CursorStyle::BlockHollow,
            cursor_default_blink: Some(true),

            shell_redraw: osc_sp::Redraw::Last,
            modify_other_keys_2: true,
            mouse_event: mouse::Event::Any,
            mouse_format: mouse::Format::SgrPixels,
            mouse_shift_capture: Some(false),
            mouse_shape: mouse::Shape::ZoomOut,
            password_input: true,

            current_modes,
            saved_modes,
            default_modes,

            background: DynamicRgb {
                default: Some(Rgb { r: 1, g: 2, b: 3 }),
                r#override: None,
            },
            foreground: DynamicRgb {
                default: None,
                r#override: Some(Rgb { r: 4, g: 5, b: 6 }),
            },
            cursor_color: DynamicRgb {
                default: Some(Rgb { r: 7, g: 8, b: 9 }),
                r#override: Some(Rgb { r: 10, g: 11, b: 12 }),
            },

            max_scrollback_bytes: None,
            max_scrollback_rows: Some(0x0102030405060708),
        }
    }

    fn header_fixture() -> Vec<u8> {
        fixture::parse(include_str!("testdata/terminal-header-v1.hex"))
    }

    fn assert_palette_eq(expected: &color::DynamicPalette, actual: &color::DynamicPalette) {
        assert_eq!(expected.current, actual.current);
        assert_eq!(expected.original(), actual.original());
        assert_eq!(expected.mask, actual.mask);
    }

    fn encode_to_vec(terminal: &Terminal) -> Result<Vec<u8>, Error> {
        let mut destination = Vec::new();
        {
            let mut stream = record::Writer::init(&mut destination);
            encode(terminal, &mut stream)?;
        }
        Ok(destination)
    }

    fn decode_all(bytes: &[u8]) -> Result<Terminal, Error> {
        let mut source = FixedReader::new(bytes);
        let header = record::next_header(&mut source)?;
        decode(&mut source, header)
    }

    #[test]
    fn terminal_mode_bit_layout() {
        assert_eq!(43, mode_bits());

        let mut first = ModePacked { bits: 0 };
        first.set_disable_keyboard(true);
        assert_eq!(1u64 << 0, first.bits);

        let mut visibility = ModePacked { bits: 0 };
        visibility.set_report_visibility(true);
        assert_eq!(1u64 << 40, visibility.bits);

        let mut size_reports = ModePacked { bits: 0 };
        size_reports.set_in_band_size_reports(true);
        assert_eq!(1u64 << 41, size_reports.bits);

        let mut last = ModePacked { bits: 0 };
        last.set_kitty_paste_events(true);
        assert_eq!(1u64 << 42, last.bits);
    }

    #[test]
    fn terminal_header_golden_encoding_and_decoding() {
        let mut encoded = [0u8; Header::LEN];
        let mut writer = FixedWriter::new(&mut encoded);
        test_header().encode(&mut writer).unwrap();
        let reference = header_fixture();
        fixture::expect_equal(
            fixture::Kind::Bytes,
            "src/terminal/snapshot/testdata/terminal-header-v1.hex",
            "snapshot_fixture-terminal-header-v1.hex",
            &reference,
            writer.buffered(),
        );
        assert_eq!(Header::LEN, reference.len());

        // Exercise the streaming path with less buffered data than every integer.
        let mut limited = Limited::new(FixedReader::new(&reference), None, 1);
        assert_eq!(test_header(), Header::decode(&mut limited).unwrap());

        for fixture_len in 0..Header::LEN {
            let mut truncated = FixedReader::new(&reference[..fixture_len]);
            assert_eq!(Err(Error::EndOfStream), Header::decode(&mut truncated));
        }
    }

    #[test]
    fn terminal_header_encoding_rejects_noncanonical_values() {
        // Semantic values are validated before encoding writes anything.
        let mut invalid_header = test_header();
        invalid_header.columns = 0;
        let mut encoded = [0u8; Header::LEN];
        {
            let mut writer = FixedWriter::new(&mut encoded);
            assert_eq!(Err(Error::InvalidDimensions), invalid_header.encode(&mut writer));
            assert_eq!(0, writer.end);
        }

        invalid_header = test_header();
        invalid_header.max_scrollback_bytes = Some(u64::MAX);
        let mut writer = FixedWriter::new(&mut encoded);
        assert_eq!(Err(Error::InvalidScrollbackLimit), invalid_header.encode(&mut writer));
        assert_eq!(0, writer.end);
    }

    #[test]
    fn terminal_header_decoding_rejects_structural_values() {
        let mut invalid_dimensions = header_fixture();
        invalid_dimensions[0] = 0;
        invalid_dimensions[1] = 0;
        let mut dimensions_reader = FixedReader::new(&invalid_dimensions);
        assert_eq!(Err(Error::InvalidDimensions), Header::decode(&mut dimensions_reader));

        let mut invalid_screen_count = header_fixture();
        invalid_screen_count[23] = 3;
        let mut screen_count_reader = FixedReader::new(&invalid_screen_count);
        assert_eq!(Err(Error::InvalidScreenCount), Header::decode(&mut screen_count_reader));
    }

    #[test]
    fn terminal_header_decoding_normalizes_semantic_values() {
        let mut fixture = header_fixture();

        // Unknown enum and boolean encodings use their neutral native defaults.
        fixture[20] = 2;
        fixture[21] = 2;
        fixture[22] = 0;
        fixture[29] = 2;
        fixture[30] = 4;
        fixture[31] = 3;
        fixture[32] = 3;
        fixture[33] = 2;
        fixture[34] = 5;
        fixture[35] = 5;
        fixture[36] = 3;
        fixture[37] = 34;
        fixture[38] = 2;

        // Reserved mode bits are ignored while known low bits survive.
        fixture[44] = 4;

        // Unknown presence and nonzero absent-value bytes both become absent.
        fixture[63] = 2;
        fixture[68] = 1;
        fixture[72] = 0xAA;

        // An invalid scrolling region becomes the full terminal region.
        fixture[14] = 0x04;
        fixture[15] = 0x03;

        // Surrogates cannot become native previous-character state.
        fixture[25] = 0x00;
        fixture[26] = 0xD8;
        fixture[27] = 0x00;
        fixture[28] = 0x00;

        let th = test_header();
        let mut reader = FixedReader::new(&fixture);
        let decoded = Header::decode(&mut reader).unwrap();
        assert_eq!(ansi::StatusDisplay::Main, decoded.status_display);
        assert_eq!(TerminalScreenKey::Primary, decoded.active_screen_key);
        assert_eq!(0, decoded.scrolling_region_top);
        assert_eq!(th.rows - 1, decoded.scrolling_region_bottom);
        assert_eq!(th.scrolling_region_left, decoded.scrolling_region_left);
        assert_eq!(th.scrolling_region_right, decoded.scrolling_region_right);
        assert_eq!(None, decoded.previous_codepoint);
        assert!(decoded.cursor_is_default);
        assert_eq!(CursorStyle::Block, decoded.cursor_default_style);
        assert_eq!(None, decoded.cursor_default_blink);
        assert_eq!(osc_sp::Redraw::True, decoded.shell_redraw);
        assert!(!decoded.modify_other_keys_2);
        assert_eq!(mouse::Event::None, decoded.mouse_event);
        assert_eq!(mouse::Format::X10, decoded.mouse_format);
        assert_eq!(None, decoded.mouse_shift_capture);
        assert_eq!(mouse::Shape::Text, decoded.mouse_shape);
        assert!(!decoded.password_input);
        assert!(decoded.current_modes.disable_keyboard());
        assert_eq!(DynamicRgb::UNSET, decoded.background);
        assert_eq!(None, decoded.foreground.default);
        assert_eq!(Rgb { r: 4, g: 5, b: 6 }, decoded.foreground.r#override.unwrap());

        // Each scrolling axis is normalized independently.
        let mut invalid_horizontal = header_fixture();
        invalid_horizontal[18] = 0x02;
        invalid_horizontal[19] = 0x01;
        let mut horizontal_reader = FixedReader::new(&invalid_horizontal);
        let horizontal = Header::decode(&mut horizontal_reader).unwrap();
        assert_eq!(th.scrolling_region_top, horizontal.scrolling_region_top);
        assert_eq!(th.scrolling_region_bottom, horizontal.scrolling_region_bottom);
        assert_eq!(0, horizontal.scrolling_region_left);
        assert_eq!(th.columns - 1, horizontal.scrolling_region_right);

        // Alternate cannot be active when only the primary screen is declared.
        let mut missing_active_screen = header_fixture();
        missing_active_screen[23] = 1;
        let mut active_screen_reader = FixedReader::new(&missing_active_screen);
        assert_eq!(
            TerminalScreenKey::Primary,
            Header::decode(&mut active_screen_reader).unwrap().active_screen_key
        );

        let largest_finite = u64::MAX - 1;
        assert_eq!(
            largest_finite.min(usize::MAX as u64 - 1) as usize,
            native_scrollback_limit(Some(largest_finite)).unwrap()
        );
    }

    #[test]
    fn terminal_payload_round_trip() {
        let th = test_header();

        // Include a partial final tab-stop byte so its bit order and padding are
        // both exercised.
        let mut tabstops = Tabstops::init(th.columns as usize, 0).unwrap();
        tabstops.set(0);
        tabstops.set(8);
        tabstops.set(th.columns as usize - 1);

        // Use overrides at both ends of the palette to make ordering explicit.
        let mut palette = color::DynamicPalette::DEFAULT;
        palette.set(0, Rgb { r: 1, g: 2, b: 3 });
        palette.set(255, Rgb { r: 4, g: 5, b: 6 });

        let mut destination: Vec<u8> = Vec::new();
        encode_payload(
            &th,
            &tabstops,
            &palette,
            b"file:///tmp/example",
            b"snapshot title",
            &mut destination,
        )
        .unwrap();

        // Tab stops and the sparse palette mask both use least-significant-bit
        // first ordering.
        let bytes = &destination;
        let tabstop_len = (th.columns as usize).div_ceil(8);
        assert_eq!(0x01, bytes[Header::LEN]);
        assert_eq!(0x01, bytes[Header::LEN + 1]);
        assert_eq!(0x02, bytes[Header::LEN + tabstop_len - 1]);
        let palette_mask_offset = Header::LEN + tabstop_len + 256 * 3;
        assert_eq!(0x01, bytes[palette_mask_offset]);
        assert_eq!(0x80, bytes[palette_mask_offset + 31]);

        // Decode through a one-byte reader buffer to cover streaming of the large
        // fixed palette and owned strings.
        let mut limited = Limited::new(FixedReader::new(bytes), None, 1);
        let decoded = decode_payload(&mut limited).unwrap();

        assert_eq!(th, decoded.header);
        for column in 0..th.columns as usize {
            assert_eq!(tabstops.get(column), decoded.tabstops.get(column));
        }
        assert_palette_eq(&palette, &decoded.palette);
        assert_eq!(b"file:///tmp/example", &decoded.pwd[..]);
        assert_eq!(b"snapshot title", &decoded.title[..]);
    }

    #[test]
    fn terminal_payload_rejects_noncanonical_state() {
        let th = test_header();

        // A tab-stop set for different dimensions cannot represent this header.
        let mut tabstops = Tabstops::init(th.columns as usize - 1, 0).unwrap();
        let mut palette = color::DynamicPalette::DEFAULT;
        let mut encoded = [0u8; 2048];
        {
            let mut writer = FixedWriter::new(&mut encoded);
            assert_eq!(
                Err(Error::InvalidTabStops),
                encode_payload(&th, &tabstops, &palette, b"", b"", &mut writer)
            );
            assert_eq!(0, writer.end);
        }

        // Unmasked current colors must remain equal to their originals.
        tabstops.cols = th.columns as usize;
        palette.current[7] = Rgb { r: 1, g: 2, b: 3 };
        let mut writer = FixedWriter::new(&mut encoded);
        assert_eq!(
            Err(Error::InvalidPalette),
            encode_payload(&th, &tabstops, &palette, b"", b"", &mut writer)
        );
        assert_eq!(0, writer.end);
    }

    #[test]
    fn terminal_payload_ignores_tab_stop_padding() {
        let th = test_header();

        let tabstops = Tabstops::init(th.columns as usize, 0).unwrap();
        let mut palette = color::DynamicPalette::DEFAULT;
        palette.set(1, Rgb { r: 1, g: 2, b: 3 });

        let mut destination: Vec<u8> = Vec::new();
        encode_payload(&th, &tabstops, &palette, b"pwd", b"title", &mut destination).unwrap();

        // Only two bits in the last byte correspond to terminal columns.
        let tabstop_len = (th.columns as usize).div_ceil(8);
        let last_tabstop = Header::LEN + tabstop_len - 1;
        let mut invalid_padding = destination.clone();
        invalid_padding[last_tabstop] |= 1 << 7;
        let mut padding_reader = FixedReader::new(&invalid_padding);
        let decoded = decode_payload(&mut padding_reader).unwrap();
        for column in 0..th.columns as usize {
            assert_eq!(tabstops.get(column), decoded.tabstops.get(column));
        }
    }

    #[test]
    fn terminal_record_encodes_native_terminal_state() {
        let mut terminal = Terminal::init(terminal_mod::Options {
            cols: 10,
            rows: 3,
            max_scrollback_bytes: Some(12_345),
            max_scrollback_lines: Some(67),
            ..Default::default()
        })
        .unwrap();

        // Exercise every native header section plus sentinel-backed strings.
        terminal.width_px = 800;
        terminal.height_px = 600;
        terminal.scrolling_region = ScrollingRegion {
            top: 1,
            bottom: 2,
            left: 2,
            right: 8,
        };
        terminal.status_display = ansi::StatusDisplay::StatusLine;
        terminal.previous_char = Some('X' as u32);
        terminal.cursor = terminal_mod::Cursor {
            is_default: false,
            default_style: CursorStyle::Underline,
            default_blink: Some(true),
        };
        terminal.flags.shell_redraws_prompt = osc_sp::Redraw::Last;
        terminal.flags.modify_other_keys_2 = true;
        terminal.flags.mouse_event = mouse::Event::Button;
        terminal.flags.mouse_format = mouse::Format::Sgr;
        terminal.flags.mouse_shift_capture = MouseShiftCapture::True;
        terminal.flags.password_input = true;
        terminal.mouse_shape = mouse::Shape::Pointer;
        terminal.modes.values.set_disable_keyboard(true);
        terminal.modes.saved.set_in_band_size_reports(true);
        terminal.modes.default.set_bracketed_paste(true);
        terminal.colors.background = DynamicRgb {
            default: Some(Rgb { r: 1, g: 2, b: 3 }),
            r#override: Some(Rgb { r: 4, g: 5, b: 6 }),
        };
        terminal.colors.palette.set(7, Rgb { r: 7, g: 8, b: 9 });
        terminal.tabstops.reset(0);
        terminal.tabstops.set(1);
        terminal.tabstops.set(9);
        terminal.set_pwd(b"file:///tmp/native").unwrap();
        terminal.set_title(b"native title").unwrap();

        // Encode and restore through the public record codec.
        let destination = encode_to_vec(&terminal).unwrap();
        let restored = decode_all(&destination).unwrap();

        assert_eq!(10, restored.cols);
        assert_eq!(3, restored.rows);
        assert_eq!(800, restored.width_px);
        assert_eq!(600, restored.height_px);
        assert_eq!(terminal.scrolling_region, restored.scrolling_region);
        assert_eq!(terminal.status_display, restored.status_display);
        assert_eq!(TerminalScreenKey::Primary, restored.screens.active_key);
        assert!(restored.screens.get(TerminalScreenKey::Alternate).is_none());
        assert_eq!(Some('X' as u32), restored.previous_char);
        assert_eq!(terminal.cursor, restored.cursor);
        assert_eq!(terminal.flags.shell_redraws_prompt, restored.flags.shell_redraws_prompt);
        assert_eq!(terminal.flags.modify_other_keys_2, restored.flags.modify_other_keys_2);
        assert_eq!(terminal.flags.mouse_event, restored.flags.mouse_event);
        assert_eq!(terminal.flags.mouse_format, restored.flags.mouse_format);
        assert_eq!(terminal.flags.mouse_shift_capture, restored.flags.mouse_shift_capture);
        assert_eq!(terminal.flags.password_input, restored.flags.password_input);
        assert_eq!(terminal.mouse_shape, restored.mouse_shape);
        assert_eq!(terminal.modes, restored.modes);
        assert_eq!(terminal.colors.background, restored.colors.background);
        assert_eq!(terminal.colors.foreground, restored.colors.foreground);
        assert_eq!(terminal.colors.cursor, restored.colors.cursor);
        assert_palette_eq(&terminal.colors.palette, &restored.colors.palette);
        assert_eq!(12_345, restored.screens.active.pages.limits.bytes.explicit);
        assert_eq!(67, restored.screens.active.pages.limits.lines.explicit);
        assert!(restored.tabstops.get(1));
        assert!(restored.tabstops.get(9));
        assert_eq!(b"file:///tmp/native", restored.get_pwd().unwrap());
        assert_eq!(b"native title", restored.get_title().unwrap());
    }

    #[test]
    fn terminal_record_declares_an_initialized_alternate_screen() {
        let mut terminal = Terminal::init(terminal_mod::Options::new(10, 3)).unwrap();
        terminal.switch_screen(TerminalScreenKey::Alternate).unwrap();

        let destination = encode_to_vec(&terminal).unwrap();
        let restored = decode_all(&destination).unwrap();

        let primary = restored.screens.get(TerminalScreenKey::Primary).unwrap();
        let alternate = restored.screens.get(TerminalScreenKey::Alternate).unwrap();
        assert_eq!(TerminalScreenKey::Alternate, restored.screens.active_key);
        assert_eq!(alternate.as_ptr(), restored.screens.active.as_ptr());
        assert_ne!(primary.as_ptr(), alternate.as_ptr());
        assert_eq!(10, alternate.pages.cols);
        assert_eq!(3, alternate.pages.rows);
        assert_eq!(0, alternate.pages.limits.bytes.explicit);
    }

    #[test]
    fn terminal_payload_failure_emits_no_incomplete_record() {
        let mut terminal = Terminal::init(terminal_mod::Options::new(10, 3)).unwrap();

        let mut destination: Vec<u8> = b"prefix".to_vec();

        // Payload validation occurs in the reusable record scratch buffer. The
        // invalid TERMINAL is never emitted to the destination.
        terminal.colors.palette.current[7] = Rgb { r: 1, g: 2, b: 3 };
        {
            let mut stream = record::Writer::init(&mut destination);
            assert_eq!(Err(Error::InvalidPalette), encode(&terminal, &mut stream));
        }
        assert_eq!(b"prefix", &destination[..]);
    }

    #[test]
    fn terminal_record_decoding_rejects_malformed_input_transactionally() {
        // A valid record of another type is not accepted as TERMINAL state.
        let mut wrong_tag: Vec<u8> = Vec::new();
        {
            let mut wrong_tag_stream = record::Writer::init(&mut wrong_tag);
            let _ = wrong_tag_stream.begin(record::Tag::SCREEN);
            wrong_tag_stream.finish().unwrap();
        }
        assert_eq!(Some(Error::UnexpectedRecordTag), decode_all(&wrong_tag).err());

        // Build one canonical record for exhaustive truncation checks below.
        let mut terminal = Terminal::init(terminal_mod::Options::new(10, 3)).unwrap();
        terminal.set_pwd(b"pwd").unwrap();
        terminal.set_title(b"title").unwrap();

        let encoded = encode_to_vec(&terminal).unwrap();

        for fixture_len in 0..encoded.len() {
            let mut source = FixedReader::new(&encoded[..fixture_len]);
            let Ok(header) = record::next_header(&mut source) else {
                continue;
            };
            if decode(&mut source, header).is_ok() {
                panic!("truncated TERMINAL record decoded");
            }
        }

        // Zig additionally decodes with an allocator that fails on its first
        // allocation. Terminal construction in the port uses the global
        // allocator, so that injected-failure check is not ported.
    }
}
