//! Port of `src/cli/tui.zig`.
//!
//! STATUS: ported
//!
//! `tui.zig` itself only defines [`CAN_PRETTY_PRINT`]. The CLI actions that
//! draw to the terminal (`+list-themes`, `+boo`, and the pretty printers of
//! `+list-colors` and `+list-keybinds`) are built on the Zig `vaxis`
//! library (libvaxis). There is no Rust port of libvaxis, so the [`vaxis`]
//! module below is a small stand-in that reproduces the parts of its API
//! those actions use, with the same semantics (cell screen, child windows
//! with borders, `print`/`printSegment` with the same wrapping rules,
//! `prettyPrint`, a diffing renderer, key matching rules, the `TextInput`
//! widget). Terminal I/O (raw mode, input decoding, mouse) uses `crossterm`.
//! It lives here because every TUI action already imports this module.
//!
//! Differences from libvaxis (documented port deviations):
//! * Input is decoded by `crossterm`. Keypad keys are only distinguished
//!   when the terminal reports them (kitty keyboard protocol).
//! * The terminal color scheme is queried once at startup (`CSI ? 996 n`);
//!   live color scheme updates (mode 2031) are not decoded.
//! * Grapheme widths use `unicode-width` on extended grapheme clusters
//!   (`unicode-segmentation`) instead of vaxis' own width tables.

/// Whether the current platform can pretty print to a terminal.
pub const CAN_PRETTY_PRINT: bool = !cfg!(any(
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos"
));

/// Stand-in for the parts of the Zig `vaxis` library used by the CLI.
pub mod vaxis {
    use std::cell::RefCell;
    use std::fs::File;
    use std::rc::Rc;
    use std::io::{self, BufWriter, Write};
    use std::time::Duration;

    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    /// Control sequences (subset of vaxis `ctlseqs`).
    pub mod ctlseqs {
        pub const UNICODE_SET: &str = "\x1b[?2027h";
        pub const UNICODE_RESET: &str = "\x1b[?2027l";
        pub const SYNC_SET: &str = "\x1b[?2026h";
        pub const SYNC_RESET: &str = "\x1b[?2026l";
        pub const HIDE_CURSOR: &str = "\x1b[?25l";
        pub const SHOW_CURSOR: &str = "\x1b[?25h";
        pub const SMCUP: &str = "\x1b[?1049h";
        pub const RMCUP: &str = "\x1b[?1049l";
        pub const SGR_RESET: &str = "\x1b[m";
        pub const FG_RESET: &str = "\x1b[39m";
        pub const BG_RESET: &str = "\x1b[49m";
        pub const UL_RESET: &str = "\x1b[59m";
        pub const UL_OFF: &str = "\x1b[24m";
        pub const UL_SINGLE: &str = "\x1b[4m";
        pub const UL_DOUBLE: &str = "\x1b[4:2m";
        pub const UL_CURLY: &str = "\x1b[4:3m";
        pub const UL_DOTTED: &str = "\x1b[4:4m";
        pub const UL_DASHED: &str = "\x1b[4:5m";
        pub const BOLD_SET: &str = "\x1b[1m";
        pub const DIM_SET: &str = "\x1b[2m";
        pub const ITALIC_SET: &str = "\x1b[3m";
        pub const REVERSE_SET: &str = "\x1b[7m";
        pub const BOLD_DIM_RESET: &str = "\x1b[22m";
        pub const ITALIC_RESET: &str = "\x1b[23m";
        pub const REVERSE_RESET: &str = "\x1b[27m";
        pub const COLOR_SCHEME_REQUEST: &str = "\x1b[?996n";
        pub const COLOR_SCHEME_SET: &str = "\x1b[?2031h";
        pub const COLOR_SCHEME_RESET: &str = "\x1b[?2031l";
        pub const PRIMARY_DEVICE_ATTRS: &str = "\x1b[c";
        pub const MOUSE_SET: &str = "\x1b[?1002;1003;1004;1006h";
        pub const MOUSE_RESET: &str = "\x1b[?1002;1003;1004;1006;1016l";
        pub const ERASE_ALL: &str = "\x1b[H\x1b[2J";
    }

    // -----------------------------------------------------------------
    // Cells
    // -----------------------------------------------------------------

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum Color {
        #[default]
        Default,
        Index(u8),
        Rgb([u8; 3]),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum UnderlineStyle {
        #[default]
        Off,
        Single,
        Double,
        Curly,
        Dotted,
        Dashed,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Style {
        pub fg: Color,
        pub bg: Color,
        pub ul: Color,
        pub ul_style: UnderlineStyle,
        pub bold: bool,
        pub dim: bool,
        pub italic: bool,
        pub reverse: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Character {
        pub grapheme: String,
        /// width should only be provided when the application is sure the
        /// terminal will measure the same width.
        pub width: u8,
    }

    impl Default for Character {
        fn default() -> Self {
            Character {
                grapheme: " ".to_owned(),
                width: 1,
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct Cell {
        pub char: Character,
        pub style: Style,
        /// The hyperlink URI (empty for none).
        pub link: String,
        /// Set when the cell ends a soft-wrapped line.
        pub wrapped: bool,
        /// Set for cells written by `clear`: rendered as blank with the
        /// default style and skipped by `prettyPrint`.
        pub default: bool,
    }

    impl Cell {
        /// The cell `Window::clear` fills with (vaxis `.{ .default = true }`).
        pub fn cleared() -> Cell {
            Cell {
                default: true,
                ..Cell::default()
            }
        }

        /// A blank cell with the given style (vaxis `.{ .style = style }`).
        pub fn styled(style: Style) -> Cell {
            Cell {
                style,
                ..Cell::default()
            }
        }
    }

    /// A styled run of text.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Segment<'a> {
        pub text: &'a str,
        pub style: Style,
        /// The hyperlink URI (empty for none).
        pub link: &'a str,
    }

    /// Returns the width of the grapheme (vaxis `gwidth` with the unicode
    /// method).
    pub fn gwidth(s: &str) -> u16 {
        UnicodeWidthStr::width(s) as u16
    }

    // -----------------------------------------------------------------
    // Screen and Window
    // -----------------------------------------------------------------

    /// The internal screen buffer.
    #[derive(Debug, Clone, Default)]
    pub struct Screen {
        pub width: u16,
        pub height: u16,
        pub buf: Vec<Cell>,
        pub cursor_vis: bool,
        pub cursor_row: u16,
        pub cursor_col: u16,
    }

    impl Screen {
        pub fn init(width: u16, height: u16) -> Screen {
            Screen {
                width,
                height,
                buf: vec![Cell::default(); width as usize * height as usize],
                cursor_vis: false,
                cursor_row: 0,
                cursor_col: 0,
            }
        }

        pub fn write_cell(&mut self, col: u16, row: u16, cell: Cell) {
            if col >= self.width || row >= self.height {
                return;
            }
            let i = row as usize * self.width as usize + col as usize;
            self.buf[i] = cell;
        }

        pub fn read_cell(&self, col: u16, row: u16) -> Option<&Cell> {
            if col >= self.width || row >= self.height {
                return None;
            }
            self.buf.get(row as usize * self.width as usize + col as usize)
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum BorderWhere {
        #[default]
        None,
        All,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct BorderOptions {
        pub style: Style,
        pub where_: BorderWhere,
    }

    const SINGLE_ROUNDED: [&str; 6] = ["╭", "─", "╮", "│", "╯", "╰"];

    #[derive(Debug, Clone, Copy, Default)]
    pub struct ChildOptions {
        pub x_off: i32,
        pub y_off: i32,
        /// the width of the resulting child, including any borders
        pub width: Option<u16>,
        /// the height of the resulting child, including any borders
        pub height: Option<u16>,
        pub border: BorderOptions,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum Wrap {
        /// wrap at grapheme boundaries
        #[default]
        Grapheme,
        /// stop printing after one line
        None,
    }

    /// Options to use when printing Segments to a window
    #[derive(Debug, Clone, Copy, Default)]
    pub struct PrintOptions {
        /// vertical offset to start printing at
        pub row_offset: u16,
        /// horizontal offset to start printing at
        pub col_offset: u16,
        /// wrap behavior for printing
        pub wrap: Wrap,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct PrintResult {
        pub col: u16,
        pub row: u16,
        pub overflow: bool,
    }

    /// A view into the screen (vaxis `Window`).
    #[derive(Debug, Clone, Copy)]
    pub struct Window<'a> {
        /// absolute horizontal offset from the screen
        pub x_off: i32,
        /// absolute vertical offset from the screen
        pub y_off: i32,
        /// relative horizontal offset from parent window (only accumulates
        /// if negative so that we can clip the window correctly)
        pub parent_x_off: i32,
        pub parent_y_off: i32,
        pub width: u16,
        pub height: u16,
        pub screen: &'a RefCell<Screen>,
    }

    impl<'a> Window<'a> {
        fn init_child(
            &self,
            x_off: i32,
            y_off: i32,
            maybe_width: Option<u16>,
            maybe_height: Option<u16>,
        ) -> Window<'a> {
            let max_height = (self.height as i32 - y_off).max(0) as u16;
            let max_width = (self.width as i32 - x_off).max(0) as u16;
            let width = maybe_width.unwrap_or(max_width);
            let height = maybe_height.unwrap_or(max_height);
            Window {
                x_off: x_off + self.x_off,
                y_off: y_off + self.y_off,
                parent_x_off: (self.parent_x_off + x_off).min(0),
                parent_y_off: (self.parent_y_off + y_off).min(0),
                width: width.min(max_width),
                height: height.min(max_height),
                screen: self.screen,
            }
        }

        /// create a child window
        pub fn child(&self, opts: ChildOptions) -> Window<'a> {
            let result = self.init_child(opts.x_off, opts.y_off, opts.width, opts.height);
            if opts.border.where_ == BorderWhere::None {
                return result;
            }

            let glyph = |g: &str| Cell {
                char: Character {
                    grapheme: g.to_owned(),
                    width: 1,
                },
                style: opts.border.style,
                ..Cell::default()
            };
            let h = result.height;
            let w = result.width;
            for i in 0..w {
                result.write_cell(i, 0, glyph(SINGLE_ROUNDED[1]));
                result.write_cell(i, h.saturating_sub(1), glyph(SINGLE_ROUNDED[1]));
            }
            for i in 0..h {
                result.write_cell(0, i, glyph(SINGLE_ROUNDED[3]));
                result.write_cell(w.saturating_sub(1), i, glyph(SINGLE_ROUNDED[3]));
            }
            result.write_cell(0, 0, glyph(SINGLE_ROUNDED[0]));
            result.write_cell(w.saturating_sub(1), 0, glyph(SINGLE_ROUNDED[2]));
            result.write_cell(0, h.saturating_sub(1), glyph(SINGLE_ROUNDED[5]));
            result.write_cell(
                w.saturating_sub(1),
                h.saturating_sub(1),
                glyph(SINGLE_ROUNDED[4]),
            );

            let h_ch = h.saturating_sub(2);
            let w_ch = w.saturating_sub(2);
            result.init_child(1, 1, Some(w_ch), Some(h_ch))
        }

        fn visible(&self, col: u16, row: u16) -> bool {
            !(self.height <= row
                || self.width <= col
                || self.x_off + (col as i32) < 0
                || self.y_off + (row as i32) < 0
                || self.parent_x_off + (col as i32) < 0
                || self.parent_y_off + (row as i32) < 0)
        }

        /// writes a cell to the location in the window
        pub fn write_cell(&self, col: u16, row: u16, cell: Cell) {
            if !self.visible(col, row) {
                return;
            }
            self.screen.borrow_mut().write_cell(
                (col as i32 + self.x_off) as u16,
                (row as i32 + self.y_off) as u16,
                cell,
            );
        }

        /// reads a cell at the location in the window
        pub fn read_cell(&self, col: u16, row: u16) -> Option<Cell> {
            if !self.visible(col, row) {
                return None;
            }
            self.screen
                .borrow()
                .read_cell(
                    (col as i32 + self.x_off) as u16,
                    (row as i32 + self.y_off) as u16,
                )
                .cloned()
        }

        /// fills the window with the default cell
        pub fn clear(&self) {
            self.fill(Cell::cleared());
        }

        /// returns the width of the grapheme.
        pub fn gwidth(&self, s: &str) -> u16 {
            gwidth(s)
        }

        /// fills the window with the provided cell
        pub fn fill(&self, cell: Cell) {
            let mut screen = self.screen.borrow_mut();
            if self.x_off + (self.width as i32) < 0
                || self.y_off + (self.height as i32) < 0
                || (screen.width as i32) < self.x_off
                || (screen.height as i32) < self.y_off
            {
                return;
            }
            let first_row = self.y_off.max(0) as usize;
            let first_col = self.x_off.max(0) as usize;
            let last_row = ((self.height as i32 + self.y_off) as usize).min(screen.height as usize);
            let sw = screen.width as usize;
            let len = screen.buf.len();
            for row in first_row..last_row {
                let start = (first_col + row * sw).min(len);
                let end = (start + self.width as usize)
                    .min(start + sw.saturating_sub(first_col))
                    .min(len);
                for c in &mut screen.buf[start..end] {
                    *c = cell.clone();
                }
            }
        }

        /// hide the cursor
        pub fn hide_cursor(&self) {
            self.screen.borrow_mut().cursor_vis = false;
        }

        /// show the cursor at the given coordinates, 0 indexed
        pub fn show_cursor(&self, col: u16, row: u16) {
            if self.x_off + (col as i32) < 0
                || self.y_off + (row as i32) < 0
                || row >= self.height
                || col >= self.width
            {
                return;
            }
            let mut screen = self.screen.borrow_mut();
            screen.cursor_vis = true;
            screen.cursor_row = (row as i32 + self.y_off) as u16;
            screen.cursor_col = (col as i32 + self.x_off) as u16;
        }

        /// prints segments to the window. returns true if the text
        /// overflowed with the given wrap strategy and size.
        pub fn print(&self, segments: &[Segment<'_>], opts: PrintOptions) -> PrintResult {
            let mut row = opts.row_offset;
            let mut col = opts.col_offset;
            match opts.wrap {
                Wrap::Grapheme => {
                    let mut overflow = false;
                    'blk: for segment in segments {
                        for s in segment.text.graphemes(true) {
                            if col >= self.width {
                                row += 1;
                                col = 0;
                            }
                            if row >= self.height {
                                overflow = true;
                                break 'blk;
                            }
                            if s == "\n" {
                                row = row.saturating_add(1);
                                col = 0;
                                continue;
                            }
                            let w = self.gwidth(s);
                            if w == 0 {
                                continue;
                            }
                            self.write_cell(
                                col,
                                row,
                                Cell {
                                    char: Character {
                                        grapheme: s.to_owned(),
                                        width: w as u8,
                                    },
                                    style: segment.style,
                                    link: segment.link.to_owned(),
                                    wrapped: col + w >= self.width,
                                    default: false,
                                },
                            );
                            col += w;
                        }
                    }
                    if col >= self.width {
                        row += 1;
                        col = 0;
                    }
                    PrintResult { row, col, overflow }
                }
                Wrap::None => {
                    let mut overflow = false;
                    'blk: for segment in segments {
                        for s in segment.text.graphemes(true) {
                            if col >= self.width || s == "\n" {
                                overflow = true;
                                break 'blk;
                            }
                            let w = self.gwidth(s);
                            if w == 0 {
                                continue;
                            }
                            self.write_cell(
                                col,
                                row,
                                Cell {
                                    char: Character {
                                        grapheme: s.to_owned(),
                                        width: w as u8,
                                    },
                                    style: segment.style,
                                    link: segment.link.to_owned(),
                                    wrapped: false,
                                    default: false,
                                },
                            );
                            col = col.saturating_add(w);
                        }
                    }
                    PrintResult { row, col, overflow }
                }
            }
        }

        /// print a single segment. This is just a shortcut for
        /// print(&[segment], opts)
        pub fn print_segment(&self, segment: Segment<'_>, opts: PrintOptions) -> PrintResult {
            self.print(&[segment], opts)
        }

        /// returns the mouse event if the mouse event occurred within the
        /// window. If the mouse event occurred outside the window, None is
        /// returned
        pub fn has_mouse(&self, mouse: Option<Mouse>) -> Option<Mouse> {
            let event = mouse?;
            let (col, row) = (event.col as i32, event.row as i32);
            if col >= self.x_off
                && col < self.x_off + self.width as i32
                && row >= self.y_off
                && row < self.y_off + self.height as i32
            {
                Some(event)
            } else {
                None
            }
        }
    }

    /// Returns a Window comprising the entire screen.
    pub fn window_for(screen: &RefCell<Screen>) -> Window<'_> {
        let s = screen.borrow();
        Window {
            x_off: 0,
            y_off: 0,
            parent_x_off: 0,
            parent_y_off: 0,
            width: s.width,
            height: s.height,
            screen,
        }
    }

    // -----------------------------------------------------------------
    // Input
    // -----------------------------------------------------------------

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Modifiers {
        pub shift: bool,
        pub alt: bool,
        pub ctrl: bool,
        pub super_: bool,
    }

    impl Modifiers {
        pub const NONE: Modifiers = Modifiers {
            shift: false,
            alt: false,
            ctrl: false,
            super_: false,
        };
        pub const CTRL: Modifiers = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        pub const SHIFT: Modifiers = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        pub const ALT: Modifiers = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
    }

    /// A key press (vaxis `Key`). Special keys use the kitty keyboard
    /// protocol codepoints, like vaxis.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct Key {
        pub codepoint: u32,
        pub shifted_codepoint: Option<u32>,
        pub text: Option<String>,
        pub mods: Modifiers,
    }

    impl Key {
        pub const TAB: u32 = 0x09;
        pub const ENTER: u32 = 0x0D;
        pub const ESCAPE: u32 = 0x1B;
        pub const SPACE: u32 = 0x20;
        pub const BACKSPACE: u32 = 0x7F;
        pub const INSERT: u32 = 57348;
        pub const DELETE: u32 = 57349;
        pub const LEFT: u32 = 57350;
        pub const RIGHT: u32 = 57351;
        pub const UP: u32 = 57352;
        pub const DOWN: u32 = 57353;
        pub const PAGE_UP: u32 = 57354;
        pub const PAGE_DOWN: u32 = 57355;
        pub const HOME: u32 = 57356;
        pub const END: u32 = 57357;
        pub const F1: u32 = 57364;
        pub const KP_SUBTRACT: u32 = 57412;
        pub const KP_ADD: u32 = 57413;
        pub const KP_ENTER: u32 = 57414;
        pub const KP_UP: u32 = 57419;
        pub const KP_DOWN: u32 = 57420;
        pub const KP_PAGE_UP: u32 = 57421;
        pub const KP_PAGE_DOWN: u32 = 57422;
        pub const KP_HOME: u32 = 57423;
        pub const KP_END: u32 = 57424;

        /// Returns true if the key matches the codepoint and modifiers
        /// (vaxis rules: exact, text, shifted codepoint).
        pub fn matches(&self, cp: u32, mods: Modifiers) -> bool {
            self.match_exact(cp, mods) || self.match_text(cp, mods) || self.match_shifted_codepoint(cp, mods)
        }

        /// matches against any of the provided codepoints.
        pub fn matches_any(&self, cps: &[u32], mods: Modifiers) -> bool {
            cps.iter().any(|&cp| self.matches(cp, mods))
        }

        pub fn match_exact(&self, cp: u32, mods: Modifiers) -> bool {
            self.codepoint == cp && self.mods == mods
        }

        pub fn match_shifted_codepoint(&self, cp: u32, mods: Modifiers) -> bool {
            let Some(shifted) = self.shifted_codepoint else {
                return false;
            };
            if !self.mods.shift {
                return false;
            }
            let mut self_mods = self.mods;
            self_mods.shift = false;
            cp == shifted && self_mods == mods
        }

        pub fn match_text(&self, cp: u32, mods: Modifiers) -> bool {
            let Some(text) = self.text.as_deref() else {
                return false;
            };
            if text.is_empty() {
                return false;
            }
            let mut self_mods = self.mods;
            self_mods.shift = false;
            let cp = if cp < 128 && mods.shift {
                (cp as u8).to_ascii_uppercase() as u32
            } else {
                cp
            };
            let mut arg_mods = mods;
            arg_mods.shift = false;
            let Some(c) = char::from_u32(cp) else {
                return false;
            };
            let mut buf = [0u8; 4];
            text == c.encode_utf8(&mut buf) && self_mods == arg_mods
        }

        /// Convert a crossterm key event. Returns None for releases.
        pub fn from_crossterm(ev: crossterm::event::KeyEvent) -> Option<Key> {
            use crossterm::event::{KeyCode, KeyEventKind, KeyEventState, KeyModifiers};
            if ev.kind == KeyEventKind::Release {
                return None;
            }
            let mods = Modifiers {
                shift: ev.modifiers.contains(KeyModifiers::SHIFT),
                alt: ev.modifiers.contains(KeyModifiers::ALT),
                ctrl: ev.modifiers.contains(KeyModifiers::CONTROL),
                super_: ev.modifiers.contains(KeyModifiers::SUPER),
            };
            let keypad = ev.state.contains(KeyEventState::KEYPAD);
            let mut key = Key {
                mods,
                ..Key::default()
            };
            key.codepoint = match ev.code {
                KeyCode::Char(c) => {
                    if keypad && c == '+' {
                        Key::KP_ADD
                    } else if keypad && c == '-' {
                        Key::KP_SUBTRACT
                    } else {
                        let lower = c.to_lowercase().next().unwrap_or(c);
                        if !mods.ctrl && !mods.alt && !mods.super_ {
                            key.text = Some(c.to_string());
                        }
                        if lower != c {
                            key.shifted_codepoint = Some(c as u32);
                            key.mods.shift = true;
                        } else if mods.shift {
                            key.shifted_codepoint = Some(c as u32);
                        }
                        lower as u32
                    }
                }
                KeyCode::Enter if keypad => Key::KP_ENTER,
                KeyCode::Enter => Key::ENTER,
                KeyCode::Esc => Key::ESCAPE,
                KeyCode::Tab => Key::TAB,
                KeyCode::BackTab => {
                    key.mods.shift = true;
                    Key::TAB
                }
                KeyCode::Backspace => Key::BACKSPACE,
                KeyCode::Insert => Key::INSERT,
                KeyCode::Delete => Key::DELETE,
                KeyCode::Left => Key::LEFT,
                KeyCode::Right => Key::RIGHT,
                KeyCode::Up if keypad => Key::KP_UP,
                KeyCode::Up => Key::UP,
                KeyCode::Down if keypad => Key::KP_DOWN,
                KeyCode::Down => Key::DOWN,
                KeyCode::PageUp if keypad => Key::KP_PAGE_UP,
                KeyCode::PageUp => Key::PAGE_UP,
                KeyCode::PageDown if keypad => Key::KP_PAGE_DOWN,
                KeyCode::PageDown => Key::PAGE_DOWN,
                KeyCode::Home if keypad => Key::KP_HOME,
                KeyCode::Home => Key::HOME,
                KeyCode::End if keypad => Key::KP_END,
                KeyCode::End => Key::END,
                KeyCode::F(n) if n >= 1 => Key::F1 + (n as u32 - 1),
                _ => return None,
            };
            Some(key)
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum MouseButton {
        Left,
        Middle,
        Right,
        WheelUp,
        WheelDown,
        #[default]
        None,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum MouseType {
        #[default]
        Press,
        Release,
        Motion,
        Drag,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Mouse {
        pub col: i16,
        pub row: i16,
        pub button: MouseButton,
        pub mods: Modifiers,
        pub type_: MouseType,
    }

    impl Mouse {
        pub fn from_crossterm(ev: crossterm::event::MouseEvent) -> Mouse {
            use crossterm::event::{MouseButton as B, MouseEventKind as K};
            let button_of = |b: B| match b {
                B::Left => MouseButton::Left,
                B::Right => MouseButton::Right,
                B::Middle => MouseButton::Middle,
            };
            let (button, type_) = match ev.kind {
                K::Down(b) => (button_of(b), MouseType::Press),
                K::Up(b) => (button_of(b), MouseType::Release),
                K::Drag(b) => (button_of(b), MouseType::Drag),
                K::Moved => (MouseButton::None, MouseType::Motion),
                K::ScrollUp => (MouseButton::WheelUp, MouseType::Press),
                K::ScrollDown => (MouseButton::WheelDown, MouseType::Press),
                K::ScrollLeft | K::ScrollRight => (MouseButton::None, MouseType::Press),
            };
            use crossterm::event::KeyModifiers;
            Mouse {
                col: ev.column as i16,
                row: ev.row as i16,
                button,
                type_,
                mods: Modifiers {
                    shift: ev.modifiers.contains(KeyModifiers::SHIFT),
                    alt: ev.modifiers.contains(KeyModifiers::ALT),
                    ctrl: ev.modifiers.contains(KeyModifiers::CONTROL),
                    super_: false,
                },
            }
        }
    }

    /// The terminal color scheme (vaxis `Color.Scheme`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ColorScheme {
        Dark,
        Light,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Winsize {
        pub rows: u16,
        pub cols: u16,
        pub x_pixel: u16,
        pub y_pixel: u16,
    }

    /// The events produced by [`Loop`].
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Event {
        KeyPress(Key),
        Mouse(Mouse),
        ColorScheme(ColorScheme),
        Winsize(Winsize),
    }

    // -----------------------------------------------------------------
    // Tty
    // -----------------------------------------------------------------

    /// The controlling terminal (vaxis `Tty`). Output is written to
    /// `/dev/tty` (falling back to stdout).
    pub struct Tty {
        writer: BufWriter<Box<dyn Write>>,
        fd: i32,
        raw: bool,
    }

    impl Tty {
        pub fn init() -> io::Result<Tty> {
            let (writer, fd): (Box<dyn Write>, i32) = match File::options().read(true).write(true).open("/dev/tty") {
                Ok(f) => {
                    use std::os::unix::io::AsRawFd;
                    let fd = f.as_raw_fd();
                    (Box::new(f), fd)
                }
                Err(_) => (Box::new(io::stdout()), 1),
            };
            Ok(Tty {
                writer: BufWriter::with_capacity(4096, writer),
                fd,
                raw: false,
            })
        }

        pub fn writer(&mut self) -> &mut BufWriter<Box<dyn Write>> {
            &mut self.writer
        }

        /// Get the window size from the kernel.
        pub fn get_winsize(&self) -> io::Result<Winsize> {
            // SAFETY: TIOCGWINSZ fills a `winsize` struct; the fd is valid
            // for the lifetime of `self`.
            unsafe {
                let mut ws: libc::winsize = std::mem::zeroed();
                if libc::ioctl(self.fd, libc::TIOCGWINSZ, &mut ws) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Winsize {
                    rows: ws.ws_row,
                    cols: ws.ws_col,
                    x_pixel: ws.ws_xpixel,
                    y_pixel: ws.ws_ypixel,
                })
            }
        }

        fn enable_raw(&mut self) -> io::Result<()> {
            if !self.raw {
                crossterm::terminal::enable_raw_mode()?;
                self.raw = true;
            }
            Ok(())
        }

        fn disable_raw(&mut self) {
            if self.raw {
                let _ = crossterm::terminal::disable_raw_mode();
                self.raw = false;
            }
        }

        /// Read pending input bytes with a timeout, used to read query
        /// responses before the event loop starts.
        fn read_timeout(&self, timeout: Duration) -> Vec<u8> {
            use std::os::unix::io::AsRawFd;
            // crossterm reads from stdin if it is a tty, else /dev/tty.
            // SAFETY: isatty is always safe to call.
            let input_fd = if unsafe { libc::isatty(0) } == 1 {
                0
            } else {
                self.fd
            };
            let _ = io::stdin().as_raw_fd();
            let mut out = Vec::new();
            let deadline = std::time::Instant::now() + timeout;
            loop {
                let now = std::time::Instant::now();
                if now >= deadline {
                    break;
                }
                let ms = (deadline - now).as_millis().min(i32::MAX as u128) as i32;
                let mut pfd = libc::pollfd {
                    fd: input_fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: pfd is a valid pollfd for the duration of the call.
                let n = unsafe { libc::poll(&mut pfd, 1, ms) };
                if n <= 0 {
                    break;
                }
                let mut buf = [0u8; 256];
                // SAFETY: buf is valid for writes of buf.len() bytes.
                let r = unsafe { libc::read(input_fd, buf.as_mut_ptr().cast(), buf.len()) };
                if r <= 0 {
                    break;
                }
                out.extend_from_slice(&buf[..r as usize]);
                // Stop once the primary device attributes response arrives.
                if contains_da1_response(&out) {
                    break;
                }
            }
            out
        }
    }

    impl Drop for Tty {
        fn drop(&mut self) {
            let _ = self.writer.flush();
            self.disable_raw();
        }
    }

    fn contains_da1_response(buf: &[u8]) -> bool {
        // ESC [ ? ... c
        let mut i = 0;
        while i + 2 < buf.len() {
            if buf[i] == 0x1b && buf[i + 1] == b'[' && buf[i + 2] == b'?' {
                let mut j = i + 3;
                while j < buf.len() && (buf[j].is_ascii_digit() || buf[j] == b';') {
                    j += 1;
                }
                if j < buf.len() && buf[j] == b'c' {
                    return true;
                }
            }
            i += 1;
        }
        false
    }

    /// Parse a color scheme report (`CSI ? 997 ; 1|2 n`).
    pub fn parse_color_scheme_report(buf: &[u8]) -> Option<ColorScheme> {
        let needle = b"\x1b[?997;";
        let pos = buf.windows(needle.len()).position(|w| w == needle)?;
        match buf.get(pos + needle.len()..pos + needle.len() + 2)? {
            b"1n" => Some(ColorScheme::Dark),
            b"2n" => Some(ColorScheme::Light),
            _ => None,
        }
    }

    // -----------------------------------------------------------------
    // Vaxis
    // -----------------------------------------------------------------

    #[derive(Debug, Clone, Copy, Default)]
    pub struct Capabilities {
        pub unicode: bool,
        pub color_scheme_updates: bool,
    }

    #[derive(Debug, Clone, Copy, Default)]
    struct State {
        alt_screen: bool,
        mouse: bool,
        color_scheme_updates: bool,
    }

    /// The vaxis state: the screen being drawn and the last rendered one.
    pub struct Vaxis {
        /// Shared so that windows can be created without borrowing the
        /// `Vaxis` (see [`window_for`]).
        pub screen: Rc<RefCell<Screen>>,
        screen_last: Screen,
        pub caps: Capabilities,
        state: State,
        refresh: bool,
        /// The color scheme reported by the terminal during
        /// [`Vaxis::query_terminal`], delivered as the first event.
        pending_color_scheme: Option<ColorScheme>,
    }

    impl Default for Vaxis {
        fn default() -> Self {
            Vaxis::init()
        }
    }

    impl Vaxis {
        pub fn init() -> Vaxis {
            Vaxis {
                screen: Rc::new(RefCell::new(Screen::default())),
                screen_last: Screen::default(),
                caps: Capabilities::default(),
                state: State::default(),
                refresh: false,
                pending_color_scheme: None,
            }
        }

        /// Restore the terminal state.
        pub fn deinit<W: Write + ?Sized>(&mut self, tty: &mut W) {
            if self.state.color_scheme_updates {
                let _ = tty.write_all(ctlseqs::COLOR_SCHEME_RESET.as_bytes());
                self.state.color_scheme_updates = false;
            }
            if self.state.mouse {
                let _ = self.set_mouse_mode(tty, false);
            }
            if self.state.alt_screen {
                let _ = self.exit_alt_screen(tty);
            } else {
                let _ = tty.write_all(ctlseqs::SGR_RESET.as_bytes());
            }
            let _ = tty.write_all(ctlseqs::SHOW_CURSOR.as_bytes());
            let _ = tty.flush();
        }

        /// Resize the screen buffers.
        pub fn resize<W: Write + ?Sized>(&mut self, tty: &mut W, winsize: Winsize) -> io::Result<()> {
            *self.screen.borrow_mut() = Screen::init(winsize.cols, winsize.rows);
            self.screen_last = Screen::init(winsize.cols, winsize.rows);
            if self.state.alt_screen {
                tty.write_all(ctlseqs::ERASE_ALL.as_bytes())?;
            }
            self.refresh = true;
            tty.flush()
        }

        /// Returns a Window comprising the entire terminal screen.
        pub fn window(&self) -> Window<'_> {
            window_for(&self.screen)
        }

        pub fn enter_alt_screen<W: Write + ?Sized>(&mut self, tty: &mut W) -> io::Result<()> {
            tty.write_all(ctlseqs::SMCUP.as_bytes())?;
            tty.flush()?;
            self.state.alt_screen = true;
            Ok(())
        }

        pub fn exit_alt_screen<W: Write + ?Sized>(&mut self, tty: &mut W) -> io::Result<()> {
            tty.write_all(ctlseqs::SGR_RESET.as_bytes())?;
            tty.write_all(ctlseqs::RMCUP.as_bytes())?;
            tty.flush()?;
            self.state.alt_screen = false;
            Ok(())
        }

        pub fn set_title<W: Write + ?Sized>(&mut self, tty: &mut W, title: &str) -> io::Result<()> {
            write!(tty, "\x1b]2;{}\x1b\\", title)?;
            tty.flush()
        }

        pub fn set_mouse_mode<W: Write + ?Sized>(&mut self, tty: &mut W, enable: bool) -> io::Result<()> {
            if enable {
                tty.write_all(ctlseqs::MOUSE_SET.as_bytes())?;
            } else {
                tty.write_all(ctlseqs::MOUSE_RESET.as_bytes())?;
            }
            self.state.mouse = enable;
            tty.flush()
        }

        /// Copy text to the system clipboard (OSC 52).
        pub fn copy_to_system_clipboard<W: Write + ?Sized>(&mut self, tty: &mut W, text: &str) -> io::Result<()> {
            use base64::Engine as _;
            let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
            write!(tty, "\x1b]52;c;{}\x1b\\", encoded)?;
            tty.flush()
        }

        /// Query the terminal for capabilities. Only the color scheme
        /// (`CSI ? 996 n`) is queried; the response is read until the
        /// primary device attributes response arrives or `timeout`
        /// passes. Requires raw mode (started by [`Loop::start`]).
        pub fn query_terminal(&mut self, tty: &mut Tty, timeout: Duration) -> io::Result<()> {
            tty.writer.write_all(ctlseqs::COLOR_SCHEME_REQUEST.as_bytes())?;
            tty.writer.write_all(ctlseqs::PRIMARY_DEVICE_ATTRS.as_bytes())?;
            tty.writer.flush()?;
            let response = tty.read_timeout(timeout);
            if let Some(scheme) = parse_color_scheme_report(&response) {
                self.caps.color_scheme_updates = true;
                self.pending_color_scheme = Some(scheme);
            }
            Ok(())
        }

        /// Subscribe to color scheme updates (mode 2031).
        pub fn subscribe_to_color_scheme_updates<W: Write + ?Sized>(&mut self, tty: &mut W) -> io::Result<()> {
            tty.write_all(ctlseqs::COLOR_SCHEME_SET.as_bytes())?;
            self.state.color_scheme_updates = true;
            tty.flush()
        }

        /// Draw the screen to the terminal (diffing against the last
        /// rendered screen).
        pub fn render<W: Write + ?Sized>(&mut self, tty: &mut W) -> io::Result<()> {
            let screen = self.screen.borrow();
            let mut out: Vec<u8> = Vec::with_capacity(16 * 1024);
            out.extend_from_slice(ctlseqs::SYNC_SET.as_bytes());
            out.extend_from_slice(ctlseqs::HIDE_CURSOR.as_bytes());
            out.extend_from_slice(ctlseqs::SGR_RESET.as_bytes());

            let refresh = self.refresh
                || self.screen_last.width != screen.width
                || self.screen_last.height != screen.height;

            let width = screen.width.max(1) as usize;
            let mut cursor = Style::default();
            let mut link = String::new();
            let mut cursor_pos: Option<(usize, usize)> = None;
            let mut i = 0usize;
            while i < screen.buf.len() {
                let cell = &screen.buf[i];
                let w = (cell.char.width.max(1)) as usize;
                let row = i / width;
                let col = i % width;
                if !refresh && self.screen_last.buf.get(i) == Some(cell) {
                    i += w;
                    continue;
                }
                if cursor_pos != Some((row, col)) {
                    write!(out, "\x1b[{};{}H", row + 1, col + 1)?;
                }
                let style = if cell.default { Style::default() } else { cell.style };
                write_style_diff(&mut out, &cursor, &style)?;
                cursor = style;
                let cell_link = if cell.default { "" } else { cell.link.as_str() };
                if link != cell_link {
                    write!(out, "\x1b]8;;{}\x1b\\", cell_link)?;
                    link = cell_link.to_owned();
                }
                if cell.default {
                    out.push(b' ');
                } else {
                    out.extend_from_slice(cell.char.grapheme.as_bytes());
                }
                cursor_pos = if col + w < width { Some((row, col + w)) } else { None };
                i += w;
            }
            if !link.is_empty() {
                out.extend_from_slice(b"\x1b]8;;\x1b\\");
            }
            out.extend_from_slice(ctlseqs::SGR_RESET.as_bytes());
            if screen.cursor_vis {
                write!(out, "\x1b[{};{}H", screen.cursor_row + 1, screen.cursor_col + 1)?;
                out.extend_from_slice(ctlseqs::SHOW_CURSOR.as_bytes());
            }
            out.extend_from_slice(ctlseqs::SYNC_RESET.as_bytes());
            tty.write_all(&out)?;
            self.screen_last = screen.clone();
            self.refresh = false;
            Ok(())
        }

        /// Prints the screen to the tty without the alt screen (vaxis
        /// `prettyPrint`): cells written by `clear` are skipped and the
        /// cursor is repositioned with newlines and `CUF`.
        pub fn pretty_print<W: Write + ?Sized>(&mut self, tty: &mut W) -> io::Result<()> {
            if self.state.alt_screen {
                return Err(io::Error::other("NotInPrimaryScreen"));
            }
            let screen = self.screen.borrow();
            let mut out: Vec<u8> = Vec::with_capacity(4096);
            out.extend_from_slice(ctlseqs::HIDE_CURSOR.as_bytes());
            out.extend_from_slice(ctlseqs::SYNC_SET.as_bytes());
            out.extend_from_slice(ctlseqs::SGR_RESET.as_bytes());

            let mut reposition = false;
            let mut row: u16 = 0;
            let mut col: u16 = 0;
            let mut cursor = Style::default();
            let mut link = String::new();
            let mut cursor_pos: (u16, u16) = (0, 0);

            let mut i: usize = 0;
            while i < screen.buf.len() {
                let cell = &screen.buf[i];
                let w: u16 = if cell.char.width != 0 {
                    cell.char.width as u16
                } else {
                    gwidth(&cell.char.grapheme).max(1)
                };
                if col >= screen.width {
                    row += 1;
                    col = 0;
                    // Rely on terminal wrapping to reposition into next row
                    // instead of forcing it
                    if !cell.wrapped {
                        reposition = true;
                    }
                }
                if cell.default {
                    reposition = true;
                    col += w;
                    i += w as usize;
                    continue;
                }

                // reposition the cursor, if needed
                if reposition {
                    reposition = false;
                    link.clear();
                    if cursor_pos.0 == row {
                        let n = col - cursor_pos.1;
                        if n > 0 {
                            write!(out, "\x1b[{}C", n)?;
                        }
                    } else {
                        let n = row - cursor_pos.0;
                        for _ in 0..n {
                            out.push(b'\n');
                        }
                        out.push(b'\r');
                        if col > 0 {
                            write!(out, "\x1b[{}C", col)?;
                        }
                    }
                }

                write_style_diff(&mut out, &cursor, &cell.style)?;
                if link != cell.link {
                    write!(out, "\x1b]8;;{}\x1b\\", cell.link)?;
                }
                out.extend_from_slice(cell.char.grapheme.as_bytes());
                cursor_pos = (row, col + w);
                cursor = cell.style;
                link = cell.link.clone();

                col += w;
                i += w as usize;
            }
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(ctlseqs::SGR_RESET.as_bytes());
            out.extend_from_slice(ctlseqs::SYNC_RESET.as_bytes());
            tty.write_all(&out)?;
            tty.flush()
        }

        /// Take the color scheme reported during `query_terminal`.
        fn take_pending_color_scheme(&mut self) -> Option<ColorScheme> {
            self.pending_color_scheme.take()
        }
    }

    fn write_color(out: &mut Vec<u8>, color: Color, base: u8, bright: u16, ext: u8, reset: &str) -> io::Result<()> {
        match color {
            Color::Default => out.extend_from_slice(reset.as_bytes()),
            Color::Index(idx) if idx < 8 => write!(out, "\x1b[{}{}m", base, idx)?,
            Color::Index(idx) if idx < 16 => write!(out, "\x1b[{}m", bright + (idx as u16 - 8))?,
            Color::Index(idx) => write!(out, "\x1b[{}:5:{}m", ext, idx)?,
            Color::Rgb([r, g, b]) => write!(out, "\x1b[{}:2:{}:{}:{}m", ext, r, g, b)?,
        }
        Ok(())
    }

    /// Write the SGR sequences to go from style `cursor` to `style`.
    fn write_style_diff(out: &mut Vec<u8>, cursor: &Style, style: &Style) -> io::Result<()> {
        if cursor.fg != style.fg {
            write_color(out, style.fg, 3, 90, 38, ctlseqs::FG_RESET)?;
        }
        if cursor.bg != style.bg {
            write_color(out, style.bg, 4, 100, 48, ctlseqs::BG_RESET)?;
        }
        if cursor.ul != style.ul {
            match style.ul {
                Color::Default => out.extend_from_slice(ctlseqs::UL_RESET.as_bytes()),
                Color::Index(idx) => write!(out, "\x1b[58:5:{}m", idx)?,
                Color::Rgb([r, g, b]) => write!(out, "\x1b[58:2:{}:{}:{}m", r, g, b)?,
            }
        }
        if cursor.ul_style != style.ul_style {
            out.extend_from_slice(
                match style.ul_style {
                    UnderlineStyle::Off => ctlseqs::UL_OFF,
                    UnderlineStyle::Single => ctlseqs::UL_SINGLE,
                    UnderlineStyle::Double => ctlseqs::UL_DOUBLE,
                    UnderlineStyle::Curly => ctlseqs::UL_CURLY,
                    UnderlineStyle::Dotted => ctlseqs::UL_DOTTED,
                    UnderlineStyle::Dashed => ctlseqs::UL_DASHED,
                }
                .as_bytes(),
            );
        }
        if cursor.bold != style.bold {
            out.extend_from_slice(if style.bold { ctlseqs::BOLD_SET } else { ctlseqs::BOLD_DIM_RESET }.as_bytes());
            if style.dim {
                out.extend_from_slice(ctlseqs::DIM_SET.as_bytes());
            }
        }
        if cursor.dim != style.dim {
            out.extend_from_slice(if style.dim { ctlseqs::DIM_SET } else { ctlseqs::BOLD_DIM_RESET }.as_bytes());
            if style.bold {
                out.extend_from_slice(ctlseqs::BOLD_SET.as_bytes());
            }
        }
        if cursor.italic != style.italic {
            out.extend_from_slice(if style.italic { ctlseqs::ITALIC_SET } else { ctlseqs::ITALIC_RESET }.as_bytes());
        }
        if cursor.reverse != style.reverse {
            out.extend_from_slice(if style.reverse { ctlseqs::REVERSE_SET } else { ctlseqs::REVERSE_RESET }.as_bytes());
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Loop
    // -----------------------------------------------------------------

    /// The event loop (vaxis `Loop`): puts the terminal in raw mode and
    /// decodes input events.
    pub struct Loop;

    impl Loop {
        /// Enter raw mode and start reading events.
        pub fn start(tty: &mut Tty) -> io::Result<Loop> {
            tty.enable_raw()?;
            Ok(Loop)
        }

        /// Leave raw mode.
        pub fn stop(self, tty: &mut Tty) {
            tty.disable_raw();
        }

        fn convert(ev: crossterm::event::Event) -> Option<Event> {
            use crossterm::event::Event as E;
            match ev {
                E::Key(k) => Key::from_crossterm(k).map(Event::KeyPress),
                E::Mouse(m) => Some(Event::Mouse(Mouse::from_crossterm(m))),
                E::Resize(cols, rows) => Some(Event::Winsize(Winsize {
                    rows,
                    cols,
                    x_pixel: 0,
                    y_pixel: 0,
                })),
                _ => None,
            }
        }

        /// Wait for an event (or `timeout`). Returns None on timeout.
        pub fn next_event(&mut self, vx: &mut Vaxis, timeout: Option<Duration>) -> io::Result<Option<Event>> {
            if let Some(scheme) = vx.take_pending_color_scheme() {
                return Ok(Some(Event::ColorScheme(scheme)));
            }
            let deadline = timeout.map(|t| std::time::Instant::now() + t);
            loop {
                let wait = match deadline {
                    Some(d) => {
                        let now = std::time::Instant::now();
                        if now >= d {
                            return Ok(None);
                        }
                        d - now
                    }
                    None => Duration::from_secs(3600),
                };
                if !crossterm::event::poll(wait)? {
                    if deadline.is_some() {
                        return Ok(None);
                    }
                    continue;
                }
                if let Some(ev) = Loop::convert(crossterm::event::read()?) {
                    return Ok(Some(ev));
                }
            }
        }

        /// Block until at least one event is available (vaxis `pollEvent`).
        pub fn poll_event(&mut self, vx: &mut Vaxis) -> io::Result<Option<Event>> {
            self.next_event(vx, None)
        }

        /// Return a pending event without blocking (vaxis `tryEvent`).
        pub fn try_event(&mut self, vx: &mut Vaxis) -> io::Result<Option<Event>> {
            if let Some(scheme) = vx.take_pending_color_scheme() {
                return Ok(Some(Event::ColorScheme(scheme)));
            }
            while crossterm::event::poll(Duration::ZERO)? {
                if let Some(ev) = Loop::convert(crossterm::event::read()?) {
                    return Ok(Some(ev));
                }
            }
            Ok(None)
        }
    }

    // -----------------------------------------------------------------
    // Widgets
    // -----------------------------------------------------------------

    pub mod widgets {
        use super::*;

        /// A single line text input (vaxis `widgets.TextInput`). The text
        /// is stored as a gap buffer: `first_half` is before the cursor,
        /// `second_half` after it.
        #[derive(Debug, Clone, Default)]
        pub struct TextInput {
            pub buf: Buffer,
            /// the number of graphemes to skip when drawing. Used for
            /// horizontal scrolling
            pub draw_offset: u16,
            /// the column we placed the cursor the last time we drew
            pub prev_cursor_col: u16,
            /// the grapheme index of the cursor the last time we drew
            pub prev_cursor_idx: u16,
        }

        /// The text buffer of a [`TextInput`].
        #[derive(Debug, Clone, Default)]
        pub struct Buffer {
            text: String,
            /// byte offset of the cursor
            cursor: usize,
        }

        impl Buffer {
            pub fn first_half(&self) -> &str {
                &self.text[..self.cursor]
            }

            pub fn second_half(&self) -> &str {
                &self.text[self.cursor..]
            }

            pub fn real_length(&self) -> usize {
                self.text.len()
            }

            pub fn clear_retaining_capacity(&mut self) {
                self.text.clear();
                self.cursor = 0;
            }

            fn insert_slice_at_cursor(&mut self, s: &str) {
                self.text.insert_str(self.cursor, s);
                self.cursor += s.len();
            }

            fn move_gap_left(&mut self, n: usize) {
                self.cursor -= n.min(self.cursor);
            }

            fn move_gap_right(&mut self, n: usize) {
                self.cursor = (self.cursor + n).min(self.text.len());
            }

            fn grow_gap_left(&mut self, n: usize) {
                let n = n.min(self.cursor);
                self.text.replace_range(self.cursor - n..self.cursor, "");
                self.cursor -= n;
            }

            fn grow_gap_right(&mut self, n: usize) {
                let end = (self.cursor + n).min(self.text.len());
                self.text.replace_range(self.cursor..end, "");
            }
        }

        const ELLIPSIS: &str = "…";

        fn is_word_char(c: char) -> bool {
            c == '_' || c.is_alphanumeric()
        }

        impl TextInput {
            pub fn init() -> TextInput {
                TextInput::default()
            }

            pub fn update(&mut self, key: &Key) {
                let none = Modifiers::NONE;
                if key.matches(Key::BACKSPACE, none) {
                    self.delete_before_cursor();
                } else if key.matches(Key::DELETE, none) || key.matches('d' as u32, Modifiers::CTRL) {
                    self.delete_after_cursor();
                } else if key.matches(Key::LEFT, none) || key.matches('b' as u32, Modifiers::CTRL) {
                    self.cursor_left();
                } else if key.matches(Key::RIGHT, none) || key.matches('f' as u32, Modifiers::CTRL) {
                    self.cursor_right();
                } else if key.matches('a' as u32, Modifiers::CTRL) || key.matches(Key::HOME, none) {
                    let n = self.buf.first_half().len();
                    self.buf.move_gap_left(n);
                } else if key.matches('e' as u32, Modifiers::CTRL) || key.matches(Key::END, none) {
                    let n = self.buf.second_half().len();
                    self.buf.move_gap_right(n);
                } else if key.matches('k' as u32, Modifiers::CTRL) {
                    let n = self.buf.second_half().len();
                    self.buf.grow_gap_right(n);
                } else if key.matches('u' as u32, Modifiers::CTRL) {
                    let n = self.buf.cursor;
                    self.buf.grow_gap_left(n);
                } else if key.matches('b' as u32, Modifiers::ALT) || key.matches(Key::LEFT, Modifiers::ALT) {
                    self.move_backward_wordwise();
                } else if key.matches('f' as u32, Modifiers::ALT) || key.matches(Key::RIGHT, Modifiers::ALT) {
                    self.move_forward_wordwise();
                } else if key.matches(Key::BACKSPACE, Modifiers::ALT) {
                    self.delete_word_before();
                } else if key.matches('w' as u32, Modifiers::CTRL) {
                    self.delete_word_before_whitespace();
                } else if key.matches('d' as u32, Modifiers::ALT) {
                    self.delete_word_after();
                } else if let Some(text) = key.text.as_deref() {
                    self.insert_slice_at_cursor(text);
                }
            }

            /// insert text at the cursor position
            pub fn insert_slice_at_cursor(&mut self, data: &str) {
                for g in data.graphemes(true) {
                    self.buf.insert_slice_at_cursor(g);
                }
            }

            pub fn cursor_left(&mut self) {
                let len = self.buf.first_half().graphemes(true).last().map_or(0, str::len);
                self.buf.move_gap_left(len);
            }

            pub fn cursor_right(&mut self) {
                let len = self.buf.second_half().graphemes(true).next().map_or(0, str::len);
                self.buf.move_gap_right(len);
            }

            pub fn delete_before_cursor(&mut self) {
                let len = self.buf.first_half().graphemes(true).last().map_or(0, str::len);
                self.buf.grow_gap_left(len);
            }

            pub fn delete_after_cursor(&mut self) {
                let len = self.buf.second_half().graphemes(true).next().map_or(0, str::len);
                self.buf.grow_gap_right(len);
            }

            fn word_start_before(&self) -> usize {
                let first = self.buf.first_half();
                let chars: Vec<(usize, char)> = first.char_indices().collect();
                let mut i = chars.len();
                while i > 0 && !is_word_char(chars[i - 1].1) {
                    i -= 1;
                }
                while i > 0 && is_word_char(chars[i - 1].1) {
                    i -= 1;
                }
                chars.get(i).map_or(first.len(), |c| c.0)
            }

            fn word_end_after(&self) -> usize {
                let second = self.buf.second_half();
                let mut iter = second.char_indices().peekable();
                while let Some(&(_, c)) = iter.peek() {
                    if is_word_char(c) {
                        break;
                    }
                    iter.next();
                }
                while let Some(&(_, c)) = iter.peek() {
                    if !is_word_char(c) {
                        break;
                    }
                    iter.next();
                }
                iter.peek().map_or(second.len(), |c| c.0)
            }

            pub fn move_backward_wordwise(&mut self) {
                let start = self.word_start_before();
                self.buf.move_gap_left(self.buf.cursor - start);
            }

            pub fn move_forward_wordwise(&mut self) {
                let n = self.word_end_after();
                self.buf.move_gap_right(n);
            }

            pub fn delete_word_before(&mut self) {
                let start = self.word_start_before();
                self.buf.grow_gap_left(self.buf.cursor - start);
            }

            pub fn delete_word_after(&mut self) {
                let n = self.word_end_after();
                self.buf.grow_gap_right(n);
            }

            pub fn delete_word_before_whitespace(&mut self) {
                let first = self.buf.first_half();
                let trimmed = first.trim_end_matches(char::is_whitespace);
                let start = trimmed
                    .char_indices()
                    .rev()
                    .find(|(_, c)| c.is_whitespace())
                    .map_or(0, |(i, c)| i + c.len_utf8());
                self.buf.grow_gap_left(self.buf.cursor - start);
            }

            pub fn graphemes_before_cursor(&self) -> u16 {
                self.buf.first_half().graphemes(true).count() as u16
            }

            /// calculates the display width from the draw_offset to the
            /// cursor
            pub fn width_to_cursor(&self) -> u16 {
                self.buf
                    .first_half()
                    .graphemes(true)
                    .skip(self.draw_offset as usize)
                    .map(gwidth)
                    .sum()
            }

            pub fn clear_retaining_capacity(&mut self) {
                self.buf.clear_retaining_capacity();
                self.reset();
            }

            pub fn reset(&mut self) {
                self.draw_offset = 0;
                self.prev_cursor_col = 0;
                self.prev_cursor_idx = 0;
            }

            pub fn draw_with_style(&mut self, win: Window<'_>, style: Style) {
                let cursor_idx = self.graphemes_before_cursor();
                if cursor_idx < self.draw_offset {
                    self.draw_offset = cursor_idx;
                }
                if win.width == 0 {
                    return;
                }
                loop {
                    let width = self.width_to_cursor();
                    if width >= win.width {
                        self.draw_offset = self.draw_offset.saturating_add(width - win.width + 1);
                        continue;
                    }
                    break;
                }

                self.prev_cursor_idx = cursor_idx;
                self.prev_cursor_col = 0;

                let cell = |g: &str, w: u16| Cell {
                    char: Character {
                        grapheme: g.to_owned(),
                        width: w as u8,
                    },
                    style,
                    ..Cell::default()
                };

                let mut col: u16 = 0;
                let mut i: u16 = 0;
                let mut done = false;
                for g in self.buf.first_half().graphemes(true) {
                    if i < self.draw_offset {
                        i += 1;
                        continue;
                    }
                    let w = win.gwidth(g);
                    if col + w >= win.width {
                        win.write_cell(win.width - 1, 0, cell(ELLIPSIS, 1));
                        done = true;
                        break;
                    }
                    win.write_cell(col, 0, cell(g, w));
                    col += w;
                    i += 1;
                    if i == cursor_idx {
                        self.prev_cursor_col = col;
                    }
                }
                if !done {
                    for g in self.buf.second_half().graphemes(true) {
                        if i < self.draw_offset {
                            i += 1;
                            continue;
                        }
                        let w = win.gwidth(g);
                        if col + w > win.width {
                            win.write_cell(win.width - 1, 0, cell(ELLIPSIS, 1));
                            break;
                        }
                        win.write_cell(col, 0, cell(g, w));
                        col += w;
                        i += 1;
                        if i == cursor_idx {
                            self.prev_cursor_col = col;
                        }
                    }
                }
                if self.draw_offset > 0 {
                    win.write_cell(0, 0, cell(ELLIPSIS, 1));
                }
                win.show_cursor(self.prev_cursor_col, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::vaxis::widgets::TextInput;
    use super::vaxis::*;
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn can_pretty_print_on_desktop() {
        assert!(CAN_PRETTY_PRINT);
    }

    fn window(screen: &RefCell<Screen>) -> Window<'_> {
        let s = screen.borrow();
        Window {
            x_off: 0,
            y_off: 0,
            parent_x_off: 0,
            parent_y_off: 0,
            width: s.width,
            height: s.height,
            screen,
        }
    }

    #[test]
    fn window_child_sizes() {
        let screen = RefCell::new(Screen::init(20, 20));
        let parent = window(&screen);
        let ch = parent.child(ChildOptions {
            x_off: 1,
            y_off: 1,
            ..Default::default()
        });
        assert_eq!(19, ch.width);
        assert_eq!(19, ch.height);

        let ch = parent.child(ChildOptions {
            width: Some(21),
            height: Some(21),
            ..Default::default()
        });
        assert_eq!(20, ch.width);
        assert_eq!(20, ch.height);

        // Borders shrink the child by one cell on each side.
        let ch = parent.child(ChildOptions {
            width: Some(10),
            height: Some(5),
            border: BorderOptions {
                where_: BorderWhere::All,
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(8, ch.width);
        assert_eq!(3, ch.height);
        assert_eq!("╭", screen.borrow().read_cell(0, 0).unwrap().char.grapheme);
        assert_eq!("╯", screen.borrow().read_cell(9, 4).unwrap().char.grapheme);
    }

    #[test]
    fn window_print_wraps() {
        let screen = RefCell::new(Screen::init(4, 3));
        let win = window(&screen);
        let result = win.print_segment(
            Segment {
                text: "abcdef",
                ..Default::default()
            },
            PrintOptions::default(),
        );
        assert_eq!(PrintResult { col: 2, row: 1, overflow: false }, result);
        assert_eq!("e", screen.borrow().read_cell(0, 1).unwrap().char.grapheme);

        let result = win.print_segment(
            Segment {
                text: "abcdef",
                ..Default::default()
            },
            PrintOptions {
                row_offset: 2,
                wrap: Wrap::None,
                ..Default::default()
            },
        );
        assert!(result.overflow);
        assert_eq!(4, result.col);
    }

    #[test]
    fn pretty_print_skips_default_cells() {
        let mut vx = Vaxis::init();
        let mut out: Vec<u8> = Vec::new();
        vx.resize(
            &mut out,
            Winsize {
                rows: 1,
                cols: 5,
                ..Default::default()
            },
        )
        .unwrap();
        let win = vx.window();
        win.clear();
        win.print_segment(
            Segment {
                text: "ab",
                ..Default::default()
            },
            PrintOptions::default(),
        );
        out.clear();
        vx.pretty_print(&mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("ab\r\n"));
    }

    #[test]
    fn key_matching() {
        let key = |code: crossterm::event::KeyCode, mods: crossterm::event::KeyModifiers| {
            Key::from_crossterm(crossterm::event::KeyEvent::new(code, mods)).unwrap()
        };
        use crossterm::event::{KeyCode, KeyModifiers};
        let none = Modifiers::NONE;

        let k = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(k.matches('c' as u32, Modifiers::CTRL));
        assert!(!k.matches('c' as u32, none));

        let k = key(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert!(k.matches('G' as u32, none));
        assert!(!k.matches('g' as u32, none));
        assert!(k.matches('g' as u32, Modifiers::SHIFT));

        let k = key(KeyCode::Char('C'), KeyModifiers::SHIFT);
        assert!(k.matches('c' as u32, Modifiers::SHIFT));
        assert!(!k.matches('c' as u32, none));

        let k = key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(k.matches_any(&['?' as u32, Key::F1], none));

        let k = key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(k.matches(Key::ESCAPE, none));
    }

    #[test]
    fn text_input_editing() {
        let key = |c: char| {
            Key::from_crossterm(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap()
        };
        let mut ti = TextInput::init();
        for c in "hello world".chars() {
            ti.update(&key(c));
        }
        assert_eq!("hello world", ti.buf.first_half());
        ti.cursor_left();
        assert_eq!("hello worl", ti.buf.first_half());
        assert_eq!("d", ti.buf.second_half());
        ti.delete_word_before_whitespace();
        assert_eq!("hello ", ti.buf.first_half());
        ti.delete_before_cursor();
        assert_eq!("hello", ti.buf.first_half());
        assert_eq!(6, ti.buf.real_length());
        ti.clear_retaining_capacity();
        assert_eq!(0, ti.buf.real_length());
    }

    #[test]
    fn color_scheme_report() {
        assert_eq!(Some(ColorScheme::Dark), parse_color_scheme_report(b"\x1b[?997;1n\x1b[?62c"));
        assert_eq!(Some(ColorScheme::Light), parse_color_scheme_report(b"\x1b[?997;2n"));
        assert_eq!(None, parse_color_scheme_report(b"\x1b[?62c"));
    }
}
