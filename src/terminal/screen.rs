//! Port of `src/terminal/Screen.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - The `io` and `alloc` fields are dropped (global allocator, no `std.Io`).
//! - The cursor caches raw pointers into page memory (`page_row`,
//!   `page_cell`) and a tracked pin pointer (`page_pin`), exactly like the
//!   Zig version. They are dereferenced through small private helpers; the
//!   invariant (maintained by every method here) is that they always point
//!   at live memory of the page the cursor pin is on.
//! - The heap-allocated cursor hyperlink is an `Option<Box<Hyperlink>>`.
//! - `deinit` became `Drop` (it releases the Kitty image storage; the
//!   PageList and the cursor hyperlink drop on their own).
//! - Functions taking `page: *Page, row: *Row, cells: []Cell` take mutable
//!   references; callers create them from page pointers.
//! - Writers are `std::io::Write`.

use std::borrow::Cow;
use std::io;

use crate::fastmem;
use crate::terminal::ansi;
use crate::terminal::build_options;
use crate::terminal::charsets;
use crate::terminal::cursor::Style as CursorStyle;
use crate::terminal::formatter::{self, PinMap, PinMapMap, ScreenFormatter, ScreenFormatterContent};
use crate::terminal::hyperlink::{self, Hyperlink, HyperlinkId};
use crate::terminal::kitty;
use crate::terminal::kitty::graphics_image::Limits as LoadingImageLimits;
use crate::terminal::kitty::graphics_storage::ImageStorage;
use crate::terminal::osc::semantic_prompt as osc_sp;
use crate::terminal::page::{self as pagepkg, Cell, Page, Row, SemanticContent, Wide};
use crate::terminal::page_list::{
    self, CellSubset, CloneError, Direction, IncreaseCapacity, IncreaseCapacityError, Node, NodePtr, PageList, Pin,
    PinPtr, SplitError, Viewport,
};
use crate::terminal::point::{self, Coordinate, Point, Tag};
use crate::terminal::ref_counted_set::AddError;
use crate::terminal::selection::{Bounds, Order, Selection};
use crate::terminal::selection_codepoints;
use crate::terminal::sgr;
use crate::terminal::size::{self, AllocError, CellCountInt};
use crate::terminal::string_map::StringMap;
use crate::terminal::style::{self, Style};
use crate::tripwire;
use crate::unicode;

const LOG_TARGET: &str = "screen";

/// Dereference a node pointer.
#[inline(always)]
fn nd<'a>(node: NodePtr) -> &'a mut Node {
    // SAFETY: every node pointer used here comes from the screen's live
    // PageList (or a valid pin into it).
    unsafe { &mut *node.as_ptr() }
}

/// Dereference a tracked pin pointer.
#[inline(always)]
fn tp<'a>(p: PinPtr) -> &'a mut Pin {
    // SAFETY: tracked pins are live while tracked by the PageList.
    unsafe { &mut *p.as_ptr() }
}

/// The page of a node.
#[inline(always)]
fn node_page<'a>(node: NodePtr) -> &'a mut Page {
    nd(node).page()
}

/// The cells of a row in a page.
#[inline(always)]
fn row_cells<'a>(page: &Page, row: &Row) -> &'a mut [Cell] {
    // SAFETY: the row belongs to the page.
    unsafe { &mut *page.get_cells(row) }
}

/// Mutable access to a row through a raw pointer.
#[inline(always)]
fn rw<'a>(row: *mut Row) -> &'a mut Row {
    // SAFETY: row pointers come from live page memory.
    unsafe { &mut *row }
}

/// Mutable access to a cell through a raw pointer.
#[inline(always)]
fn cl<'a>(cell: *mut Cell) -> &'a mut Cell {
    // SAFETY: cell pointers come from live page memory.
    unsafe { &mut *cell }
}

/// A screen: a PageList plus cursor and other per-screen state.
pub struct Screen {
    /// The list of pages in the screen.
    pub pages: PageList,

    /// Special-case where we want no scrollback whatsoever. We have to flag
    /// this because max_size 0 in PageList gets rounded up to two pages so
    /// we can always have an active screen.
    pub no_scrollback: bool,

    /// The current cursor position
    pub cursor: Cursor,

    /// The saved cursor
    pub saved_cursor: Option<SavedCursor>,

    /// The selection for this screen (if any). This MUST be a tracked selection
    /// otherwise the selection will become invalid. Instead of accessing this
    /// directly to set it, use the `select` function which will assert and
    /// automatically setup tracking.
    pub selection: Option<Selection>,

    /// The charset state
    pub charset: CharsetState,

    /// The current or most recent protected mode. Once a protection mode is
    /// set, this will never become "off" again until the screen is reset.
    /// The current state of whether protection attributes should be set is
    /// set on the Cell pen; this is only used to determine the most recent
    /// protection mode since some sequences such as ECH depend on this.
    pub protected_mode: ansi::ProtectedMode,

    /// The kitty keyboard settings.
    pub kitty_keyboard: kitty::KeyFlagStack,

    /// Kitty graphics protocol state.
    pub kitty_images: ImageStorage,

    /// Semantic prompt (OSC133) state.
    pub semantic_prompt: SemanticPrompt,

    /// Dirty flags for the renderer.
    pub dirty: Dirty,
}

/// See Terminal.Dirty. This behaves the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dirty {
    /// Set when the selection is set or unset, regardless of if the
    /// selection is changed or not.
    pub selection: bool,

    /// When an OSC8 hyperlink is hovered, we set the full screen as dirty
    /// because links can span multiple lines.
    pub hyperlink_hover: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticPrompt {
    /// This is flipped to true when any sort of semantic content is
    /// seen. In particular, this is set to true only when a `prompt` type
    /// is ever set on our cursor.
    ///
    /// This is used to optimize away semantic content operations if we know
    /// we've never seen them.
    pub seen: bool,

    /// This is set on any `cl` or `click_events` option set on the
    /// most recent OSC 133 commands to specify how click handling in a
    /// prompt is handling.
    pub click: SemanticClick,
}

impl SemanticPrompt {
    pub const DISABLED: SemanticPrompt = SemanticPrompt {
        seen: false,
        click: SemanticClick::None,
    };
}

impl Default for SemanticPrompt {
    fn default() -> Self {
        SemanticPrompt::DISABLED
    }
}

/// (Zig: `SemanticPrompt.SemanticClickKind`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum SemanticClickKind {
    None,
    ClickEvents,
    Cl,
}

/// (Zig: `SemanticPrompt.SemanticClick`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticClick {
    None,
    ClickEvents(osc_sp::ClickEvents),
    Cl(osc_sp::Click),
}

impl SemanticClick {
    pub fn kind(&self) -> SemanticClickKind {
        match self {
            SemanticClick::None => SemanticClickKind::None,
            SemanticClick::ClickEvents(_) => SemanticClickKind::ClickEvents,
            SemanticClick::Cl(_) => SemanticClickKind::Cl,
        }
    }
}

/// The cursor position and style.
#[derive(Debug, Clone)]
pub struct Cursor {
    // The x/y position within the active area.
    pub x: CellCountInt,
    pub y: CellCountInt,

    /// The visual style of the cursor. This defaults to block because
    /// it has to default to something, but users of this struct are
    /// encouraged to set their own default.
    pub cursor_style: CursorStyle,

    /// The "last column flag (LCF)" as its called. If this is set then the
    /// next character print will force a soft-wrap.
    pub pending_wrap: bool,

    /// The protected mode state of the cursor. If this is true then
    /// all new characters printed will have the protected state set.
    pub protected: bool,

    /// The currently active style. This is the concrete style value
    /// that should be kept up to date. The style ID to use for cell writing
    /// is below.
    pub style: Style,

    /// The currently active style ID. The style is page-specific so when
    /// we change pages we need to ensure that we update that page with
    /// our style when used.
    pub style_id: style::Id,

    /// The hyperlink ID that is currently active for the cursor. A value
    /// of zero means no hyperlink is active. (Implements OSC8, saying that
    /// so code search can find it.).
    pub hyperlink_id: hyperlink::Id,

    /// This is the implicit ID to use for hyperlinks that don't specify
    /// an ID. We do an overflowing add to this so repeats can technically
    /// happen with carefully crafted inputs but for real workloads its
    /// highly unlikely -- and the fix is for the TUI program to use explicit
    /// IDs.
    pub hyperlink_implicit_id: size::OffsetInt,

    /// Heap-allocated hyperlink state so that we can recreate it when
    /// the cursor page pin changes. We can't get it from the old screen
    /// state because the page may be cleared. This is heap allocated
    /// because its most likely null.
    pub hyperlink: Option<Box<Hyperlink<'static>>>,

    /// The current semantic content type for the cursor that will be
    /// applied to any newly written cells.
    pub semantic_content: SemanticContent,
    pub semantic_content_clear_eol: bool,

    /// The pointers into the page list where the cursor is currently
    /// located. This makes it faster to move the cursor.
    pub page_pin: PinPtr,
    pub page_row: *mut Row,
    pub page_cell: *mut Cell,
}

impl Cursor {
    /// A cursor at the given position with all other fields defaulted.
    pub fn new(page_pin: PinPtr, page_row: *mut Row, page_cell: *mut Cell) -> Cursor {
        Cursor {
            x: 0,
            y: 0,
            cursor_style: CursorStyle::Block,
            pending_wrap: false,
            protected: false,
            style: Style::default(),
            style_id: style::DEFAULT_ID,
            hyperlink_id: 0,
            hyperlink_implicit_id: 0,
            hyperlink: None,
            semantic_content: SemanticContent::Output,
            semantic_content_clear_eol: false,
            page_pin,
            page_row,
            page_cell,
        }
    }

    pub fn deinit(&mut self) {
        self.hyperlink = None;
    }
}

/// Saved cursor state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SavedCursor {
    pub x: CellCountInt,
    pub y: CellCountInt,
    pub style: Style,
    pub protected: bool,
    pub pending_wrap: bool,
    pub origin: bool,
    pub charset: CharsetState,
}

/// State required for all charset operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharsetState {
    /// The list of graphical charsets by slot
    pub charsets: CharsetArray,

    /// GL is the slot to use when using a 7-bit printable char (up to 127)
    /// GR used for 8-bit printable chars.
    pub gl: charsets::Slots,
    pub gr: charsets::Slots,

    /// Single shift where a slot is used for exactly one char.
    pub single_shift: Option<charsets::Slots>,
}

impl Default for CharsetState {
    fn default() -> Self {
        CharsetState {
            charsets: CharsetArray::default(),
            gl: charsets::Slots::G0,
            gr: charsets::Slots::G2,
            single_shift: None,
        }
    }
}

/// An array to map a charset slot to a lookup table
/// (Zig: `CharsetState.CharsetArray`).
///
/// We use this bespoke struct instead of `std.EnumArray` because
/// accessing these slots is very performance critical since it's
/// done for every single print. This benchmarks faster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharsetArray {
    pub g0: charsets::Charset,
    pub g1: charsets::Charset,
    pub g2: charsets::Charset,
    pub g3: charsets::Charset,
}

impl Default for CharsetArray {
    fn default() -> Self {
        CharsetArray {
            g0: charsets::Charset::Utf8,
            g1: charsets::Charset::Utf8,
            g2: charsets::Charset::Utf8,
            g3: charsets::Charset::Utf8,
        }
    }
}

impl CharsetArray {
    #[inline(always)]
    pub fn get(&self, slot: charsets::Slots) -> charsets::Charset {
        match slot {
            charsets::Slots::G0 => self.g0,
            charsets::Slots::G1 => self.g1,
            charsets::Slots::G2 => self.g2,
            charsets::Slots::G3 => self.g3,
        }
    }

    #[inline(always)]
    pub fn set(&mut self, slot: charsets::Slots, charset: charsets::Charset) {
        match slot {
            charsets::Slots::G0 => self.g0 = charset,
            charsets::Slots::G1 => self.g1 = charset,
            charsets::Slots::G2 => self.g2 = charset,
            charsets::Slots::G3 => self.g3 = charset,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub cols: CellCountInt,
    pub rows: CellCountInt,

    /// The maximum size of scrollback in bytes. Null is unlimited, zero
    /// disables scrollback, and any other value is clamped to support a
    /// minimum of the active area.
    pub max_scrollback_bytes: Option<usize>,

    /// The maximum number of physical scrollback rows, excluding the active
    /// area. Null is unlimited. The effective limit permits at least one
    /// standard page and only complete historical pages are pruned.
    pub max_scrollback_lines: Option<usize>,

    /// The total storage limit for Kitty images in bytes for this
    /// screen. Kitty image storage is per-screen.
    pub kitty_image_storage_limit: usize,

    /// The limits for what medium types are allowed for Kitty image loading.
    pub kitty_image_loading_limits: LoadingImageLimits,
}

impl Options {
    /// A simple, default terminal. If you rely on specific dimensions or
    /// scrollback (or lack of) then do not use this directly. This is just
    /// for callers that need some defaults.
    pub fn default_value() -> Options {
        Options {
            cols: 80,
            rows: 24,
            max_scrollback_bytes: Some(0),
            max_scrollback_lines: None,
            kitty_image_storage_limit: match build_options::ARTIFACT {
                build_options::Artifact::Ghostty => 320 * 1000 * 1000, // 320MB
                build_options::Artifact::Lib => 10 * 1000 * 1000,      // 10MB
            },
            kitty_image_loading_limits: LoadingImageLimits::DIRECT,
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Options::default_value()
    }
}

/// The memory held by a screen. Returned by `memoryUsage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryUsage {
    /// Page memory, which holds every cell along with its styles,
    /// graphemes and hyperlinks.
    pub pages: page_list::MemoryUsage,

    /// Bytes of image data stored through the Kitty graphics protocol.
    /// Images are stored outside of pages, so this is not part of `pages`.
    /// Always zero when Kitty graphics is disabled at build time.
    pub image_bytes: usize,
}

/// Options for scrolling the viewport of the terminal grid. The reason
/// we have this in addition to PageList.Scroll is because we have additional
/// scroll behaviors that are not part of the PageList.Scroll enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// For all of these, see PageList.Scroll.
    Active,
    Top,
    Pin(Pin),
    Row(usize),
    DeltaRow(isize),
    DeltaPrompt(isize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resize {
    /// The new size to resize to
    pub cols: CellCountInt,
    pub rows: CellCountInt,

    /// Whether to reflow soft-wrapped text.
    ///
    /// This will reflow soft-wrapped text. If the screen size is getting
    /// smaller and the maximum scrollback size is exceeded, data will be
    /// lost from the top of the scrollback.
    pub reflow: bool,

    /// Set this to enable prompt redraw on resize. This signals
    /// that the running program can redraw the prompt if the cursor is
    /// currently at a prompt. This detects OSC133 prompts lines and clears
    /// them. If set to `.last`, only the most recent prompt line is cleared.
    pub prompt_redraw: osc_sp::Redraw,

    /// Whether the resize may pull rows out of scrollback back into the
    /// active area. See PageList.Resize for details.
    pub pull_scrollback: bool,
}

impl Default for Resize {
    fn default() -> Self {
        Resize {
            cols: 0,
            rows: 0,
            reflow: true,
            prompt_redraw: osc_sp::Redraw::False,
            pull_scrollback: true,
        }
    }
}

/// Tripwire failure points for `resize` (Zig: `resize_tw`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResizeFailPoint {
    SavedCursorPin,
    Pages,
}

pub static RESIZE_TW: tripwire::Module<ResizeFailPoint, AllocError> = tripwire::Module::new();

/// Rust-only failure point standing in for the allocation of the cursor
/// hyperlink in `startHyperlinkOnce` (Zig tests inject it with a
/// `FailingAllocator`; Rust's global allocator cannot fail).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HyperlinkFailPoint {
    LinkAlloc,
}

pub static HYPERLINK_TW: tripwire::Module<HyperlinkFailPoint, AllocError> = tripwire::Module::new();

/// Errors from `startHyperlinkOnce` (Zig: `Allocator.Error ||
/// Page.InsertHyperlinkError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartHyperlinkOnceError {
    OutOfMemory,
    Insert(pagepkg::InsertHyperlinkError),
}

impl From<AllocError> for StartHyperlinkOnceError {
    fn from(_: AllocError) -> Self {
        StartHyperlinkOnceError::OutOfMemory
    }
}

/// Options for [`Screen::cursor_copy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorCopy {
    /// Copy the hyperlink from the other cursor. If not set, this will
    /// clear our current hyperlink.
    pub hyperlink: bool,
}

impl Default for CursorCopy {
    fn default() -> Self {
        CursorCopy { hyperlink: true }
    }
}

/// The argument of [`Screen::cursor_set_semantic_content`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticContentUpdate {
    Prompt(osc_sp::PromptKind),
    Output,
    Input(InputClear),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputClear {
    ClearExplicit,
    ClearEol,
}

#[derive(Debug, Clone, Copy)]
pub struct SelectionString {
    /// The selection to convert to a string.
    pub sel: Selection,

    /// If true, trim whitespace around the selection.
    pub trim: bool,
}

impl SelectionString {
    pub fn new(sel: Selection) -> SelectionString {
        SelectionString { sel, trim: true }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SelectLine<'a> {
    /// The pin of some part of the line to select.
    pub pin: Pin,

    /// These are the codepoints to consider whitespace to trim
    /// from the ends of the selection.
    pub whitespace: Option<&'a [u32]>,

    /// If true, line selection will consider semantic prompt
    /// state changing a boundary. State changing is ANY state
    /// change.
    pub semantic_prompt_boundary: bool,
}

impl SelectLine<'static> {
    pub fn new(pin: Pin) -> SelectLine<'static> {
        SelectLine {
            pin,
            whitespace: Some(&selection_codepoints::DEFAULT_LINE_WHITESPACE),
            semantic_prompt_boundary: true,
        }
    }
}

pub struct LineIterator<'a> {
    pub screen: &'a Screen,
    pub current: Option<Pin>,
}

impl LineIterator<'_> {
    pub fn next(&mut self) -> Option<Selection> {
        let current = self.current?;
        let Some(result) = self.screen.select_line(SelectLine {
            pin: current,
            whitespace: None,
            semantic_prompt_boundary: false,
        }) else {
            self.current = None;
            return None;
        };

        self.current = result.end().down(1);
        Some(result)
    }
}

impl Iterator for LineIterator<'_> {
    type Item = Selection;
    fn next(&mut self) -> Option<Selection> {
        LineIterator::next(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PromptClickMove {
    pub left: usize,
    pub right: usize,
}

impl PromptClickMove {
    pub const ZERO: PromptClickMove = PromptClickMove { left: 0, right: 0 };
}

/// Options for [`Screen::dump_string`].
#[derive(Debug, Clone, Copy)]
pub struct DumpString {
    /// The start and end points of the dump, both inclusive. The x will
    /// be ignored and the full row will always be dumped.
    pub tl: Pin,
    pub br: Option<Pin>,

    /// If true, this will unwrap soft-wrapped lines. If false, this will
    /// dump the screen as it is visually seen in a rendered window.
    pub unwrap: bool,
}

impl DumpString {
    pub fn new(tl: Pin) -> DumpString {
        DumpString {
            tl,
            br: None,
            unwrap: true,
        }
    }
}

/// Errors from `dumpStringAlloc`.
#[derive(Debug)]
pub enum DumpStringError {
    UnknownPoint,
    Write(io::Error),
}

impl std::fmt::Display for DumpStringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for DumpStringError {}

/// Errors from `testWriteString`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestWriteError {
    OutOfMemory,
    OutOfSpace,
}

impl From<AllocError> for TestWriteError {
    fn from(_: AllocError) -> Self {
        TestWriteError::OutOfMemory
    }
}

impl From<IncreaseCapacityError> for TestWriteError {
    fn from(e: IncreaseCapacityError) -> Self {
        match e {
            IncreaseCapacityError::OutOfMemory => TestWriteError::OutOfMemory,
            IncreaseCapacityError::OutOfSpace => TestWriteError::OutOfSpace,
        }
    }
}

impl std::fmt::Display for TestWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for TestWriteError {}

fn split_to_increase(e: SplitError) -> IncreaseCapacityError {
    match e {
        SplitError::OutOfMemory => IncreaseCapacityError::OutOfMemory,
        SplitError::OutOfSpace => IncreaseCapacityError::OutOfSpace,
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        if build_options::KITTY_GRAPHICS {
            self.kitty_images.deinit(&mut self.pages);
        }
        self.cursor.deinit();
    }
}

impl Screen {
    // -----------------------------------------------------------------
    // Cursor pointer helpers (Rust-only)
    // -----------------------------------------------------------------

    /// The cursor's tracked pin (Zig: `self.cursor.page_pin.*`).
    #[inline(always)]
    fn cpin<'a>(&self) -> &'a mut Pin {
        tp(self.cursor.page_pin)
    }

    /// The cursor row (Zig: `self.cursor.page_row.*`).
    #[inline(always)]
    fn crow<'a>(&self) -> &'a mut Row {
        rw(self.cursor.page_row)
    }

    /// The cursor cell (Zig: `self.cursor.page_cell.*`).
    #[inline(always)]
    fn ccell<'a>(&self) -> &'a mut Cell {
        cl(self.cursor.page_cell)
    }

    /// The page the cursor is on.
    #[inline(always)]
    fn cpage<'a>(&self) -> &'a mut Page {
        node_page(self.cpin().node)
    }

    /// Refresh the cached row/cell pointers from the cursor pin.
    #[inline(always)]
    fn reload_cursor_rac(&mut self) {
        let page_rac = self.cpin().row_and_cell();
        self.cursor.page_row = page_rac.row;
        self.cursor.page_cell = page_rac.cell;
    }

    /// Initialize a new screen.
    ///
    /// max_scrollback_bytes is the amount of scrollback to keep in bytes. This
    /// will be rounded UP to the nearest page size because our minimum allocation
    /// size is that anyways.
    ///
    /// If max scrollback is 0, then no scrollback is kept at all.
    pub fn init(opts: Options) -> Result<Screen, AllocError> {
        // Initialize our backing pages.
        let mut pages = PageList::init(page_list::Options {
            cols: opts.cols,
            rows: opts.rows,
            max_size: opts.max_scrollback_bytes,
            max_lines: opts.max_scrollback_lines,
        })?;

        // Create our tracked pin for the cursor.
        let page_pin = pages.track_pin(Pin::new(pages.pages.first.unwrap()))?;
        let page_rac = tp(page_pin).row_and_cell();

        let mut result = Screen {
            pages,
            no_scrollback: opts.max_scrollback_bytes == Some(0),
            cursor: Cursor::new(page_pin, page_rac.row, page_rac.cell),
            saved_cursor: None,
            selection: None,
            charset: CharsetState::default(),
            protected_mode: ansi::ProtectedMode::Off,
            kitty_keyboard: kitty::KeyFlagStack::default(),
            kitty_images: ImageStorage::default(),
            semantic_prompt: SemanticPrompt::DISABLED,
            dirty: Dirty::default(),
        };

        if build_options::KITTY_GRAPHICS {
            result
                .kitty_images
                .set_limit(&mut result.pages, opts.kitty_image_storage_limit);
            result.kitty_images.image_limits = opts.kitty_image_loading_limits;
        }

        Ok(result)
    }

    /// Assert that the screen is in a consistent state. This doesn't check
    /// all pages in the page list because that is SO SLOW even just for
    /// tests. This only asserts the screen specific data so callers should
    /// ensure they're also calling page integrity checks if necessary.
    pub fn assert_integrity(&self) {
        if build_options::SLOW_RUNTIME_SAFETY {
            assert!(self.cursor.x < self.pages.cols);
            assert!(self.cursor.y < self.pages.rows);

            // Our cursor x/y should always match the pin. If this doesn't
            // match then it indicates that the tracked pin moved and we didn't
            // account for it by either calling cursorReload or manually
            // adjusting.
            let pt = self
                .pages
                .point_from_pin(Tag::Active, *self.cpin())
                .unwrap_or_else(|| unreachable!());
            assert_eq!(self.cursor.x, pt.coord().x);
            assert_eq!(self.cursor.y as u32, pt.coord().y);

            // The cursor style and hyperlink if non-zero must reference
            // real data in the page the pin is in.
            let page = self.cpage();
            if self.cursor.style_id != style::DEFAULT_ID {
                // SAFETY: page memory is the base of its style set.
                assert!(unsafe { page.styles.ref_count(page.memory, self.cursor.style_id) } > 0);
            }
            if self.cursor.hyperlink_id != 0 {
                // SAFETY: as above.
                assert!(unsafe { page.hyperlink_set.ref_count(page.memory, self.cursor.hyperlink_id) } > 0);
            }
        }
    }

    /// Return the memory held by this screen. Like `PageList.memoryUsage`,
    /// this never restores a compressed page but does visit every page.
    pub fn memory_usage(&self) -> MemoryUsage {
        MemoryUsage {
            pages: self.pages.memory_usage(),
            image_bytes: if build_options::KITTY_GRAPHICS {
                self.kitty_images.total_bytes
            } else {
                0
            },
        }
    }

    /// Reset the screen according to the logic of a DEC RIS sequence.
    ///
    /// - Clears the screen and attempts to reclaim memory.
    /// - Moves the cursor to the top-left.
    /// - Clears any cursor state: style, hyperlink, etc.
    /// - Resets the charset
    /// - Clears the selection
    /// - Deletes all Kitty graphics
    /// - Resets Kitty Keyboard settings
    /// - Disables protection mode
    ///
    pub fn reset(&mut self) {
        // Reset our pages
        self.pages.reset();

        // The above reset preserves tracked pins so we can still use
        // our cursor pin, which should be at the top-left already. The
        // reset marks every tracked pin as garbage, but we keep using
        // this one at its new valid position, so clear the flag: copies
        // of the cursor pin (e.g. for Kitty image placements) must not
        // be born garbage.
        let cursor_pin = self.cpin();
        debug_assert!(Some(cursor_pin.node) == self.pages.pages.first);
        debug_assert!(cursor_pin.x == 0);
        debug_assert!(cursor_pin.y == 0);
        cursor_pin.garbage = false;
        let cursor_rac = cursor_pin.row_and_cell();
        self.cursor.deinit();
        self.cursor = Cursor::new(self.cursor.page_pin, cursor_rac.row, cursor_rac.cell);

        if build_options::KITTY_GRAPHICS {
            // Reset kitty graphics storage
            let image_limits = self.kitty_images.image_limits.clone();
            let total_limit = self.kitty_images.total_limit;
            let dirty = self.kitty_images.dirty;
            self.kitty_images.deinit(&mut self.pages);
            self.kitty_images = ImageStorage {
                // Carry the counter over instead of starting from zero.
                // A consumer that saw the old storage's value must see a
                // different one after the reset, and a fresh counter could
                // happen to match it.
                dirty,
                image_limits,
                total_limit,
                ..Default::default()
            };
            self.kitty_images.mark_dirty();
        }

        // Reset our basic state
        self.saved_cursor = None;
        self.charset = CharsetState::default();
        self.kitty_keyboard = kitty::KeyFlagStack::default();
        self.protected_mode = ansi::ProtectedMode::Off;
        self.semantic_prompt = SemanticPrompt::DISABLED;
        self.clear_selection();
    }

    /// Clone the screen.
    ///
    /// This will copy:
    ///
    ///   - Screen dimensions
    ///   - Screen data (cell state, etc.) for the region
    ///
    /// Anything not mentioned above is NOT copied. Some of this is for
    /// very good reason:
    ///
    ///   - Kitty images have a LOT of data. This is not efficient to copy.
    ///     Use a lock and access the image data. The dirty bit is there for
    ///     a reason.
    ///   - Cursor location can be expensive to calculate with respect to the
    ///     specified region. It is faster to grab the cursor from the old
    ///     screen and then move it to the new screen.
    ///   - Current hyperlink cursor state has heap allocations. Since clone
    ///     is only for read-only operations, it is better to not have any
    ///     hyperlink state. Note that already-written hyperlinks are cloned.
    ///
    /// If not mentioned above, then there isn't a specific reason right now
    /// to not copy some data other than we probably didn't need it and it
    /// isn't necessary for screen coherency.
    ///
    /// Other notes:
    ///
    ///   - The viewport will always be set to the active area of the new
    ///     screen. This is the bottom "rows" rows.
    ///   - If the clone region is smaller than a viewport area, blanks will
    ///     be filled in at the bottom.
    ///
    pub fn clone(&self, top: Point, bot: Option<Point>) -> Result<Screen, CloneError> {
        // Create a tracked pin remapper for our selection and cursor. Note
        // that we may want to expose this generally in the future but at the
        // time of doing this we don't need to.
        let mut pin_remap = page_list::TrackedPinsRemap::new();

        let mut pages = self.pages.clone(page_list::Clone {
            top,
            bot,
            tracked_pins: Some(&mut pin_remap),
        })?;

        // Find our cursor. If the cursor isn't in the cloned area, we move it
        // to the top-left arbitrarily because a screen must have SOME cursor.
        let cursor: Cursor = 'cursor: {
            if let Some(&p) = pin_remap.get(&self.cursor.page_pin) {
                let page_rac = tp(p).row_and_cell();
                if let Some(pt) = pages.point_from_pin(Tag::Active, *tp(p)) {
                    let mut c = Cursor::new(p, page_rac.row, page_rac.cell);
                    c.x = pt.coord().x;
                    c.y = pt.coord().y as CellCountInt;
                    break 'cursor c;
                }
            }

            let page_pin = pages.track_pin(Pin::new(pages.pages.first.unwrap()))?;
            let page_rac = tp(page_pin).row_and_cell();
            Cursor::new(page_pin, page_rac.row, page_rac.cell)
        };

        // Preserve our selection if we have one.
        let sel: Option<Selection> = 'sel: {
            let Some(sel) = self.selection else { break 'sel None };
            debug_assert!(sel.tracked());
            let Bounds::Tracked { start, end } = sel.bounds else {
                unreachable!()
            };

            let (tl, br) = match sel.order(self) {
                Order::Forward | Order::MirroredForward => (start, end),
                Order::Reverse | Order::MirroredReverse => (end, start),
            };

            let start_pin = match pin_remap.get(&tl) {
                Some(&p) => p,
                None => {
                    // No start means it is outside the cloned area.

                    // If we have no end pin then either
                    // (1) our whole selection is outside the cloned area or
                    // (2) our cloned area is within the selection
                    if !pin_remap.contains_key(&br) {
                        // We check if the selection bottom right pin is above
                        // the cloned area or if the top left pin is below the
                        // cloned area, in either of these cases it means that
                        // the selection is fully out of bounds, so we have no
                        // selection in the cloned area and break out now.
                        let Some(clone_top) = self.pages.pin(top) else {
                            break 'sel None;
                        };
                        let clone_top_y = self.pages.point_from_pin(Tag::Screen, clone_top).unwrap().coord().y;
                        if self.pages.point_from_pin(Tag::Screen, *tp(br)).unwrap().coord().y < clone_top_y {
                            break 'sel None;
                        }
                        if self.pages.point_from_pin(Tag::Screen, *tp(tl)).unwrap().coord().y > clone_top_y {
                            break 'sel None;
                        }
                    }

                    // We move the top pin back in bounds to the top row.
                    let node = pages.pages.first.unwrap();
                    let mut p = Pin::new(node);
                    p.x = if sel.rectangle {
                        tp(tl).x.min(nd(node).cols() - 1)
                    } else {
                        0
                    };
                    pages.track_pin(p)?
                }
            };

            // If we got to this point it means that the selection is not
            // fully out of bounds, so we move the bottom right pin back
            // in bounds if it isn't already.
            let end_pin = match pin_remap.get(&br) {
                Some(&p) => p,
                None => {
                    let node = pages.pages.last.unwrap();
                    let mut p = Pin::new(node);
                    p.x = if sel.rectangle {
                        tp(br).x.min(nd(node).cols() - 1)
                    } else {
                        nd(node).cols() - 1
                    };
                    p.y = nd(node).rows() - 1;
                    pages.track_pin(p)?
                }
            };

            Some(Selection {
                bounds: Bounds::Tracked {
                    start: start_pin,
                    end: end_pin,
                },
                rectangle: sel.rectangle,
            })
        };

        let result = Screen {
            pages,
            no_scrollback: self.no_scrollback,
            cursor,
            saved_cursor: None,
            selection: sel,
            charset: CharsetState::default(),
            protected_mode: ansi::ProtectedMode::Off,
            kitty_keyboard: kitty::KeyFlagStack::default(),
            kitty_images: ImageStorage::default(),
            semantic_prompt: SemanticPrompt::DISABLED,
            dirty: self.dirty,
        };
        result.assert_integrity();
        Ok(result)
    }

    pub fn increase_capacity(
        &mut self,
        node: NodePtr,
        adjustment: Option<IncreaseCapacity>,
    ) -> Result<NodePtr, IncreaseCapacityError> {
        // If the page being modified isn't our cursor page then
        // this is a quick operation because we have no additional
        // accounting. We have to do this check here BEFORE calling
        // increaseCapacity because increaseCapacity will update all
        // our tracked pins (including our cursor).
        if node != self.cpin().node {
            return self.pages.increase_capacity(node, adjustment);
        }

        // We're modifying the cursor page. When we increase the
        // capacity below it will be short the ref count on our
        // current style and hyperlink, so we need to init those.
        let new_node = self.pages.increase_capacity(node, adjustment)?;
        debug_assert!(self.cpin().node == new_node);
        self.cursor_page_replaced();

        Ok(new_node)
    }

    /// Rebuild the cursor state that lives in page memory after the page the
    /// cursor was on has been destroyed and PageList moved the tracked cursor
    /// pin onto another page. This happens when increaseCapacity replaces the
    /// cursor page or when scrollback pruning removes it.
    ///
    /// The cursor style and hyperlink IDs, as well as the cached row and cell
    /// pointers, all refer to the destroyed page, so this never touches it.
    /// The style and hyperlink are added to the page the pin now points to and
    /// then the cursor is reloaded.
    fn cursor_page_replaced(&mut self) {
        let new_page = self.cpage();

        // Re-add the style, if the page somehow doesn't have enough
        // memory to add it, we emit a warning and gracefully degrade
        // to the default style for the cursor.
        if self.cursor.style_id != style::DEFAULT_ID {
            // SAFETY: page memory is the base of its style set.
            self.cursor.style_id = match unsafe { new_page.styles.add(new_page.memory, self.cursor.style) } {
                Ok(id) => id,
                Err(err) => {
                    // TODO: Should we increase the capacity further in this case?
                    log::warn!(
                        target: LOG_TARGET,
                        "(Screen.cursorPageReplaced) Failed to add cursor style back to page, err={err:?}"
                    );

                    // Reset the cursor style.
                    self.cursor.style = Style::default();
                    style::DEFAULT_ID
                }
            };
        }

        // Re-add the hyperlink, if the page somehow doesn't have enough
        // memory to add it, we emit a warning and gracefully degrade to
        // no hyperlink.
        if let Some(link) = self.cursor.hyperlink.take() {
            // So we don't attempt to free any memory in the replaced page.
            self.cursor.hyperlink_id = 0;

            // Re-add
            if let Err(err) = self.start_hyperlink_once(&link) {
                // TODO: Should we increase the capacity further in this case?
                log::warn!(
                    target: LOG_TARGET,
                    "(Screen.cursorPageReplaced) Failed to add cursor hyperlink back to page, err={err:?}"
                );
            }

            // Remove our old link
            drop(link);
        }

        // Reload the cursor information because the pin changed.
        // So our page row/cell and so on are all off.
        self.cursor_reload();
    }

    /// Clone the cells in columns [x_start, x_end) of a source row into
    /// the row at `dst_y` of the given node's page, increasing the node's
    /// capacity as necessary to fit the managed memory (styles,
    /// hyperlinks, etc.) of the copied cells.
    ///
    /// This is the Screen-level analog of PageList.cloneRowGrowCapacity:
    /// capacity increases are routed through Screen.increaseCapacity so
    /// that the cursor's style/hyperlink references are migrated when the
    /// destination node is the cursor's page.
    ///
    /// Since increasing capacity replaces the node in the page list, the
    /// (possibly replaced) node is returned and the caller must use it in
    /// place of the old node. Tracked pins are updated automatically. The
    /// source must NOT be on the given node since the node's page memory
    /// may be freed on capacity increase.
    ///
    /// Callers use this mid-mutation (after rows have been rotated or
    /// shifted), so a failure can't be propagated without leaving the
    /// page list half-mutated (and corrupt). If the capacity can't be
    /// increased (system OOM or the page is already at max capacity),
    /// this panics: a crash is better than corruption.
    pub fn clone_partial_row_grow_capacity(
        &mut self,
        node: NodePtr,
        dst_y: usize,
        src_page: *const Page,
        src_row: *const Row,
        x_start: usize,
        x_end: usize,
    ) -> NodePtr {
        debug_assert!(!core::ptr::eq(src_page, node_page(node) as *const Page));

        let mut current = node;
        loop {
            let cur_page = node_page(current);
            let dst_row = rw(cur_page.get_row(dst_y));
            // SAFETY: src_page is a live page that is not cur_page and
            // src_row is one of its rows.
            let r = unsafe { cur_page.clone_partial_row_from(src_page, dst_row, &*src_row, x_start, x_end) };
            match r {
                Ok(()) => return current,
                Err(err) => {
                    // Adjust our page capacity to make room for what we
                    // didn't have space for and retry the copy.
                    current = match self.increase_capacity(current, IncreaseCapacity::for_clone_error(err)) {
                        Ok(n) => n,
                        // We can't gracefully recover from either of these
                        // here: our callers have already rotated or shifted
                        // rows, so returning an error would leave the page
                        // list half-mutated (and corrupt), so a crash is
                        // better.
                        Err(IncreaseCapacityError::OutOfMemory) => {
                            panic!("increaseCapacity system allocator OOM")
                        }
                        Err(IncreaseCapacityError::OutOfSpace) => panic!("increaseCapacity OutOfSpace"),
                    };
                }
            }
        }
    }

    #[inline]
    pub fn cursor_cell_right(&self, n: CellCountInt) -> *mut Cell {
        debug_assert!(self.cursor.x + n < self.pages.cols);
        // SAFETY: stays within the cursor row.
        unsafe { self.cursor.page_cell.add(n as usize) }
    }

    #[inline]
    pub fn cursor_cell_left(&self, n: CellCountInt) -> *mut Cell {
        debug_assert!(self.cursor.x >= n);
        // SAFETY: stays within the cursor row.
        unsafe { self.cursor.page_cell.sub(n as usize) }
    }

    pub fn cursor_cell_end_of_prev(&self) -> *mut Cell {
        debug_assert!(self.cursor.y > 0);

        let mut page_pin = self.cpin().up(1).unwrap();
        page_pin.x = nd(page_pin.node).cols() - 1;
        let page_rac = page_pin.row_and_cell();
        page_rac.cell
    }

    /// Move the cursor right. This is a specialized function that is very fast
    /// if the caller can guarantee we have space to move right (no wrapping).
    pub fn cursor_right(&mut self, n: CellCountInt) {
        debug_assert!(self.cursor.x + n < self.pages.cols);

        // SAFETY: stays within the cursor row.
        self.cursor.page_cell = unsafe { self.cursor.page_cell.add(n as usize) };
        self.cpin().x += n;
        self.cursor.x += n;
        self.assert_integrity();
    }

    /// Move the cursor left.
    pub fn cursor_left(&mut self, n: CellCountInt) {
        debug_assert!(self.cursor.x >= n);

        // SAFETY: stays within the cursor row.
        self.cursor.page_cell = unsafe { self.cursor.page_cell.sub(n as usize) };
        self.cpin().x -= n;
        self.cursor.x -= n;
        self.assert_integrity();
    }

    /// Move the cursor up.
    ///
    /// Precondition: The cursor is not at the top of the screen.
    pub fn cursor_up(&mut self, n: CellCountInt) {
        debug_assert!(self.cursor.y >= n);

        self.cursor.y -= n; // Must be set before cursorChangePin
        self.cursor_change_pin(self.cpin().up(n as usize).unwrap());
        self.reload_cursor_rac();
        self.assert_integrity();
    }

    pub fn cursor_row_up(&self, n: CellCountInt) -> *mut Row {
        debug_assert!(self.cursor.y >= n);

        let page_pin = self.cpin().up(n as usize).unwrap();
        let page_rac = page_pin.row_and_cell();
        self.assert_integrity();
        page_rac.row
    }

    /// Move the cursor down.
    ///
    /// Precondition: The cursor is not at the bottom of the screen.
    pub fn cursor_down(&mut self, n: CellCountInt) {
        debug_assert!(self.cursor.y + n < self.pages.rows);

        self.cursor.y += n; // Must be set before cursorChangePin

        // We move the offset into our page list to the next row and then
        // get the pointers to the row/cell and set all the cursor state up.
        self.cursor_change_pin(self.cpin().down(n as usize).unwrap());
        self.reload_cursor_rac();
        self.assert_integrity();
    }

    /// Move the cursor to some absolute horizontal position.
    pub fn cursor_horizontal_absolute(&mut self, x: CellCountInt) {
        debug_assert!(x < self.pages.cols);

        self.cpin().x = x;
        let page_rac = self.cpin().row_and_cell();
        self.cursor.page_cell = page_rac.cell;
        self.cursor.x = x;
        self.assert_integrity();
    }

    /// Move the cursor to some absolute position.
    pub fn cursor_absolute(&mut self, x: CellCountInt, y: CellCountInt) {
        debug_assert!(x < self.pages.cols);
        debug_assert!(y < self.pages.rows);

        let mut page_pin = if y < self.cursor.y {
            self.cpin().up((self.cursor.y - y) as usize).unwrap()
        } else if y > self.cursor.y {
            self.cpin().down((y - self.cursor.y) as usize).unwrap()
        } else {
            *self.cpin()
        };
        page_pin.x = x;
        self.cursor.x = x; // Must be set before cursorChangePin
        self.cursor.y = y;
        self.cursor_change_pin(page_pin);
        self.reload_cursor_rac();
        self.assert_integrity();
    }

    /// Reloads the cursor pointer information into the screen. This is expensive
    /// so it should only be done in cases where the pointers are invalidated
    /// in such a way that its difficult to recover otherwise.
    pub fn cursor_reload(&mut self) {
        // Our tracked pin is ALWAYS accurate, so we derive the active
        // point from the pin. If this returns null it means our pin
        // points outside the active area. In that case, we update the
        // pin to be the top-left.
        let pt: Point = match self.pages.point_from_pin(Tag::Active, *self.cpin()) {
            Some(pt) => pt,
            None => {
                // Our cached row/cell pointers may be invalid (that is often
                // the reason cursorReload is being called), so refresh them
                // from the pin first since cursorChangePin below marks the
                // old cursor row as dirty.
                self.reload_cursor_rac();

                // The cursor style and hyperlink IDs are only valid within the
                // page that the pin points at, so the pin change must go through
                // cursorChangePin, which migrates them when the active top-left
                // is on a different page. Writing the pin directly here would
                // leave the cursor holding IDs that are dead or alias unrelated
                // entries on the new page.
                let pin = self.pages.pin(Point::Active(Coordinate::default())).unwrap();
                self.cursor.x = 0; // Must be set before cursorChangePin
                self.cursor.y = 0;
                self.cursor_change_pin(pin);

                // cursorChangePin can trigger a page capacity adjustment which
                // moves the pin again, so we re-read it to derive our point.
                self.pages.point_from_pin(Tag::Active, *self.cpin()).unwrap()
            }
        };

        self.cursor.x = pt.coord().x;
        self.cursor.y = pt.coord().y as CellCountInt;
        self.reload_cursor_rac();
        self.assert_integrity();
    }

    /// Fill the cursor row with the cursor's background-colored blank
    /// cell if the cursor has a non-default style with a background.
    fn fill_cursor_row_bg(&mut self) {
        if self.cursor.style_id != style::DEFAULT_ID {
            // The newly created line needs to be styled according to
            // the bg color if it is set.
            if let Some(blank_cell) = self.cursor.style.bg_cell() {
                // SAFETY: the cursor row has `cols` cells starting at
                // `page_cell - x`.
                let cells = unsafe {
                    core::slice::from_raw_parts_mut(
                        self.cursor.page_cell.sub(self.cursor.x as usize),
                        self.pages.cols as usize,
                    )
                };
                cells.fill(blank_cell);
            }
        }
    }

    /// Scroll the active area and keep the cursor at the bottom of the screen.
    /// This is a very specialized function but it keeps it fast.
    pub fn cursor_down_scroll(&mut self) -> Result<(), AllocError> {
        let r = self.cursor_down_scroll_();
        self.assert_integrity();
        r
    }

    fn cursor_down_scroll_(&mut self) -> Result<(), AllocError> {
        debug_assert!(self.cursor.y == self.pages.rows - 1);

        if build_options::KITTY_GRAPHICS {
            // Scrolling dirties the images because it updates their placements pins.
            self.kitty_images.mark_dirty();
        }

        // If we have no scrollback, then we shift all our rows instead.
        if self.no_scrollback {
            // If we have a single-row screen, we have no rows to shift
            // so our cursor is in the correct place we just have to clear
            // the cells.
            if self.pages.rows == 1 {
                let page = self.cpage();
                let row = self.crow();
                self.clear_cells(page, row, row_cells(page, row));

                // The row is a fresh blank row now and must not retain
                // metadata (wrap state, semantic prompt) from the
                // discarded content.
                self.crow().reset();

                self.cursor_mark_dirty();
            } else {
                // The call to `eraseRow` will move the tracked cursor pin up by one
                // row, but we don't actually want that, so we keep the old pin and
                // put it back after calling `eraseRow`.
                let old_pin = *self.cpin();

                // eraseRow will shift everything below it up.
                self.pages.erase_row(Point::Active(Coordinate::default()));

                // Note we don't need to mark anything dirty in this branch
                // because eraseRow will mark all the rotated rows as dirty
                // in the entire page.

                // We don't use `cursorChangePin` here because we aren't
                // actually changing the pin, we're keeping it the same.
                *self.cpin() = old_pin;

                // We do, however, need to refresh the cached page row
                // and cell, because `eraseRow` will have moved the row.
                self.reload_cursor_rac();
            }
        } else {
            let old_pin = *self.cpin();

            // Grow our pages by one row. The PageList will handle if we need to
            // allocate, prune scrollback, whatever.
            let _ = self.pages.grow()?;

            // If our pin page changed, then grow() pruned the page our cursor
            // was on. This can only happen with a single row active area. With
            // more rows, the old cursor row is still in the active area after
            // growing and pruning stops at the page containing the active top.
            //
            // The pruned page took the cursor's cached row and cell pointers
            // and its style and hyperlink references with it, so none of those
            // may be used until they're rebuilt on the new page.
            let pruned = old_pin.node != self.cpin().node;

            let new_pin = {
                // Calculate this before cursorChangePin because that function may
                // adjust the underlying page and invalidate references to its pin.

                // If our page was pruned, grow() moved the pin to the top-left
                // of the new page. This effectively moves it by one already, we
                // just need to fix up the x value. Pruning also marks the pin as
                // garbage, but it now points exactly where the cursor belongs.
                let page_pin = if !pruned {
                    self.cpin().down(1).unwrap()
                } else {
                    let mut pin = *self.cpin();
                    pin.x = self.cursor.x;
                    pin.garbage = false;
                    pin
                };

                // These assertions help catch some pagelist math errors. Our
                // x/y should be unchanged after the grow.
                if build_options::SLOW_RUNTIME_SAFETY {
                    let active = self.pages.point_from_pin(Tag::Active, page_pin).unwrap().coord();
                    assert_eq!(active.x, self.cursor.x);
                    assert_eq!(active.y, self.cursor.y as u32);
                }

                page_pin
            };

            if pruned {
                // The old page no longer exists, so we can't go through
                // cursorChangePin (which releases references from it).
                *self.cpin() = new_pin;
                self.cursor_page_replaced();
            } else if self.cpin().node == new_pin.node {
                // Scrolling normally stays within one page. Resolve the page once
                // while refreshing the cursor pointers. The cursor already holds
                // live row and cell pointers into this mapping, and grow does not
                // compress pages, so the node must still be resident here.
                self.cursor_mark_dirty();
                let page = nd(new_pin.node).page_assume_resident();
                let page_rac = page.get_row_and_cell(new_pin.x as usize, new_pin.y as usize);
                *self.cpin() = new_pin;
                self.cursor.page_row = page_rac.row;
                self.cursor.page_cell = page_rac.cell;
            } else {
                // Crossing a page may require migrating the cursor's style and
                // hyperlink references, so retain the general path here.
                self.cursor_change_pin(new_pin);
                self.reload_cursor_rac();
            }

            // Our new row is always dirty
            self.cursor_mark_dirty();

            // Clear the new row so it gets our bg color. We only do this
            // if we have a bg color at all.
            if self.cursor.style.bg_color != style::Color::None {
                let page = self.cpage();
                let row = self.crow();
                self.clear_cells(page, row, row_cells(page, row));
            }
        }

        self.fill_cursor_row_bg();
        Ok(())
    }

    /// This scrolls the active area at and above the cursor.
    /// The lines below the cursor are not scrolled.
    pub fn cursor_scroll_above(&mut self) -> Result<(), AllocError> {
        // We unconditionally mark the cursor row as dirty here because
        // the cursor always changes page rows inside this function, and
        // when that happens it can mean the text in the old row needs to
        // be re-shaped because the cursor splits runs to break ligatures.
        self.cursor_mark_dirty();

        // If the cursor is on the bottom of the screen, its faster to use
        // our specialized function for that case.
        if self.cursor.y == self.pages.rows - 1 {
            return self.cursor_down_scroll();
        }

        let r = self.cursor_scroll_above_();
        self.assert_integrity();
        r
    }

    fn cursor_scroll_above_(&mut self) -> Result<(), AllocError> {
        // Logic below assumes we always have at least one row that isn't moving
        debug_assert!(self.cursor.y < self.pages.rows - 1);

        // Explanation:
        //  We don't actually move everything that's at or above the cursor row,
        //  since this would require us to shift up our ENTIRE scrollback, which
        //  would be ridiculously expensive. Instead, we insert a new row at the
        //  end of the pagelist (`grow()`), and move everything BELOW the cursor
        //  DOWN by one row. This has the same practical result but it's a whole
        //  lot cheaper in 99% of cases.

        let old_pin = *self.cpin();
        if let Some(new_node) = self.pages.grow()? {
            self.cursor_scroll_above_rotate(Some(new_node));
        } else {
            // In this case, it means grow() didn't allocate a new page.

            if Some(self.cpin().node) == self.pages.pages.last {
                // If we're on the last page we can do a very fast path because
                // all the rows we need to move around are within a single page.

                // Note: we don't need to call cursorChangePin here because
                // the pin page is the same so there is no accounting to do
                // for styles or any of that.
                debug_assert!(old_pin.node == self.cpin().node);
                *self.cpin() = self.cpin().down(1).unwrap();

                let pin = *self.cpin();
                let page = node_page(pin.node);

                // Rotate the rows so that the newly created empty row is at the
                // beginning. e.g. [ 0 1 2 3 ] in to [ 3 0 1 2 ].
                // Rotating this suffix changes which logical row its coordinates identify.
                self.pages.invalidate_node_layout(pin.node);
                {
                    let rows = page_rows(page);
                    fastmem::rotate_once_r(&mut rows[pin.y as usize..page.size.rows as usize]);
                }

                // Mark the whole page as dirty.
                //
                // Technically we only need to mark from the cursor row to the
                // end but this is a hot function, so we want to minimize work.
                page.dirty = true;

                // Setup our cursor caches after the rotation so it points to the
                // correct data
                self.reload_cursor_rac();
            } else {
                // We didn't grow pages but our cursor isn't on the last page.
                // In this case we need to do more work because we need to copy
                // elements between pages.
                //
                // An example scenario of this is shown below:
                //
                //      +----------+ = PAGE 0
                //  ... :          :
                //     +-------------+ ACTIVE
                // 4302 |1A00000000| | 0
                // 4303 |2B00000000| | 1
                //      :^         : : = PIN 0
                // 4304 |3C00000000| | 2
                //      +----------+ :
                //      +----------+ : = PAGE 1
                //    0 |4D00000000| | 3
                //    1 |5E00000000| | 4
                //      +----------+ :
                //     +-------------+
                self.cursor_scroll_above_rotate(None);
            }
        }

        self.fill_cursor_row_bg();
        Ok(())
    }

    fn cursor_scroll_above_rotate(&mut self, fresh_node: Option<NodePtr>) {
        // A fresh node given to us is always the newly allocated tail of
        // the page list (see grow), which is where our iteration starts.
        debug_assert!(fresh_node.is_none() || fresh_node == self.pages.pages.last);

        self.cursor_change_pin(self.cpin().down(1).unwrap());

        // Go through each of the pages following our pin, shift all rows
        // down by one, and copy the last row of the previous page. We start
        // at the tail, which is the only node that can be freshly allocated.
        let mut current = self.pages.pages.last.unwrap();
        let mut current_is_fresh = fresh_node.is_some();
        while current != self.cpin().node {
            let prev = nd(current).prev.unwrap();

            // A newly allocated tail has no earlier references to invalidate.
            if !current_is_fresh {
                // Rotating this page moves every cached row coordinate down by one.
                self.pages.invalidate_node_layout(current);
            }

            // Rotate the pages down: [ 0 1 2 3 ] => [ 3 0 1 2 ]
            {
                let cur_page = node_page(current);
                let rows = page_rows(cur_page);
                fastmem::rotate_once_r(&mut rows[..cur_page.size.rows as usize]);
            }

            // Copy the last row of the previous page to the top of current.
            // If the current page doesn't have enough capacity for the
            // managed memory of the copied row (styles, hyperlinks, etc.)
            // then its capacity is increased and the copy retried, which
            // may replace `current` in the page list. Our loop condition
            // guarantees `current` is never the cursor page, and `prev` is
            // unaffected by the replacement.
            {
                let prev_page = node_page(prev);
                let src_row = prev_page.get_row(prev_page.size.rows as usize - 1);
                current = self.clone_partial_row_grow_capacity(
                    current,
                    0,
                    prev_page as *const Page,
                    src_row,
                    0,
                    self.pages.cols as usize,
                );
            }

            // Mark dirty on the page, since we are dirtying all rows with this.
            node_page(current).dirty = true;

            current = prev;
            current_is_fresh = false;
        }

        // Our current is our cursor page, we need to rotate down from
        // our cursor and clear our row.
        debug_assert!(current == self.cpin().node);
        let cur_page = node_page(current);
        // Rotating the cursor-page suffix changes its cached row coordinates.
        self.pages.invalidate_node_layout(current);
        let pin_y = self.cpin().y as usize;
        {
            let rows = page_rows(cur_page);
            fastmem::rotate_once_r(&mut rows[pin_y..cur_page.size.rows as usize]);
        }
        let row = rw(cur_page.get_row(pin_y));
        self.clear_cells(cur_page, row, row_cells(cur_page, row));

        // The recycled storage becomes the new blank cursor row and must
        // not retain metadata (wrap state, semantic prompt) from the row
        // whose content was moved to the next page.
        row.reset();

        // Mark the whole page as dirty.
        //
        // Technically we only need to mark from the cursor row to the
        // end but this is a hot function, so we want to minimize work.
        cur_page.dirty = true;

        // Setup cursor cache data after all the rotations so our
        // row is valid.
        self.reload_cursor_rac();
    }

    /// Scroll a full-width scroll region that ends at the cursor row up by
    /// one row. The cursor must be on the bottom row of the region. `limit`
    /// is the number of rows in the region above the cursor (region height
    /// minus one) and must be at least 1.
    ///
    /// The top row of the region is discarded (NOT moved into scrollback).
    /// All other rows in the region shift up by one and the cursor row
    /// becomes a blank row, filled with the current background color (like
    /// other scroll operations such as deleteLines).
    ///
    /// The cursor stays at the same screen position (the new blank row).
    /// Content outside of the region is unmodified.
    ///
    /// This is a very hot path for scroll region usage (e.g. a program
    /// on the alt screen using DECSTBM and scrolling via LF/IND) so this
    /// is optimized for the common case where the full region is within
    /// a single page.
    pub fn cursor_scroll_region_up(&mut self, limit: usize) -> Result<(), AllocError> {
        let r = self.cursor_scroll_region_up_(limit);
        self.assert_integrity();
        r
    }

    fn cursor_scroll_region_up_(&mut self, limit: usize) -> Result<(), AllocError> {
        debug_assert!(limit >= 1);
        debug_assert!(self.cursor.y as usize >= limit);

        let pin_ptr = self.cursor.page_pin;
        let pin = *tp(pin_ptr);

        // If the region crosses a page boundary we take a slower path. This
        // is rare: it requires the active area to span multiple pages with
        // the split point inside the scroll region.
        if (pin.y as usize) < limit {
            return self.cursor_scroll_region_up_slow(limit);
        }

        // Fast path: the entire region is in a single page. We can clear
        // the top row and rotate it down to the cursor row, updating any
        // tracked pins along the way.
        //
        // The cursor's cached row and cell pointers refer into this mapping.
        // PageList cannot compress the cursor page while those pointers are
        // installed, so the node is known to be resident here.
        let page: &mut Page = nd(pin.node).page_assume_resident();
        let pin_y = pin.y as usize;
        let rows = &mut page_rows(page)[pin_y - limit..pin_y + 1];

        // Clear the erased (top) row.
        {
            let row: &mut Row = rw(&mut rows[0] as *mut Row);

            // Whether our blank cell is a plain zero cell. This is true
            // unless the cursor has a background color set (see blankCell).
            let blank_is_zero =
                self.cursor.style_id == style::DEFAULT_ID || self.cursor.style.bg_color == style::Color::None;

            if !row.managed_memory() && blank_is_zero {
                // Hot path: the row has no managed memory (styles,
                // graphemes, hyperlinks) and our blank is zero so this is
                // a straight zero fill. This is the overwhelmingly common
                // case for scroll region usage.
                let cells = row_cells(page, row);
                cells.fill(Cell::default());
            } else {
                // The generic clear handles managed memory and fills the
                // row with our blank cell, preserving the background color.
                self.clear_cells(page, row, row_cells(page, row));
            }

            // The row becomes the new blank cursor row after the rotation
            // below and must not retain metadata (wrap state, semantic
            // prompt) from the discarded content.
            row.reset();
        }

        // Rotate the region rows so the now-blank top row moves to the
        // bottom (the cursor row) and everything else shifts up by one.
        // Rotating the region changes which logical row its coordinates identify.
        self.pages.invalidate_node_layout(pin.node);
        fastmem::rotate_once(rows);

        // Mark the whole page as dirty.
        //
        // Technically we only need to mark the rotated rows but this is
        // a hot function, so we want to minimize work.
        page.dirty = true;

        // If our viewport is a pin and it's within the rotated region
        // then we need to shift its cached row offset up. See
        // PageList.eraseRowBounded for details; this mirrors that logic.
        if self.pages.viewport == Viewport::Pin {
            let p = *tp(self.pages.viewport_pin);
            if let Some(v) = self.pages.viewport_pin_row_offset.as_mut() {
                let skip = p.node != pin.node || (p.y as usize) < pin_y - limit || p.y > pin.y || p.y == 0;
                if !skip {
                    *v -= 1;
                }
            }
        }

        // Update tracked pins within the region since their rows moved up
        // by one. The cursor's own pin is skipped because the cursor stays
        // at the region bottom (the new blank row).
        for &pp in self.pages.tracked_pins().iter() {
            let p = tp(pp);
            if p.node != pin.node || pp == pin_ptr || (p.y as usize) < pin_y - limit || p.y > pin.y {
                continue;
            }
            if p.y == 0 {
                p.x = 0;
            } else {
                p.y -= 1;
            }
        }

        // The cursor pin is unchanged, but the Row structure at the pin
        // position now contains the blank row, so we need to refresh our
        // cached row/cell pointers. We compute them directly from the row
        // slice we already have rather than going through the pin since
        // this is a hot path.
        let cursor_row: *mut Row = &mut rows[limit];
        self.cursor.page_row = cursor_row;
        self.cursor.page_cell = &mut row_cells(page, rw(cursor_row))[self.cursor.x as usize];
        Ok(())
    }

    /// Slow path for cursorScrollRegionUp: the scroll region spans
    /// multiple pages so we use the generic PageList erase machinery.
    fn cursor_scroll_region_up_slow(&mut self, limit: usize) -> Result<(), AllocError> {
        // The call to eraseRowBounded below will move our tracked cursor
        // pin up by one row since it is inside the erased region, but we
        // don't actually want that: the cursor stays put, on the new blank
        // row at the region bottom. We keep the old pin and put it back
        // after, exactly like cursorDownScroll does for the no-scrollback
        // case.
        //
        // This matters beyond performance: when the cursor is on the first
        // row of a page, eraseRowBounded moves the tracked pin to the
        // previous page. Moving the cursor back down with cursorDown would
        // then cross pages via cursorChangePin, which migrates the cursor
        // style refcount from a page that never held it, corrupting the
        // style accounting.
        let old_pin = *self.cpin();

        self.pages.erase_row_bounded(
            Point::Active(Coordinate {
                x: 0,
                y: (self.cursor.y as usize - limit) as u32,
            }),
            limit,
        );

        // We don't use `cursorChangePin` here because we aren't actually
        // changing the pin, we're keeping it the same. Since the page
        // never changes, the cursor's style ref stays valid and no style
        // accounting needs to be updated.
        *self.cpin() = old_pin;

        // We do, however, need to refresh the cached page row and cell,
        // because the row contents at our pin position changed (it now
        // contains the blank row).
        self.reload_cursor_rac();

        // eraseRowBounded clears the new row with zero cells so if our
        // blank cell isn't zero (bg color is set) we need to fill it.
        let blank = self.blank_cell();
        if !blank.is_zero() {
            // SAFETY: the cursor row has `cols` cells starting at
            // `page_cell - x`.
            let cells = unsafe {
                core::slice::from_raw_parts_mut(
                    self.cursor.page_cell.sub(self.cursor.x as usize),
                    self.pages.cols as usize,
                )
            };
            cells.fill(blank);
        }
        Ok(())
    }

    /// Move the cursor down if we're not at the bottom of the screen. Otherwise
    /// scroll. Currently only used for testing.
    #[inline]
    fn cursor_down_or_scroll(&mut self) -> Result<(), AllocError> {
        if self.cursor.y + 1 < self.pages.rows {
            self.cursor_down(1);
        } else {
            self.cursor_down_scroll()?;
        }
        Ok(())
    }

    /// Copy another cursor. The cursor can be on any screen but the x/y
    /// must be within our screen bounds.
    ///
    /// Port note: unlike Zig (which copies `other` wholesale, including its
    /// cached row/cell pointers, before `cursorAbsolute` recomputes them),
    /// the cached row/cell pointers of our own cursor are kept until the
    /// cursor is moved, so dirty marking never touches the other screen.
    pub fn cursor_copy(&mut self, other: &Cursor, opts: CursorCopy) -> Result<(), IncreaseCapacityError> {
        debug_assert!(other.x < self.pages.cols);
        debug_assert!(other.y < self.pages.rows);

        // End any currently active hyperlink on our cursor.
        self.end_hyperlink();

        let old = self.cursor.clone();
        let mut c = other.clone();

        // Keep our old style ID so it can be properly cleaned up below.
        c.style_id = old.style_id;

        // Hyperlinks will be managed separately below.
        c.hyperlink_id = 0;
        c.hyperlink = None;

        // Keep our old page pin and X/Y because:
        // 1. The old style will need to be cleaned up from the page it's from.
        // 2. The new position navigated to by `cursorAbsolute` needs to be in our
        //    own screen.
        c.page_pin = old.page_pin;
        c.page_row = old.page_row;
        c.page_cell = old.page_cell;
        c.x = old.x;
        c.y = old.y;
        self.cursor = c;

        // Call manual style update in order to clean up our old style, if we have
        // one, and also to load the style from the other cursor, if it had one.
        if let Err(e) = self.manual_style_update() {
            self.cursor = old;
            return Err(e);
        }

        // Move to the correct location to match the other cursor.
        self.cursor_absolute(other.x, other.y);

        // If the other cursor had a hyperlink, add it to ours.
        if opts.hyperlink && other.hyperlink_id != 0 {
            // Get the hyperlink from the other cursor's page.
            let other_page = node_page(tp(other.page_pin).node);
            // SAFETY: the other cursor's hyperlink id is live in its page.
            let other_link = unsafe { *other_page.hyperlink_set.get(other_page.memory, other.hyperlink_id) };

            // Copy the strings out of page memory before we possibly
            // modify any page.
            // SAFETY: the slices are in the other page's memory.
            let uri: Vec<u8> = unsafe { other_link.uri.slice(other_page.memory) }.to_vec();
            let id_: Option<Vec<u8>> = match other_link.id {
                hyperlink::PageEntryId::Explicit(id) => Some(unsafe { id.slice(other_page.memory) }.to_vec()),
                hyperlink::PageEntryId::Implicit(_) => None,
            };

            // And it to our cursor.
            if let Err(err) = self.start_hyperlink(&uri, id_.as_deref()) {
                // This shouldn't happen because startHyperlink should handle
                // resizing. This only happens if we're truly out of RAM. Degrade
                // to forgetting the hyperlink.
                log::error!(target: LOG_TARGET, "failed to update hyperlink on cursor change err={err:?}");
            }
        }

        Ok(())
    }

    /// Always use this to write to cursor.page_pin.*.
    ///
    /// This specifically handles the case when the new pin is on a different
    /// page than the old AND we have a style or hyperlink set. In that case,
    /// we must release our old one and insert the new one, since styles are
    /// stored per-page.
    ///
    /// Note that this can change the cursor pin AGAIN if the process of
    /// setting up our cursor forces a capacity adjustment of the underlying
    /// cursor page, so any references to the page pin should be re-read
    /// from `self.cursor.page_pin` after calling this.
    #[inline]
    fn cursor_change_pin(&mut self, new: Pin) {
        // Moving the cursor affects text run splitting (ligatures) so
        // we must mark the old and new page dirty. We do this as long
        // as the pins are not equal
        if !self.cpin().eql(new) {
            self.cursor_mark_dirty();
            new.mark_dirty();
        }

        // If our pin is on the same page, then we can just update the pin.
        // We don't need to migrate any state.
        if self.cpin().node == new.node {
            *self.cpin() = new;
            return;
        }

        // If we have an old style then we need to release it from the old page.
        let old_style_: Option<Style> = if self.cursor.style_id == style::DEFAULT_ID {
            None
        } else {
            Some(self.cursor.style)
        };
        if old_style_.is_some() {
            // Release the style directly from the old page instead of going through
            // manualStyleUpdate, because the cursor position may have already been
            // updated but the pin has not, which would fail integrity checks.
            let old_page = self.cpage();
            // SAFETY: page memory is the base of its style set.
            unsafe { old_page.styles.release(old_page.memory, self.cursor.style_id) };
            self.cursor.style = Style::default();
            self.cursor.style_id = style::DEFAULT_ID;
        }

        // If we have a hyperlink then we need to release it from the old page.
        if self.cursor.hyperlink.is_some() {
            let old_page = self.cpage();
            // SAFETY: as above.
            unsafe { old_page.hyperlink_set.release(old_page.memory, self.cursor.hyperlink_id) };
            // Zero the ID, it is invalid now and style changes below may
            // run integrity checks. We still have self.cursor.hyperlink to
            // rebuild this later.
            self.cursor.hyperlink_id = 0;
        }

        // Update our pin to the new page
        *self.cpin() = new;

        // On the new page, we need to migrate our style
        if let Some(old_style) = old_style_ {
            self.cursor.style = old_style;
            if let Err(err) = self.manual_style_update() {
                // This failure should not happen because manualStyleUpdate
                // handles page splitting, overflow, and more. This should only
                // happen if we're out of RAM. In this case, we'll just degrade
                // gracefully back to the default style.
                log::error!(target: LOG_TARGET, "failed to update style on cursor change err={err:?}");
                self.cursor.style = Style::default();
                self.cursor.style_id = 0;
            }
        }

        // On the new page, we need to migrate our hyperlink
        if let Some(link) = self.cursor.hyperlink.take() {
            // startHyperlink will try to free old hyperlinks, so set this
            // to null. We free it ourselves later since we're doing some
            // ref-counting shenanigans in this function.

            // Re-add
            let id = match &link.id {
                HyperlinkId::Explicit(v) => Some(&v[..]),
                HyperlinkId::Implicit(_) => None,
            };
            if let Err(err) = self.start_hyperlink(&link.uri, id) {
                // This shouldn't happen because startHyperlink should handle
                // resizing. This only happens if we're truly out of RAM. Degrade
                // to forgetting the hyperlink.
                log::error!(target: LOG_TARGET, "failed to update hyperlink on cursor change err={err:?}");
            }

            // Remove our old link
            drop(link);
        }
    }

    /// Mark the cursor position as dirty.
    /// TODO: test
    #[inline]
    pub fn cursor_mark_dirty(&mut self) {
        self.crow().set_dirty(true);
    }

    /// Reset the cursor row's soft-wrap state and the cursor's pending wrap.
    /// Also handles clearing the spacer head on the cursor row and resetting
    /// the wrap_continuation flag on the next row if necessary.
    ///
    /// NOTE(qwerasd): This method is not scrolling region aware, and cannot be
    /// since it's on Screen not Terminal. This needs to be addressed down the
    /// line. Not an extremely urgent issue since it's an edge case of an edge
    /// case, but not ideal.
    pub fn cursor_reset_wrap(&mut self) {
        // Reset the cursor's pending wrap state
        self.cursor.pending_wrap = false;

        let page_row = self.crow();

        if !page_row.wrap() {
            return;
        }

        // This row does not wrap and the next row is not wrapped to
        page_row.set_wrap(false);

        if let Some(next_row) = self.cpin().down(1) {
            rw(next_row.row_and_cell().row).set_wrap_continuation(false);
        }

        // If the last cell in the row is a spacer head we need to clear it.
        // SAFETY: valid cursor pin.
        let cells = unsafe { &mut *self.cpin().cells(CellSubset::All) };
        let cols = nd(self.cpin().node).cols() as usize;
        let cell = cells[cols - 1];
        if cell.wide() == Wide::SpacerHead {
            let page = self.cpage();
            self.clear_cells(page, page_row, &mut cells[cols - 1..cols]);
        }
    }

    /// Scroll the viewport of the terminal grid.
    #[inline]
    pub fn scroll(&mut self, behavior: Scroll) {
        if build_options::KITTY_GRAPHICS {
            // No matter what, scrolling marks our image state as dirty since
            // it could move placements. If there are no placements or no images
            // this is still a very cheap operation.
            self.kitty_images.mark_dirty();
        }

        match behavior {
            Scroll::Active => self.pages.scroll(page_list::Scroll::Active),
            Scroll::Top => self.pages.scroll(page_list::Scroll::Top),
            Scroll::Pin(p) => self.pages.scroll(page_list::Scroll::Pin(p)),
            Scroll::Row(v) => self.pages.scroll(page_list::Scroll::Row(v)),
            Scroll::DeltaRow(v) => self.pages.scroll(page_list::Scroll::DeltaRow(v)),
            Scroll::DeltaPrompt(v) => self.pages.scroll(page_list::Scroll::DeltaPrompt(v)),
        }
        self.assert_integrity();
    }

    /// Clear the screen by scrolling its contents up into the scrollback,
    /// instead of erasing them. Everything up to and including the last
    /// non-empty row of the active area moves into history, so the user can
    /// still scroll back to see it. This is how "clear" behaves in many
    /// terminals and is what `Terminal.eraseDisplay(.scroll_complete)` uses.
    ///
    /// The cursor is kept valid afterwards:
    ///
    ///   - If the cursor's row is still in the active area (it was below all
    ///     of the written content), it stays on that row, which is now
    ///     closer to the top.
    ///   - Otherwise the cursor's row has scrolled into history, and the
    ///     cursor moves to the top-left of the active area.
    ///
    /// In both cases the cursor keeps its current style and hyperlink, so
    /// the next printed text looks the same as it would have before the
    /// clear. The viewport is not changed. See `PageList.scrollClear` for
    /// the details of how rows are scrolled.
    ///
    /// Returns an error if memory for the new rows can't be allocated.
    #[inline]
    pub fn scroll_clear(&mut self) -> Result<(), AllocError> {
        let r = self.scroll_clear_();
        self.assert_integrity();
        r
    }

    fn scroll_clear_(&mut self) -> Result<(), AllocError> {
        // Scrolling adds rows one at a time, and each new row can cause the
        // oldest scrollback page to be deleted to stay within the scrollback
        // limit. If the deleted page held the cursor, the page list moves the
        // cursor pin to a page that still exists.
        //
        // The cursor's style and hyperlink are stored by ID, and IDs only
        // mean something on the page that created them. So when the cursor
        // ends up on a different page, we have to register the style and
        // hyperlink again on that page. A plain cursorReload isn't enough.
        // cursorDownScroll handles the same situation for a single row.
        let old_node = self.cpin().node;
        self.pages.scroll_clear()?;
        if self.cpin().node != old_node {
            // The page list marks a moved pin as garbage. The pin does point
            // at a real location, and cursorReload needs to read it to place
            // the cursor, so we clear the flag.
            self.cpin().garbage = false;

            // The old page has been freed, so nothing can be released from
            // it. cursorPageReplaced adds the style and hyperlink to the new
            // page and then reloads the cursor.
            self.cursor_page_replaced();
        } else {
            self.cursor_reload();
        }

        if build_options::KITTY_GRAPHICS {
            // No matter what, scrolling marks our image state as dirty since
            // it could move placements. If there are no placements or no images
            // this is still a very cheap operation.
            self.kitty_images.mark_dirty();
        }
        Ok(())
    }

    /// Returns true if the viewport is scrolled to the bottom of the screen.
    #[inline]
    pub fn viewport_is_bottom(&self) -> bool {
        self.pages.viewport == Viewport::Active
    }

    /// Erase the region specified by tl and br, inclusive. This will physically
    /// erase the rows meaning the memory will be reclaimed (if the underlying
    /// page is empty) and other rows will be shifted up.
    #[inline]
    pub fn erase_history(&mut self, bl: Option<Point>) {
        // Placements anchored in the erased rows are removed first, while
        // their pins still identify those rows. Erasing marks those pins as
        // garbage and moves them to the top-left of the retained rows.
        if build_options::KITTY_GRAPHICS {
            self.kitty_images.erase_history(&mut self.pages, bl);
        }

        self.pages.erase_history(bl);
        self.cursor_reload();

        // Erasing rows shifts the placements below them, so placements may
        // have moved.
        if build_options::KITTY_GRAPHICS {
            self.kitty_images.mark_dirty();
        }
        self.assert_integrity();
    }

    #[inline]
    pub fn erase_active(&mut self, y: CellCountInt) {
        self.pages.erase_active(y);
        self.cursor_reload();

        // Erasing rows removes placements attached to them and shifts the
        // ones below, so placements may have moved.
        if build_options::KITTY_GRAPHICS {
            self.kitty_images.mark_dirty();
        }
        self.assert_integrity();
    }

    // Clear the region specified by tl and bl, inclusive. Cleared cells are
    // colored with the current style background color. This will clear all
    // cells in the rows.
    //
    // If protected is true, the protected flag will be respected and only
    // unprotected cells will be cleared. Otherwise, all cells will be cleared.
    pub fn clear_rows(&mut self, tl: Point, bl: Option<Point>, protected: bool) {
        let it = self.pages.page_iterator(Direction::RightDown, tl, bl);
        for chunk in it {
            let page = node_page(chunk.node);
            // SAFETY: chunk rows are live rows of the page.
            let rows = unsafe { &mut *chunk.rows() };
            for row in rows.iter_mut() {
                let row = rw(row as *mut Row);
                let cells_offset = row.cells();
                let cells = row_cells(page, row);

                // Clear all cells
                if protected {
                    self.clear_unprotected_cells(page, row, cells);
                    // We need to preserve other row attributes since we only
                    // cleared unprotected cells.
                    row.set_cells(cells_offset);
                } else {
                    self.clear_cells(page, row, cells);
                    *row = Row::init(cells_offset);
                }

                row.set_dirty(true);
            }
        }
        self.assert_integrity();
    }

    /// Clear the cells with the blank cell.
    ///
    /// This takes care to handle cleaning up graphemes and styles.
    pub fn clear_cells(&mut self, page: &mut Page, row: &mut Row, cells: &mut [Cell]) {
        if cells.is_empty() {
            return;
        }

        // This whole operation does unsafe things, so we just want to assert
        // the end state.
        page.pause_integrity_checks(true);

        if cfg!(debug_assertions) {
            // Our row and cells should be within the page.
            let first_row = page.get_row(0) as usize;
            let last_row = page.get_row(page.size.rows as usize - 1) as usize;
            let row_addr = row as *mut Row as usize;
            assert!(row_addr >= first_row);
            assert!(row_addr <= last_row);

            let all = page.get_cells(row) as *mut Cell;
            let all_len = page.size.cols as usize;
            let first = all as usize;
            let last = all.wrapping_add(all_len - 1) as usize;
            assert!(cells.as_ptr() as usize >= first);
            assert!(cells.as_ptr().wrapping_add(cells.len() - 1) as usize <= last);
        }

        // If we have managed memory (styles, graphemes, or hyperlinks)
        // in this row then we go cell by cell and clear them if present.
        if row.grapheme() {
            for cell in cells.iter_mut() {
                if cell.has_grapheme() {
                    page.clear_grapheme(cell);
                }
            }

            // If we have no left/right scroll region we can be sure
            // that we've cleared all the graphemes, so we clear the
            // flag, otherwise we ask the page to update the flag.
            if cells.len() == page.size.cols as usize {
                row.set_grapheme(false);
            } else {
                page.update_row_grapheme_flag(row);
            }
        }

        if row.hyperlink() {
            for cell in cells.iter_mut() {
                if cell.hyperlink() {
                    page.clear_hyperlink(cell);
                }
            }

            // If we have no left/right scroll region we can be sure
            // that we've cleared all the hyperlinks, so we clear the
            // flag, otherwise we ask the page to update the flag.
            if cells.len() == page.size.cols as usize {
                row.set_hyperlink(false);
            } else {
                page.update_row_hyperlink_flag(row);
            }
        }

        if row.styled() {
            // Styled cells overwhelmingly come in runs sharing the same
            // style (e.g. a colored status bar or a highlighted region),
            // so group them and release each run with a single ref-count
            // update rather than per cell.
            let mut i: usize = 0;
            while i < cells.len() {
                let id = cells[i].style_id();
                if id == style::DEFAULT_ID {
                    i += 1;
                    continue;
                }
                let mut j = i + 1;
                while j < cells.len() && cells[j].style_id() == id {
                    j += 1;
                }
                // SAFETY: page memory is the base of its style set.
                unsafe { page.styles.release_multiple(page.memory, id, (j - i) as style::Id) };
                i = j;
            }

            // If we have no left/right scroll region we can be sure
            // that we've cleared all the styles, so we clear the
            // flag, otherwise we ask the page to update the flag.
            if cells.len() == page.size.cols as usize {
                row.set_styled(false);
            } else {
                page.update_row_styled_flag(row);
            }
        }

        if build_options::KITTY_GRAPHICS && cells.len() == page.size.cols as usize {
            row.set_kitty_virtual_placeholder(false);
        }

        cells.fill(self.blank_cell());

        page.pause_integrity_checks(false);
        page.assert_integrity();
        self.assert_integrity();
    }

    /// Clear cells but only if they are not protected.
    pub fn clear_unprotected_cells(&mut self, page: &mut Page, row: &mut Row, cells: &mut [Cell]) {
        let mut x0: usize = 0;
        let mut x1: usize;

        'outer: while x0 < cells.len() {
            while cells[x0].protected() {
                x0 += 1;
                if x0 >= cells.len() {
                    break 'outer;
                }
            }
            x1 = x0 + 1;
            while x1 < cells.len() && !cells[x1].protected() {
                x1 += 1;
            }
            self.clear_cells(page, row, &mut cells[x0..x1]);
            x0 = x1;
        }

        page.assert_integrity();
        self.assert_integrity();
    }

    /// Clean up boundary conditions where a cell will become discontiguous with
    /// a neighboring cell because either one of them will be moved and/or cleared.
    ///
    /// For performance reasons this is specialized to operate on the cursor row.
    ///
    /// Handles the boundary between the cell at `x` and the cell at `x - 1`.
    ///
    /// So, for example, when moving a region of cells [a, b] (inclusive), call this
    /// function with `x = a` and `x = b + 1`. It is okay if `x` is out of bounds by
    /// 1, this will be interpreted correctly.
    ///
    /// DOES NOT MODIFY ROW WRAP STATE! See `cursorResetWrap` for that.
    ///
    /// The following boundary conditions are handled:
    ///
    /// - `x - 1` is a wide character and `x` is a spacer tail:
    ///   o Both cells will be cleared.
    ///   o If `x - 1` is the start of the row and was wrapped from a previous row
    ///     then the previous row is checked for a spacer head, which is cleared if
    ///     present.
    ///
    /// - `x == 0` and is a wide character:
    ///   o If the row is a wrap continuation then the previous row will be checked
    ///     for a spacer head, which is cleared if present.
    ///
    /// - `x == cols` and `x - 1` is a spacer head:
    ///   o `x - 1` will be cleared.
    ///
    /// NOTE(qwerasd): This method is not scrolling region aware, and cannot be
    /// since it's on Screen not Terminal. This needs to be addressed down the
    /// line. Not an extremely urgent issue since it's an edge case of an edge
    /// case, but not ideal.
    pub fn split_cell_boundary(&mut self, x: CellCountInt) {
        let page = self.cpage();

        page.pause_integrity_checks(true);
        self.split_cell_boundary_(page, x);
        page.pause_integrity_checks(false);
    }

    fn split_cell_boundary_(&mut self, page: &mut Page, x: CellCountInt) {
        let cols = nd(self.cpin().node).cols();
        let cols_us = cols as usize;

        // `x` may be up to an INCLUDING `cols`, since that signifies splitting
        // the boundary to the right of the final cell in the row.
        debug_assert!(x <= cols);

        // [ A B C D E F|]
        //              ^ Boundary between final cell and row end.
        if x == cols {
            if !self.crow().wrap() {
                return;
            }

            // SAFETY: valid cursor pin.
            let cells = unsafe { &mut *self.cpin().cells(CellSubset::All) };

            // Spacer head at end of wrapped row.
            if cells[cols_us - 1].wide() == Wide::SpacerHead {
                self.clear_cells(page, self.crow(), &mut cells[cols_us - 1..cols_us]);
            }

            return;
        }

        // [|A B C D E F ]
        //  ^ Boundary between first cell and row start.
        //
        //  OR
        //
        // [ A|B C D E F ]
        //    ^ Boundary between first cell and second cell.
        //
        // First cell may be a wrapped wide cell with a spacer
        // head on the previous row that needs to be cleared.
        if (x == 0 || x == 1) && self.crow().wrap_continuation() {
            // SAFETY: valid cursor pin.
            let cells = unsafe { &*self.cpin().cells(CellSubset::All) };

            // If the first cell in a row is wide the previous row
            // may have a spacer head which needs to be cleared.
            if cells[0].wide() == Wide::Wide {
                if let Some(p_row) = self.cpin().up(1) {
                    let p_rac = p_row.row_and_cell();
                    // SAFETY: valid pin.
                    let p_cells = unsafe { &mut *p_row.cells(CellSubset::All) };
                    let p_cols = nd(p_row.node).cols() as usize;
                    let p_cell = p_cells[p_cols - 1];
                    if p_cell.wide() == Wide::SpacerHead {
                        self.clear_cells(node_page(p_row.node), rw(p_rac.row), &mut p_cells[p_cols - 1..p_cols]);

                        // `clearCells` does not mark rows dirty, and our
                        // callers only mark the cursor row, so mark the
                        // previous row here.
                        p_row.mark_dirty();
                    }
                }
            }
        }

        // If x is 0 then we're done.
        if x == 0 {
            return;
        }

        // [ ... X|Y ... ]
        //        ^ Boundary between two cells in the middle of the row.
        {
            debug_assert!(x > 0);
            debug_assert!(x < cols);

            // SAFETY: valid cursor pin.
            let cells = unsafe { &mut *self.cpin().cells(CellSubset::All) };

            let xu = x as usize;
            let left = cells[xu - 1];
            match left.wide() {
                // There should not be spacer heads in the middle of the row.
                Wide::SpacerHead => unreachable!(),

                // We don't need to do anything for narrow cells or spacer tails.
                Wide::Narrow | Wide::SpacerTail => {}

                // A wide char would be split, so must be cleared.
                Wide::Wide => {
                    self.clear_cells(page, self.crow(), &mut cells[xu - 1..xu + 1]);
                }
            }
        }
    }

    /// Returns the blank cell to use when doing terminal operations that
    /// require preserving the bg color.
    #[inline]
    pub fn blank_cell(&self) -> Cell {
        if self.cursor.style_id == style::DEFAULT_ID {
            return Cell::default();
        }
        self.cursor.style.bg_cell().unwrap_or_default()
    }

    /// Resize the screen. The rows or cols can be bigger or smaller. If this
    /// returns an error, the screen is unchanged.
    #[inline]
    pub fn resize(&mut self, opts: Resize) -> Result<(), AllocError> {
        let tw = &RESIZE_TW;

        // We need to insert a tracked pin for our saved cursor so we can
        // modify its X/Y for reflow. Do this before changing any state since
        // tracking the pin can fail.
        let saved_cursor_pin: Option<PinPtr> = 'saved_cursor: {
            let Some(sc) = self.saved_cursor else { break 'saved_cursor None };
            let Some(pin) = self.pages.pin(Point::Active(Coordinate {
                x: sc.x,
                y: sc.y as u32,
            })) else {
                break 'saved_cursor None;
            };
            if let Err(e) = tw.check(ResizeFailPoint::SavedCursorPin) {
                self.assert_integrity();
                return Err(e);
            }
            match self.pages.track_pin(pin) {
                Ok(p) => Some(p),
                Err(e) => {
                    self.assert_integrity();
                    return Err(e);
                }
            }
        };

        let r = self.resize_(opts, saved_cursor_pin);
        if let Some(p) = saved_cursor_pin {
            self.pages.untrack_pin(p);
        }
        self.assert_integrity();
        r
    }

    fn resize_(&mut self, opts: Resize, saved_cursor_pin: Option<PinPtr>) -> Result<(), AllocError> {
        let tw = &RESIZE_TW;

        // A cursor style and hyperlink are partly stored in the page containing
        // the cursor. Their IDs only have meaning within that page, and the page
        // keeps reference counts for them. Resizing can replace or destroy the
        // page, so below we temporarily remove both values from the cursor before
        // asking PageList to resize.
        //
        // Save everything first so we can put the cursor back together afterward,
        // or restore it unchanged if the resize fails. We also save the original
        // node and serial so after a successful resize we can tell whether that
        // page survived and still needs its temporary references released.
        let cursor_node = self.cpin().node;
        let cursor_node_serial = nd(cursor_node).serial;
        let cursor_style = self.cursor.style;
        let cursor_style_id = self.cursor.style_id;
        let cursor_hyperlink_id = self.cursor.hyperlink_id;

        // Keep an extra reference to the cursor style and hyperlink while the
        // resize is in progress. Releasing the cursor references below therefore
        // can't delete either entry, and an error can restore the cursor without
        // any allocation.
        {
            let page = node_page(cursor_node);
            if cursor_style_id != style::DEFAULT_ID {
                // SAFETY: page memory is the base of its style set.
                unsafe { page.styles.use_(page.memory, cursor_style_id) };
            }
            if cursor_hyperlink_id != 0 {
                // SAFETY: as above.
                unsafe { page.hyperlink_set.use_(page.memory, cursor_hyperlink_id) };
            }
        }

        // Release the cursor style while resizing just in case the cursor ends
        // up on a different page.
        self.cursor.style = Style::default();
        self.manual_style_update().unwrap_or_else(|_| unreachable!());

        // If we have a hyperlink, release it from the old page
        // and then we need to re-add it to the new page. This needs
        // to happen because resize below typically reallocates a
        // new page so the old hyperlink is invalid.
        let mut cursor_hyperlink: Option<Box<Hyperlink<'static>>> = None;
        if self.cursor.hyperlink_id != 0 {
            // Note we do NOT use endHyperlink because we want to keep
            // our allocated self.cursor.hyperlink valid.
            let page = self.cpage();
            // SAFETY: as above.
            unsafe { page.hyperlink_set.release(page.memory, self.cursor.hyperlink_id) };
            self.cursor.hyperlink_id = 0;
            cursor_hyperlink = self.cursor.hyperlink.take();
        }

        // Perform the resize operation.
        let r = tw.check(ResizeFailPoint::Pages).and_then(|_| {
            self.pages.resize(page_list::Resize {
                rows: Some(opts.rows),
                cols: Some(opts.cols),
                reflow: opts.reflow,
                cursor: Some(page_list::ResizeCursor {
                    x: self.cursor.x,
                    y: self.cursor.y,
                    pin: Some(self.cursor.page_pin),
                }),
                pull_scrollback: opts.pull_scrollback,
            })
        });
        if let Err(e) = r {
            // errdefer: restore the cursor unchanged.
            self.cursor.style = cursor_style;
            self.cursor.style_id = cursor_style_id;
            self.cursor.hyperlink = cursor_hyperlink;
            self.cursor.hyperlink_id = cursor_hyperlink_id;
            return Err(e);
        }

        // No more failures are possible after this.

        // Resizing moves image placements, so mark their geometry as dirty.
        if build_options::KITTY_GRAPHICS {
            self.kitty_images.mark_dirty();
        }

        // If we have no scrollback and we shrunk our rows, we must explicitly
        // erase our history. This is because PageList always keeps at least
        // a page size of history.
        if self.no_scrollback {
            self.pages.erase_history(None);
        }

        // If our cursor was updated, we do a full reload so all our cursor
        // state is correct.
        self.cursor_reload();

        // If resize moved a cursor with pending wrap away from the right edge,
        // advance to the next cell instead of wrapping on the next print.
        if self.cursor.pending_wrap && self.cursor.x != opts.cols - 1 {
            self.cursor.pending_wrap = false;
            self.cursor_right(1);
        }

        // Clear any redrawable prompt after the fallible resize but before
        // restoring the cursor style and hyperlink, so cleared cells retain the
        // same default styling they had with the previous ordering.
        self.clear_prompt_for_redraw(opts.prompt_redraw);

        // Restore the cursor style.
        self.cursor.style = cursor_style;
        if let Err(err) = self.manual_style_update() {
            // This failure should not happen because manualStyleUpdate handles
            // page splitting, overflow, and more. This should only happen if
            // we're out of RAM. In this case, we'll just degrade gracefully back
            // to the default style.
            log::error!(target: LOG_TARGET, "failed to update style on cursor reload err={err:?}");
            self.cursor.style = Style::default();
            self.cursor.style_id = 0;
        }

        // If we reflowed a saved cursor, update it.
        if let Some(p) = saved_cursor_pin {
            // This should never fail because a non-null saved_cursor_pin
            // implies a non-null saved_cursor.
            let pt = self.pages.point_from_pin(Tag::Active, *tp(p));
            let sc = self.saved_cursor.as_mut().unwrap();
            if let Some(pt) = pt {
                sc.x = pt.coord().x;
                sc.y = pt.coord().y as CellCountInt;

                // If we had pending wrap set and we're no longer at the end of
                // the line, we unset the pending wrap and move the cursor to
                // reflect the correct next position.
                if sc.pending_wrap && sc.x != opts.cols - 1 {
                    sc.pending_wrap = false;
                    sc.x += 1;
                }
            } else {
                // I think this can happen if the screen is resized to be
                // less rows or less cols and our saved cursor moves outside
                // the active area. In this case, there isn't anything really
                // reasonable we can do so we just move the cursor to the
                // top-left. It may be reasonable to also move the cursor to
                // match the primary cursor. Any behavior is fine since this is
                // totally unspecified.
                sc.x = 0;
                sc.y = 0;
                sc.pending_wrap = false;
            }
        }

        // Fix up our hyperlink if we had one.
        if let Some(link) = cursor_hyperlink {
            let id = match &link.id {
                HyperlinkId::Explicit(v) => Some(&v[..]),
                HyperlinkId::Implicit(_) => None,
            };
            if let Err(err) = self.start_hyperlink(&link.uri, id) {
                // This shouldn't happen because startHyperlink should handle
                // resizing. This only happens if we're truly out of RAM. Degrade
                // to forgetting the hyperlink.
                log::error!(target: LOG_TARGET, "failed to update hyperlink on resize err={err:?}");
            }

            // Remove our old link
            drop(link);
        }

        // A tracked pin follows its content when PageList moves or replaces a
        // page. If the cursor's pin still points to the node we started with,
        // that node survived the resize and still owns the temporary style and
        // hyperlink references we added above.
        //
        // Comparing pointers is not enough by itself. PageList keeps a pool of
        // nodes, so it can destroy a node and create a replacement at the same
        // memory address. Every new use of a node gets a different serial number,
        // making the pointer-and-serial pair an O(1) identity check.
        let cursor_node_survived = self.cpin().node == cursor_node && nd(self.cpin().node).serial == cursor_node_serial;
        if build_options::SLOW_RUNTIME_SAFETY {
            assert_eq!(cursor_node_survived, self.pages.node_is_valid(cursor_node, cursor_node_serial));
        }

        // A surviving node still contains our temporary references, so remove
        // them now. If the node was replaced, its destruction removed those
        // references along with the rest of the old page so we don't need to
        // fix it up.
        if cursor_node_survived {
            let page = node_page(cursor_node);
            if cursor_style_id != style::DEFAULT_ID {
                // SAFETY: as above.
                unsafe { page.styles.release(page.memory, cursor_style_id) };
            }
            if cursor_hyperlink_id != 0 {
                // SAFETY: as above.
                unsafe { page.hyperlink_set.release(page.memory, cursor_hyperlink_id) };
            }
        }

        Ok(())
    }

    fn clear_prompt_for_redraw(&mut self, redraw: osc_sp::Redraw) {
        // If our cursor is on a prompt or input line, clear it so the shell can
        // redraw it. This works with OSC 133 semantic prompts. We do this after
        // the fallible resize so an error leaves the original prompt untouched.
        //
        // We check cursor.semantic_content rather than page_row.semantic_prompt
        // because some shells (e.g., Nu) mark input areas with OSC 133 B but don't
        // mark continuation lines with k=s. If the input spans multiple lines and
        // continuation lines are unmarked, checking only page_row.semantic_prompt
        // would miss them. By checking semantic_content, we assume that if the
        // cursor is on anything other than command output, we're at a prompt/input
        // line and should clear from there.
        if redraw == osc_sp::Redraw::False || self.cursor.semantic_content == SemanticContent::Output {
            return;
        }

        match redraw {
            osc_sp::Redraw::False => unreachable!(),

            // For `.last`, only clear the current line where the cursor is.
            // For `.true`, clear all prompt lines starting from the beginning.
            osc_sp::Redraw::Last => {
                let page = self.cpage();
                let row = self.crow();
                self.clear_cells(page, row, row_cells(page, row));
            }

            osc_sp::Redraw::True => {
                let mut it = self.cpin().prompt_iterator(Direction::LeftUp, None);
                let Some(start) = it.next() else {
                    // This should never happen because promptIterator should always
                    // find a prompt if we already verified our row is some kind of
                    // prompt.
                    log::warn!(target: LOG_TARGET, "cursor on prompt line but promptIterator found no prompt");
                    return;
                };

                // Clear cells from our start down. We replace it with spaces,
                // and do not physically erase the rows (eraseRows) because the
                // shell is going to expect this space to be available.
                let it = start.row_iterator(Direction::RightDown, None);
                for pin in it {
                    let page = node_page(pin.node);
                    let row = rw(pin.row_and_cell().row);
                    self.clear_cells(page, row, row_cells(page, row));
                }
            }
        }
    }

    /// Set a style attribute for the current cursor.
    ///
    /// If the style can't be set due to any internal errors (memory-related),
    /// then this will revert back to the existing style and return an error.
    pub fn set_attribute(&mut self, attr: sgr::Attribute<'_>) -> Result<(), IncreaseCapacityError> {
        use sgr::Attribute as A;

        // If we fail to set our style for any reason, we should revert
        // back to the old style. If we fail to do that, we revert back to
        // the default style.
        let old_style = self.cursor.style;

        let s = &mut self.cursor.style;
        match attr {
            A::Unset => *s = Style::default(),
            A::Bold => s.flags.bold = true,
            A::ResetBold => {
                // Bold and faint share the same SGR code for this
                s.flags.bold = false;
                s.flags.faint = false;
            }
            A::Italic => s.flags.italic = true,
            A::ResetItalic => s.flags.italic = false,
            A::Faint => s.flags.faint = true,
            A::Underline(v) => s.flags.underline = v,
            A::UnderlineColor(rgb) => {
                s.underline_color = style::Color::Rgb(crate::terminal::color::Rgb {
                    r: rgb.r,
                    g: rgb.g,
                    b: rgb.b,
                })
            }
            A::UnderlineColor256(idx) => s.underline_color = style::Color::Palette(idx),
            A::ResetUnderlineColor => s.underline_color = style::Color::None,
            A::Overline => s.flags.overline = true,
            A::ResetOverline => s.flags.overline = false,
            A::Blink => s.flags.blink = true,
            A::ResetBlink => s.flags.blink = false,
            A::Inverse => s.flags.inverse = true,
            A::ResetInverse => s.flags.inverse = false,
            A::Invisible => s.flags.invisible = true,
            A::ResetInvisible => s.flags.invisible = false,
            A::Strikethrough => s.flags.strikethrough = true,
            A::ResetStrikethrough => s.flags.strikethrough = false,
            A::DirectColorFg(rgb) => {
                s.fg_color = style::Color::Rgb(crate::terminal::color::Rgb {
                    r: rgb.r,
                    g: rgb.g,
                    b: rgb.b,
                })
            }
            A::DirectColorBg(rgb) => {
                s.bg_color = style::Color::Rgb(crate::terminal::color::Rgb {
                    r: rgb.r,
                    g: rgb.g,
                    b: rgb.b,
                })
            }
            A::Fg8(n) => s.fg_color = style::Color::Palette(n.0),
            A::Bg8(n) => s.bg_color = style::Color::Palette(n.0),
            A::ResetFg => s.fg_color = style::Color::None,
            A::ResetBg => s.bg_color = style::Color::None,
            A::BrightFg8(n) => s.fg_color = style::Color::Palette(n.0),
            A::BrightBg8(n) => s.bg_color = style::Color::Palette(n.0),
            A::Fg256(idx) => s.fg_color = style::Color::Palette(idx),
            A::Bg256(idx) => s.bg_color = style::Color::Palette(idx),
            A::Unknown(_) => return Ok(()),
        }

        // If the attribute didn't change our style then we can skip the
        // style update entirely: our current style ID is already correct.
        // This is a common case in the wild where programs re-assert the
        // same style repeatedly (e.g. per span or per line).
        if self.cursor.style.eql(&old_style) {
            return Ok(());
        }

        if let Err(e) = self.manual_style_update() {
            // errdefer
            self.cursor.style = old_style;
            if let Err(err) = self.manual_style_update() {
                log::warn!(
                    target: LOG_TARGET,
                    "setAttribute error restoring old style after failure err={err:?}"
                );
                self.cursor.style = Style::default();
                self.manual_style_update().unwrap_or_else(|_| unreachable!());
            }
            return Err(e);
        }
        Ok(())
    }

    /// Call this whenever you manually change the cursor style.
    ///
    /// This function can NOT fail if the cursor style is changing to the
    /// default style.
    ///
    /// If this returns an error, the style change did not take effect and
    /// the cursor style is reverted back to the default. The only scenario
    /// this returns an error is if there is a physical memory allocation failure
    /// or if there is no possible way to increase style capacity to store
    /// the style.
    ///
    /// This function WILL split pages as necessary to accommodate the new style.
    /// So if OutOfSpace is returned, it means that even after splitting the page
    /// there was still no room for the new style.
    pub fn manual_style_update(&mut self) -> Result<(), IncreaseCapacityError> {
        let r = self.manual_style_update_();
        self.assert_integrity();
        r
    }

    fn manual_style_update_(&mut self) -> Result<(), IncreaseCapacityError> {
        let mut page = self.cpage();

        // Release our previous style if it was not default.
        if self.cursor.style_id != style::DEFAULT_ID {
            // SAFETY: page memory is the base of its style set.
            unsafe { page.styles.release(page.memory, self.cursor.style_id) };
        }

        // If our new style is the default, just reset to that
        if self.cursor.style.is_default() {
            self.cursor.style_id = style::DEFAULT_ID;
            return Ok(());
        }

        // Clear the cursor style ID to prevent weird things from happening
        // if the page capacity has to be adjusted which would end up calling
        // manualStyleUpdate again.
        //
        // This also ensures that if anything fails below, we fall back to
        // clearing our style.
        self.cursor.style_id = style::DEFAULT_ID;

        // After setting the style, we need to update our style map.
        // Note that we COULD lazily do this in print. We should look into
        // if that makes a meaningful difference. Our priority is to keep print
        // fast because setting a ton of styles that do nothing is uncommon
        // and weird.
        // SAFETY: as above.
        let id = match unsafe { page.styles.add(page.memory, self.cursor.style) } {
            Ok(id) => id,
            Err(err) => {
                // Our style map is full or needs to be rehashed, so we need to
                // increase style capacity (or rehash).
                let adjustment = match err {
                    AddError::OutOfMemory => Some(IncreaseCapacity::Styles),
                    AddError::NeedsRehash => None,
                };
                let node = match self.increase_capacity(self.cpin().node, adjustment) {
                    Ok(n) => n,
                    Err(IncreaseCapacityError::OutOfMemory) => return Err(IncreaseCapacityError::OutOfMemory),
                    Err(IncreaseCapacityError::OutOfSpace) => {
                        // Out of space, we need to split the page. Split wherever
                        // is using less capacity and hope that works. If it doesn't
                        // work, we tried.
                        self.split_for_capacity(*self.cpin()).map_err(split_to_increase)?;
                        self.cpin().node
                    }
                };

                page = node_page(node);
                // SAFETY: as above.
                match unsafe { page.styles.add(page.memory, self.cursor.style) } {
                    Ok(id) => id,
                    Err(AddError::OutOfMemory) => {
                        // This shouldn't happen because increaseCapacity is
                        // guaranteed to increase our capacity by at least one and
                        // we only need one space, but again, I don't want to crash
                        // here so let's log loudly and reset.
                        log::error!(target: LOG_TARGET, "style addition failed after capacity increase");
                        return Err(IncreaseCapacityError::OutOfMemory);
                    }
                    Err(AddError::NeedsRehash) => {
                        // This should be impossible because we rehash above
                        // and rehashing should never result in a duplicate. But
                        // we don't want to simply hard crash so log it and
                        // clear our style.
                        log::error!(target: LOG_TARGET, "style rehash resulted in needs rehash");
                        return Ok(());
                    }
                }
            }
        };

        self.cursor.style_id = id;
        Ok(())
    }

    /// Split at the given pin so that the pinned row moves to the page
    /// with less used capacity after the split.
    ///
    /// The primary use case for this is to handle IncreaseCapacityError
    /// OutOfSpace conditions where we need to split the page in order
    /// to make room for more managed memory.
    ///
    /// If the caller cares about where the pin moves to, they should
    /// setup a tracked pin before calling this and then check that.
    /// In many calling cases, the input pin is tracked (e.g. the cursor
    /// pin).
    ///
    /// If this returns OOM then its a system OOM. If this returns OutOfSpace
    /// then it means the page can't be split further.
    fn split_for_capacity(&mut self, pin: Pin) -> Result<(), SplitError> {
        // Get our capacities. We include our target row because its
        // capacity will be preserved.
        let page = node_page(pin.node);
        let bytes_above = Page::layout(page.exact_row_capacity(0, pin.y as usize + 1)).total_size;
        let bytes_below = Page::layout(page.exact_row_capacity(pin.y as usize, nd(pin.node).rows() as usize)).total_size;

        // We need to track the old cursor pin because if our split
        // moves the cursor pin we need to update our accounting.
        let old_cursor = *self.cpin();

        // If our bytes above are less than bytes below, we move the pin
        // to split down one since splitting includes the pinned row in
        // the new node.
        self.pages.split(if bytes_above < bytes_below {
            pin.down(1).unwrap_or(pin)
        } else {
            pin
        })?;

        // Cursor didn't change nodes, we're done.
        if self.cpin().node == old_cursor.node {
            return Ok(());
        }

        // Cursor changed, we need to restore the old pin then use
        // cursorChangePin to move to the new pin. The old node is guaranteed
        // to still exist, just not the row.
        //
        // Note that page_row and all that will be invalid, it points to the
        // new node, but at the time of writing this we don't need any of that
        // to be right in cursorChangePin.
        let new_cursor = *self.cpin();
        *self.cpin() = old_cursor;
        self.cursor_change_pin(new_cursor);
        Ok(())
    }

    /// The index of a cell in the cursor row (Rust helper for
    /// appendGrapheme/setGraphemes).
    fn cursor_row_cell_index(&self, cell: *mut Cell) -> usize {
        // SAFETY: both pointers are in the cursor row.
        let zero = unsafe { self.cursor.page_cell.sub(self.cursor.x as usize) };
        (cell as usize - zero as usize) / core::mem::size_of::<Cell>()
    }

    /// Reload a cell of the cursor row by index after a capacity change.
    fn cursor_row_cell_reload(&self, cell_idx: usize) -> *mut Cell {
        let x = self.cursor.x as usize;
        match cell_idx.cmp(&x) {
            core::cmp::Ordering::Equal => self.cursor.page_cell,
            core::cmp::Ordering::Less => self.cursor_cell_left((x - cell_idx) as CellCountInt),
            core::cmp::Ordering::Greater => self.cursor_cell_right((cell_idx - x) as CellCountInt),
        }
    }

    /// Append a grapheme to the given cell within the current cursor row.
    pub fn append_grapheme(&mut self, cell: *mut Cell, cp: u32) -> Result<(), IncreaseCapacityError> {
        let r = self.append_grapheme_(cell, cp);
        self.cpage().assert_integrity();
        r
    }

    fn append_grapheme_(&mut self, cell: *mut Cell, cp: u32) -> Result<(), IncreaseCapacityError> {
        match self.cpage().append_grapheme(self.crow(), cl(cell), cp) {
            Ok(()) => Ok(()),
            Err(AllocError::OutOfMemory) => {
                // We need to determine the actual cell index of the cell so
                // that after we adjust the capacity we can reload the cell.
                let cell_idx = self.cursor_row_cell_index(cell);

                // Adjust our capacity. This will update our cursor page pin and
                // force us to reload.
                let _ = self.increase_capacity(self.cpin().node, Some(IncreaseCapacity::GraphemeBytes))?;

                // The cell pointer is now invalid, so we need to get it from
                // the reloaded cursor pointers.
                let reloaded_cell = self.cursor_row_cell_reload(cell_idx);

                if let Err(err2) = self.cpage().append_grapheme(self.crow(), cl(reloaded_cell), cp) {
                    // This should never happen because we just increased capacity.
                    // Log loudly but still return an error so we don't just
                    // crash.
                    log::error!(target: LOG_TARGET, "grapheme append failed after capacity increase");
                    return Err(err2.into());
                }
                Ok(())
            }
        }
    }

    /// Set the graphemes of the given cell within the current cursor row.
    /// The cell must have a codepoint and no graphemes yet.
    pub fn set_graphemes(&mut self, cell: *mut Cell, cps: &[u32]) -> Result<(), IncreaseCapacityError> {
        let r = self.set_graphemes_(cell, cps);
        self.cpage().assert_integrity();
        r
    }

    fn set_graphemes_(&mut self, cell: *mut Cell, cps: &[u32]) -> Result<(), IncreaseCapacityError> {
        match self.cpage().set_graphemes(self.crow(), cl(cell), cps) {
            Ok(()) => Ok(()),
            Err(pagepkg::GraphemeError::GraphemeMapOutOfMemory)
            | Err(pagepkg::GraphemeError::GraphemeAllocOutOfMemory) => {
                // See appendGrapheme for the capacity increase and cell reload.
                let cell_idx = self.cursor_row_cell_index(cell);

                let _ = self.increase_capacity(self.cpin().node, Some(IncreaseCapacity::GraphemeBytes))?;

                let reloaded_cell = self.cursor_row_cell_reload(cell_idx);

                if let Err(err2) = self.cpage().set_graphemes(self.crow(), cl(reloaded_cell), cps) {
                    // This should never happen because we just increased capacity.
                    // Log loudly but still return an error so we don't just
                    // crash.
                    log::error!(target: LOG_TARGET, "grapheme set failed after capacity increase err={err2:?}");
                    return Err(IncreaseCapacityError::OutOfMemory);
                }
                Ok(())
            }
        }
    }

    /// Start the hyperlink state. Future cells will be marked as hyperlinks with
    /// this state. Note that various terminal operations may clear the hyperlink
    /// state, such as switching screens (alt screen).
    pub fn start_hyperlink(&mut self, uri: &[u8], id_: Option<&[u8]>) -> Result<(), IncreaseCapacityError> {
        // Create our pending entry.
        let link = Hyperlink {
            uri: Cow::Borrowed(uri),
            id: match id_ {
                Some(id) => HyperlinkId::Explicit(Cow::Borrowed(id)),
                None => {
                    let v = self.cursor.hyperlink_implicit_id;
                    self.cursor.hyperlink_implicit_id = v.wrapping_add(1);
                    HyperlinkId::Implicit(v)
                }
            },
        };

        // Loop until we have enough page memory to add the hyperlink
        loop {
            use pagepkg::InsertHyperlinkError as E;
            let r = match self.start_hyperlink_once(&link) {
                Ok(()) => return Ok(()),

                // An actual self.alloc OOM is a fatal error.
                Err(StartHyperlinkOnceError::OutOfMemory) => Err(IncreaseCapacityError::OutOfMemory),

                // strings table is out of memory, adjust it up
                Err(StartHyperlinkOnceError::Insert(E::StringsOutOfMemory)) => {
                    self.increase_capacity(self.cpin().node, Some(IncreaseCapacity::StringBytes))
                }

                // hyperlink set is out of memory, adjust it up
                Err(StartHyperlinkOnceError::Insert(E::SetOutOfMemory)) => {
                    self.increase_capacity(self.cpin().node, Some(IncreaseCapacity::HyperlinkBytes))
                }

                // hyperlink set is too full, rehash it
                Err(StartHyperlinkOnceError::Insert(E::SetNeedsRehash)) => {
                    self.increase_capacity(self.cpin().node, None)
                }
            };

            if let Err(e) = r {
                // errdefer
                if let HyperlinkId::Implicit(_) = link.id {
                    self.cursor.hyperlink_implicit_id = self.cursor.hyperlink_implicit_id.wrapping_sub(1);
                }
                return Err(e);
            }

            self.assert_integrity();
        }
    }

    /// This is like startHyperlink but if we have to adjust page capacities
    /// this returns error.PageAdjusted. This is useful so that we unwind
    /// all the previous state and try again.
    fn start_hyperlink_once(&mut self, source: &Hyperlink<'_>) -> Result<(), StartHyperlinkOnceError> {
        // Allocate our new Hyperlink entry in non-page memory. This
        // lets us quickly get access to URI, ID.
        HYPERLINK_TW.check(HyperlinkFailPoint::LinkAlloc)?;
        let link = Box::new(source.dupe());

        // End any prior hyperlink only after duplicating the new value. The
        // source slices are allowed to reference our current hyperlink.
        self.end_hyperlink();

        // Insert the hyperlink into page memory
        let page = self.cpage();
        let id: hyperlink::Id = page.insert_hyperlink(&link).map_err(StartHyperlinkOnceError::Insert)?;

        // Save it all
        self.cursor.hyperlink = Some(link);
        self.cursor.hyperlink_id = id;
        Ok(())
    }

    /// End the hyperlink state so that future cells aren't part of the
    /// current hyperlink (if any). This is safe to call multiple times.
    pub fn end_hyperlink(&mut self) {
        // If we have no hyperlink state then do nothing
        if self.cursor.hyperlink_id == 0 {
            debug_assert!(self.cursor.hyperlink.is_none());
            return;
        }

        // Release the old hyperlink state. If there are cells using the
        // hyperlink this will work because the creation creates a reference
        // and all additional cells create a new reference. This release will
        // just release our initial reference.
        //
        // If the ref count reaches zero the set will not delete the item
        // immediately; it is kept around in case it is used again (this is
        // how RefCountedSet works). This causes some memory fragmentation but
        // is fine because if it is ever pruned the context deleted callback
        // will be called.
        let page = self.cpage();
        // SAFETY: page memory is the base of its hyperlink set.
        unsafe { page.hyperlink_set.release(page.memory, self.cursor.hyperlink_id) };
        self.cursor.hyperlink = None;
        self.cursor.hyperlink_id = 0;
    }

    /// Set the current hyperlink state on the current cell.
    pub fn cursor_set_hyperlink(&mut self) -> Result<(), IncreaseCapacityError> {
        debug_assert!(self.cursor.hyperlink_id != 0);

        let mut page = self.cpage();
        match page.set_hyperlink(self.crow(), self.ccell(), self.cursor.hyperlink_id) {
            Ok(()) => {
                // Success, increase the refcount for the hyperlink.
                // SAFETY: page memory is the base of its hyperlink set.
                unsafe { page.hyperlink_set.use_(page.memory, self.cursor.hyperlink_id) };
                Ok(())
            }

            // hyperlink_map is out of space, realloc the page to be larger
            Err(pagepkg::HyperlinkError::HyperlinkMapOutOfMemory) => {
                // Attempt to allocate the space that would be required to
                // insert a new copy of the cursor hyperlink uri in to the
                // string alloc, since right now increaseCapacity always just
                // adds an extra copy even if one already exists in the page.
                // If this alloc fails then we know we also need to grow our
                // string bytes.
                //
                // FIXME: increaseCapacity should not do this.
                while let Some(uri_len) = self.cursor.hyperlink.as_ref().map(|l| l.uri.len()) {
                    // SAFETY: page memory is the base of its string alloc.
                    if unsafe { page.string_alloc.alloc::<u8>(page.memory, uri_len) }.is_ok() {
                        // We don't bother freeing because we're
                        // about to free the entire page anyway.
                        break;
                    }

                    // We didn't have enough room, let's increase string bytes
                    let new_node = self.increase_capacity(self.cpin().node, Some(IncreaseCapacity::StringBytes))?;
                    debug_assert!(new_node == self.cpin().node);
                    page = node_page(new_node);
                }

                // The hyperlink map is fixed-capacity, so reaching this error
                // means live entries fill the usable map capacity and the page
                // must grow.
                let _ = self.increase_capacity(self.cpin().node, Some(IncreaseCapacity::HyperlinkBytes))?;

                // Retry
                //
                // We check that the cursor hyperlink hasn't been destroyed
                // by the capacity adjustment first though- since despite the
                // terrible code above, that can still apparently happen ._.
                if self.cursor.hyperlink_id > 0 {
                    return self.cursor_set_hyperlink();
                }
                Ok(())
            }

            // The only error returned is `HyperlinkMapOutOfMemory`.
            Err(_) => unreachable!(),
        }
    }

    /// Modify the semantic content type of the cursor. This should
    /// be preferred over setting it manually since it handles all the
    /// proper accounting.
    pub fn cursor_set_semantic_content(&mut self, t: SemanticContentUpdate) {
        match t {
            SemanticContentUpdate::Output => {
                self.cursor.semantic_content = SemanticContent::Output;
                self.cursor.semantic_content_clear_eol = false;
            }

            SemanticContentUpdate::Input(clear) => {
                self.cursor.semantic_content = SemanticContent::Input;
                self.cursor.semantic_content_clear_eol = match clear {
                    InputClear::ClearExplicit => false,
                    InputClear::ClearEol => true,
                };
            }

            SemanticContentUpdate::Prompt(kind) => {
                self.semantic_prompt.seen = true;
                self.cursor.semantic_content = SemanticContent::Prompt;
                self.cursor.semantic_content_clear_eol = false;
                self.crow().set_semantic_prompt(match kind {
                    osc_sp::PromptKind::Initial | osc_sp::PromptKind::Right => pagepkg::SemanticPrompt::Prompt,
                    osc_sp::PromptKind::Continuation | osc_sp::PromptKind::Secondary => {
                        pagepkg::SemanticPrompt::PromptContinuation
                    }
                });
            }
        }
    }

    /// Set the selection to the given selection. If this is a tracked selection
    /// then the screen will take ownership of the selection. If this is untracked
    /// then the screen will convert it to tracked internally. This will automatically
    /// untrack the prior selection (if any).
    ///
    /// Set the selection to null to clear any previous selection.
    ///
    /// This is always recommended over setting `selection` directly. Beyond
    /// managing memory for you, it also performs safety checks that the selection
    /// is always tracked.
    pub fn select(&mut self, sel_: Option<Selection>) -> Result<(), AllocError> {
        let Some(sel) = sel_ else {
            self.clear_selection();
            return Ok(());
        };

        // If this selection is untracked then we track it.
        let tracked_sel = if sel.tracked() { sel } else { sel.track(self)? };

        // Untrack prior selection pins that aren't also owned by the replacement.
        // A caller may pass our current tracked selection back to us by value, so
        // releasing both old pins unconditionally would leave the replacement
        // pointing at freed pool entries.
        if let Some(old) = self.selection {
            let Bounds::Tracked {
                start: new_start,
                end: new_end,
            } = tracked_sel.bounds
            else {
                unreachable!()
            };
            match old.bounds {
                Bounds::Untracked { .. } => old.deinit(self),
                Bounds::Tracked { start, end } => {
                    if start != new_start && start != new_end {
                        self.pages.untrack_pin(start);
                    }
                    if end != new_start && end != new_end {
                        self.pages.untrack_pin(end);
                    }
                }
            }
        }
        self.selection = Some(tracked_sel);
        self.dirty.selection = true;
        Ok(())
    }

    /// Same as select(null) but can't fail.
    pub fn clear_selection(&mut self) {
        if let Some(sel) = self.selection {
            sel.deinit(self);
            self.dirty.selection = true;
        }
        self.selection = None;
    }

    /// Returns the raw text associated with a selection. This will unwrap
    /// soft-wrapped edges. The returned slice is owned by the caller.
    ///
    /// For more flexibility, use a ScreenFormatter directly.
    pub fn selection_string(&self, opts: SelectionString) -> String {
        self.selection_string_impl(opts, None)
    }

    /// Returns a StringMap associated with a selection. The map contains the
    /// selection's raw text and a mapping from each byte to its screen location.
    ///
    /// The returned map is owned by the caller.
    pub fn selection_string_map(&self, opts: SelectionString) -> StringMap {
        let mut pins = PinMapMap::default();
        let string = self.selection_string_impl(opts, Some(&mut pins));
        StringMap { string, map: pins }
    }

    fn selection_string_impl(&self, opts: SelectionString, pins: Option<&mut PinMapMap>) -> String {
        // We'll use this as our buffer to build our string.
        let mut aw: Vec<u8> = Vec::new();

        // Create a formatter and use that to emit our text.
        let mut formatter = ScreenFormatter::init(
            self,
            formatter::Options {
                emit: formatter::Format::Plain,
                unwrap: true,
                trim: opts.trim,
                ..formatter::Options::PLAIN
            },
        );
        formatter.content = ScreenFormatterContent::Selection(Some(opts.sel));

        if let Some(map) = pins {
            formatter.pin_map = Some(PinMap { map });
        }

        // Emit. Since this is an allocating writer, a failed write
        // just becomes an OOM.
        formatter.format(&mut aw).expect("writing to a Vec cannot fail");
        bytes_to_string(aw)
    }

    /// Select the line under the given point. This will select across soft-wrapped
    /// lines and will omit the leading and trailing whitespace. If the point is
    /// over whitespace but the line has non-whitespace characters elsewhere, the
    /// line will be selected.
    pub fn select_line(&self, opts: SelectLine<'_>) -> Option<Selection> {
        // Get the current point semantic prompt state since that determines
        // boundary conditions too. This makes it so that line selection can
        // only happen within the same prompt state. For example, if you triple
        // click output, but the shell uses spaces to soft-wrap to the prompt
        // then the selection will stop prior to the prompt. See issue #1329.
        let semantic_prompt_state: Option<SemanticContent> = if !opts.semantic_prompt_boundary {
            None
        } else {
            let rac = opts.pin.row_and_cell();
            Some(cl(rac.cell).semantic_content())
        };

        // The real start of the row is the first row in the soft-wrap.
        let start_pin: Pin = 'start_pin: {
            let mut it = opts.pin.row_iterator(Direction::LeftUp, None);
            let mut it_prev: Pin = it.next().unwrap(); // skip self

            // First, check the current row for semantic boundaries before the clicked position.
            if let Some(v) = semantic_prompt_state {
                let row = rw(it_prev.row_and_cell().row);
                let cells = row_cells(node_page(it_prev.node), row);
                // Scan backwards from clicked position to find where our content starts
                for i in 0..opts.pin.x as usize + 1 {
                    let x_rev = opts.pin.x as usize - i;
                    if cells[x_rev].semantic_content() != v {
                        let mut copy = it_prev;
                        copy.x = (x_rev + 1) as CellCountInt;
                        break 'start_pin copy;
                    }
                }

                // No boundary found before clicked position on current row.
                // If row doesn't wrap from above, start is at column 0.
                // Otherwise, continue checking previous rows.
            }

            for p in it.by_ref() {
                let row = rw(p.row_and_cell().row);

                if !row.wrap() {
                    let mut copy = it_prev;
                    copy.x = 0;
                    break 'start_pin copy;
                }

                if let Some(v) = semantic_prompt_state {
                    // We need to check every cell in this row in reverse
                    // order since we're going up and back.
                    let cells = row_cells(node_page(p.node), row);
                    for x in 0..cells.len() {
                        let x_rev = cells.len() - 1 - x;
                        let cell = cells[x_rev];
                        if cell.semantic_content() != v {
                            break 'start_pin it_prev;
                        }
                        it_prev = p;
                        it_prev.x = x_rev as CellCountInt;
                    }

                    continue;
                }

                it_prev = p;
            }

            let mut copy = it_prev;
            copy.x = 0;
            copy
        };

        // The real end of the row is the final row in the soft-wrap.
        let end_pin: Pin = 'end_pin: {
            let it = opts.pin.row_iterator(Direction::RightDown, None);
            for p in it {
                let row = rw(p.row_and_cell().row);

                if let Some(v) = semantic_prompt_state {
                    // We need to check every cell in this row
                    let cells = row_cells(node_page(p.node), row);

                    // If this is our pin row we can start from our x because
                    // the start_pin logic already found the real start.
                    let start_offset = if p.node == opts.pin.node && p.y == opts.pin.y {
                        opts.pin.x as usize
                    } else {
                        0
                    };

                    // Handle the zero case specially because if the first
                    // col doesn't match then we end at the end of the prior
                    // row. But if this is the first row, we can't go back,
                    // so we scan forward to find where our content ends.
                    if start_offset == 0 && cells[0].semantic_content() != v {
                        let mut prev = p.up(1).unwrap();
                        prev.x = nd(prev.node).cols() - 1;
                        break 'end_pin prev;
                    }

                    // For every other case, we end at the prior cell.
                    for (x, cell) in cells.iter().enumerate().skip(start_offset) {
                        if cell.semantic_content() != v {
                            let mut copy = p;
                            copy.x = (x - 1) as CellCountInt;
                            break 'end_pin copy;
                        }
                    }
                }

                if !row.wrap() {
                    let mut copy = p;
                    copy.x = nd(p.node).cols() - 1;
                    break 'end_pin copy;
                }
            }

            return None;
        };

        // Go forward from the start to find the first non-whitespace character.
        let start: Pin = 'start: {
            let Some(whitespace) = opts.whitespace else { break 'start start_pin };
            let it = start_pin.cell_iterator(Direction::RightDown, Some(end_pin));
            for p in it {
                // CellIterator only limits the row, so bound the column too.
                if end_pin.before(p) {
                    return None;
                }

                let cell = cl(p.row_and_cell().cell);
                if !cell.has_text() {
                    continue;
                }

                // Non-empty means we found it.
                let this_whitespace = whitespace.contains(&cell.content_codepoint());
                if this_whitespace {
                    continue;
                }

                break 'start p;
            }

            return None;
        };

        // Go backward from the end to find the first non-whitespace character.
        let end: Pin = 'end: {
            let Some(whitespace) = opts.whitespace else { break 'end end_pin };
            let it = end_pin.cell_iterator(Direction::LeftUp, Some(start_pin));
            for p in it {
                let cell = cl(p.row_and_cell().cell);
                if !cell.has_text() {
                    continue;
                }

                // Non-empty means we found it.
                let this_whitespace = whitespace.contains(&cell.content_codepoint());
                if this_whitespace {
                    continue;
                }

                break 'end p;
            }

            return None;
        };

        Some(Selection::init(start, end, false))
    }

    /// Return the selection for all contents on the screen. Surrounding
    /// whitespace is omitted. If there is no selection, this returns null.
    pub fn select_all(&self) -> Option<Selection> {
        let whitespace: [u32; 3] = [0, ' ' as u32, '\t' as u32];

        let start: Pin = 'start: {
            let it = self
                .pages
                .cell_iterator(Direction::RightDown, Point::Screen(Coordinate::default()), None);
            for p in it {
                let cell = cl(p.row_and_cell().cell);
                if !cell.has_text() {
                    continue;
                }

                // Non-empty means we found it.
                let this_whitespace = whitespace.contains(&cell.content_codepoint());
                if this_whitespace {
                    continue;
                }

                break 'start p;
            }

            return None;
        };

        let end: Pin = 'end: {
            let it = self
                .pages
                .cell_iterator(Direction::LeftUp, Point::Screen(Coordinate::default()), None);
            for p in it {
                let cell = cl(p.row_and_cell().cell);
                if !cell.has_text() {
                    continue;
                }

                // Non-empty means we found it.
                let this_whitespace = whitespace.contains(&cell.content_codepoint());
                if this_whitespace {
                    continue;
                }

                break 'end p;
            }

            return None;
        };

        Some(Selection::init(start, end, false))
    }

    /// Select the nearest word to start point that is between start_pt and
    /// end_pt (inclusive). Because it selects "nearest" to start point, start
    /// point can be before or after end point.
    ///
    /// The boundary_codepoints parameter should be a slice of u21 codepoints that
    /// mark word boundaries, passed through to selectWord.
    ///
    /// TODO: test this
    pub fn select_word_between(&self, start: Pin, end: Pin, boundary_codepoints: &[u32]) -> Option<Selection> {
        let dir = if start.before(end) {
            Direction::RightDown
        } else {
            Direction::LeftUp
        };
        let it = start.cell_iterator(dir, Some(end));
        for pin in it {
            // Boundary conditions
            match dir {
                Direction::RightDown => {
                    if end.before(pin) {
                        return None;
                    }
                }
                Direction::LeftUp => {
                    if pin.before(end) {
                        return None;
                    }
                }
            }

            // If we found a word, then return it
            if let Some(sel) = self.select_word(pin, boundary_codepoints) {
                return Some(sel);
            }
        }

        None
    }

    /// Select the word under the given point. A word is any consecutive series
    /// of characters that are exclusively whitespace or exclusively non-whitespace.
    /// A selection can span multiple physical lines if they are soft-wrapped.
    ///
    /// This will return null if a selection is impossible. The only scenario
    /// this happens is if the point pt is outside of the written screen space.
    ///
    /// The boundary_codepoints parameter should be a slice of u21 codepoints that
    /// mark word boundaries. This is expected to be pre-parsed from the config.
    pub fn select_word(&self, pin: Pin, boundary_codepoints: &[u32]) -> Option<Selection> {
        // If our cell is empty we can't select a word, because we can't select
        // areas where the screen is not yet written.
        let start_codepoint = select_word_codepoint(pin)?;

        // Determine if we are a boundary or not to determine what our boundary is.
        let expect_boundary = boundary_codepoints.contains(&start_codepoint);

        // Go forwards to find our end boundary
        let end: Pin = 'end: {
            let mut it = pin.cell_iterator(Direction::RightDown, None);
            let mut prev = it.next().unwrap(); // Consume one, our start
            for p in it {
                // Only cross a row boundary if the previous row wraps.
                if prev.x == nd(prev.node).cols() - 1 && !rw(prev.row_and_cell().row).wrap() {
                    break 'end prev;
                }

                // If we reached an empty cell its always a boundary
                let Some(codepoint) = select_word_codepoint(p) else { break 'end prev };

                // If we do not match our expected set, we hit a boundary
                let this_boundary = boundary_codepoints.contains(&codepoint);
                if this_boundary != expect_boundary {
                    break 'end prev;
                }

                prev = p;
            }

            prev
        };

        // Go backwards to find our start boundary
        let start: Pin = 'start: {
            let mut it = pin.cell_iterator(Direction::LeftUp, None);
            let mut prev = it.next().unwrap(); // Consume one, our start
            for p in it {
                let rac = p.row_and_cell();

                // If we are going to the next row and it isn't wrapped, we
                // return the previous.
                if p.x == nd(p.node).cols() - 1 && !rw(rac.row).wrap() {
                    break 'start prev;
                }

                // If we reached an empty cell its always a boundary
                let Some(codepoint) = select_word_codepoint(p) else { break 'start prev };

                // If we do not match our expected set, we hit a boundary
                let this_boundary = boundary_codepoints.contains(&codepoint);
                if this_boundary != expect_boundary {
                    break 'start prev;
                }

                prev = p;
            }

            prev
        };

        Some(Selection::init(start, end, false))
    }

    /// Select the command output under the given point. The limits of the output
    /// are determined by semantic prompt information provided by shell integration.
    /// A selection can span multiple physical lines if they are soft-wrapped.
    ///
    /// This will return null if a selection is impossible:
    ///  - the point pt is outside of the written screen space.
    ///  - the point pt is on a prompt / input line.
    pub fn select_output(&self, pin: Pin) -> Option<Selection> {
        // If our pin right now is not on output, then we return nothing.
        if cl(pin.row_and_cell().cell).semantic_content() != SemanticContent::Output {
            return None;
        }

        // Get the post prior prompt from this pin. This is the prompt whose
        // output we'll be capturing.
        let prompt_pin: Pin = {
            // If we have a prompt above this point (including this point),
            // then thats the prompt we want to capture output from.
            let mut it = pin.prompt_iterator(Direction::LeftUp, None);
            if let Some(p) = it.next() {
                p
            } else {
                // If we don't have a prompt, then we assume that we're
                // capturing all the output up to the next prompt.
                let mut it = pin.prompt_iterator(Direction::RightDown, None);
                let next = it.next()?;

                // We'll capture from the start of the screen to just above
                // the prompt and will trim the trailing whitespace.
                let start_pin = self.pages.get_top_left(Tag::Screen);
                let mut end_pin = next.up(1)?;
                end_pin.x = nd(end_pin.node).cols() - 1;
                let cell_it = end_pin.cell_iterator(Direction::LeftUp, Some(start_pin));
                for p in cell_it {
                    let cell = cl(p.row_and_cell().cell);
                    end_pin = p;
                    if cell.has_text() {
                        break;
                    }
                }

                return Some(Selection::init(start_pin, end_pin, false));
            }
        };

        // Grab our content
        let mut hl = self
            .pages
            .highlight_semantic_content(prompt_pin, SemanticContent::Output)?;

        // Trim our trailing whitespace
        let cell_it = hl.end.cell_iterator(Direction::LeftUp, Some(hl.start));
        for p in cell_it {
            let cell = cl(p.row_and_cell().cell);
            hl.end = p;
            if cell.has_text() {
                break;
            }
        }

        Some(Selection::init(hl.start, hl.end, false))
    }

    /// Returns an iterator to move through the soft-wrapped lines starting
    /// from pin.
    pub fn line_iterator(&self, start: Pin) -> LineIterator<'_> {
        LineIterator {
            screen: self,
            current: Some(start),
        }
    }

    /// Determine the inputs necessary to move the cursor to the given
    /// click location within a prompt input area.
    ///
    /// If the cursor isn't currently at a prompt input location, this
    /// returns no movement.
    ///
    /// This feature depends on well-behaved OSC133 shell integration. Specifically,
    /// this only moves over designated input areas (OSC 133 B). It is assumed
    /// that the shell will only move the cursor to input cells, so prompt cells
    /// and other blank cells are ignored as part of the movement calculation.
    pub fn prompt_click_move(&self, click_pin: Pin) -> PromptClickMove {
        // If we're not at an input cell with our cursor, no movement will
        // ever be possible.
        if self.cursor.semantic_content != SemanticContent::Input
            && self.ccell().semantic_content() != SemanticContent::Input
        {
            return PromptClickMove::ZERO;
        }

        match self.semantic_prompt.click {
            // None doesn't support movement and click_events must use a
            // different mechanism (SGR mouse events) that callers must handle.
            SemanticClick::None | SemanticClick::ClickEvents(_) => PromptClickMove::ZERO,
            SemanticClick::Cl(cl_) => match cl_ {
                // All of these currently use dumb line-based navigation.
                // But eventually we'll support more.
                osc_sp::Click::Line
                | osc_sp::Click::Multiple
                | osc_sp::Click::ConservativeVertical
                | osc_sp::Click::SmartVertical => self.prompt_click_line(click_pin),
            },
        }
    }

    /// Determine the inputs required to move from the cursor to the given
    /// click location. If the cursor isn't currently at a prompt input
    /// location, this will return zero.
    ///
    /// This currently only supports moving a single line.
    fn prompt_click_line(&self, click_pin: Pin) -> PromptClickMove {
        // If our click pin is our cursor pin, no movement is needed.
        // Do this early so we can assume later that they are different.
        let cursor_pin = *self.cpin();
        if cursor_pin.eql(click_pin) {
            return PromptClickMove::ZERO;
        }

        // If our cursor is before our click, we're only emitting right inputs.
        if cursor_pin.before(click_pin) {
            let mut count: usize = 0;

            // We go row-by-row because soft-wrapped rows are still a single
            // line to a shell, so we can't just look at our page row.
            let row_it = cursor_pin.row_iterator(Direction::RightDown, Some(click_pin));
            'row_it: for row_pin in row_it {
                let rac = row_pin.row_and_cell();
                let row = rw(rac.row);
                let cells = row_cells(node_page(row_pin.node), row);

                // Determine if this row is our cursor.
                let is_cursor_row = row_pin.node == cursor_pin.node && row_pin.y == cursor_pin.y;

                // If this is not the cursor row, verify it's still part of the
                // continuation of our starting prompt.
                if !is_cursor_row && row.semantic_prompt() != pagepkg::SemanticPrompt::PromptContinuation {
                    break;
                }

                // Determine where our input starts.
                let start_x: usize = 'start_x: {
                    // If this is our cursor row then we start after the cursor.
                    if is_cursor_row {
                        break 'start_x cursor_pin.x as usize + 1;
                    }

                    // Otherwise, we start at the first input cell, because
                    // we expect the shell to properly translate arrows across
                    // lines to the start of the input. Some shells indent
                    // where input starts on subsequent lines so we must do
                    // this.
                    for (x, cell) in cells.iter().enumerate() {
                        if cell.semantic_content() == SemanticContent::Input {
                            break 'start_x x;
                        }
                    }

                    // We never found an input cell, so we need to move to the
                    // next row.
                    cells.len()
                };

                // Iterate over the input cells and assume arrow keys only
                // jump to input cells.
                for (x, cell) in cells.iter().enumerate().skip(start_x) {
                    // Ignore non-input cells, but allow breaks. We assume
                    // the shell will translate arrow keys to only input
                    // areas.
                    if cell.semantic_content() != SemanticContent::Input {
                        continue;
                    }

                    // Increment our input count
                    count += 1;

                    // If this is our target, we're done.
                    if row_pin.node == click_pin.node && row_pin.y == click_pin.y && x == click_pin.x as usize {
                        break 'row_it;
                    }
                }

                // If this row isn't soft-wrapped, we need to break out
                // because line based moving only handles single lines.
                // We're done!
                if !row.wrap() {
                    // If we never found our pin, that means we clicked further
                    // right/beyond it. If we're already on a non-empty input cell
                    // then we add one so we can move to the newest, empty cell
                    // at the end, matching typical editor behavior.
                    if self.ccell().semantic_content() == SemanticContent::Input {
                        count += 1;
                    }

                    break;
                }
            }

            return PromptClickMove { left: 0, right: count };
        }

        // Otherwise, cursor is after click, so we're emitting left inputs.
        let mut count: usize = 0;

        // We go row-by-row because soft-wrapped rows are still a single
        // line to a shell, so we can't just look at our page row.
        let row_it = cursor_pin.row_iterator(Direction::LeftUp, Some(click_pin));
        'row_it: for row_pin in row_it {
            let rac = row_pin.row_and_cell();
            let row = rw(rac.row);
            let cells = row_cells(node_page(row_pin.node), row);

            // Determine the length of the cells we look at in this row.
            let end_len: usize = if row_pin.node == cursor_pin.node && row_pin.y == cursor_pin.y {
                // If this is our cursor row then we end before the cursor.
                cursor_pin.x as usize
            } else {
                // Otherwise, we end at the last cell in the row.
                cells.len()
            };

            // Iterate backwards over the input cells.
            for rev_x in 0..end_len {
                let x: usize = end_len - 1 - rev_x;
                let cell = cells[x];

                // Ignore non-input cells.
                if cell.semantic_content() != SemanticContent::Input {
                    continue;
                }

                // Increment our input count
                count += 1;

                // If this is our target, we're done.
                if row_pin.node == click_pin.node && row_pin.y == click_pin.y && x == click_pin.x as usize {
                    break 'row_it;
                }
            }

            // If this row is not a wrap continuation, then break out
            if !row.wrap_continuation() {
                break;
            }
        }

        PromptClickMove { left: count, right: 0 }
    }

    /// Dump the screen to a string. The writer given should be buffered;
    /// this function does not attempt to efficiently write and generally writes
    /// one byte at a time.
    pub fn dump_string(&self, writer: &mut dyn io::Write, opts: DumpString) -> io::Result<()> {
        // Create a formatter and use that to emit our text.
        let mut formatter = ScreenFormatter::init(
            self,
            formatter::Options {
                emit: formatter::Format::Plain,
                unwrap: opts.unwrap,
                trim: false,
                ..formatter::Options::PLAIN
            },
        );

        // Set up the selection based on the pins
        let tl = opts.tl;
        let br = opts.br.unwrap_or_else(|| self.pages.get_bottom_right(Tag::Screen).unwrap());

        formatter.content = ScreenFormatterContent::Selection(Some(Selection::init(
            tl, br, false, // not rectangle
        )));

        // Emit
        formatter.format(writer)
    }

    /// You should use dumpString, this is a restricted version mostly for
    /// legacy and convenience reasons for unit tests.
    pub fn dump_string_alloc(&self, tl: Point) -> Result<String, DumpStringError> {
        let mut builder: Vec<u8> = Vec::new();

        self.dump_string(
            &mut builder,
            DumpString {
                tl: self.pages.get_top_left(tl.tag()),
                br: Some(
                    self.pages
                        .get_bottom_right(tl.tag())
                        .ok_or(DumpStringError::UnknownPoint)?,
                ),
                unwrap: false,
            },
        )
        .map_err(DumpStringError::Write)?;

        Ok(bytes_to_string(builder))
    }

    /// You should use dumpString, this is a restricted version mostly for
    /// legacy and convenience reasons for unit tests.
    pub fn dump_string_alloc_unwrapped(&self, tl: Point) -> Result<String, DumpStringError> {
        let mut builder: Vec<u8> = Vec::new();

        self.dump_string(
            &mut builder,
            DumpString {
                tl: self.pages.get_top_left(tl.tag()),
                br: Some(
                    self.pages
                        .get_bottom_right(tl.tag())
                        .ok_or(DumpStringError::UnknownPoint)?,
                ),
                unwrap: true,
            },
        )
        .map_err(DumpStringError::Write)?;

        Ok(bytes_to_string(builder))
    }

    /// This is basically a really jank version of Terminal.printString. We
    /// have to reimplement it here because we want a way to print to the screen
    /// to test it but don't want all the features of Terminal.
    pub fn test_write_string(&mut self, text: &str) -> Result<(), TestWriteError> {
        for ch in text.chars() {
            let c = ch as u32;

            // Explicit newline forces a new row
            if c == '\n' as u32 {
                self.cursor_down_or_scroll()?;
                self.cursor_horizontal_absolute(0);
                self.cursor.pending_wrap = false;
                if self.cursor.semantic_content_clear_eol {
                    self.cursor_set_semantic_content(SemanticContentUpdate::Output);
                } else {
                    match self.cursor.semantic_content {
                        SemanticContent::Output => {}
                        SemanticContent::Prompt | SemanticContent::Input => {
                            self.crow()
                                .set_semantic_prompt(pagepkg::SemanticPrompt::PromptContinuation)
                        }
                    }
                }
                continue;
            }

            let width: usize = if c <= 0xFF {
                1
            } else {
                unicode::TABLE.get(c).width as usize
            };
            if width == 0 {
                let cell = {
                    let mut cell = self.cursor_cell_left(1);
                    match cl(cell).wide() {
                        Wide::Narrow => {}
                        Wide::Wide => {}
                        Wide::SpacerHead => unreachable!(),
                        Wide::SpacerTail => cell = self.cursor_cell_left(2),
                    }

                    cell
                };

                self.cpage().append_grapheme(self.crow(), cl(cell), c)?;
                continue;
            }

            if self.cursor.pending_wrap {
                debug_assert!(self.cursor.x == self.pages.cols - 1);
                self.cursor.pending_wrap = false;
                self.crow().set_wrap(true);
                self.cursor_down_or_scroll()?;
                self.cursor_horizontal_absolute(0);
                self.crow().set_wrap_continuation(true);
                match self.cursor.semantic_content {
                    SemanticContent::Output => {}
                    SemanticContent::Input | SemanticContent::Prompt => self
                        .crow()
                        .set_semantic_prompt(pagepkg::SemanticPrompt::PromptContinuation),
                }
            }

            debug_assert!(width == 1 || width == 2);
            match width {
                1 => {
                    let mut cell = Cell::init(c);
                    cell.set_style_id(self.cursor.style_id);
                    cell.set_protected(self.cursor.protected);
                    cell.set_semantic_content(self.cursor.semantic_content);
                    *self.ccell() = cell;

                    // If we have a ref-counted style, increase.
                    if self.cursor.style_id != style::DEFAULT_ID {
                        let page = self.cpage();
                        // SAFETY: page memory is the base of its style set.
                        unsafe { page.styles.use_(page.memory, self.cursor.style_id) };
                        self.crow().set_styled(true);
                    }

                    // If we have a hyperlink, add it to the cell.
                    if self.cursor.hyperlink_id > 0 {
                        self.cursor_set_hyperlink()?;
                    }
                }

                2 => {
                    // Need a wide spacer head
                    if self.cursor.x == self.pages.cols - 1 {
                        let mut cell = Cell::init(0);
                        cell.set_wide(Wide::SpacerHead);
                        cell.set_protected(self.cursor.protected);
                        cell.set_semantic_content(self.cursor.semantic_content);
                        *self.ccell() = cell;

                        // If we have a hyperlink, add it to the cell.
                        if self.cursor.hyperlink_id > 0 {
                            self.cursor_set_hyperlink()?;
                        }

                        self.crow().set_wrap(true);
                        self.cursor_down_or_scroll()?;
                        self.cursor_horizontal_absolute(0);
                        self.crow().set_wrap_continuation(true);
                    }

                    // Write our wide char
                    let mut cell = Cell::init(c);
                    cell.set_style_id(self.cursor.style_id);
                    cell.set_wide(Wide::Wide);
                    cell.set_protected(self.cursor.protected);
                    cell.set_semantic_content(self.cursor.semantic_content);
                    *self.ccell() = cell;

                    // If we have a hyperlink, add it to the cell.
                    if self.cursor.hyperlink_id > 0 {
                        self.cursor_set_hyperlink()?;
                    }

                    // Write our tail
                    self.cursor_right(1);
                    let mut cell = Cell::init(0);
                    cell.set_wide(Wide::SpacerTail);
                    cell.set_protected(self.cursor.protected);
                    cell.set_semantic_content(self.cursor.semantic_content);
                    *self.ccell() = cell;

                    // If we have a hyperlink, add it to the cell.
                    if self.cursor.hyperlink_id > 0 {
                        self.cursor_set_hyperlink()?;
                    }

                    // If we have a ref-counted style, increase twice.
                    if self.cursor.style_id != style::DEFAULT_ID {
                        let page = self.cpage();
                        // SAFETY: as above.
                        unsafe {
                            page.styles.use_(page.memory, self.cursor.style_id);
                            page.styles.use_(page.memory, self.cursor.style_id);
                        }
                        self.crow().set_styled(true);
                    }
                }

                _ => unreachable!(),
            }

            if self.cursor.x + 1 < self.pages.cols {
                self.cursor_right(1);
            } else {
                self.cursor.pending_wrap = true;
            }
        }
        Ok(())
    }
}

/// The rows of a page as a mutable slice of its full row capacity.
#[inline(always)]
fn page_rows<'a>(page: &Page) -> &'a mut [Row] {
    // SAFETY: the page memory holds `capacity.rows` rows at `rows`.
    unsafe { core::slice::from_raw_parts_mut(page.rows.ptr(page.memory), page.capacity.rows as usize) }
}

/// Convert formatter output to a String. Formatter output is UTF-8
/// (codepoints are encoded and invalid ones replaced) except for
/// hyperlink URIs in styled formats, which are lossily converted.
fn bytes_to_string(v: Vec<u8>) -> String {
    match String::from_utf8(v) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }
}

/// Return the codepoint for word selection, following wide-character spacers.
fn select_word_codepoint(pin: Pin) -> Option<u32> {
    let rac = pin.row_and_cell();
    let cell: Cell = match cl(rac.cell).wide() {
        Wide::Narrow | Wide::Wide => *cl(rac.cell),
        Wide::SpacerTail => *cl(pin.left(1).row_and_cell().cell),
        Wide::SpacerHead => {
            let next_row = pin.down(1)?;
            // SAFETY: valid pin.
            let owner = unsafe { (*next_row.cells(CellSubset::All))[0] };
            if owner.wide() != Wide::Wide {
                return None;
            }
            owner
        }
    };
    if cell.has_text() {
        Some(cell.content_codepoint())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::bool_assert_comparison)]
    #![allow(unused_imports)]
    use super::*;
    use crate::terminal::color::Rgb;
    use crate::terminal::page::{ContentTag, SemanticPrompt as RowSemanticPrompt};
    use crate::terminal::selection_codepoints::DEFAULT_WORD_BOUNDARIES;

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    fn init(cols: CellCountInt, rows: CellCountInt, max_scrollback_bytes: Option<usize>) -> Screen {
        Screen::init(Options {
            cols,
            rows,
            max_scrollback_bytes,
            ..Default::default()
        })
        .unwrap()
    }

    fn coord(x: CellCountInt, y: u32) -> Coordinate {
        Coordinate { x, y }
    }
    fn act(x: CellCountInt, y: u32) -> Point {
        Point::Active(coord(x, y))
    }
    fn scr(x: CellCountInt, y: u32) -> Point {
        Point::Screen(coord(x, y))
    }
    fn vp(x: CellCountInt, y: u32) -> Point {
        Point::Viewport(coord(x, y))
    }
    #[allow(dead_code)]
    fn hist(x: CellCountInt, y: u32) -> Point {
        Point::History(coord(x, y))
    }

    fn pt0(tag: Tag) -> Point {
        let c = Coordinate::default();
        match tag {
            Tag::Active => Point::Active(c),
            Tag::Viewport => Point::Viewport(c),
            Tag::Screen => Point::Screen(c),
            Tag::History => Point::History(c),
        }
    }

    /// `dumpStringAlloc` with an x/y of zero.
    fn dump(s: &Screen, tag: Tag) -> String {
        s.dump_string_alloc(pt0(tag)).unwrap()
    }

    #[allow(dead_code)]
    fn dump_unwrapped(s: &Screen, tag: Tag) -> String {
        s.dump_string_alloc_unwrapped(pt0(tag)).unwrap()
    }

    fn pg<'a>(n: NodePtr) -> &'a mut Page {
        node_page(n)
    }

    fn first(s: &Screen) -> NodePtr {
        s.pages.pages.first.unwrap()
    }

    fn last(s: &Screen) -> NodePtr {
        s.pages.pages.last.unwrap()
    }

    /// The cursor pin (Zig: `s.cursor.page_pin.*`).
    fn cp(s: &Screen) -> Pin {
        *tp(s.cursor.page_pin)
    }

    /// The page the cursor is on (Zig: `s.cursor.page_pin.node.page()`).
    fn cpage<'a>(s: &Screen) -> &'a mut Page {
        pg(cp(s).node)
    }

    fn pin(s: &Screen, pt: Point) -> Pin {
        s.pages.pin(pt).unwrap()
    }

    fn pfp(s: &Screen, tag: Tag, p: Pin) -> Option<Point> {
        s.pages.point_from_pin(tag, p)
    }

    #[allow(dead_code)]
    fn sel_init(s: &Screen, a: Point, b: Point, rect: bool) -> Selection {
        Selection::init(pin(s, a), pin(s, b), rect)
    }

    fn get_cell(s: &Screen, pt: Point) -> Cell {
        unsafe { *s.pages.get_cell(pt).unwrap().cell }
    }

    #[allow(dead_code)]
    fn get_row(s: &Screen, pt: Point) -> Row {
        unsafe { *s.pages.get_cell(pt).unwrap().row }
    }

    fn style_of<'a>(page: &Page, id: style::Id) -> &'a Style {
        unsafe { &*page.styles.get(page.memory, id) }
    }

    fn bold() -> sgr::Attribute<'static> {
        sgr::Attribute::Bold
    }

    fn bg_rgb(r: u8, g: u8, b: u8) -> sgr::Attribute<'static> {
        sgr::Attribute::DirectColorBg(Rgb { r, g, b })
    }

    fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// Fill the scrollback of the first page with blank lines until
    /// there are only `left` rows left on it.
    fn fill_first_page(s: &mut Screen, left: usize) {
        let first_page_size = nd(first(s)).capacity().rows as usize;
        pg(first(s)).pause_integrity_checks(true);
        for _ in 0..first_page_size - left {
            s.test_write_string("\n").unwrap();
        }
        pg(first(s)).pause_integrity_checks(false);
    }

    #[allow(dead_code)]
    fn resize(s: &mut Screen, cols: CellCountInt, rows: CellCountInt, reflow: bool) {
        s.resize(Resize {
            cols,
            rows,
            reflow,
            ..Default::default()
        })
        .unwrap();
    }

    #[test]
    fn screen_forwards_optional_scrollback_limits() {
        let max_lines: usize = 123;
        let s = Screen::init(Options {
            cols: 80,
            rows: 24,
            max_scrollback_bytes: None,
            max_scrollback_lines: Some(max_lines),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(usize::MAX, s.pages.limits.bytes.explicit);
        assert_eq!(max_lines, s.pages.limits.lines.explicit);
        assert!(!s.no_scrollback);
    }

    #[test]
    fn screen_reset_cursor_pin_is_not_garbage() {
        let mut s = init(80, 24, Some(1000));
        s.test_write_string("hello, world").unwrap();

        // The page reset marks every tracked pin garbage but the screen
        // keeps using the cursor pin, so it must come back clean: anything
        // that copies it (e.g. Kitty image placements) would otherwise be
        // born garbage and reaped.
        s.reset();
        assert!(!cp(&s).garbage);
    }

    #[test]
    fn screen_read_and_write() {
        let mut s = init(80, 24, Some(1000));
        assert_eq!(0 as style::Id, s.cursor.style_id);

        s.test_write_string("hello, world").unwrap();
        assert_eq!("hello, world", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_read_and_write_newline() {
        let mut s = init(80, 24, Some(1000));
        assert_eq!(0 as style::Id, s.cursor.style_id);

        s.test_write_string("hello\nworld").unwrap();
        assert_eq!("hello\nworld", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_read_and_write_scrollback() {
        let mut s = init(80, 2, Some(1000));

        s.test_write_string("hello\nworld\ntest").unwrap();
        assert_eq!("hello\nworld\ntest", dump(&s, Tag::Screen));
        assert_eq!("world\ntest", dump(&s, Tag::Active));
    }

    #[test]
    fn screen_read_and_write_no_scrollback_small() {
        let mut s = init(80, 2, Some(0));

        s.test_write_string("hello\nworld\ntest").unwrap();
        assert_eq!("world\ntest", dump(&s, Tag::Screen));
        assert_eq!("world\ntest", dump(&s, Tag::Active));
    }

    #[test]
    fn screen_read_and_write_no_scrollback_large() {
        let mut s = init(80, 2, Some(0));

        for i in 0..1_000 {
            s.test_write_string(&format!("{i}\n")).unwrap();
        }
        s.test_write_string("1000").unwrap();

        assert_eq!("999\n1000", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_cursor_copy_x_y() {
        let mut s = init(10, 10, Some(0));
        s.cursor_absolute(2, 3);
        assert!(s.cursor.x == 2);
        assert!(s.cursor.y == 3);

        let mut s2 = init(10, 10, Some(0));
        s2.cursor_copy(&s.cursor, CursorCopy::default()).unwrap();
        assert!(s2.cursor.x == 2);
        assert!(s2.cursor.y == 3);
        s2.test_write_string("Hello").unwrap();

        assert_eq!("\n\n\n  Hello", dump(&s2, Tag::Screen));
    }

    #[test]
    fn screen_cursor_copy_style_deref() {
        let s = init(10, 10, Some(0));

        let mut s2 = init(10, 10, Some(0));
        let page = cpage(&s2);

        // Bold should create our style
        s2.set_attribute(bold()).unwrap();
        assert_eq!(1, page.styles.count());
        assert!(s2.cursor.style.flags.bold);

        // Copy default style, should release our style
        s2.cursor_copy(&s.cursor, CursorCopy::default()).unwrap();
        assert!(!s2.cursor.style.flags.bold);
        assert_eq!(0, page.styles.count());
    }

    #[test]
    fn screen_cursor_copy_style_deref_new_page() {
        let s = init(10, 10, Some(0));

        let mut s2 = init(10, 10, Some(2048));

        // We need to get the cursor on a new page.
        // Fill the scrollback with blank lines until
        // there are only 5 rows left on the first page.
        fill_first_page(&mut s2, 5);

        s2.test_write_string("1\n2\n3\n4\n5\n6\n7\n8\n9\n10").unwrap();

        // This should be PAGE 1
        let page: *mut Page = cpage(&s2);

        // It should be the last page in the list.
        assert_eq!(pg(last(&s2)) as *mut Page, page);
        // It should have a previous page.
        assert!(nd(cp(&s2).node).prev.is_some());

        // The cursor should be at 2, 9
        assert!(s2.cursor.x == 2);
        assert!(s2.cursor.y == 9);

        // Bold should create our style in page 1.
        s2.set_attribute(bold()).unwrap();
        assert_eq!(1, unsafe { (*page).styles.count() });
        assert!(s2.cursor.style.flags.bold);

        // Copy the cursor for the first screen. This should release
        // the style from page 1 and move the cursor back to page 0.
        s2.cursor_copy(&s.cursor, CursorCopy::default()).unwrap();
        assert!(!s2.cursor.style.flags.bold);
        assert_eq!(0, unsafe { (*page).styles.count() });
        // The page after the page the cursor is now in should be page 1.
        assert_eq!(page, pg(nd(cp(&s2).node).next.unwrap()) as *mut Page);
        // The cursor should be at 0, 0
        assert!(s2.cursor.x == 0);
        assert!(s2.cursor.y == 0);
    }

    #[test]
    fn screen_cursor_copy_style_copy() {
        let mut s = init(10, 10, Some(0));
        s.set_attribute(bold()).unwrap();

        let mut s2 = init(10, 10, Some(0));
        let page = cpage(&s2);
        s2.cursor_copy(&s.cursor, CursorCopy::default()).unwrap();
        assert!(s2.cursor.style.flags.bold);
        assert_eq!(1, page.styles.count());
    }

    #[test]
    fn screen_cursor_copy_hyperlink_deref() {
        let s = init(10, 10, Some(0));

        let mut s2 = init(10, 10, Some(0));
        let page = cpage(&s2);

        // Create a hyperlink for the cursor.
        s2.start_hyperlink(b"https://example.com/", None).unwrap();
        assert_eq!(1, page.hyperlink_set.count());
        assert!(s2.cursor.hyperlink_id != 0);

        // Copy a cursor with no hyperlink, should release our hyperlink.
        s2.cursor_copy(&s.cursor, CursorCopy::default()).unwrap();
        assert_eq!(0, page.hyperlink_set.count());
        assert!(s2.cursor.hyperlink_id == 0);
    }

    #[test]
    fn screen_write_regrows_compacted_page_capacity() {
        let mut s = init(80, 24, Some(0));

        // Compact the active page so every managed capacity dimension is
        // zero, then reload the cursor since its cached row/cell pointers
        // point into the replaced page.
        {
            let node = s.pages.compact(cp(&s).node).unwrap().unwrap();
            assert_eq!(0, nd(node).capacity().styles);
            assert_eq!(0, nd(node).capacity().grapheme_bytes);
            assert_eq!(0, nd(node).capacity().string_bytes);
            assert_eq!(0, nd(node).capacity().hyperlink_bytes);
            s.cursor_reload();
        }

        // Styled write: exercises the manualStyleUpdate single-retry
        // path. Prior to increaseCapacity handling zero dimensions, the
        // retry would fail and the style would be dropped.
        s.set_attribute(bold()).unwrap();
        s.test_write_string("A").unwrap();

        // Grapheme write: exercises the appendGrapheme single-retry path.
        // We can't use testWriteString here because it appends graphemes
        // directly on the page without the capacity retry.
        s.test_write_string("a").unwrap();
        s.append_grapheme(s.cursor_cell_left(1), 0x0301).unwrap();

        // Hyperlink: exercises the startHyperlink retry loop, which used
        // to loop forever when capacity growth from zero didn't grow.
        s.start_hyperlink(b"https://example.com/", None).unwrap();
        s.test_write_string("B").unwrap();
        s.end_hyperlink();

        // Verify the content landed on the page.
        let page = cpage(&s);
        assert!(page.styles.count() >= 1);
        assert!(page.hyperlink_set.count() >= 1);
        assert!(page.grapheme_count() >= 1);
    }

    // The cursor style and hyperlink IDs are only meaningful within the page
    // the cursor pin points at. scrollClear can move the active area onto a
    // later page while the cursor pin stays with its content on an earlier
    // page (now scrollback), so the reset in cursorReload must migrate both
    // references to the destination page. It previously replaced the pin
    // directly and then released the old style ID on the new page.
    #[test]
    fn screen_scroll_clear_across_pages_migrates_cursor_style_and_hyperlink() {
        let mut s = init(10, 10, Some(usize::MAX));

        // Fill the first page so the active area spans two pages.
        fill_first_page(&mut s, 5);
        s.test_write_string("1\n2\n3\n4\n5\n6\n7\n8\n9\n10").unwrap();
        assert!(s.pages.pages.first != s.pages.pages.last);

        // Move the cursor to the top of the active area, which is on the
        // first page, and give it a style and a hyperlink there.
        s.cursor_absolute(0, 0);
        assert!(cp(&s).node == first(&s));
        s.set_attribute(bold()).unwrap();
        s.start_hyperlink(b"https://example.com/", None).unwrap();

        let old_page: *mut Page = cpage(&s);
        let old_style_id = s.cursor.style_id;
        let old_hyperlink_id = s.cursor.hyperlink_id;
        assert!(old_style_id != style::DEFAULT_ID);
        assert!(old_hyperlink_id != 0);

        // All ten active rows are non-empty, so this moves the active area
        // fully onto the second page while the cursor pin stays with its
        // old row, which is now scrollback.
        s.scroll_clear().unwrap();

        // The cursor was moved to the new active top-left on the second
        // page with its style and hyperlink references rebuilt there.
        let new_page = cpage(&s);
        assert!(new_page as *mut Page != old_page);
        assert!(s.cursor.style_id != style::DEFAULT_ID);
        assert!(s.cursor.hyperlink_id != 0);
        unsafe {
            assert!(new_page.styles.ref_count(new_page.memory, s.cursor.style_id) > 0);
            assert!(new_page.hyperlink_set.ref_count(new_page.memory, s.cursor.hyperlink_id) > 0);

            // The cursor's references on the old page were released. Nothing
            // else referenced either entry, so both are dead there now.
            assert_eq!(0, (*old_page).styles.ref_count((*old_page).memory, old_style_id));
            assert_eq!(0, (*old_page).hyperlink_set.ref_count((*old_page).memory, old_hyperlink_id));
        }

        // Printing attaches the migrated style and hyperlink to a cell.
        s.test_write_string("B").unwrap();
    }

    #[test]
    fn screen_cursor_copy_hyperlink_deref_new_page() {
        let s = init(10, 10, Some(0));

        let mut s2 = init(10, 10, Some(2048));

        // We need to get the cursor on a new page.
        fill_first_page(&mut s2, 5);

        s2.test_write_string("1\n2\n3\n4\n5\n6\n7\n8\n9\n10").unwrap();

        // This should be PAGE 1
        let page: *mut Page = cpage(&s2);

        // It should be the last page in the list.
        assert_eq!(pg(last(&s2)) as *mut Page, page);
        // It should have a previous page.
        assert!(nd(cp(&s2).node).prev.is_some());

        // The cursor should be at 2, 9
        assert!(s2.cursor.x == 2);
        assert!(s2.cursor.y == 9);

        // Create a hyperlink for the cursor, should be in page 1.
        s2.start_hyperlink(b"https://example.com/", None).unwrap();
        assert_eq!(1, unsafe { (*page).hyperlink_set.count() });
        assert!(s2.cursor.hyperlink_id != 0);

        // Copy the cursor for the first screen. This should release
        // the hyperlink from page 1 and move the cursor back to page 0.
        s2.cursor_copy(&s.cursor, CursorCopy::default()).unwrap();
        assert_eq!(0, unsafe { (*page).hyperlink_set.count() });
        assert!(s2.cursor.hyperlink_id == 0);
        // The page after the page the cursor is now in should be page 1.
        assert_eq!(page, pg(nd(cp(&s2).node).next.unwrap()) as *mut Page);
        // The cursor should be at 0, 0
        assert!(s2.cursor.x == 0);
        assert!(s2.cursor.y == 0);
    }

    #[test]
    fn screen_cursor_copy_hyperlink_copy() {
        let mut s = init(10, 10, Some(0));

        // Create a hyperlink for the cursor.
        s.start_hyperlink(b"https://example.com/", None).unwrap();
        assert_eq!(1, cpage(&s).hyperlink_set.count());
        assert!(s.cursor.hyperlink_id != 0);

        let mut s2 = init(10, 10, Some(0));
        let page = cpage(&s2);

        assert_eq!(0, page.hyperlink_set.count());
        assert!(s2.cursor.hyperlink_id == 0);

        // Copy the cursor with the hyperlink.
        s2.cursor_copy(&s.cursor, CursorCopy::default()).unwrap();
        assert_eq!(1, page.hyperlink_set.count());
        assert!(s2.cursor.hyperlink_id != 0);
    }

    #[test]
    fn screen_cursor_copy_hyperlink_copy_disabled() {
        let mut s = init(10, 10, Some(0));

        // Create a hyperlink for the cursor.
        s.start_hyperlink(b"https://example.com/", None).unwrap();
        assert_eq!(1, cpage(&s).hyperlink_set.count());
        assert!(s.cursor.hyperlink_id != 0);

        let mut s2 = init(10, 10, Some(0));
        let page = cpage(&s2);

        assert_eq!(0, page.hyperlink_set.count());
        assert!(s2.cursor.hyperlink_id == 0);

        // Copy the cursor with the hyperlink.
        s2.cursor_copy(&s.cursor, CursorCopy { hyperlink: false }).unwrap();
        assert_eq!(0, page.hyperlink_set.count());
        assert!(s2.cursor.hyperlink_id == 0);
    }

    #[test]
    fn screen_style_basics() {
        let mut s = init(80, 24, Some(1000));
        let page = cpage(&s);
        assert_eq!(0, page.styles.count());

        // Set a new style
        s.set_attribute(bold()).unwrap();
        assert!(s.cursor.style_id != 0);
        assert_eq!(1, page.styles.count());
        assert!(s.cursor.style.flags.bold);

        // Set another style, we should still only have one since it was unused
        s.set_attribute(sgr::Attribute::Italic).unwrap();
        assert!(s.cursor.style_id != 0);
        assert_eq!(1, page.styles.count());
        assert!(s.cursor.style.flags.italic);
    }

    #[test]
    fn screen_style_reset_to_default() {
        let mut s = init(80, 24, Some(1000));
        let page = cpage(&s);
        assert_eq!(0, page.styles.count());

        // Set a new style
        s.set_attribute(bold()).unwrap();
        assert!(s.cursor.style_id != 0);
        assert_eq!(1, page.styles.count());

        // Reset to default
        s.set_attribute(sgr::Attribute::ResetBold).unwrap();
        assert!(s.cursor.style_id == 0);
        assert_eq!(0, page.styles.count());
    }

    #[test]
    fn screen_style_reset_with_unset() {
        let mut s = init(80, 24, Some(1000));
        let page = cpage(&s);
        assert_eq!(0, page.styles.count());

        // Set a new style
        s.set_attribute(bold()).unwrap();
        assert!(s.cursor.style_id != 0);
        assert_eq!(1, page.styles.count());

        // Reset to default
        s.set_attribute(sgr::Attribute::Unset).unwrap();
        assert!(s.cursor.style_id == 0);
        assert_eq!(0, page.styles.count());
    }

    #[test]
    fn screen_clear_rows_active_one_line() {
        let mut s = init(80, 24, Some(1000));

        s.test_write_string("hello, world").unwrap();
        s.clear_rows(act(0, 0), None, false);
        assert!(s.pages.is_dirty(act(0, 0)));
        assert_eq!("", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_clear_rows_active_multi_line() {
        let mut s = init(80, 24, Some(1000));

        s.test_write_string("hello\nworld").unwrap();
        s.clear_rows(act(0, 0), None, false);
        assert!(s.pages.is_dirty(act(0, 0)));
        assert!(s.pages.is_dirty(act(0, 1)));
        assert_eq!("", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_clear_rows_active_styled_line() {
        let mut s = init(80, 24, Some(1000));

        s.set_attribute(bold()).unwrap();
        s.test_write_string("hello world").unwrap();
        s.set_attribute(sgr::Attribute::Unset).unwrap();

        // We should have one style
        let page = cpage(&s);
        assert_eq!(1, page.styles.count());

        s.clear_rows(act(0, 0), None, false);

        // We should have none because active cleared it
        assert_eq!(0, page.styles.count());

        assert_eq!("", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_clear_cells_empty_range() {
        let mut s = Screen::init(Options::default_value()).unwrap();

        let page = cpage(&s);
        let row = rw(s.cursor.page_row);
        let cells = row_cells(page, row);
        s.clear_cells(page, row, &mut cells[0..0]);
    }

    #[test]
    fn screen_clear_rows_uses_stored_page_width() {
        let mut s = init(2, 1, Some(0));

        s.test_write_string("AB").unwrap();
        let node = first(&s);
        s.pages.cols = 4;

        s.clear_rows(scr(0, 0), None, false);
        let page = pg(node);
        let cells = row_cells(page, rw(page.get_row(0)));
        assert_eq!(2, cells.len());
        for cell in cells.iter() {
            assert!(cell.is_empty());
        }
    }

    #[test]
    fn screen_clear_rows_protected() {
        let mut s = init(80, 24, Some(1000));

        s.test_write_string("UNPROTECTED").unwrap();
        s.cursor.protected = true;
        s.test_write_string("PROTECTED").unwrap();
        s.cursor.protected = false;
        s.test_write_string("UNPROTECTED").unwrap();
        s.test_write_string("\n").unwrap();
        s.cursor.protected = true;
        s.test_write_string("PROTECTED").unwrap();
        s.cursor.protected = false;
        s.test_write_string("UNPROTECTED").unwrap();
        s.cursor.protected = true;
        s.test_write_string("PROTECTED").unwrap();
        s.cursor.protected = false;

        s.clear_rows(act(0, 0), None, true);

        assert_eq!("           PROTECTED\nPROTECTED           PROTECTED", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_erase_rows_history() {
        let mut s = init(5, 5, Some(1000));

        s.test_write_string("1\n2\n3\n4\n5\n6").unwrap();

        assert_eq!("2\n3\n4\n5\n6", dump(&s, Tag::Active));
        assert_eq!("1\n2\n3\n4\n5\n6", dump(&s, Tag::Screen));

        s.erase_history(None);

        assert_eq!("2\n3\n4\n5\n6", dump(&s, Tag::Active));
        assert_eq!("2\n3\n4\n5\n6", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_erase_rows_history_with_more_lines() {
        let mut s = init(5, 5, Some(1000));

        s.test_write_string("A\nB\nC\n1\n2\n3\n4\n5\n6").unwrap();

        assert_eq!("2\n3\n4\n5\n6", dump(&s, Tag::Active));
        assert_eq!("A\nB\nC\n1\n2\n3\n4\n5\n6", dump(&s, Tag::Screen));

        s.erase_history(None);

        assert_eq!("2\n3\n4\n5\n6", dump(&s, Tag::Active));
        assert_eq!("2\n3\n4\n5\n6", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_erase_rows_active_partial() {
        let mut s = init(5, 5, Some(0));

        s.test_write_string("1\n2\n3").unwrap();

        assert_eq!("1\n2\n3", dump(&s, Tag::Active));

        s.erase_active(1);

        assert_eq!("3", dump(&s, Tag::Active));
        assert_eq!("3", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_cursor_cell_end_of_prev_across_mixed_width_pages() {
        let mut s = init(4, 2, Some(0));

        s.test_write_string("ABCDE").unwrap();
        let f = first(&s);
        let mut p = Pin::new(f);
        p.y = 1;
        s.pages.split(p).unwrap();
        s.cursor_reload();
        let second = nd(f).next.unwrap();
        pg(f).size.cols = 2;

        assert_eq!(second, cp(&s).node);
        let mut ep = Pin::new(f);
        ep.x = 1;
        let expected = ep.row_and_cell().cell;
        assert_eq!(expected, s.cursor_cell_end_of_prev());
    }

    /// Scroll down enough to go to another page (shared setup).
    fn scroll_to_new_page(s: &mut Screen) -> *mut Page {
        let start_page = pg(last(s));
        let rem = start_page.capacity.rows;
        start_page.pause_integrity_checks(true);
        for _ in 0..rem {
            s.cursor_down_or_scroll().unwrap();
        }
        start_page.pause_integrity_checks(false);
        start_page
    }

    #[test]
    fn screen_cursor_down_across_pages_preserves_style() {
        let mut s = init(10, 3, Some(1));

        // Scroll down enough to go to another page
        let start_page = scroll_to_new_page(&mut s);

        // We need our page to change for this test o make sense. If this
        // assertion fails then the bug is in the test: we should be scrolling
        // above enough for a new page to show up.
        assert!(start_page != cpage(&s) as *mut Page);

        // Scroll back to the previous page
        s.cursor_up(1);
        assert!(start_page == cpage(&s) as *mut Page);

        // Go back up, set a style
        s.set_attribute(bold()).unwrap();
        {
            let page = cpage(&s);
            assert!(style_of(page, s.cursor.style_id).flags.bold);
        }

        // Go back down into the next page and we should have that style
        s.cursor_down(1);
        {
            let page = cpage(&s);
            assert!(style_of(page, s.cursor.style_id).flags.bold);
        }
    }

    #[test]
    fn screen_cursor_up_across_pages_preserves_style() {
        let mut s = init(10, 3, Some(1));

        // Scroll down enough to go to another page
        let start_page = scroll_to_new_page(&mut s);

        assert!(start_page != cpage(&s) as *mut Page);

        // Go back up, set a style
        s.set_attribute(bold()).unwrap();
        {
            let page = cpage(&s);
            assert!(style_of(page, s.cursor.style_id).flags.bold);
        }

        // Go back down into the prev page and we should have that style
        s.cursor_up(1);
        {
            let page = cpage(&s);
            assert!(start_page == page as *mut Page);
            assert!(style_of(page, s.cursor.style_id).flags.bold);
        }
    }

    #[test]
    fn screen_cursor_absolute_across_pages_preserves_style() {
        let mut s = init(10, 3, Some(1));

        // Scroll down enough to go to another page
        let start_page = scroll_to_new_page(&mut s);

        assert!(start_page != cpage(&s) as *mut Page);

        // Go back up, set a style
        s.set_attribute(bold()).unwrap();
        {
            let page = cpage(&s);
            assert!(style_of(page, s.cursor.style_id).flags.bold);
        }

        // Go back down into the prev page and we should have that style
        s.cursor_absolute(1, 1);
        {
            let page = cpage(&s);
            assert!(start_page == page as *mut Page);
            assert!(style_of(page, s.cursor.style_id).flags.bold);
        }
    }

    #[test]
    fn screen_cursor_absolute_to_page_with_insufficient_capacity() {
        // This test checks for a very specific edge case
        // which previously resulted in memory corruption.
        //
        // The conditions for this edge case are as such:
        // - The cursor has an associated style or other managed memory.
        // - The cursor moves to a different page.
        // - The new page is at capacity and must have its capacity adjusted.

        let mut s = init(10, 3, Some(1));

        // Scroll down enough to go to another page
        let start_page = scroll_to_new_page(&mut s);

        let new_page: *mut Page = cpage(&s);

        // We need our page to change for this test to make sense. If this
        // assertion fails then the bug is in the test: we should be scrolling
        // above enough for a new page to show up.
        assert!(start_page != new_page);

        // Add styles to the start page until it reaches capacity.
        unsafe {
            // Pause integrity checks because they're slow and
            // we're not testing this, this is just setup.
            let sp = &mut *start_page;
            sp.pause_integrity_checks(true);

            let mut n: u32 = 1;
            loop {
                let st = Style {
                    bg_color: style::Color::Rgb(rgb(n as u8, (n >> 8) as u8, (n >> 16) as u8)),
                    ..Style::default()
                };
                if sp.styles.add(sp.memory, st).is_err() {
                    break;
                }
                n += 1;
            }
            sp.assert_integrity();
            sp.pause_integrity_checks(false);
        }

        // Set a style on the cursor.
        s.set_attribute(bold()).unwrap();
        assert!(style_of(unsafe { &*new_page }, s.cursor.style_id).flags.bold);

        // Go back up into the start page and we should still have that style.
        s.cursor_absolute(1, 1);
        {
            let cur_page = cpage(&s);
            // The page we're on now should NOT equal start_page, since its
            // capacity should have been adjusted, which invalidates our ptr.
            assert!(start_page != cur_page as *mut Page);
            // To make sure we DID change pages we check we're not on new_page.
            assert!(new_page != cur_page as *mut Page);

            assert!(style_of(cur_page, s.cursor.style_id).flags.bold);
        }

        cpage(&s).assert_integrity();
        unsafe { (*new_page).assert_integrity() };
    }

    #[test]
    fn screen_scrolling() {
        let mut s = init(10, 3, Some(0));
        s.set_attribute(bg_rgb(155, 0, 0)).unwrap();
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();

        // Scroll down, should still be bottom
        s.cursor_down_scroll().unwrap();
        assert_eq!("2EFGH\n3IJKL", dump(&s, Tag::Viewport));
        {
            let cell = get_cell(&s, act(0, 2));
            assert!(cell.content_tag() == ContentTag::BgColorRgb);
            assert_eq!(rgb(155, 0, 0), cell.content_color_rgb());
        }

        // Everything is dirty because we have no scrollback
        assert!(s.pages.is_dirty(act(0, 0)));
        assert!(s.pages.is_dirty(act(0, 1)));
        assert!(s.pages.is_dirty(act(0, 2)));

        // Scrolling to the bottom does nothing
        s.scroll(Scroll::Active);

        assert_eq!("2EFGH\n3IJKL", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_scrolling_with_a_single_row_screen_no_scrollback() {
        let mut s = init(10, 1, Some(0));
        s.test_write_string("1ABCD").unwrap();

        // Scroll down, should still be bottom
        s.cursor_down_scroll().unwrap();
        assert_eq!("", dump(&s, Tag::Viewport));

        // Screen should be dirty
        assert!(s.pages.is_dirty(act(0, 0)));
    }

    #[test]
    fn screen_scrolling_with_a_single_row_screen_with_scrollback() {
        let mut s = init(10, 1, Some(1));
        s.test_write_string("1ABCD").unwrap();

        // Scroll down, should still be bottom
        s.cursor_down_scroll().unwrap();
        assert_eq!("", dump(&s, Tag::Viewport));

        // Active should be dirty
        assert!(s.pages.is_dirty(act(0, 0)));

        // Scrollback also dirty because cursor moved from there
        assert!(s.pages.is_dirty(scr(0, 0)));

        s.scroll(Scroll::DeltaRow(-1));
        assert_eq!("1ABCD", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_scrolling_across_pages_preserves_style() {
        let mut s = init(10, 3, Some(1));
        s.set_attribute(bold()).unwrap();
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();
        let start_page: *mut Page = pg(last(&s));

        // Scroll down enough to go to another page
        let sp = unsafe { &mut *start_page };
        let rem = sp.capacity.rows - sp.size.rows + 1;
        sp.pause_integrity_checks(true);
        for _ in 0..rem {
            s.cursor_down_or_scroll().unwrap();
        }
        sp.pause_integrity_checks(false);

        // We need our page to change for this test o make sense. If this
        // assertion fails then the bug is in the test: we should be scrolling
        // above enough for a new page to show up.
        let page = pg(last(&s));
        assert!(start_page != page as *mut Page);

        assert!(style_of(page, s.cursor.style_id).flags.bold);
    }

    #[test]
    fn screen_scroll_down_from_0() {
        let mut s = init(10, 3, Some(0));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();

        // Scrolling up does nothing, but allows it
        s.scroll(Scroll::DeltaRow(-1));
        assert!(s.pages.viewport == Viewport::Active);

        assert_eq!("1ABCD\n2EFGH\n3IJKL", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_scrollback_various_cases() {
        let mut s = init(10, 3, Some(1));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();
        s.cursor_down_scroll().unwrap();

        assert_eq!("2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Scrolling to the bottom
        s.scroll(Scroll::Active);
        assert_eq!("2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Scrolling back should make it visible again
        s.scroll(Scroll::DeltaRow(-1));
        assert!(s.pages.viewport != Viewport::Active);
        assert_eq!("1ABCD\n2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Scrolling back again should do nothing
        s.scroll(Scroll::DeltaRow(-1));
        assert_eq!("1ABCD\n2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Scrolling to the bottom
        s.scroll(Scroll::Active);
        assert_eq!("2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Scrolling forward with no grow should do nothing
        s.scroll(Scroll::DeltaRow(1));
        assert_eq!("2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Scrolling to the top should work
        s.scroll(Scroll::Top);
        assert_eq!("1ABCD\n2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Should be able to easily clear active area only
        s.clear_rows(act(0, 0), None, false);
        assert_eq!("1ABCD", dump(&s, Tag::Viewport));

        // Scrolling to the bottom
        s.scroll(Scroll::Active);
        assert_eq!("", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_scrollback_with_multi_row_delta() {
        let mut s = init(10, 3, Some(3));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH\n6IJKL").unwrap();

        // Scroll to top
        s.scroll(Scroll::Top);
        assert_eq!("1ABCD\n2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Scroll down multiple
        s.scroll(Scroll::DeltaRow(5));
        assert!(s.pages.viewport == Viewport::Active);
        assert_eq!("4ABCD\n5EFGH\n6IJKL", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_scrollback_empty() {
        let mut s = init(10, 3, Some(50));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();
        s.scroll(Scroll::DeltaRow(1));
        assert_eq!("1ABCD\n2EFGH\n3IJKL", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_scrollback_doesnt_move_viewport_if_not_at_bottom() {
        let mut s = init(10, 3, Some(3));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH").unwrap();

        // First test: we scroll up by 1, so we're not at the bottom anymore.
        s.scroll(Scroll::DeltaRow(-1));
        assert_eq!("2EFGH\n3IJKL\n4ABCD", dump(&s, Tag::Viewport));

        // Next, we scroll back down by 1, this grows the scrollback but we
        // shouldn't move.
        s.cursor_down_scroll().unwrap();
        assert_eq!("2EFGH\n3IJKL\n4ABCD", dump(&s, Tag::Viewport));

        // Scroll again, this clears scrollback so we should move viewports
        // but still see the same thing since our original view fits.
        s.cursor_down_scroll().unwrap();
        assert_eq!("2EFGH\n3IJKL\n4ABCD", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_scrolling_moves_selection() {
        let mut s = init(5, 3, Some(1));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();

        // Select a single line
        let cols = s.pages.cols;
        let sel = Selection::init(pin(&s, act(0, 1)), pin(&s, act(cols - 1, 1)), false);
        s.select(Some(sel)).unwrap();

        // Scroll down, should still be bottom
        s.cursor_down_scroll().unwrap();

        // Our selection should've moved up
        {
            let sel = s.selection.unwrap();
            assert_eq!(act(0, 0), pfp(&s, Tag::Active, sel.start()).unwrap());
            assert_eq!(act(cols - 1, 0), pfp(&s, Tag::Active, sel.end()).unwrap());
        }

        // Test our contents rotated
        assert_eq!("2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Scrolling to the bottom does nothing
        s.scroll(Scroll::Active);

        // Our selection should've stayed the same
        {
            let sel = s.selection.unwrap();
            assert_eq!(act(0, 0), pfp(&s, Tag::Active, sel.start()).unwrap());
            assert_eq!(act(cols - 1, 0), pfp(&s, Tag::Active, sel.end()).unwrap());
        }

        // Test our contents rotated
        assert_eq!("2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Scroll up again
        s.cursor_down_scroll().unwrap();

        // Test our contents rotated
        assert_eq!("3IJKL", dump(&s, Tag::Viewport));

        // Our selection should be null because it left the screen.
        {
            let sel = s.selection.unwrap();
            assert!(pfp(&s, Tag::Active, sel.start()).is_none());
            assert!(pfp(&s, Tag::Active, sel.end()).is_none());
        }
    }

    #[test]
    fn screen_cursor_scroll_region_up_simple() {
        let mut s = init(5, 5, Some(0));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL\n4MNOP\n5QRST").unwrap();

        // Scroll a region ending at row 2 (zero-indexed) up by one. This
        // emulates a scroll region of rows 0-2 with the cursor at the
        // region bottom.
        s.cursor_absolute(1, 2);
        s.cursor_scroll_region_up(2).unwrap();

        // The cursor stays in place, on the new blank row.
        assert_eq!(1, s.cursor.x);
        assert_eq!(2, s.cursor.y);

        // Rows in the region scrolled, rows below are unchanged, and
        // nothing was moved into scrollback.
        assert_eq!("2EFGH\n3IJKL\n\n4MNOP\n5QRST", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_cursor_scroll_region_up_renews_page_generation() {
        let mut s = init(5, 5, Some(0));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL\n4MNOP\n5QRST").unwrap();
        s.cursor_absolute(0, 2);

        let node = cp(&s).node;
        let serial = nd(node).serial;
        s.cursor_scroll_region_up(2).unwrap();

        assert!(!s.pages.node_is_valid(node, serial));
    }

    #[test]
    fn screen_cursor_scroll_region_up_moves_selection() {
        let mut s = init(5, 5, Some(0));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL\n4MNOP\n5QRST").unwrap();

        // Select the second row.
        let cols = s.pages.cols;
        let sel = Selection::init(pin(&s, act(0, 1)), pin(&s, act(cols - 1, 1)), false);
        s.select(Some(sel)).unwrap();

        s.cursor_absolute(0, 2);
        s.cursor_scroll_region_up(2).unwrap();

        // Our selection should've moved up with its row.
        {
            let sel = s.selection.unwrap();
            assert_eq!(act(0, 0), pfp(&s, Tag::Active, sel.start()).unwrap());
            assert_eq!(act(cols - 1, 0), pfp(&s, Tag::Active, sel.end()).unwrap());
        }

        assert_eq!("2EFGH\n3IJKL\n\n4MNOP\n5QRST", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_cursor_scroll_region_up_region_spans_pages() {
        let mut s = init(10, 5, Some(10));

        // We need to get the cursor to a new page
        fill_first_page(&mut s, 3);
        s.test_write_string("1A\n2B\n3C\n4D\n5E").unwrap();

        // Move the cursor to the first row of the second page and give it
        // a non-default style. This is important: it verifies that the
        // cursor's style ref stays accounted on the correct page even
        // though eraseRowBounded moves the cursor's tracked pin across
        // the page boundary.
        s.cursor_absolute(0, 3);
        s.set_attribute(bold()).unwrap();
        assert!(cp(&s).node == last(&s));
        assert_eq!(0, cp(&s).y);

        // Scroll a region of active rows 1-3 with the cursor at the region
        // bottom. The region spans the page boundary so this exercises the
        // slow path.
        s.cursor_scroll_region_up(2).unwrap();

        // The cursor stays in place, on the new blank row.
        assert_eq!(0, s.cursor.x);
        assert_eq!(3, s.cursor.y);

        assert_eq!("1A\n3C\n4D\n\n5E", dump(&s, Tag::Viewport));

        // Our cursor style must remain usable: write a styled cell and
        // verify the style ref counting is intact on the cursor's page.
        s.test_write_string("X").unwrap();
        assert_eq!(1, cpage(&s).styles.count());
    }

    #[test]
    fn screen_cursor_scroll_region_up_region_spans_pages_with_background_sgr() {
        let mut s = init(10, 5, Some(10));

        // We need to get the cursor to a new page. See the previous test
        // for a diagram of the page layout.
        fill_first_page(&mut s, 3);
        s.test_write_string("1A\n2B\n3C\n4D\n5E").unwrap();

        s.cursor_absolute(0, 3);
        s.set_attribute(bg_rgb(0xFF, 0, 0)).unwrap();
        assert!(cp(&s).node == last(&s));
        assert_eq!(0, cp(&s).y);

        s.cursor_scroll_region_up(2).unwrap();

        assert_eq!("1A\n3C\n4D\n\n5E", dump(&s, Tag::Viewport));

        // The new blank row must be filled with our background color.
        for x in 0..s.pages.cols {
            let cell = get_cell(&s, act(x, 3));
            assert!(cell.content_tag() == ContentTag::BgColorRgb);
            assert_eq!(rgb(0xFF, 0, 0), cell.content_color_rgb());
        }
    }

    #[test]
    fn screen_cursor_scroll_region_up_with_styled_erased_row() {
        let mut s = init(5, 3, Some(0));

        // Write a styled row at the top so the erased row has managed
        // memory that must be released.
        s.set_attribute(bold()).unwrap();
        s.test_write_string("1ABCD").unwrap();
        s.set_attribute(sgr::Attribute::Unset).unwrap();
        s.test_write_string("\n2EFGH\n3IJKL").unwrap();

        s.cursor_absolute(0, 2);
        s.cursor_scroll_region_up(2).unwrap();

        assert_eq!("2EFGH\n3IJKL", dump(&s, Tag::Screen));

        // The style should be gone from the page since the only user
        // was the erased row.
        assert_eq!(0, cpage(&s).styles.count());
    }

    #[test]
    fn screen_scrolling_moves_viewport() {
        let mut s = init(10, 3, Some(1));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL\n").unwrap();
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();
        s.scroll(Scroll::DeltaRow(-2));

        // Test our contents rotated
        assert_eq!("2EFGH\n3IJKL\n1ABCD", dump(&s, Tag::Viewport));

        assert_eq!(
            Some(scr(0, 1)),
            pfp(&s, Tag::Screen, s.pages.get_top_left(Tag::Viewport))
        );
    }

    #[test]
    fn screen_scrolling_when_viewport_is_pruned() {
        let mut s = init(215, 3, Some(1));

        // Write some to create scrollback and move back into our scrollback.
        s.test_write_string("1ABCD\n2EFGH\n3IJKL\n").unwrap();
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();
        s.scroll(Scroll::DeltaRow(-2));

        // Our viewport is now somewhere pinned. Create so much scrollback
        // that we prune it.
        s.test_write_string("\n").unwrap();
        for _ in 0..1000 {
            s.test_write_string("1ABCD\n2EFGH\n3IJKL\n").unwrap();
        }
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();

        assert_eq!(
            Some(scr(0, 0)),
            pfp(&s, Tag::Screen, s.pages.get_top_left(Tag::Viewport))
        );
    }

    #[test]
    fn screen_scroll_and_clear_full_screen() {
        let mut s = init(10, 3, Some(5));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();

        assert_eq!("1ABCD\n2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        s.scroll_clear().unwrap();
        assert_eq!("", dump(&s, Tag::Viewport));
        assert_eq!("1ABCD\n2EFGH\n3IJKL", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_scroll_and_clear_partial_screen() {
        let mut s = init(10, 3, Some(5));
        s.test_write_string("1ABCD\n2EFGH").unwrap();

        assert_eq!("1ABCD\n2EFGH", dump(&s, Tag::Viewport));

        s.scroll_clear().unwrap();
        assert_eq!("", dump(&s, Tag::Viewport));
        assert_eq!("1ABCD\n2EFGH", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_scroll_and_clear_empty_screen() {
        let mut s = init(10, 3, Some(5));
        s.scroll_clear().unwrap();
        assert_eq!("", dump(&s, Tag::Viewport));
        assert_eq!("", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_scroll_and_clear_ignore_blank_lines() {
        let mut s = init(10, 3, Some(10));
        s.test_write_string("1ABCD\n2EFGH").unwrap();
        s.scroll_clear().unwrap();
        assert_eq!("", dump(&s, Tag::Viewport));

        // Move back to top-left
        s.cursor_absolute(0, 0);

        // Write and clear
        s.test_write_string("3ABCD\n").unwrap();
        assert_eq!("3ABCD", dump(&s, Tag::Active));

        s.scroll_clear().unwrap();
        assert_eq!("", dump(&s, Tag::Viewport));

        // Move back to top-left
        s.cursor_absolute(0, 0);
        s.test_write_string("X").unwrap();

        assert_eq!("1ABCD\n2EFGH\n3ABCD\nX", dump(&s, Tag::Screen));
    }

    fn assert_bg155(s: &Screen, pt: Point) {
        let cell = get_cell(s, pt);
        assert!(cell.content_tag() == ContentTag::BgColorRgb);
        assert_eq!(rgb(155, 0, 0), cell.content_color_rgb());
    }

    #[test]
    fn screen_scroll_above_same_page() {
        let mut s = init(10, 3, Some(10));
        s.set_attribute(bg_rgb(155, 0, 0)).unwrap();
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();
        s.cursor_absolute(0, 1);
        s.pages.clear_dirty();

        // At this point:
        //  +-------------+ ACTIVE
        //   +----------+ : = PAGE 0
        // 0 |1ABCD00000| | 0
        // 1 |2EFGH00000| | 1
        //   :^         : : = PIN 0
        // 2 |3IJKL00000| | 2
        //   +----------+ :
        //  +-------------+

        let node = cp(&s).node;
        let serial = nd(node).serial;
        s.cursor_scroll_above().unwrap();
        assert!(!s.pages.node_is_valid(node, serial));

        //   +----------+ = PAGE 0
        // 0 |1ABCD00000|
        //  +-------------+ ACTIVE
        // 1 |2EFGH00000| | 0
        // 2 |          | | 1
        //   :^         : : = PIN 0
        // 3 |3IJKL00000| | 2
        //   +----------+ :
        //  +-------------+

        assert_eq!("2EFGH\n\n3IJKL", dump(&s, Tag::Viewport));
        assert_bg155(&s, act(0, 1));

        // Page 0 row 1 (active row 0) is dirty because the cursor moved off of it.
        assert!(s.pages.is_dirty(act(0, 0)));
        // Page 0 row 2 (active row 1) is dirty because it was cleared.
        assert!(s.pages.is_dirty(act(0, 1)));
        // Page 0 row 3 (active row 2) is dirty because it's new.
        assert!(s.pages.is_dirty(act(0, 2)));
    }

    #[test]
    fn screen_scroll_above_same_page_but_cursor_on_previous_page() {
        let mut s = init(10, 5, Some(10));

        // We need to get the cursor to a new page
        fill_first_page(&mut s, 3);

        s.set_attribute(bg_rgb(155, 0, 0)).unwrap();
        s.test_write_string("1A\n2B\n3C\n4D\n5E").unwrap();
        s.cursor_absolute(0, 1);
        s.pages.clear_dirty();

        // Ensure we're still on the first page and have a second
        assert!(cp(&s).node == first(&s));
        assert!(nd(first(&s)).next.is_some());

        let first_node = first(&s);
        let second_node = nd(first_node).next.unwrap();
        let first_serial = nd(first_node).serial;
        let second_serial = nd(second_node).serial;
        s.cursor_scroll_above().unwrap();
        assert!(!s.pages.node_is_valid(first_node, first_serial));
        assert!(!s.pages.node_is_valid(second_node, second_serial));

        assert_eq!("2B\n\n3C\n4D\n5E", dump(&s, Tag::Viewport));
        assert_bg155(&s, act(0, 1));

        // Page 0's penultimate row is dirty because the cursor moved off of it.
        assert!(s.pages.is_dirty(act(0, 0)));
        // The rest of the rows are dirty because they've been modified or are new.
        assert!(s.pages.is_dirty(act(0, 1)));
        assert!(s.pages.is_dirty(act(0, 2)));
        assert!(s.pages.is_dirty(act(0, 3)));
        assert!(s.pages.is_dirty(act(0, 4)));
    }

    #[test]
    fn screen_scroll_above_same_page_but_cursor_on_previous_page_last_row() {
        let mut s = init(10, 5, Some(10));

        // We need to get the cursor to a new page
        fill_first_page(&mut s, 2);

        s.set_attribute(bg_rgb(155, 0, 0)).unwrap();
        s.test_write_string("1A\n2B\n3C\n4D\n5E").unwrap();
        s.cursor_absolute(0, 1);
        s.pages.clear_dirty();

        // Ensure we're still on the first page and have a second
        assert!(cp(&s).node == first(&s));
        assert!(nd(first(&s)).next.is_some());

        s.cursor_scroll_above().unwrap();

        assert_eq!("2B\n\n3C\n4D\n5E", dump(&s, Tag::Viewport));
        assert_bg155(&s, act(0, 1));

        // Page 0's final row is dirty because the cursor moved off of it.
        assert!(s.pages.is_dirty(act(0, 0)));
        // Page 1's rows are all dirty because every row was moved.
        assert!(s.pages.is_dirty(act(0, 1)));
        assert!(s.pages.is_dirty(act(0, 2)));
        assert!(s.pages.is_dirty(act(0, 3)));
        assert!(s.pages.is_dirty(act(0, 4)));

        // Attempt to clear the style from the cursor and
        // then assert the integrity of both of our pages.
        //
        // This catches a case of memory corruption where the cursor
        // is moved between pages without accounting for style refs.
        s.set_attribute(sgr::Attribute::ResetBg).unwrap();
        pg(first(&s)).assert_integrity();
        pg(last(&s)).assert_integrity();
    }

    #[test]
    fn screen_scroll_above_creates_new_page() {
        let mut s = init(10, 3, Some(10));

        // We need to get the cursor to a new page
        fill_first_page(&mut s, 3);

        s.set_attribute(bg_rgb(155, 0, 0)).unwrap();
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();
        s.cursor_absolute(0, 1);
        s.pages.clear_dirty();

        // Ensure we're still on the first page
        assert!(cp(&s).node == first(&s));

        let node = first(&s);
        let serial = nd(node).serial;
        s.cursor_scroll_above().unwrap();
        assert!(!s.pages.node_is_valid(node, serial));

        assert_eq!("2EFGH\n\n3IJKL", dump(&s, Tag::Viewport));
        assert_bg155(&s, act(0, 1));

        // Page 0's penultimate row is dirty because the cursor moved off of it.
        assert!(s.pages.is_dirty(act(0, 0)));
        // Page 0's final row is dirty because it was cleared.
        assert!(s.pages.is_dirty(act(0, 1)));
        // Page 1's row is dirty because it's new.
        assert!(s.pages.is_dirty(act(0, 2)));
    }

    #[test]
    fn screen_scroll_above_with_cursor_on_non_final_row() {
        let mut s = init(10, 4, Some(10));

        // Get the cursor to be 2 rows above a new page
        fill_first_page(&mut s, 3);

        // Write 3 lines of text, forcing the last line into the first
        // row of a new page. Move our cursor onto the previous page.
        s.set_attribute(bg_rgb(155, 0, 0)).unwrap();
        s.test_write_string("1AB\n2BC\n3DE\n4FG").unwrap();
        s.cursor_absolute(0, 1);
        s.pages.clear_dirty();

        // Ensure we're still on the first page. So our cursor is on the first
        // page but we have two pages of data.
        assert!(cp(&s).node == first(&s));

        s.cursor_scroll_above().unwrap();

        assert_eq!("2BC\n\n3DE\n4FG", dump(&s, Tag::Viewport));
        assert_bg155(&s, act(0, 1));

        // Page 0's penultimate row is dirty because the cursor moved off of it.
        assert!(s.pages.is_dirty(act(0, 0)));
        // Page 0's final row is dirty because it was cleared.
        assert!(s.pages.is_dirty(act(0, 1)));
        // Page 1's row is dirty because it's new.
        assert!(s.pages.is_dirty(act(0, 2)));
    }

    #[test]
    fn screen_scroll_above_no_scrollback_bottom_of_page() {
        let mut s = init(10, 3, Some(0));

        fill_first_page(&mut s, 3);

        s.set_attribute(bg_rgb(155, 0, 0)).unwrap();
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();
        s.cursor_absolute(0, 1);
        s.pages.clear_dirty();

        s.cursor_scroll_above().unwrap();

        assert_eq!("2EFGH\n\n3IJKL", dump(&s, Tag::Viewport));
        assert_bg155(&s, act(0, 1));

        // Page 0 row 1 (active row 0) is dirty because the cursor moved off of it.
        assert!(s.pages.is_dirty(act(0, 0)));
        // Page 0 row 2 (active row 1) is dirty because it was cleared.
        assert!(s.pages.is_dirty(act(0, 1)));
        // Page 0 row 3 (active row 2) is dirty because it is new.
        assert!(s.pages.is_dirty(act(0, 2)));
    }

    /// Check that every hyperlink-flagged cell resolves to a map entry.
    fn assert_hyperlinks_resolve(s: &Screen) {
        let mut node_ = s.pages.pages.first;
        while let Some(node) = node_ {
            let page = pg(node);
            page.assert_integrity();
            for y in 0..page.size.rows as usize {
                let row = rw(page.get_row(y));
                if !row.hyperlink() {
                    continue;
                }
                for cell in row_cells(page, row).iter() {
                    if !cell.hyperlink() {
                        continue;
                    }
                    assert!(page.lookup_hyperlink(cell).is_some());
                }
            }
            node_ = nd(node).next;
        }
    }

    #[test]
    fn screen_scroll_above_hyperlink_dense_row_to_fresh_page() {
        // Regression test for https://github.com/ghostty-org/ghostty/discussions/13160
        //
        // When a scroll-above operation pushes a row carrying more unique
        // hyperlinks than a fresh page's default hyperlink capacity across
        // a page boundary, the cross-page row clone must increase the
        // destination page's capacity (like insertLines/deleteLines do)
        // rather than error out mid-operation, which leaves the page list
        // half-mutated and aborts later (e.g. in clearCells).
        let mut s = init(10, 5, Some(10000));

        // Fill the first page so it is exactly full and the cursor is on
        // its last row (which is also the bottom row of the active area).
        // The next grow() will then allocate a fresh page.
        fill_first_page(&mut s, 1);
        assert!(s.pages.pages.first == s.pages.pages.last);
        assert_eq!(nd(first(&s)).capacity().rows, pg(first(&s)).size.rows);
        assert_eq!(s.pages.rows - 1, s.cursor.y);

        // Fill the bottom row with unique hyperlinks: more than a fresh
        // page can hold with default hyperlink capacity.
        for i in 0..s.pages.cols {
            let uri = format!("http://example.com/{i}");
            s.start_hyperlink(uri.as_bytes(), None).unwrap();
            s.test_write_string("A").unwrap();
            s.end_hyperlink();
        }
        assert_eq!(s.pages.cols as usize, cpage(&s).hyperlink_set.count());

        // Move the cursor above the bottom row and scroll. The dense row is
        // pushed across the page boundary into the freshly allocated page.
        s.cursor_absolute(0, 1);
        s.cursor_scroll_above().unwrap();

        // We must have created a second page and the dense row must now be
        // the top row of that page.
        assert!(s.pages.pages.first != s.pages.pages.last);

        // All hyperlinks must have survived the scroll intact: every cell
        // flagged as a hyperlink must resolve to a real entry in its page's
        // hyperlink map. A half-applied scroll leaves cells whose hyperlink
        // flag is set but that have no map entry, which aborts in
        // clearCells later.
        assert_hyperlinks_resolve(&s);
        assert_eq!(s.pages.cols as usize, pg(last(&s)).hyperlink_set.count());

        // The dense row is still the bottom row of the active area.
        for x in 0..s.pages.cols {
            let list_cell = s.pages.get_cell(act(x, 4)).unwrap();
            let cell = unsafe { &*list_cell.cell };
            assert!(cell.hyperlink());
            let page = pg(list_cell.node);
            let id = page.lookup_hyperlink(cell).unwrap();
            let link = unsafe { *page.hyperlink_set.get(page.memory, id) };
            let expected = format!("http://example.com/{x}");
            assert_eq!(expected.as_bytes(), unsafe { link.uri.slice(page.memory) });
        }
    }

    #[test]
    fn screen_scroll_above_hyperlink_dense_row_to_existing_page() {
        // Same as the fresh page variant above but the destination page
        // already exists (fresh_node == null path in cursorScrollAboveRotate).
        let mut s = init(10, 5, Some(10000));

        // Fill the first page so it is exactly full and the cursor is on
        // its last row.
        fill_first_page(&mut s, 1);
        assert_eq!(s.pages.rows - 1, s.cursor.y);

        // Fill the last row of the first page with unique hyperlinks:
        // more than a page can hold with default hyperlink capacity.
        for i in 0..s.pages.cols {
            let uri = format!("http://example.com/{i}");
            s.start_hyperlink(uri.as_bytes(), None).unwrap();
            s.test_write_string("A").unwrap();
            s.end_hyperlink();
        }

        // Scroll twice so the active area straddles the page boundary:
        // the last two active rows are on a second page while the dense
        // row remains the last row of the first page.
        s.test_write_string("\n\n").unwrap();
        assert!(s.pages.pages.first != s.pages.pages.last);
        assert!(cp(&s).node == last(&s));

        // Move the cursor to an active row that is still on the first page
        // and above the dense row, then scroll. grow() has capacity in the
        // last page so no fresh page is allocated, but the dense row still
        // crosses the page boundary during the rotate.
        s.cursor_absolute(0, 0);
        assert!(cp(&s).node == first(&s));
        s.cursor_scroll_above().unwrap();

        // All hyperlinks must have survived the scroll intact: every cell
        // flagged as a hyperlink must resolve to a real entry in its page's
        // hyperlink map.
        assert_hyperlinks_resolve(&s);
        assert_eq!(s.pages.cols as usize, pg(last(&s)).hyperlink_set.count());
    }

    fn act_y(y: u32) -> Point {
        act(0, y)
    }

    #[test]
    fn screen_clone() {
        let mut s = init(10, 3, Some(10));
        s.test_write_string("1ABCD\n2EFGH").unwrap();
        assert_eq!("1ABCD\n2EFGH", dump(&s, Tag::Active));
        assert_eq!(5, s.cursor.x);
        assert_eq!(1, s.cursor.y);

        // Clone
        let s2 = s.clone(act(0, 0), None).unwrap();
        assert_eq!("1ABCD\n2EFGH", dump(&s2, Tag::Active));
        assert_eq!(5, s2.cursor.x);
        assert_eq!(1, s2.cursor.y);

        // Write to s1, should not be in s2
        s.test_write_string("\n34567").unwrap();
        assert_eq!("1ABCD\n2EFGH\n34567", dump(&s, Tag::Active));
        assert_eq!("1ABCD\n2EFGH", dump(&s2, Tag::Active));
        assert_eq!(5, s2.cursor.x);
        assert_eq!(1, s2.cursor.y);
    }

    #[test]
    fn screen_clone_partial() {
        let mut s = init(10, 3, Some(10));
        s.test_write_string("1ABCD\n2EFGH").unwrap();
        assert_eq!("1ABCD\n2EFGH", dump(&s, Tag::Active));
        assert_eq!(5, s.cursor.x);
        assert_eq!(1, s.cursor.y);

        // Clone
        let s2 = s.clone(act_y(1), None).unwrap();
        assert_eq!("2EFGH", dump(&s2, Tag::Active));

        // Cursor is shifted since we cloned partial
        assert_eq!(5, s2.cursor.x);
        assert_eq!(0, s2.cursor.y);
    }

    #[test]
    fn screen_clone_partial_cursor_out_of_bounds() {
        let mut s = init(10, 3, Some(10));
        s.test_write_string("1ABCD\n2EFGH").unwrap();
        assert_eq!("1ABCD\n2EFGH", dump(&s, Tag::Active));
        assert_eq!(5, s.cursor.x);
        assert_eq!(1, s.cursor.y);

        // Clone
        let s2 = s.clone(act_y(0), Some(act_y(0))).unwrap();
        assert_eq!("1ABCD", dump(&s2, Tag::Active));

        // Cursor is shifted since we cloned partial
        assert_eq!(0, s2.cursor.x);
        assert_eq!(0, s2.cursor.y);
    }

    #[test]
    fn screen_clone_contains_full_selection() {
        let mut s = init(5, 3, Some(1));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();

        // Select a single line
        let cols = s.pages.cols;
        let sel = sel_init(&s, act(0, 1), act(cols - 1, 1), false);
        s.select(Some(sel)).unwrap();

        // Clone
        let s2 = s.clone(act(0, 0), None).unwrap();

        // Our selection should remain valid
        let sel = s2.selection.unwrap();
        assert_eq!(act(0, 1), pfp(&s2, Tag::Active, sel.start()).unwrap());
        assert_eq!(act(s2.pages.cols - 1, 1), pfp(&s2, Tag::Active, sel.end()).unwrap());
    }

    #[test]
    fn screen_clone_contains_none_of_selection() {
        let mut s = init(5, 3, Some(1));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();

        // Select a single line
        let cols = s.pages.cols;
        let sel = sel_init(&s, act(0, 0), act(cols - 1, 0), false);
        s.select(Some(sel)).unwrap();

        // Clone
        let s2 = s.clone(act_y(1), None).unwrap();

        // Our selection should be null
        assert!(s2.selection.is_none());
    }

    #[test]
    fn screen_clone_contains_selection_start_cutoff() {
        let mut s = init(5, 3, Some(1));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();

        // Select a single line
        let cols = s.pages.cols;
        let sel = sel_init(&s, act(0, 0), act(cols - 1, 1), false);
        s.select(Some(sel)).unwrap();

        // Clone
        let s2 = s.clone(act_y(1), None).unwrap();

        // Our selection should remain valid
        let sel = s2.selection.unwrap();
        assert_eq!(act(0, 0), pfp(&s2, Tag::Active, sel.start()).unwrap());
        assert_eq!(act(s2.pages.cols - 1, 0), pfp(&s2, Tag::Active, sel.end()).unwrap());
    }

    #[test]
    fn screen_clone_contains_selection_end_cutoff() {
        let mut s = init(5, 3, Some(1));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();

        // Select a single line
        let sel = sel_init(&s, act(0, 1), act(2, 2), false);
        s.select(Some(sel)).unwrap();

        // Clone
        let s2 = s.clone(act_y(0), Some(act_y(1))).unwrap();

        // Our selection should remain valid
        let sel = s2.selection.unwrap();
        assert_eq!(act(0, 1), pfp(&s2, Tag::Active, sel.start()).unwrap());
        assert_eq!(act(s2.pages.cols - 1, 2), pfp(&s2, Tag::Active, sel.end()).unwrap());
    }

    #[test]
    fn screen_clone_contains_selection_end_cutoff_reversed() {
        let mut s = init(5, 3, Some(1));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();

        // Select a single line
        let sel = sel_init(&s, act(2, 2), act(0, 1), false);
        s.select(Some(sel)).unwrap();

        // Clone
        let s2 = s.clone(act_y(0), Some(act_y(1))).unwrap();

        // Our selection should remain valid
        let sel = s2.selection.unwrap();
        assert_eq!(act(0, 1), pfp(&s2, Tag::Active, sel.start()).unwrap());
        assert_eq!(act(s2.pages.cols - 1, 2), pfp(&s2, Tag::Active, sel.end()).unwrap());
    }

    #[test]
    fn screen_clone_contains_subset_of_selection() {
        let mut s = init(5, 4, Some(1));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL\n4ABCD").unwrap();

        // Select the full screen
        let sel = sel_init(&s, act(0, 0), act(0, 3), false);
        s.select(Some(sel)).unwrap();

        // Clone
        let s2 = s.clone(act_y(1), Some(act_y(2))).unwrap();

        // Our selection should remain valid
        let sel = s2.selection.unwrap();
        assert_eq!(act(0, 0), pfp(&s2, Tag::Active, sel.start()).unwrap());
        assert_eq!(act(s2.pages.cols - 1, 3), pfp(&s2, Tag::Active, sel.end()).unwrap());
    }

    #[test]
    fn screen_clone_clamps_clipped_selections_to_mixed_width_pages() {
        let mut s = init(4, 3, Some(0));

        let f = first(&s);
        let mut p = Pin::new(f);
        p.y = 2;
        s.pages.split(p).unwrap();
        p.y = 1;
        s.pages.split(p).unwrap();
        let middle = nd(f).next.unwrap();
        let l = nd(middle).next.unwrap();
        pg(middle).size.cols = 2;

        let mut end = Pin::new(l);
        end.x = 3;
        s.select(Some(Selection::init(Pin::new(f), end, false))).unwrap();
        let linear = s.clone(scr(0, 0), Some(scr(0, 1))).unwrap();
        let linear_end = linear.selection.unwrap().end();
        let _ = linear_end.row_and_cell();
        assert_eq!(1, linear_end.x);

        let mut start = Pin::new(f);
        start.x = 3;
        s.select(Some(Selection::init(start, end, true))).unwrap();
        let rectangle = s.clone(scr(0, 1), None).unwrap();
        let rectangle_start = rectangle.selection.unwrap().start();
        let _ = rectangle_start.row_and_cell();
        assert_eq!(1, rectangle_start.x);
    }

    #[test]
    fn screen_clone_contains_subset_of_rectangle_selection() {
        let mut s = init(5, 4, Some(1));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL\n4ABCD").unwrap();

        // Select the full screen from x=1 to x=3
        let sel = sel_init(&s, act(1, 0), act(3, 3), true);
        s.select(Some(sel)).unwrap();

        // Clone
        let s2 = s.clone(act_y(1), Some(act_y(2))).unwrap();

        // Our selection should remain valid and be properly clipped
        // preserving the columns of the start and end points of the
        // selection.
        let sel = s2.selection.unwrap();
        assert_eq!(act(1, 0), pfp(&s2, Tag::Active, sel.start()).unwrap());
        assert_eq!(act(3, 3), pfp(&s2, Tag::Active, sel.end()).unwrap());
    }

    #[test]
    fn screen_clone_basic() {
        let mut s = init(10, 3, Some(0));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();

        {
            let s2 = s.clone(act_y(1), Some(act_y(1))).unwrap();

            // Test our contents rotated
            assert_eq!("2EFGH", dump(&s2, Tag::Active));
        }

        {
            let s2 = s.clone(act_y(1), Some(act_y(2))).unwrap();

            // Test our contents rotated
            assert_eq!("2EFGH\n3IJKL", dump(&s2, Tag::Active));
        }
    }

    #[test]
    fn screen_clone_empty_viewport() {
        let s = init(10, 3, Some(0));

        let s2 = s.clone(vp(0, 0), Some(vp(0, 0))).unwrap();

        // Test our contents rotated
        assert_eq!("", dump(&s2, Tag::Viewport));
    }

    #[test]
    fn screen_clone_one_line_viewport() {
        let mut s = init(10, 3, Some(0));
        s.test_write_string("1ABC").unwrap();

        let s2 = s.clone(vp(0, 0), Some(vp(0, 0))).unwrap();

        // Test our contents
        assert_eq!("1ABC", dump(&s2, Tag::Viewport));
    }

    #[test]
    fn screen_clone_empty_active() {
        let s = init(10, 3, Some(0));

        let s2 = s.clone(act_y(0), Some(act_y(0))).unwrap();

        // Test our contents rotated
        assert_eq!("", dump(&s2, Tag::Active));
    }

    #[test]
    fn screen_clone_one_line_active_with_extra_space() {
        let mut s = init(10, 3, Some(0));
        s.test_write_string("1ABC").unwrap();

        let s2 = s.clone(act_y(0), None).unwrap();

        // Test our contents rotated
        assert_eq!("1ABC", dump(&s2, Tag::Active));
    }

    #[test]
    fn screen_clear_history_with_no_history() {
        let mut s = init(10, 3, Some(3));
        s.test_write_string("4ABCD\n5EFGH\n6IJKL").unwrap();
        assert!(s.pages.viewport == Viewport::Active);
        s.erase_history(None);
        assert!(s.pages.viewport == Viewport::Active);
        assert_eq!("4ABCD\n5EFGH\n6IJKL", dump(&s, Tag::Viewport));
        assert_eq!("4ABCD\n5EFGH\n6IJKL", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_clear_history() {
        let mut s = init(10, 3, Some(3));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH\n6IJKL").unwrap();
        assert!(s.pages.viewport == Viewport::Active);

        // Scroll to top
        s.scroll(Scroll::Top);
        assert_eq!("1ABCD\n2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        s.erase_history(None);
        assert!(s.pages.viewport == Viewport::Active);
        assert_eq!("4ABCD\n5EFGH\n6IJKL", dump(&s, Tag::Viewport));
        assert_eq!("4ABCD\n5EFGH\n6IJKL", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_clear_above_cursor() {
        let mut s = init(10, 10, Some(3));
        s.test_write_string("4ABCD\n5EFGH\n6IJKL").unwrap();
        let y = s.cursor.y as u32 - 1;
        s.clear_rows(act_y(0), Some(act_y(y)), false);
        assert_eq!("\n\n6IJKL", dump(&s, Tag::Viewport));
        assert_eq!("\n\n6IJKL", dump(&s, Tag::Screen));

        assert_eq!(5, s.cursor.x);
        assert_eq!(2, s.cursor.y);
    }

    #[test]
    fn screen_clear_above_cursor_with_history() {
        let mut s = init(10, 3, Some(3));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL\n").unwrap();
        s.test_write_string("4ABCD\n5EFGH\n6IJKL").unwrap();
        let y = s.cursor.y as u32 - 1;
        s.clear_rows(act_y(0), Some(act_y(y)), false);
        assert_eq!("\n\n6IJKL", dump(&s, Tag::Viewport));
        assert_eq!("1ABCD\n2EFGH\n3IJKL\n\n\n6IJKL", dump(&s, Tag::Screen));

        assert_eq!(5, s.cursor.x);
        assert_eq!(2, s.cursor.y);
    }

    fn resize_opts(s: &mut Screen, cols: CellCountInt, rows: CellCountInt, reflow: bool, pull_scrollback: bool) {
        s.resize(Resize {
            cols,
            rows,
            reflow,
            pull_scrollback,
            ..Default::default()
        })
        .unwrap();
    }

    /// Write only a background color into the remaining rows.
    fn write_bg_rows(s: &mut Screen) {
        for y in 1..s.pages.rows as u32 {
            let list_cell = s.pages.get_cell(act(0, y)).unwrap();
            let mut c = Cell::default();
            c.set_content_tag(ContentTag::BgColorRgb);
            c.set_content_color_rgb(rgb(0xFF, 0, 0));
            unsafe { *list_cell.cell = c };
        }
    }

    #[test]
    fn screen_resize_no_reflow_more_rows() {
        let mut s = init(10, 3, Some(0));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();

        // Resize
        resize(&mut s, 10, 10, false);
        assert_eq!(str_, dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_no_reflow_less_rows() {
        let mut s = init(10, 3, Some(0));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();
        assert_eq!(5, s.cursor.x);
        assert_eq!(2, s.cursor.y);
        resize(&mut s, 10, 2, false);

        // Since we shrunk, we should adjust our cursor
        assert_eq!(5, s.cursor.x);
        assert_eq!(1, s.cursor.y);

        assert_eq!("2EFGH\n3IJKL", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_no_reflow_less_rows_trims_blank_lines() {
        let mut s = init(10, 3, Some(0));
        let str_ = "1ABCD";
        s.test_write_string(str_).unwrap();

        // Write only a background color into the remaining rows
        write_bg_rows(&mut s);

        let (cx, cy) = (s.cursor.x, s.cursor.y);
        resize(&mut s, 6, 2, false);

        // Cursor should not move
        assert_eq!(cx, s.cursor.x);
        assert_eq!(cy, s.cursor.y);

        assert_eq!("1ABCD", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_no_reflow_more_rows_trims_blank_lines() {
        let mut s = init(10, 3, Some(0));
        let str_ = "1ABCD";
        s.test_write_string(str_).unwrap();

        // Write only a background color into the remaining rows
        write_bg_rows(&mut s);

        let (cx, cy) = (s.cursor.x, s.cursor.y);
        resize(&mut s, 10, 7, false);

        // Cursor should not move
        assert_eq!(cx, s.cursor.x);
        assert_eq!(cy, s.cursor.y);

        assert_eq!("1ABCD", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_no_reflow_more_cols() {
        let mut s = init(10, 3, Some(0));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();
        resize(&mut s, 20, 3, false);

        assert_eq!(str_, dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_no_reflow_less_cols() {
        let mut s = init(10, 3, Some(0));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();
        resize(&mut s, 4, 3, false);

        assert_eq!("1ABC\n2EFG\n3IJK", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_no_reflow_more_rows_with_scrollback_cursor_end() {
        let mut s = init(7, 3, Some(2));
        let str_ = "1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH";
        s.test_write_string(str_).unwrap();
        resize(&mut s, 7, 10, false);

        assert_eq!(str_, dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_no_reflow_more_rows_no_scrollback_pull() {
        let mut s = init(7, 3, Some(2));
        let str_ = "1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH";
        s.test_write_string(str_).unwrap();

        // Cursor is at the bottom so this would normally pull scrollback.
        assert_eq!(2, s.cursor.y);
        resize_opts(&mut s, 7, 10, false, false);
        assert_eq!(2, s.cursor.y);

        assert_eq!("3IJKL\n4ABCD\n5EFGH", dump(&s, Tag::Active));
        assert_eq!(str_, dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_resize_more_cols_no_scrollback_pull() {
        let mut s = init(5, 3, Some(2));
        s.test_write_string("1AAAA\n2BBBB\n3CCCCDD\n4E").unwrap();
        assert_eq!("3CCCC\nDD\n4E", dump(&s, Tag::Active));

        // The wrapped line in the active area unwraps, freeing up a row. This
        // would normally pull "2BBBB" back but we should get a blank row at
        // the bottom instead.
        resize_opts(&mut s, 10, 3, true, false);
        assert_eq!(2, s.cursor.x);
        assert_eq!(1, s.cursor.y);

        assert_eq!("3CCCCDD\n4E", dump(&s, Tag::Active));
        assert_eq!("1AAAA\n2BBBB\n3CCCCDD\n4E", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_resize_more_cols_no_scrollback_pull_wrap_straddles_scrollback() {
        let mut s = init(5, 3, Some(2));
        s.test_write_string("1AAAA\n2BBBBXX\n3C\n4D").unwrap();
        assert_eq!("XX\n3C\n4D", dump(&s, Tag::Active));

        // The line isn't fully in scrollback so it is allowed to unwrap
        // back into view, but nothing above it is.
        resize_opts(&mut s, 10, 3, true, false);
        assert_eq!(2, s.cursor.y);

        assert_eq!("2BBBBXX\n3C\n4D", dump(&s, Tag::Active));
    }

    #[test]
    fn screen_resize_more_cols_and_rows_no_scrollback_pull() {
        let mut s = init(5, 3, Some(2));
        s.test_write_string("1AAAA\n2BBBB\n3CCCCDD\n4E").unwrap();

        resize_opts(&mut s, 10, 5, true, false);
        assert_eq!(1, s.cursor.y);

        assert_eq!("3CCCCDD\n4E", dump(&s, Tag::Active));
    }

    #[test]
    fn screen_resize_less_cols_no_scrollback_pull() {
        let mut s = init(10, 3, Some(2));
        s.test_write_string("0Z\n1AAAA\n2BBBBXX\n3C").unwrap();

        // Wrapping needs more rows than we have so the top of the active
        // area still scrolls off as usual.
        resize_opts(&mut s, 5, 3, true, false);
        assert_eq!(2, s.cursor.y);

        assert_eq!("2BBBB\nXX\n3C", dump(&s, Tag::Active));
    }

    #[test]
    fn screen_resize_no_reflow_less_rows_with_scrollback() {
        let mut s = init(7, 3, Some(2));
        let str_ = "1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH";
        s.test_write_string(str_).unwrap();
        resize(&mut s, 7, 2, false);

        assert_eq!("4ABCD\n5EFGH", dump(&s, Tag::Viewport));
    }

    // https://github.com/mitchellh/ghostty/issues/1030
    #[test]
    fn screen_resize_no_reflow_less_rows_with_empty_trailing() {
        let mut s = init(5, 3, Some(5));
        let str_ = "1\n2\n3\n4\n5\n6\n7\n8";
        s.test_write_string(str_).unwrap();
        s.scroll_clear().unwrap();
        s.cursor_absolute(0, 0);
        s.test_write_string("A\nB").unwrap();

        let (cx, cy) = (s.cursor.x, s.cursor.y);
        resize(&mut s, 5, 2, false);
        assert_eq!(cx, s.cursor.x);
        assert_eq!(cy, s.cursor.y);

        assert_eq!("A\nB", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_no_reflow_more_rows_with_soft_wrapping() {
        let mut s = init(2, 3, Some(3));
        let str_ = "1A2B\n3C4E\n5F6G";
        s.test_write_string(str_).unwrap();

        // Every second row should be wrapped
        for y in 0..6u32 {
            let row = get_row(&s, scr(0, y));
            let wrapped = y % 2 == 0;
            assert_eq!(wrapped, row.wrap());
        }

        // Resize
        resize(&mut s, 2, 10, false);
        assert_eq!("1A\n2B\n3C\n4E\n5F\n6G", dump(&s, Tag::Viewport));

        // Every second row should be wrapped
        for y in 0..6u32 {
            let row = get_row(&s, scr(0, y));
            let wrapped = y % 2 == 0;
            assert_eq!(wrapped, row.wrap());
        }
    }

    #[test]
    fn screen_resize_more_rows_no_scrollback() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();
        let (cx, cy) = (s.cursor.x, s.cursor.y);
        resize(&mut s, 5, 10, true);

        // Cursor should not move
        assert_eq!(cx, s.cursor.x);
        assert_eq!(cy, s.cursor.y);

        assert_eq!(str_, dump(&s, Tag::Viewport));
        assert_eq!(str_, dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_resize_more_rows_with_empty_scrollback() {
        let mut s = init(5, 3, Some(10));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();
        let (cx, cy) = (s.cursor.x, s.cursor.y);
        resize(&mut s, 5, 10, true);

        // Cursor should not move
        assert_eq!(cx, s.cursor.x);
        assert_eq!(cy, s.cursor.y);

        assert_eq!(str_, dump(&s, Tag::Viewport));
        assert_eq!(str_, dump(&s, Tag::Screen));
    }

    fn cursor_cp(s: &Screen) -> u32 {
        get_cell(s, act(s.cursor.x, s.cursor.y as u32)).content_codepoint()
    }

    #[test]
    fn screen_resize_more_rows_with_populated_scrollback() {
        let mut s = init(5, 3, Some(5));
        let str_ = "1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH";
        s.test_write_string(str_).unwrap();
        assert_eq!("3IJKL\n4ABCD\n5EFGH", dump(&s, Tag::Viewport));

        // Set our cursor to be on the "4"
        s.cursor_absolute(0, 1);
        s.cursor.pending_wrap = false;
        assert_eq!('4' as u32, cursor_cp(&s));

        // Resize
        resize(&mut s, 5, 10, true);

        // Cursor should still be on the "4"
        assert_eq!('4' as u32, cursor_cp(&s));

        assert_eq!("3IJKL\n4ABCD\n5EFGH", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_more_cols_no_reflow() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();

        let (cx, cy) = (s.cursor.x, s.cursor.y);
        resize(&mut s, 10, 3, true);

        // Pending wrap becomes the next insertion position.
        assert!(!s.cursor.pending_wrap);
        assert_eq!(cx + 1, s.cursor.x);
        assert_eq!(cy, s.cursor.y);

        assert_eq!(str_, dump(&s, Tag::Viewport));
        assert_eq!(str_, dump(&s, Tag::Screen));
    }

    // https://github.com/mitchellh/ghostty/issues/272#issuecomment-1676038963
    #[test]
    fn screen_resize_more_cols_perfect_split() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1ABCD2EFGH3IJKL";
        s.test_write_string(str_).unwrap();
        resize(&mut s, 10, 3, true);

        assert_eq!("1ABCD2EFGH\n3IJKL", dump(&s, Tag::Screen));
    }

    // https://github.com/mitchellh/ghostty/issues/1159
    #[test]
    fn screen_resize_no_reflow_more_cols_with_scrollback_scrolled_up() {
        let mut s = init(5, 3, Some(5));
        let str_ = "1\n2\n3\n4\n5\n6\n7\n8";
        s.test_write_string(str_).unwrap();

        // Cursor at bottom
        assert_eq!(1, s.cursor.x);
        assert_eq!(2, s.cursor.y);

        s.scroll(Scroll::DeltaRow(-4));
        assert_eq!("2\n3\n4", dump(&s, Tag::Viewport));

        resize(&mut s, 8, 3, true);
        assert_eq!(str_, dump(&s, Tag::Screen));

        // Cursor remains at bottom
        assert_eq!(1, s.cursor.x);
        assert_eq!(2, s.cursor.y);
    }

    // https://github.com/mitchellh/ghostty/issues/1159
    #[test]
    fn screen_resize_no_reflow_less_cols_with_scrollback_scrolled_up() {
        let mut s = init(5, 3, Some(5));
        let str_ = "1\n2\n3\n4\n5\n6\n7\n8";
        s.test_write_string(str_).unwrap();

        // Cursor at bottom
        assert_eq!(1, s.cursor.x);
        assert_eq!(2, s.cursor.y);

        s.scroll(Scroll::DeltaRow(-4));
        assert_eq!("2\n3\n4", dump(&s, Tag::Viewport));

        resize(&mut s, 4, 3, true);
        assert_eq!(str_, dump(&s, Tag::Screen));
        assert_eq!("6\n7\n8", dump(&s, Tag::Active));

        // Cursor remains at bottom
        assert_eq!(1, s.cursor.x);
        assert_eq!(2, s.cursor.y);

        // Old implementation doesn't do this but it makes sense to me:
        // assert_eq!("2\n3\n4", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_more_cols_no_reflow_preserves_semantic_prompt() {
        let mut s = init(5, 3, Some(0));

        // Set one of the rows to be a prompt
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("1ABCD\n").unwrap();
        s.cursor_set_semantic_content(SemanticContentUpdate::Prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("2EFGH").unwrap();
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("\n3IJKL").unwrap();

        resize(&mut s, 10, 3, false);

        let expected = "1ABCD\n2EFGH\n3IJKL";
        assert_eq!(expected, dump(&s, Tag::Viewport));
        assert_eq!(expected, dump(&s, Tag::Screen));

        // Our one row should still be a semantic prompt, the others should not.
        assert!(get_row(&s, act(0, 0)).semantic_prompt() == RowSemanticPrompt::None);
        assert!(get_row(&s, act(0, 1)).semantic_prompt() == RowSemanticPrompt::Prompt);
        assert!(get_row(&s, act(0, 2)).semantic_prompt() == RowSemanticPrompt::None);
    }

    #[test]
    fn screen_resize_more_cols_with_reflow_that_fits_full_width() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1ABCD2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();

        // Verify we soft wrapped
        assert_eq!("1ABCD\n2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Let's put our cursor on row 2, where the soft wrap is
        s.cursor_absolute(0, 1);
        s.cursor.pending_wrap = false;
        assert_eq!('2' as u32, cursor_cp(&s));

        // Resize and verify we undid the soft wrap because we have space now
        resize(&mut s, 10, 3, true);
        assert_eq!(str_, dump(&s, Tag::Viewport));

        // Our cursor should've moved
        assert_eq!(5, s.cursor.x);
        assert_eq!(0, s.cursor.y);
    }

    #[test]
    fn screen_resize_more_cols_with_reflow_that_ends_in_newline() {
        let mut s = init(6, 3, Some(0));
        let str_ = "1ABCD2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();

        // Verify we soft wrapped
        assert_eq!("1ABCD2\nEFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Let's put our cursor on the last row
        s.cursor_absolute(0, 2);
        assert_eq!('3' as u32, cursor_cp(&s));

        // Resize and verify we undid the soft wrap because we have space now
        resize(&mut s, 10, 3, true);
        assert_eq!(str_, dump(&s, Tag::Viewport));

        // Our cursor should still be on the 3
        assert_eq!('3' as u32, cursor_cp(&s));
    }

    #[test]
    fn screen_resize_more_cols_with_reflow_that_forces_more_wrapping() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1ABCD2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();

        // Let's put our cursor on row 2, where the soft wrap is
        s.cursor_absolute(0, 1);
        s.cursor.pending_wrap = false;
        assert_eq!('2' as u32, cursor_cp(&s));

        // Verify we soft wrapped
        assert_eq!("1ABCD\n2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Resize and verify we undid the soft wrap because we have space now
        resize(&mut s, 7, 3, true);
        assert_eq!("1ABCD2E\nFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Our cursor should've moved
        assert_eq!(5, s.cursor.x);
        assert_eq!(0, s.cursor.y);
    }

    #[test]
    fn screen_resize_more_cols_with_reflow_that_unwraps_multiple_times() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1ABCD2EFGH3IJKL";
        s.test_write_string(str_).unwrap();

        // Let's put our cursor on row 2, where the soft wrap is
        s.cursor_absolute(0, 2);
        s.cursor.pending_wrap = false;
        assert_eq!('3' as u32, cursor_cp(&s));

        // Verify we soft wrapped
        assert_eq!("1ABCD\n2EFGH\n3IJKL", dump(&s, Tag::Viewport));

        // Resize and verify we undid the soft wrap because we have space now
        resize(&mut s, 15, 3, true);
        assert_eq!("1ABCD2EFGH3IJKL", dump(&s, Tag::Viewport));

        // Our cursor should've moved
        assert_eq!(10, s.cursor.x);
        assert_eq!(0, s.cursor.y);
    }

    #[test]
    fn screen_resize_more_cols_with_populated_scrollback() {
        let mut s = init(5, 3, Some(5));
        let str_ = "1ABCD\n2EFGH\n3IJKL\n4ABCD5EFGH";
        s.test_write_string(str_).unwrap();
        assert_eq!("3IJKL\n4ABCD\n5EFGH", dump(&s, Tag::Viewport));

        // // Set our cursor to be on the "5"
        s.cursor_absolute(0, 2);
        s.cursor.pending_wrap = false;
        assert_eq!('5' as u32, cursor_cp(&s));

        // Resize
        resize(&mut s, 10, 3, true);
        assert_eq!("2EFGH\n3IJKL\n4ABCD5EFGH", dump(&s, Tag::Viewport));

        // Cursor should still be on the "5"
        assert_eq!('5' as u32, cursor_cp(&s));
    }

    #[test]
    fn screen_resize_more_cols_bounded_scrollback_keeps_viewport_valid() {
        // Regression test for issue #12298.
        //
        // This needs to live at the Screen layer rather than PageList because the
        // bad state only appears once Screen forwards the active cursor into the
        // resize path. A direct PageList resize repro does not hit the same bug.
        let mut s = init(2, 10, Some(10_000));

        // Build 30 rows of scrollback on top of our 10-row viewport so we have a
        // 40-row screen with history above the active area.
        for _ in 0..30 {
            let _ = s.pages.grow().unwrap();
        }
        s.cursor_reload();
        assert_eq!(40, s.pages.scrollbar().total);

        // Fill the entire screen with two-row wrapped runs:
        // - even rows mark the end of a wrapped line
        // - odd rows mark the continuation
        //
        // With 2 columns, each logical line occupies two rows. When we grow to 4
        // columns with reflow enabled, those pairs unwrap back into single rows.
        // That cuts the total row count down and is what stresses the viewport pin.
        let it = s.pages.page_iterator(Direction::RightDown, scr(0, 0), None);
        for chunk in it {
            let page = pg(chunk.node);
            for y in chunk.start as usize..chunk.end as usize {
                let rac = page.get_row_and_cell(0, y);
                if y % 2 == 0 {
                    rw(rac.row).set_wrap(true);
                } else {
                    rw(rac.row).set_wrap_continuation(true);
                }

                for x in 0..s.pages.cols as usize {
                    *cl(page.get_row_and_cell(x, y).cell) = Cell::init('A' as u32);
                }
            }
        }

        // Pin the viewport to a history row just above the active area.
        //
        // Before resize:
        // - total rows = 40
        // - active area starts at row 30
        // - viewport is pinned at row 28
        //
        // After unwrap during resize:
        // - total rows shrinks to 20
        // - the old row 28 remaps into what is now the active area
        //
        // The bug was that resize/grow would temporarily keep the viewport as a
        // history pin even after reflow had moved it into the active area, leaving
        // fewer than `rows` visible rows beneath the pin and tripping integrity
        // checks.
        let p = pin(&s, scr(0, 28));
        s.pages.scroll(page_list::Scroll::Pin(p));
        assert!(s.pages.viewport == Viewport::Pin);
        assert!(s.pages.get_bottom_right(Tag::Viewport).is_some());

        // Growing columns triggers reflow, which unwraps the synthetic wrapped
        // rows above. This used to panic during the resize path.
        let rows = s.pages.rows;
        resize(&mut s, 4, rows, true);

        // After the fix, the viewport is normalized back to the active area as
        // soon as the pinned row lands there, so viewport queries remain valid.
        assert_eq!(4, s.pages.cols);
        assert!(s.pages.scrollbar().total < 40);
        assert!(s.pages.viewport == Viewport::Active);
        assert!(s.pages.get_bottom_right(Tag::Viewport).is_some());
    }

    #[test]
    fn screen_resize_more_cols_with_reflow() {
        let mut s = init(2, 3, Some(5));
        let str_ = "1ABC\n2DEF\n3ABC\n4DEF";
        s.test_write_string(str_).unwrap();

        // Let's put our cursor on row 2, where the soft wrap is
        s.cursor_absolute(0, 2);
        s.cursor.pending_wrap = false;
        assert_eq!('E' as u32, cursor_cp(&s));

        // Verify we soft wrapped
        assert_eq!("BC\n4D\nEF", dump(&s, Tag::Viewport));

        // Resize and verify we undid the soft wrap because we have space now
        resize(&mut s, 7, 3, true);
        assert_eq!("1ABC\n2DEF\n3ABC\n4DEF", dump(&s, Tag::Screen));

        // Our cursor should've moved
        assert_eq!(2, s.cursor.x);
        assert_eq!(2, s.cursor.y);
    }

    fn cursor_eql(a: &Cursor, b: &Cursor) -> bool {
        a.x == b.x
            && a.y == b.y
            && a.cursor_style == b.cursor_style
            && a.pending_wrap == b.pending_wrap
            && a.protected == b.protected
            && a.style == b.style
            && a.style_id == b.style_id
            && a.hyperlink_id == b.hyperlink_id
            && a.hyperlink_implicit_id == b.hyperlink_implicit_id
            && a.hyperlink == b.hyperlink
            && a.semantic_content == b.semantic_content
            && a.semantic_content_clear_eol == b.semantic_content_clear_eol
            && a.page_pin == b.page_pin
            && a.page_row == b.page_row
            && a.page_cell == b.page_cell
    }

    #[test]
    fn screen_resize_errors_preserve_state() {
        for tag in [ResizeFailPoint::SavedCursorPin, ResizeFailPoint::Pages] {
            let tw = &RESIZE_TW;

            let mut s = init(10, 3, Some(0));

            s.cursor_set_semantic_content(SemanticContentUpdate::Prompt(osc_sp::PromptKind::Initial));
            s.test_write_string("> ").unwrap();
            s.cursor_set_semantic_content(SemanticContentUpdate::Input(InputClear::ClearExplicit));
            s.test_write_string("echo").unwrap();
            s.set_attribute(bold()).unwrap();
            s.start_hyperlink(b"https://example.com", Some(b"resize")).unwrap();
            s.saved_cursor = Some(SavedCursor {
                x: 1,
                y: 0,
                style: s.cursor.style,
                protected: s.cursor.protected,
                pending_wrap: s.cursor.pending_wrap,
                origin: false,
                charset: s.charset,
            });

            // Keep a shallow copy for all non-page state and a byte-for-byte
            // copy of the sole page so reference counts and prompt contents are
            // covered as well.
            assert_eq!(s.pages.pages.first, s.pages.pages.last);
            let before_cursor = s.cursor.clone();
            let before_hyperlink_ptr = s.cursor.hyperlink.as_ref().map(|h| &**h as *const Hyperlink<'static>);
            let before_saved_cursor = s.saved_cursor;
            let before_selection = s.selection.map(|v| (v.bounds, v.rectangle));
            let before_charset = s.charset;
            let before_protected_mode = s.protected_mode;
            let before_kitty_keyboard = s.kitty_keyboard;
            let before_semantic_prompt = s.semantic_prompt;
            let before_dirty = s.dirty;
            let before_first = s.pages.pages.first;
            let before_last = s.pages.pages.last;
            let before_cols = s.pages.cols;
            let before_rows = s.pages.rows;
            let before_total_rows = s.pages.total_rows;
            let before_viewport = s.pages.viewport;
            let before_viewport_pin = *tp(s.pages.viewport_pin);
            let before_tracked_pins = s.pages.count_tracked_pins();
            let before_page: Vec<u8> = unsafe { pg(first(&s)).memory.as_slice() }.to_vec();
            let before_kitty_dirty = s.kitty_images.dirty;

            tw.error_always(tag, AllocError::OutOfMemory);
            assert_eq!(
                Err(AllocError::OutOfMemory),
                s.resize(Resize {
                    cols: 20,
                    rows: 4,
                    prompt_redraw: osc_sp::Redraw::True,
                    ..Default::default()
                })
            );

            assert!(cursor_eql(&before_cursor, &s.cursor));
            assert_eq!(
                before_hyperlink_ptr,
                s.cursor.hyperlink.as_ref().map(|h| &**h as *const Hyperlink<'static>)
            );
            assert_eq!(before_saved_cursor, s.saved_cursor);
            assert_eq!(before_selection, s.selection.map(|v| (v.bounds, v.rectangle)));
            assert_eq!(before_charset, s.charset);
            assert_eq!(before_protected_mode, s.protected_mode);
            assert_eq!(before_kitty_keyboard, s.kitty_keyboard);
            assert_eq!(before_semantic_prompt, s.semantic_prompt);
            assert_eq!(before_dirty, s.dirty);
            assert_eq!(before_first, s.pages.pages.first);
            assert_eq!(before_last, s.pages.pages.last);
            assert_eq!(before_cols, s.pages.cols);
            assert_eq!(before_rows, s.pages.rows);
            assert_eq!(before_total_rows, s.pages.total_rows);
            assert_eq!(before_viewport, s.pages.viewport);
            assert_eq!(before_viewport_pin, *tp(s.pages.viewport_pin));
            assert_eq!(before_tracked_pins, s.pages.count_tracked_pins());
            assert!(before_page.as_slice() == unsafe { pg(first(&s)).memory.as_slice() });
            if build_options::KITTY_GRAPHICS {
                assert_eq!(before_kitty_dirty, s.kitty_images.dirty);
            }

            tw.end(tripwire::ResetMode::Reset).unwrap();
        }
    }

    #[test]
    fn screen_resize_cursor_references_when_node_survives() {
        let mut s = init(5, 3, Some(1000));

        s.set_attribute(bold()).unwrap();
        s.start_hyperlink(b"https://example.com/", Some(b"resize")).unwrap();
        s.test_write_string("abc").unwrap();

        let original_node = cp(&s).node;
        let original_serial = nd(original_node).serial;
        unsafe {
            let page = pg(original_node);
            assert_eq!(4, page.styles.ref_count(page.memory, s.cursor.style_id));
            assert_eq!(4, page.hyperlink_set.ref_count(page.memory, s.cursor.hyperlink_id));
        }

        // A row-only resize grows the existing page without replacing the
        // cursor's node. The temporary resize references must be released from
        // this page after the cursor state is restored.
        resize(&mut s, 5, 4, true);

        assert_eq!(original_node, cp(&s).node);
        assert_eq!(original_serial, nd(cp(&s).node).serial);
        unsafe {
            let page = cpage(&s);
            assert_eq!(4, page.styles.ref_count(page.memory, s.cursor.style_id));
            assert_eq!(4, page.hyperlink_set.ref_count(page.memory, s.cursor.hyperlink_id));
        }
    }

    #[test]
    fn screen_resize_cursor_references_when_node_is_replaced() {
        let mut s = init(5, 3, Some(1000));

        s.set_attribute(bold()).unwrap();
        s.start_hyperlink(b"https://example.com/", Some(b"resize")).unwrap();
        s.test_write_string("abc").unwrap();

        let original_node = cp(&s).node;
        let original_serial = nd(original_node).serial;
        unsafe {
            let page = pg(original_node);
            assert_eq!(4, page.styles.ref_count(page.memory, s.cursor.style_id));
            assert_eq!(4, page.hyperlink_set.ref_count(page.memory, s.cursor.hyperlink_id));
        }

        // A column resize with reflow replaces the page and remaps the tracked
        // cursor pin. The old page owns the temporary references, so destroying
        // it must account for them without attempting to release them afterward.
        resize(&mut s, 10, 3, true);

        assert!(cp(&s).node != original_node || nd(cp(&s).node).serial != original_serial);
        unsafe {
            let page = cpage(&s);
            assert_eq!(4, page.styles.ref_count(page.memory, s.cursor.style_id));
            assert_eq!(4, page.hyperlink_set.ref_count(page.memory, s.cursor.hyperlink_id));
        }
    }

    #[test]
    fn screen_resize_more_rows_and_cols_with_wrapping() {
        let mut s = init(2, 4, Some(0));
        let str_ = "1A2B\n3C4D";
        s.test_write_string(str_).unwrap();
        assert_eq!("1A\n2B\n3C\n4D", dump(&s, Tag::Viewport));

        resize(&mut s, 5, 10, true);

        // Reflow leaves room after the last printed cell.
        assert!(!s.cursor.pending_wrap);
        assert_eq!(4, s.cursor.x);
        assert_eq!(1, s.cursor.y);

        assert_eq!(str_, dump(&s, Tag::Viewport));
        assert_eq!(str_, dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_resize_less_rows_no_scrollback() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();

        s.cursor_absolute(0, 0);
        s.cursor.pending_wrap = false;
        let (cx, cy) = (s.cursor.x, s.cursor.y);
        resize(&mut s, 5, 1, true);

        // Cursor should not move
        assert_eq!(cx, s.cursor.x);
        assert_eq!(cy, s.cursor.y);

        assert_eq!("3IJKL", dump(&s, Tag::Viewport));
        assert_eq!("3IJKL", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_resize_less_rows_moving_cursor() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();

        // Put our cursor on the last line
        s.cursor_absolute(1, 2);
        s.cursor.pending_wrap = false;
        assert_eq!('I' as u32, cursor_cp(&s));

        // Resize
        resize(&mut s, 5, 1, true);

        assert_eq!("3IJKL", dump(&s, Tag::Viewport));
        assert_eq!("3IJKL", dump(&s, Tag::Screen));

        // Cursor should be on the last line
        assert_eq!(1, s.cursor.x);
        assert_eq!(0, s.cursor.y);
    }

    #[test]
    fn screen_resize_less_rows_with_empty_scrollback() {
        let mut s = init(5, 3, Some(10));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();
        resize(&mut s, 5, 1, true);

        assert_eq!(str_, dump(&s, Tag::Screen));
        assert_eq!("3IJKL", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_less_rows_with_populated_scrollback() {
        let mut s = init(5, 3, Some(5));
        let str_ = "1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH";
        s.test_write_string(str_).unwrap();
        assert_eq!("3IJKL\n4ABCD\n5EFGH", dump(&s, Tag::Viewport));

        // Resize
        resize(&mut s, 5, 1, true);

        assert_eq!(str_, dump(&s, Tag::Screen));
        assert_eq!("5EFGH", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_less_rows_with_full_scrollback() {
        let mut s = init(5, 3, Some(3));
        let str_ = "00000\n1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH";
        s.test_write_string(str_).unwrap();
        assert_eq!("3IJKL\n4ABCD\n5EFGH", dump(&s, Tag::Viewport));

        assert_eq!(4, s.cursor.x);
        assert_eq!(2, s.cursor.y);

        // Resize
        resize(&mut s, 5, 2, true);

        // Cursor should stay in the same relative place (bottom of the
        // screen, same character).
        assert_eq!(4, s.cursor.x);
        assert_eq!(1, s.cursor.y);

        assert_eq!("00000\n1ABCD\n2EFGH\n3IJKL\n4ABCD\n5EFGH", dump(&s, Tag::Screen));
        assert_eq!("4ABCD\n5EFGH", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_less_cols_no_reflow() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1AB\n2EF\n3IJ";
        s.test_write_string(str_).unwrap();

        s.cursor_absolute(0, 0);
        let (cx, cy) = (s.cursor.x, s.cursor.y);
        resize(&mut s, 3, 3, true);

        // Cursor should not move
        assert_eq!(cx, s.cursor.x);
        assert_eq!(cy, s.cursor.y);

        assert_eq!(str_, dump(&s, Tag::Viewport));
        assert_eq!(str_, dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_resize_less_cols_with_reflow_but_row_space() {
        let mut s = init(5, 3, Some(1));
        let str_ = "1ABCD";
        s.test_write_string(str_).unwrap();

        // Put our cursor on the end
        s.cursor_absolute(4, 0);
        s.cursor.pending_wrap = false;
        assert_eq!('D' as u32, cursor_cp(&s));

        resize(&mut s, 3, 3, true);
        assert_eq!("1AB\nCD", dump(&s, Tag::Viewport));
        assert_eq!("1AB\nCD", dump(&s, Tag::Screen));

        // Cursor should be on the last line
        assert_eq!(1, s.cursor.x);
        assert_eq!(1, s.cursor.y);
    }

    #[test]
    fn screen_resize_less_cols_with_reflow_with_trimmed_rows() {
        let mut s = init(5, 3, Some(0));
        let str_ = "3IJKL\n4ABCD\n5EFGH";
        s.test_write_string(str_).unwrap();
        resize(&mut s, 3, 3, true);

        assert_eq!("CD\n5EF\nGH", dump(&s, Tag::Viewport));
        assert_eq!("CD\n5EF\nGH", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_resize_less_cols_with_reflow_with_trimmed_rows_and_scrollback() {
        let mut s = init(5, 3, Some(1));
        let str_ = "3IJKL\n4ABCD\n5EFGH";
        s.test_write_string(str_).unwrap();
        resize(&mut s, 3, 3, true);

        assert_eq!("CD\n5EF\nGH", dump(&s, Tag::Viewport));
        assert_eq!("3IJ\nKL\n4AB\nCD\n5EF\nGH", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_resize_less_cols_with_reflow_previously_wrapped() {
        let mut s = init(5, 3, Some(0));
        let str_ = "3IJKL4ABCD5EFGH";
        s.test_write_string(str_).unwrap();

        // Check
        assert_eq!("3IJKL\n4ABCD\n5EFGH", dump(&s, Tag::Screen));

        resize(&mut s, 3, 3, true);

        // {
        //     const contents = try s.testString(alloc, .viewport);
        //     defer alloc.free(contents);
        //     const expected = "CD\n5EF\nGH";
        //     try testing.expectEqualStrings(expected, contents);
        // }
        assert_eq!("ABC\nD5E\nFGH", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_resize_less_cols_with_reflow_and_scrollback() {
        let mut s = init(5, 3, Some(5));
        let str_ = "1A\n2B\n3C\n4D\n5E";
        s.test_write_string(str_).unwrap();

        // Put our cursor on the end
        let rows = s.pages.rows;
        s.cursor_absolute(1, rows - 1);
        assert_eq!('E' as u32, cursor_cp(&s));

        resize(&mut s, 3, 3, true);

        assert_eq!("3C\n4D\n5E", dump(&s, Tag::Viewport));

        // Cursor should be on the last line
        assert_eq!(1, s.cursor.x);
        assert_eq!(2, s.cursor.y);
    }

    #[test]
    fn screen_resize_less_cols_with_reflow_previously_wrapped_and_scrollback() {
        let mut s = init(5, 3, Some(2));
        let str_ = "1ABCD2EFGH3IJKL4ABCD5EFGH";
        s.test_write_string(str_).unwrap();

        // Check
        assert_eq!("3IJKL\n4ABCD\n5EFGH", dump(&s, Tag::Viewport));

        // Put our cursor on the end
        let (cols, rows) = (s.pages.cols, s.pages.rows);
        s.cursor_absolute(cols - 1, rows - 1);
        s.cursor.pending_wrap = false;
        assert_eq!('H' as u32, cursor_cp(&s));

        resize(&mut s, 3, 3, true);

        assert_eq!("CD5\nEFG\nH", dump(&s, Tag::Viewport));
        assert_eq!("1AB\nCD2\nEFG\nH3I\nJKL\n4AB\nCD5\nEFG\nH", dump(&s, Tag::Screen));

        // Cursor should be on the last line
        assert_eq!(0, s.cursor.x);
        assert_eq!(2, s.cursor.y);
        assert_eq!('H' as u32, cursor_cp(&s));
    }

    #[test]
    fn screen_resize_less_cols_with_scrollback_keeps_cursor_row() {
        let mut s = init(5, 3, Some(5));
        let str_ = "1A\n2B\n3C\n4D\n5E";
        s.test_write_string(str_).unwrap();

        // Lets do a scroll and clear operation
        s.scroll_clear().unwrap();

        // Move our cursor to the beginning
        s.cursor_absolute(0, 0);

        resize(&mut s, 3, 3, true);

        assert_eq!("", dump(&s, Tag::Viewport));

        // Cursor should be on the last line
        assert_eq!(0, s.cursor.x);
        assert_eq!(0, s.cursor.y);
    }

    #[test]
    fn screen_resize_more_rows_less_cols_with_reflow_with_scrollback() {
        let mut s = init(5, 3, Some(3));
        let str_ = "1ABCD\n2EFGH3IJKL\n4MNOP";
        s.test_write_string(str_).unwrap();

        assert_eq!("1ABCD\n2EFGH\n3IJKL\n4MNOP", dump(&s, Tag::Screen));
        assert_eq!("2EFGH\n3IJKL\n4MNOP", dump(&s, Tag::Viewport));

        resize(&mut s, 2, 10, true);

        assert_eq!("BC\nD\n2E\nFG\nH3\nIJ\nKL\n4M\nNO\nP", dump(&s, Tag::Viewport));
        assert_eq!("1A\nBC\nD\n2E\nFG\nH3\nIJ\nKL\n4M\nNO\nP", dump(&s, Tag::Screen));
    }

    // This seems like it should work fine but for some reason in practice
    // in the initial implementation I found this bug! This is a regression
    // test for that.
    #[test]
    fn screen_resize_more_rows_then_shrink_again() {
        let mut s = init(5, 3, Some(10));
        let str_ = "1ABC";
        s.test_write_string(str_).unwrap();

        // Grow
        resize(&mut s, 5, 10, true);
        assert_eq!(str_, dump(&s, Tag::Viewport));
        assert_eq!(str_, dump(&s, Tag::Screen));

        // Shrink
        resize(&mut s, 5, 3, true);
        assert_eq!(str_, dump(&s, Tag::Screen));
        assert_eq!(str_, dump(&s, Tag::Viewport));

        // Grow again
        resize(&mut s, 5, 10, true);
        assert_eq!(str_, dump(&s, Tag::Viewport));
        assert_eq!(str_, dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_resize_less_cols_to_eliminate_wide_char() {
        let mut s = init(2, 1, Some(0));
        let str_ = "😀";
        s.test_write_string(str_).unwrap();
        assert_eq!(str_, dump(&s, Tag::Screen));
        {
            let cell = get_cell(&s, scr(0, 0));
            assert_eq!(Wide::Wide, cell.wide());
            assert_eq!('😀' as u32, cell.content_codepoint());
        }

        // Resize to 1 column can't fit a wide char. So it should be deleted.
        resize(&mut s, 1, 1, true);
        assert_eq!("", dump(&s, Tag::Screen));
        {
            let cell = get_cell(&s, scr(0, 0));
            assert_eq!(0, cell.content_codepoint());
            assert_eq!(Wide::Narrow, cell.wide());
        }
    }

    #[test]
    fn screen_resize_less_cols_to_wrap_wide_char() {
        let mut s = init(3, 3, Some(0));
        let str_ = "x😀";
        s.test_write_string(str_).unwrap();
        assert_eq!(str_, dump(&s, Tag::Screen));
        {
            let cell = get_cell(&s, scr(1, 0));
            assert_eq!(Wide::Wide, cell.wide());
            assert_eq!('😀' as u32, cell.content_codepoint());
        }
        assert_eq!(Wide::SpacerTail, get_cell(&s, scr(2, 0)).wide());

        resize(&mut s, 2, 3, true);
        assert_eq!("x\n😀", dump(&s, Tag::Screen));
        {
            assert_eq!(Wide::SpacerHead, get_cell(&s, scr(1, 0)).wide());
            assert!(get_row(&s, scr(1, 0)).wrap());
        }
    }

    #[test]
    fn screen_resize_less_cols_to_eliminate_wide_char_with_row_space() {
        let mut s = init(2, 2, Some(0));
        let str_ = "😀";
        s.test_write_string(str_).unwrap();
        assert_eq!(str_, dump(&s, Tag::Screen));
        {
            let cell = get_cell(&s, scr(0, 0));
            assert_eq!(Wide::Wide, cell.wide());
            assert_eq!('😀' as u32, cell.content_codepoint());
        }
        assert_eq!(Wide::SpacerTail, get_cell(&s, scr(1, 0)).wide());

        resize(&mut s, 1, 2, true);
        assert_eq!("", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_resize_less_cols_reflows_cursor_after_wrapped_text() {
        let mut s = init(50, 7, Some(0));

        for _ in 0..30 {
            s.test_write_string("a").unwrap();
        }

        assert_eq!(0, s.cursor.y);
        assert_eq!(30, s.cursor.x);

        resize(&mut s, 25, 7, true);

        assert_eq!(1, s.cursor.y);
        assert_eq!(5, s.cursor.x);
    }

    #[test]
    fn screen_resize_less_cols_reflows_cursor_after_empty_cells() {
        let mut s = init(10, 3, Some(0));

        s.test_write_string("abc").unwrap();
        s.cursor_right(6);

        assert_eq!(0, s.cursor.y);
        assert_eq!(9, s.cursor.x);

        resize(&mut s, 5, 3, true);

        assert_eq!(1, s.cursor.y);
        assert_eq!(4, s.cursor.x);
    }

    #[test]
    fn screen_resize_more_cols_with_wide_spacer_head() {
        let mut s = init(3, 2, Some(0));
        let str_ = "  😀";
        s.test_write_string(str_).unwrap();
        assert_eq!("  \n😀", dump(&s, Tag::Screen));

        // So this is the key point: we end up with a wide spacer head at
        // the end of row 1, then the emoji, then a wide spacer tail on row 2.
        // We should expect that if we resize to more cols, the wide spacer
        // head is replaced with the emoji.
        assert_eq!(Wide::SpacerHead, get_cell(&s, scr(2, 0)).wide());
        assert_eq!(Wide::Wide, get_cell(&s, scr(0, 1)).wide());
        assert_eq!(Wide::SpacerTail, get_cell(&s, scr(1, 1)).wide());

        resize(&mut s, 4, 2, true);
        assert_eq!(str_, dump(&s, Tag::Screen));
        {
            let cell = get_cell(&s, scr(2, 0));
            assert_eq!(Wide::Wide, cell.wide());
            assert_eq!('😀' as u32, cell.content_codepoint());
        }
        assert_eq!(Wide::SpacerTail, get_cell(&s, scr(3, 0)).wide());
    }

    #[test]
    fn screen_resize_more_cols_with_wide_spacer_head_multiple_lines() {
        let mut s = init(3, 3, Some(0));
        let str_ = "xxxyy😀";
        s.test_write_string(str_).unwrap();
        assert_eq!("xxx\nyy\n😀", dump(&s, Tag::Screen));

        // Similar to the "wide spacer head" test, but this time we'er going
        // to increase our columns such that multiple rows are unwrapped.
        assert_eq!(Wide::SpacerHead, get_cell(&s, scr(2, 1)).wide());
        assert_eq!(Wide::Wide, get_cell(&s, scr(0, 2)).wide());
        assert_eq!(Wide::SpacerTail, get_cell(&s, scr(1, 2)).wide());

        resize(&mut s, 8, 2, true);
        assert_eq!(str_, dump(&s, Tag::Screen));
        {
            let cell = get_cell(&s, scr(5, 0));
            assert_eq!(Wide::Wide, cell.wide());
            assert_eq!('😀' as u32, cell.content_codepoint());
        }
        assert_eq!(Wide::SpacerTail, get_cell(&s, scr(6, 0)).wide());
    }

    #[test]
    fn screen_resize_more_cols_requiring_a_wide_spacer_head() {
        let mut s = init(2, 2, Some(0));
        let str_ = "xx😀";
        s.test_write_string(str_).unwrap();
        assert_eq!("xx\n😀", dump(&s, Tag::Screen));
        assert_eq!(Wide::Wide, get_cell(&s, scr(0, 1)).wide());
        assert_eq!(Wide::SpacerTail, get_cell(&s, scr(1, 1)).wide());

        // This resizes to 3 columns, which isn't enough space for our wide
        // char to enter row 1. But we need to mark the wide spacer head on the
        // end of the first row since we're wrapping to the next row.
        resize(&mut s, 3, 2, true);
        assert_eq!("xx\n😀", dump(&s, Tag::Screen));
        assert_eq!(Wide::SpacerHead, get_cell(&s, scr(2, 0)).wide());
        {
            let cell = get_cell(&s, scr(0, 1));
            assert_eq!(Wide::Wide, cell.wide());
            assert_eq!('😀' as u32, cell.content_codepoint());
        }
        assert_eq!(Wide::SpacerTail, get_cell(&s, scr(1, 1)).wide());
    }

    fn resize_redraw(s: &mut Screen, cols: CellCountInt, rows: CellCountInt, redraw: osc_sp::Redraw) {
        s.resize(Resize {
            cols,
            rows,
            prompt_redraw: redraw,
            ..Default::default()
        })
        .unwrap();
    }

    fn prompt(kind: osc_sp::PromptKind) -> SemanticContentUpdate {
        SemanticContentUpdate::Prompt(kind)
    }

    #[test]
    fn screen_resize_more_cols_with_cursor_at_prompt() {
        let mut s = init(10, 3, Some(5));

        s.test_write_string("ABCDE\n").unwrap();
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("> ").unwrap();
        s.cursor_set_semantic_content(SemanticContentUpdate::Input(InputClear::ClearEol));
        s.test_write_string("echo").unwrap();

        assert_eq!("ABCDE\n> echo", dump(&s, Tag::Viewport));

        resize_redraw(&mut s, 20, 3, osc_sp::Redraw::True);

        // Cursor should not move
        assert_eq!(6, s.cursor.x);
        assert_eq!(1, s.cursor.y);

        assert_eq!("ABCDE", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_more_cols_with_cursor_not_at_prompt() {
        let mut s = init(10, 3, Some(5));

        s.test_write_string("ABCDE\n").unwrap();
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("> ").unwrap();
        s.cursor_set_semantic_content(SemanticContentUpdate::Input(InputClear::ClearEol));
        s.test_write_string("echo\n").unwrap();
        s.test_write_string("output").unwrap();

        assert_eq!("ABCDE\n> echo\noutput", dump(&s, Tag::Viewport));

        resize_redraw(&mut s, 20, 3, osc_sp::Redraw::True);

        // Cursor should not move
        assert_eq!(6, s.cursor.x);
        assert_eq!(2, s.cursor.y);

        assert_eq!("ABCDE\n> echo\noutput", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_with_prompt_redraw_last_clears_only_one_line() {
        let mut s = init(10, 4, Some(5));

        s.test_write_string("ABCDE\n").unwrap();
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("> ").unwrap();
        s.cursor_set_semantic_content(SemanticContentUpdate::Input(InputClear::ClearExplicit));
        s.test_write_string("hello\n").unwrap();
        s.test_write_string("world").unwrap();

        assert_eq!("ABCDE\n> hello\nworld", dump(&s, Tag::Viewport));

        // Cursor is at end of "world" line with semantic_content = .input
        resize_redraw(&mut s, 20, 4, osc_sp::Redraw::Last);

        // With .last, only the current line where cursor is should be cleared
        assert_eq!("ABCDE\n> hello", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_resize_with_prompt_redraw_last_multiline_prompt_clears_only_last_line() {
        let mut s = init(20, 5, Some(5));

        // Create a 3-line prompt: 1 initial + 2 continuation lines
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("line1\n").unwrap();
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Continuation));
        s.test_write_string("line2\n").unwrap();
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Continuation));
        s.test_write_string("line3").unwrap();

        assert_eq!("line1\nline2\nline3", dump(&s, Tag::Viewport));

        // Cursor is at end of line3 (the last continuation line)
        resize_redraw(&mut s, 30, 5, osc_sp::Redraw::Last);

        // With .last, only line3 (where cursor is) should be cleared
        assert_eq!("line1\nline2", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_select_untracked() {
        let mut s = init(10, 10, Some(0));
        s.test_write_string("ABC  DEF\n 123\n456").unwrap();

        assert!(s.selection.is_none());
        let tracked = s.pages.count_tracked_pins();
        let sel = sel_init(&s, act(0, 0), act(3, 0), false);
        s.select(Some(sel)).unwrap();
        assert_eq!(tracked + 2, s.pages.count_tracked_pins());
        s.select(None).unwrap();
        assert_eq!(tracked, s.pages.count_tracked_pins());
    }

    #[test]
    fn screen_select_replaces_existing_pins() {
        let mut s = init(10, 10, Some(0));
        s.test_write_string("ABC  DEF\n 123\n456").unwrap();

        let tracked = s.pages.count_tracked_pins();
        let sel = sel_init(&s, act(0, 0), act(3, 0), false);
        s.select(Some(sel)).unwrap();
        assert_eq!(tracked + 2, s.pages.count_tracked_pins());

        // Replacing the selection must untrack the prior selection's pins
        // rather than leak them.
        let sel = sel_init(&s, act(0, 1), act(2, 1), false);
        s.select(Some(sel)).unwrap();
        assert_eq!(tracked + 2, s.pages.count_tracked_pins());
    }

    #[test]
    fn screen_reselecting_tracked_selection_preserves_its_pins() {
        let mut s = init(10, 2, Some(0));

        let sel = sel_init(&s, act(1, 0), act(3, 0), false);
        s.select(Some(sel)).unwrap();

        let cur = s.selection;
        s.select(cur).unwrap();
        assert_eq!(Order::Forward, s.selection.unwrap().order(&s));
    }

    /// Assert the start/end of a selection as points of the given tag.
    fn assert_sel(s: &Screen, sel: &Selection, tag: Tag, start: (u16, u32), end: (u16, u32)) {
        let mk = |c: (u16, u32)| match tag {
            Tag::Active => act(c.0, c.1),
            Tag::Viewport => vp(c.0, c.1),
            Tag::Screen => scr(c.0, c.1),
            Tag::History => hist(c.0, c.1),
        };
        assert_eq!(mk(start), pfp(s, tag, sel.start()).unwrap(), "start");
        assert_eq!(mk(end), pfp(s, tag, sel.end()).unwrap(), "end");
    }

    fn select_line_at(s: &Screen, pt: Point) -> Option<Selection> {
        s.select_line(SelectLine::new(pin(s, pt)))
    }

    #[test]
    fn screen_select_all() {
        let mut s = init(10, 10, Some(0));

        {
            s.test_write_string("ABC  DEF\n 123\n456").unwrap();
            let sel = s.select_all().unwrap();
            assert_sel(&s, &sel, Tag::Screen, (0, 0), (2, 2));
            sel.deinit(&mut s);
        }

        {
            s.test_write_string("\nFOO\n BAR\n BAZ\n QWERTY\n 12345678").unwrap();
            let sel = s.select_all().unwrap();
            assert_sel(&s, &sel, Tag::Screen, (0, 0), (8, 7));
            sel.deinit(&mut s);
        }
    }

    #[test]
    fn screen_select_line() {
        let mut s = init(10, 10, Some(0));
        s.test_write_string("ABC  DEF\n 123\n456").unwrap();

        // Outside of active area
        // try testing.expect(s.selectLine(.{ .x = 13, .y = 0 }) == null);
        // try testing.expect(s.selectLine(.{ .x = 0, .y = 5 }) == null);

        // Going forward
        let sel = select_line_at(&s, act(0, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (0, 0), (7, 0));

        // Going backward
        let sel = select_line_at(&s, act(7, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (0, 0), (7, 0));

        // Going forward and backward
        let sel = select_line_at(&s, act(3, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (0, 0), (7, 0));

        // Outside active area
        let sel = select_line_at(&s, act(9, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (0, 0), (7, 0));
    }

    #[test]
    fn screen_select_line_across_soft_wrap() {
        let mut s = init(5, 10, Some(0));
        s.test_write_string(" 12 34012   \n 123").unwrap();

        // Going forward
        let sel = select_line_at(&s, act(1, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (1, 0), (3, 1));
    }

    #[test]
    fn screen_select_line_across_full_soft_wrap() {
        let mut s = init(5, 5, Some(0));
        s.test_write_string("1ABCD2EFGH\n3IJKL").unwrap();

        let sel = select_line_at(&s, act(2, 1)).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (0, 0), (4, 1));
    }

    #[test]
    fn screen_select_line_across_soft_wrap_ignores_blank_lines() {
        let mut s = init(5, 10, Some(0));
        s.test_write_string(" 12 34012             \n 123").unwrap();

        // Going forward
        let sel = select_line_at(&s, act(1, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (1, 0), (3, 1));

        // Going backward
        let sel = select_line_at(&s, act(1, 1)).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (1, 0), (3, 1));

        // Going forward and backward
        let sel = select_line_at(&s, act(3, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (1, 0), (3, 1));
    }

    #[test]
    fn screen_select_line_disabled_whitespace_trimming() {
        let mut s = init(5, 10, Some(0));
        s.test_write_string(" 12 34012   \n 123").unwrap();

        // Going forward
        let sel = s
            .select_line(SelectLine {
                whitespace: None,
                ..SelectLine::new(pin(&s, act(1, 0)))
            })
            .unwrap();
        assert_sel(&s, &sel, Tag::Screen, (0, 0), (4, 2));

        // Non-wrapped
        let sel = s
            .select_line(SelectLine {
                whitespace: None,
                ..SelectLine::new(pin(&s, act(1, 3)))
            })
            .unwrap();
        assert_sel(&s, &sel, Tag::Screen, (0, 3), (4, 3));
    }

    #[test]
    fn screen_select_line_with_scrollback() {
        let mut s = init(2, 3, Some(5));
        s.test_write_string("1A\n2B\n3C\n4D\n5E").unwrap();

        // Selecting first line
        let sel = select_line_at(&s, act(0, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Active, (0, 0), (1, 0));

        // Selecting last line
        let sel = select_line_at(&s, act(0, 2)).unwrap();
        assert_sel(&s, &sel, Tag::Active, (0, 2), (1, 2));
    }

    // https://github.com/mitchellh/ghostty/issues/1329
    #[test]
    fn screen_select_line_semantic_prompt_boundary() {
        let mut s = init(5, 10, Some(0));
        s.test_write_string("ABCDE\n").unwrap();
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("A    ").unwrap();
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("> ").unwrap();

        assert_eq!("ABCDE\nA    \n> ", dump(&s, Tag::Screen));

        // Selecting output stops at the prompt even if soft-wrapped
        {
            let sel = select_line_at(&s, act(1, 1)).unwrap();
            let contents = s.selection_string(SelectionString { sel, trim: false });
            assert_eq!("A", contents);
        }
        {
            let sel = select_line_at(&s, act(1, 2)).unwrap();
            assert_sel(&s, &sel, Tag::Active, (0, 2), (0, 2));
        }
    }

    #[test]
    fn screen_select_line_semantic_boundary_around_unwritten_cells() {
        let mut s = init(5, 1, Some(0));

        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("x").unwrap();
        s.cursor_right(2);
        s.test_write_string("m").unwrap();

        for x in 1..3 {
            let p = pin(&s, act(x, 0));
            assert!(s.select_line(SelectLine::new(p)).is_none());
        }
    }

    fn input_explicit() -> SemanticContentUpdate {
        SemanticContentUpdate::Input(InputClear::ClearExplicit)
    }

    fn sel_str(s: &Screen, sel: Selection) -> String {
        s.selection_string(SelectionString { sel, trim: false })
    }

    #[test]
    fn screen_select_line_semantic_prompt_to_input_boundary() {
        let mut s = init(10, 5, Some(0));

        // Write prompt followed by user input on same row: "$>command"
        // Using non-whitespace to avoid whitespace trimming affecting the test
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("$>").unwrap();
        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string("command").unwrap();

        // Selecting from prompt should only select prompt
        let sel = select_line_at(&s, act(0, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Active, (0, 0), (1, 0));

        // Selecting from input should only select input
        let sel = select_line_at(&s, act(5, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Active, (2, 0), (8, 0));
    }

    #[test]
    fn screen_select_line_semantic_input_to_output_boundary() {
        let mut s = init(10, 5, Some(0));

        // Row 0: user input
        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string("ls -la\n").unwrap();
        // Row 1: command output
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("file.txt").unwrap();

        // Selecting from input should only select input
        let sel = select_line_at(&s, act(2, 0)).unwrap();
        assert_eq!("ls -la", sel_str(&s, sel));

        // Selecting from output should only select output
        let sel = select_line_at(&s, act(2, 1)).unwrap();
        assert_eq!("file.txt", sel_str(&s, sel));
    }

    #[test]
    fn screen_select_line_semantic_mid_row_boundary() {
        let mut s = init(10, 5, Some(0));

        // Single row with output then prompt then input: "out$>cmd"
        // Using non-whitespace to avoid whitespace trimming affecting the test
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("out").unwrap();
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("$>").unwrap();
        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string("cmd").unwrap();

        // Selecting from output should stop at prompt
        let sel = select_line_at(&s, act(1, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Active, (0, 0), (2, 0));

        // Selecting from prompt should only select prompt
        let sel = select_line_at(&s, act(3, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Active, (3, 0), (4, 0));

        // Selecting from input should only select input
        let sel = select_line_at(&s, act(6, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Active, (5, 0), (7, 0));
    }

    #[test]
    fn screen_select_line_semantic_boundary_soft_wrap_with_mid_row_transition() {
        let mut s = init(5, 5, Some(0));

        // Row 0: prompt "$ " + input "cmd" (soft-wraps)
        // Row 1: input continues "12" + output "out"
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("$ ").unwrap();
        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string("cmd12").unwrap();
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("out").unwrap();

        // Verify layout
        assert_eq!("$ cmd\n12out", dump(&s, Tag::Screen));

        // Selecting from input on row 0 should get all input across soft-wrap
        let sel = select_line_at(&s, act(3, 0)).unwrap();
        assert_eq!("cmd12", sel_str(&s, sel));

        // Selecting from input on row 1 should get all input across soft-wrap
        let sel = select_line_at(&s, act(0, 1)).unwrap();
        assert_eq!("cmd12", sel_str(&s, sel));

        // Selecting from output should only get output
        let sel = select_line_at(&s, act(3, 1)).unwrap();
        assert_eq!("out", sel_str(&s, sel));
    }

    #[test]
    fn screen_select_line_semantic_boundary_disabled() {
        let mut s = init(10, 5, Some(0));

        // Write prompt followed by input
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("$ ").unwrap();
        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string("command").unwrap();

        // With semantic_prompt_boundary = false, should select entire line
        let sel = s
            .select_line(SelectLine {
                semantic_prompt_boundary: false,
                ..SelectLine::new(pin(&s, act(0, 0)))
            })
            .unwrap();
        assert_eq!("$ command", sel_str(&s, sel));
    }

    #[test]
    fn screen_select_line_semantic_boundary_first_cell_of_row() {
        let mut s = init(5, 5, Some(0));

        // Row 0: input that soft-wraps
        // Row 1: output starts at first cell
        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string("12345").unwrap();
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("ABCDE").unwrap();

        // Verify soft-wrap happened
        {
            let p = pin(&s, act(0, 0));
            assert!(rw(p.row_and_cell().row).wrap());
        }

        // Selecting from input should stop before output on row 1
        let sel = select_line_at(&s, act(2, 0)).unwrap();
        assert_sel(&s, &sel, Tag::Active, (0, 0), (4, 0));

        // Selecting from output should only get output
        let sel = select_line_at(&s, act(2, 1)).unwrap();
        assert_sel(&s, &sel, Tag::Active, (0, 1), (4, 1));
    }

    #[test]
    fn screen_select_line_semantic_boundary_across_mixed_width_pages() {
        let mut s = init(4, 2, Some(0));

        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string("ABCD").unwrap();
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("E").unwrap();

        let f = first(&s);
        let mut p = Pin::new(f);
        p.y = 1;
        s.pages.split(p).unwrap();
        let second = nd(f).next.unwrap();
        pg(f).size.cols = 2;
        s.pages.pause_integrity_checks(true);

        assert_eq!(2, nd(f).cols());
        assert_eq!(4, nd(second).cols());

        let mut at = Pin::new(f);
        at.x = 1;
        let sel = s.select_line(SelectLine::new(at)).unwrap();
        let mut e0 = Pin::new(f);
        e0.x = 0;
        assert!(e0.eql(sel.start()));
        let mut e1 = Pin::new(f);
        e1.x = 1;
        assert!(e1.eql(sel.end()));
        s.pages.pause_integrity_checks(false);
    }

    #[test]
    fn screen_select_line_semantic_all_same_content() {
        let mut s = init(5, 5, Some(0));

        // All prompt content that soft-wraps
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("prompt text").unwrap();

        // Verify soft-wrap
        assert_eq!("promp\nt tex\nt", dump(&s, Tag::Screen));

        // Should select all prompt content across soft-wraps
        let sel = select_line_at(&s, act(2, 1)).unwrap();
        assert_eq!("prompt text", sel_str(&s, sel));
    }

    #[test]
    fn screen_select_word() {
        let mut s = init(10, 10, Some(0));
        s.test_write_string("ABC  DEF\n 123\n456").unwrap();

        // Default boundary codepoints for word selection
        let boundary_codepoints: &[u32] = &[
            0, ' ' as u32, '\t' as u32, '\'' as u32, '"' as u32, '│' as u32, '`' as u32, '|' as u32, ':' as u32,
            ';' as u32, ',' as u32, '(' as u32, ')' as u32, '[' as u32, ']' as u32, '{' as u32, '}' as u32,
            '<' as u32, '>' as u32, '$' as u32,
        ];

        // Outside of active area
        // try testing.expect(s.selectWord(.{ .x = 9, .y = 0 }) == null);
        // try testing.expect(s.selectWord(.{ .x = 0, .y = 5 }) == null);

        // Going forward
        let sel = s.select_word(pin(&s, act(0, 0)), boundary_codepoints).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (0, 0), (2, 0));

        // Going backward
        let sel = s.select_word(pin(&s, act(2, 0)), boundary_codepoints).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (0, 0), (2, 0));

        // Going forward and backward
        let sel = s.select_word(pin(&s, act(1, 0)), boundary_codepoints).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (0, 0), (2, 0));

        // Whitespace
        let sel = s.select_word(pin(&s, act(3, 0)), boundary_codepoints).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (3, 0), (4, 0));

        // Whitespace single char
        let sel = s.select_word(pin(&s, act(0, 1)), boundary_codepoints).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (0, 1), (0, 1));

        // End of screen
        let sel = s.select_word(pin(&s, act(1, 2)), boundary_codepoints).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (0, 2), (2, 2));
    }

    const SPACE_BOUNDARY: &[u32] = &[0, ' ' as u32];

    #[test]
    fn screen_select_word_at_hard_line_breaks() {
        let cases: [(CellCountInt, &str); 4] = [(5, "abcde\nfghij"), (5, "     \n     "), (1, "a\nb"), (1, " \n ")];
        for (cols, text) in cases {
            let mut s = init(cols, 2, Some(0));
            s.test_write_string(text).unwrap();

            for y in 0..2u32 {
                for x in 0..cols {
                    let sel = s.select_word(pin(&s, act(x, y)), SPACE_BOUNDARY).unwrap();
                    assert_sel(&s, &sel, Tag::Screen, (0, y), (cols - 1, y));
                }
            }
        }
    }

    #[test]
    fn screen_select_word_across_soft_wrap_at_right_edge() {
        let mut s = init(5, 3, Some(0));
        s.test_write_string("abcdefghij\nklmno").unwrap();

        for y in 0..2u32 {
            for x in 0..5 {
                let sel = s.select_word(pin(&s, act(x, y)), SPACE_BOUNDARY).unwrap();
                assert_sel(&s, &sel, Tag::Screen, (0, 0), (4, 1));
            }
        }
    }

    #[test]
    fn screen_select_word_wide_characters() {
        struct Case {
            text: &'static str,
            cols: CellCountInt,
            boundary_codepoints: &'static [u32],
            start: usize,
            end: usize,
            expected: &'static str,
        }
        const fn case(text: &'static str, end: usize, expected: &'static str) -> Case {
            Case {
                text,
                cols: 10,
                boundary_codepoints: SPACE_BOUNDARY,
                start: 0,
                end,
                expected,
            }
        }
        const YU: &[u32] = &[0, '語' as u32];
        let cases = [
            case("日本語", 5, "日本語"),
            Case {
                boundary_codepoints: &[' ' as u32],
                ..case("日本語", 5, "日本語")
            },
            case("a日b語c", 6, "a日b語c"),
            Case {
                start: 1,
                ..case(" 日本語 ", 6, "日本語")
            },
            Case {
                cols: 4,
                ..case("日本語", 5, "日本語")
            },
            Case {
                cols: 5,
                ..case("日本語", 6, "日本語")
            },
            Case {
                cols: 4,
                ..case("日本\n語文", 3, "日本")
            },
            Case {
                cols: 4,
                start: 4,
                ..case("日本\n語文", 7, "語文")
            },
            Case {
                boundary_codepoints: YU,
                ..case("a語b", 0, "a")
            },
            Case {
                boundary_codepoints: YU,
                start: 1,
                ..case("a語b", 2, "語")
            },
            Case {
                boundary_codepoints: YU,
                start: 3,
                ..case("a語b", 3, "b")
            },
            Case {
                cols: 5,
                boundary_codepoints: YU,
                ..case("abcd語ef", 3, "abcd")
            },
            Case {
                cols: 5,
                boundary_codepoints: YU,
                start: 4,
                ..case("abcd語ef", 6, "語")
            },
            Case {
                cols: 5,
                boundary_codepoints: YU,
                start: 7,
                ..case("abcd語ef", 8, "ef")
            },
        ];

        for c in cases {
            let mut s = init(c.cols, 4, Some(0));
            s.test_write_string(c.text).unwrap();
            let cols = c.cols as usize;

            // Selecting any cell in the word should select the whole word.
            for offset in c.start..c.end + 1 {
                let p = pin(&s, act((offset % cols) as u16, (offset / cols) as u32));
                let sel = s.select_word(p, c.boundary_codepoints).unwrap();
                assert_sel(
                    &s,
                    &sel,
                    Tag::Screen,
                    ((c.start % cols) as u16, (c.start / cols) as u32),
                    ((c.end % cols) as u16, (c.end / cols) as u32),
                );

                let contents = s.selection_string(SelectionString::new(sel));
                assert_eq!(c.expected, contents);
            }
        }
    }

    /// Default boundary codepoints for word selection.
    const WORD_BOUNDARIES: &[u32] = &[
        0, ' ' as u32, '\t' as u32, '\'' as u32, '"' as u32, '│' as u32, '`' as u32, '|' as u32, ':' as u32,
        ';' as u32, ',' as u32, '(' as u32, ')' as u32, '[' as u32, ']' as u32, '{' as u32, '}' as u32,
        '<' as u32, '>' as u32, '$' as u32,
    ];

    #[test]
    fn screen_select_word_across_soft_wrap() {
        let mut s = init(5, 10, Some(0));
        s.test_write_string(" 1234012\n 123").unwrap();

        assert_eq!(" 1234\n012\n 123", dump(&s, Tag::Screen));

        // Going forward
        let sel = s.select_word(pin(&s, act(1, 0)), WORD_BOUNDARIES).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (1, 0), (2, 1));

        // Going backward
        let sel = s.select_word(pin(&s, act(1, 1)), WORD_BOUNDARIES).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (1, 0), (2, 1));

        // Going forward and backward
        let sel = s.select_word(pin(&s, act(3, 0)), WORD_BOUNDARIES).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (1, 0), (2, 1));
    }

    #[test]
    fn screen_select_word_whitespace_across_soft_wrap() {
        let mut s = init(5, 10, Some(0));
        s.test_write_string("1       1\n 123").unwrap();

        // Going forward
        let sel = s.select_word(pin(&s, act(1, 0)), WORD_BOUNDARIES).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (1, 0), (2, 1));

        // Going backward
        let sel = s.select_word(pin(&s, act(1, 1)), WORD_BOUNDARIES).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (1, 0), (2, 1));

        // Going forward and backward
        let sel = s.select_word(pin(&s, act(3, 0)), WORD_BOUNDARIES).unwrap();
        assert_sel(&s, &sel, Tag::Screen, (1, 0), (2, 1));
    }

    #[test]
    fn screen_select_word_with_character_boundary() {
        let cases = [
            " 'abc' \n123",
            " \"abc\" \n123",
            " │abc│ \n123",
            " `abc` \n123",
            " |abc| \n123",
            " :abc: \n123",
            " ;abc; \n123",
            " ,abc, \n123",
            " (abc( \n123",
            " )abc) \n123",
            " [abc[ \n123",
            " ]abc] \n123",
            " {abc{ \n123",
            " }abc} \n123",
            " <abc< \n123",
            " >abc> \n123",
            " $abc$ \n123",
        ];

        for case in cases {
            let mut s = init(20, 10, Some(0));
            s.test_write_string(case).unwrap();

            // Inside character forward
            let sel = s.select_word(pin(&s, act(2, 0)), WORD_BOUNDARIES).unwrap();
            assert_sel(&s, &sel, Tag::Screen, (2, 0), (4, 0));

            // Inside character backward
            let sel = s.select_word(pin(&s, act(4, 0)), WORD_BOUNDARIES).unwrap();
            assert_sel(&s, &sel, Tag::Screen, (2, 0), (4, 0));

            // Inside character bidirectional
            let sel = s.select_word(pin(&s, act(3, 0)), WORD_BOUNDARIES).unwrap();
            assert_sel(&s, &sel, Tag::Screen, (2, 0), (4, 0));

            // On quote
            // NOTE: this behavior is not ideal, so we can change this one day,
            // but I think its also not that important compared to the above.
            let sel = s.select_word(pin(&s, act(1, 0)), WORD_BOUNDARIES).unwrap();
            assert_sel(&s, &sel, Tag::Screen, (0, 0), (1, 0));
        }
    }

    #[test]
    fn screen_select_output() {
        let mut s = init(10, 15, Some(0));

        // Build content with cell-level semantic content:
        // Row 0-1: output1 (output)
        // Row 2: prompt2 (prompt)
        // Row 3: input2 (input)
        // Row 4-7: output2 (output, with overflow causing wrap)
        // Row 8: "$ " (prompt) + "input3" (input)
        // Row 9-11: output3 (output)
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("output1\n").unwrap();
        s.test_write_string("output1\n").unwrap();
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("prompt2\n").unwrap();
        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string("input2\n").unwrap();
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("output2output2output2output2\n").unwrap();
        s.test_write_string("output2\n").unwrap();
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("$ ").unwrap();
        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string("input3\n").unwrap();
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("output3\n").unwrap();
        s.test_write_string("output3\n").unwrap();
        s.test_write_string("output3").unwrap();

        // First output block (rows 0-1), should select those rows
        {
            let sel = s.select_output(pin(&s, act(1, 1))).unwrap();
            assert_eq!("output1\noutput1", sel_str(&s, sel));
        }
        // Second output block (rows 4-7)
        {
            let sel = s.select_output(pin(&s, act(3, 7))).unwrap();
            assert_eq!("output2output2output2output2\noutput2", sel_str(&s, sel));
        }
        // Third output block (rows 9-11)
        {
            let sel = s.select_output(pin(&s, act(2, 10))).unwrap();
            assert_sel(&s, &sel, Tag::Active, (0, 9), (6, 11));
        }
        // Click on prompt should return null
        assert!(s.select_output(pin(&s, act(1, 8))).is_none());
        // Click on input should return null
        assert!(s.select_output(pin(&s, act(5, 8))).is_none());
    }

    fn sel_string(s: &Screen, a: Point, b: Point, trim: bool) -> String {
        let sel = Selection::init(pin(s, a), pin(s, b), false);
        s.selection_string(SelectionString { sel, trim })
    }

    #[test]
    fn screen_selection_string_basic() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();

        assert_eq!("2EFGH\n3IJ", sel_string(&s, scr(0, 1), scr(2, 2), true));
    }

    #[test]
    fn screen_selection_string_start_outside_of_written_area() {
        let mut s = init(5, 10, Some(0));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();

        assert_eq!("", sel_string(&s, scr(0, 5), scr(2, 6), true));
    }

    #[test]
    fn screen_selection_string_end_outside_of_written_area() {
        let mut s = init(5, 10, Some(0));
        let str_ = "1ABCD\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();

        assert_eq!("3IJKL", sel_string(&s, scr(0, 2), scr(2, 6), true));
    }

    #[test]
    fn screen_selection_string_trim_space() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1AB  \n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();

        assert_eq!("1AB\n2EF", sel_string(&s, scr(0, 0), scr(2, 1), true));

        // No trim
        assert_eq!("1AB  \n2EF", sel_string(&s, scr(0, 0), scr(2, 1), false));
    }

    #[test]
    fn screen_selection_string_trim_empty_line() {
        let mut s = init(5, 5, Some(0));
        let str_ = "1AB  \n\n2EFGH\n3IJKL";
        s.test_write_string(str_).unwrap();

        assert_eq!("1AB\n\n2EF", sel_string(&s, scr(0, 0), scr(2, 2), true));

        // No trim
        assert_eq!("1AB  \n\n2EF", sel_string(&s, scr(0, 0), scr(2, 2), false));
    }

    #[test]
    fn screen_selection_string_soft_wrap() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1ABCD2EFGH3IJKL";
        s.test_write_string(str_).unwrap();

        assert_eq!("2EFGH3IJ", sel_string(&s, scr(0, 1), scr(2, 2), true));
    }

    #[test]
    fn screen_selection_string_wide_char() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1A⚡";
        s.test_write_string(str_).unwrap();

        assert_eq!(str_, sel_string(&s, scr(0, 0), scr(3, 0), true));
        assert_eq!(str_, sel_string(&s, scr(0, 0), scr(2, 0), true));
        assert_eq!("⚡", sel_string(&s, scr(3, 0), scr(3, 0), true));
    }

    #[test]
    fn screen_selection_string_wide_char_with_header() {
        let mut s = init(5, 3, Some(0));
        let str_ = "1ABC⚡";
        s.test_write_string(str_).unwrap();

        assert_eq!(str_, sel_string(&s, scr(0, 0), scr(4, 0), true));
    }

    // https://github.com/mitchellh/ghostty/issues/289
    #[test]
    fn screen_selection_string_empty_with_soft_wrap() {
        let mut s = init(5, 2, Some(0));

        // Let me describe the situation that caused this because this
        // test is not obvious. By writing an emoji below, we introduce
        // one cell with the emoji and one cell as a "wide char spacer".
        // We then soft wrap the line by writing spaces.
        //
        // By selecting only the tail, we'd select nothing and we had
        // a logic error that would cause a crash.
        s.test_write_string("👨").unwrap();
        s.test_write_string("      ").unwrap();

        assert_eq!("👨", sel_string(&s, scr(1, 0), scr(2, 0), true));
    }

    #[test]
    fn screen_selection_string_with_zero_width_joiner() {
        let mut s = init(10, 1, Some(0));
        let str_ = "👨\u{200d}"; // this has a ZWJ
        s.test_write_string(str_).unwrap();

        // Integrity check
        {
            let p = pin(&s, scr(0, 0));
            let cell = cl(p.row_and_cell().cell);
            assert_eq!(0x1F468, cell.content_codepoint());
            assert_eq!(Wide::Wide, cell.wide());
            let cps = pg(p.node).lookup_grapheme(cell).unwrap();
            assert_eq!(1, cps.len());
        }

        // The real test
        assert_eq!("👨\u{200d}", sel_string(&s, scr(0, 0), scr(1, 0), true));
    }

    const LOREM: &str = "Lorem ipsum dolor\nsit amet, consectetur\nadipiscing elit, sed do\neiusmod tempor incididunt\nut labore et dolore";

    fn rect_string(s: &Screen, a: Point, b: Point) -> String {
        let sel = Selection::init(pin(s, a), pin(s, b), true);
        s.selection_string(SelectionString { sel, trim: true })
    }

    #[test]
    fn screen_selection_string_rectangle_basic() {
        let mut s = init(30, 5, Some(0));
        let expected = "t ame\nipisc\nusmod";
        s.test_write_string(LOREM).unwrap();

        assert_eq!(expected, rect_string(&s, scr(2, 1), scr(6, 3)));
    }

    #[test]
    fn screen_selection_string_rectangle_w_eol() {
        let mut s = init(30, 5, Some(0));
        let expected = "dolor\nnsectetur\nlit, sed do\nor incididunt\n dolore";
        s.test_write_string(LOREM).unwrap();

        assert_eq!(expected, rect_string(&s, scr(12, 0), scr(26, 4)));
    }

    #[test]
    fn screen_selection_string_rectangle_more_complex_w_breaks() {
        let mut s = init(30, 8, Some(0));
        let str_ = "Lorem ipsum dolor\nsit amet, consectetur\nadipiscing elit, sed do\neiusmod tempor incididunt\nut labore et dolore\n\nmagna aliqua. Ut enim\nad minim veniam, quis";
        let expected = "elit, sed do\npor incididunt\nt dolore\n\na. Ut enim\nniam, quis";
        s.test_write_string(str_).unwrap();

        assert_eq!(expected, rect_string(&s, scr(11, 2), scr(26, 7)));
    }

    #[test]
    fn screen_selection_string_multi_page() {
        let mut s = init(10, 3, Some(2048));

        // Lazy way to seek to the first page boundary.
        fill_first_page(&mut s, 1);

        s.test_write_string("123456789\n!@#$%^&*(\n123456789").unwrap();

        assert_eq!(
            "123456789\n!@#$%^&*(\n123",
            sel_string(&s, act(0, 0), act(2, 2), true)
        );
    }

    #[test]
    fn screen_line_iterator() {
        let mut s = init(5, 5, Some(0));
        let str_ = "1ABCD\n2EFGH";
        s.test_write_string(str_).unwrap();

        // Test the line iterator
        let mut iter = s.line_iterator(pin(&s, vp(0, 0)));
        let sel = iter.next().unwrap();
        assert_eq!("1ABCD", sel_str(&s, sel));
        let sel = iter.next().unwrap();
        assert_eq!("2EFGH", sel_str(&s, sel));
    }

    #[test]
    fn screen_line_iterator_soft_wrap() {
        let mut s = init(5, 5, Some(0));
        let str_ = "1ABCD2EFGH\n3ABCD";
        s.test_write_string(str_).unwrap();

        // Test the line iterator
        let mut iter = s.line_iterator(pin(&s, vp(0, 0)));
        let sel = iter.next().unwrap();
        assert_eq!("1ABCD2EFGH", sel_str(&s, sel));
        let sel = iter.next().unwrap();
        assert_eq!("3ABCD", sel_str(&s, sel));
        // try testing.expect(iter.next() == null);
    }

    #[test]
    fn screen_hyperlink_start_end() {
        let mut s = init(5, 5, Some(0));
        assert!(s.cursor.hyperlink_id == 0);
        assert_eq!(0, cpage(&s).hyperlink_set.count());

        s.start_hyperlink(b"http://example.com", None).unwrap();
        assert!(s.cursor.hyperlink_id != 0);
        assert_eq!(1, cpage(&s).hyperlink_set.count());

        s.end_hyperlink();
        assert!(s.cursor.hyperlink_id == 0);
        assert_eq!(0, cpage(&s).hyperlink_set.count());
    }

    #[test]
    fn screen_hyperlink_accepts_its_current_values() {
        let mut s = init(5, 5, Some(0));

        s.start_hyperlink(b"http://example.com", Some(b"current")).unwrap();
        // The arguments alias the current hyperlink, exactly like the Zig
        // test (startHyperlink duplicates them before releasing it).
        let (uri, id): (*const [u8], *const [u8]) = {
            let current = s.cursor.hyperlink.as_ref().unwrap();
            let HyperlinkId::Explicit(id) = &current.id else { unreachable!() };
            (&*current.uri as *const [u8], &**id as *const [u8])
        };
        s.start_hyperlink(unsafe { &*uri }, Some(unsafe { &*id })).unwrap();

        let link = s.cursor.hyperlink.as_ref().unwrap();
        assert_eq!(b"http://example.com", &*link.uri);
        let HyperlinkId::Explicit(id) = &link.id else { panic!() };
        assert_eq!(b"current", &**id);
    }

    #[test]
    fn screen_implicit_hyperlink_id_wraps() {
        let mut s = init(5, 5, Some(0));

        s.cursor.hyperlink_implicit_id = size::OffsetInt::MAX;
        s.start_hyperlink(b"http://example.com", None).unwrap();

        assert_eq!(0, s.cursor.hyperlink_implicit_id);
        assert_eq!(
            HyperlinkId::Implicit(size::OffsetInt::MAX),
            s.cursor.hyperlink.as_ref().unwrap().id
        );

        // A failed allocation must roll the wrapped counter back to its
        // original value as well.
        s.end_hyperlink();
        s.cursor.hyperlink_implicit_id = size::OffsetInt::MAX;
        {
            let tw = &HYPERLINK_TW;
            tw.error_always(HyperlinkFailPoint::LinkAlloc, AllocError::OutOfMemory);
            assert_eq!(
                Err(IncreaseCapacityError::OutOfMemory),
                s.start_hyperlink(b"http://example.com", None)
            );
            tw.end(tripwire::ResetMode::Reset).unwrap();
        }
        assert_eq!(size::OffsetInt::MAX, s.cursor.hyperlink_implicit_id);
    }

    #[test]
    fn screen_hyperlink_reuse() {
        let mut s = init(5, 5, Some(0));

        assert!(s.cursor.hyperlink_id == 0);
        assert_eq!(0, cpage(&s).hyperlink_set.count());

        // Use it for the first time
        s.start_hyperlink(b"http://example.com", None).unwrap();
        assert!(s.cursor.hyperlink_id != 0);
        let id = s.cursor.hyperlink_id;

        // Reuse the same hyperlink, expect we have the same ID
        s.start_hyperlink(b"http://example.com", None).unwrap();
        assert_eq!(id, s.cursor.hyperlink_id);
        assert_eq!(1, cpage(&s).hyperlink_set.count());

        s.end_hyperlink();
        assert!(s.cursor.hyperlink_id == 0);
        assert_eq!(0, cpage(&s).hyperlink_set.count());
    }

    #[test]
    fn screen_hyperlink_cursor_state_on_resize() {
        // This test depends on underlying PageList implementation so
        // it may be invalid one day. It's here to document/verify the
        // current behavior.

        let mut s = init(5, 10, Some(0));

        // Start a hyperlink
        s.start_hyperlink(b"http://example.com", None).unwrap();
        assert!(s.cursor.hyperlink_id != 0);
        assert_eq!(1, cpage(&s).hyperlink_set.count());

        // Resize. Any column growth will trigger a page to be reallocated.
        resize(&mut s, 10, 10, true);
        assert!(s.cursor.hyperlink_id != 0);
        assert_eq!(1, cpage(&s).hyperlink_set.count());

        s.end_hyperlink();
        assert!(s.cursor.hyperlink_id == 0);
        assert_eq!(0, cpage(&s).hyperlink_set.count());
    }

    #[test]
    fn screen_cursor_set_hyperlink_oom_uri_too_large_for_string_alloc() {
        let mut s = init(80, 24, Some(0));

        // Start a hyperlink with a URI that just barely fits in the string alloc.
        // This will ensure that additional string alloc space is needed for the
        // redundant copy of the URI when the page is re-alloced.
        let uri = vec![b'a'; pagepkg::STD_CAPACITY.string_bytes as usize - 8];
        s.start_hyperlink(&uri, None).unwrap();

        // Figure out how many cells should can have hyperlinks in this page,
        // and write twice that number, to guarantee the capacity needs to be
        // increased at some point.
        let base_capacity = cpage(&s).hyperlink_capacity();
        let base_string_bytes = nd(cp(&s).node).capacity().string_bytes;
        for _ in 0..base_capacity * 2 {
            s.cursor_set_hyperlink().unwrap();
            if s.cursor.x >= s.pages.cols - 1 {
                s.cursor_down_or_scroll().unwrap();
                s.cursor_horizontal_absolute(0);
            } else {
                s.cursor_right(1);
            }
        }

        // Make sure the capacity really did increase.
        assert!(base_capacity < cpage(&s).hyperlink_capacity());
        // And that our string_bytes increased as well.
        assert!(base_string_bytes < nd(cp(&s).node).capacity().string_bytes);
    }

    #[test]
    fn screen_increase_capacity_cursor_style_ref_count_preserved() {
        let mut s = init(5, 5, Some(0));
        s.set_attribute(bold()).unwrap();
        s.test_write_string("1ABCD").unwrap();

        // We should have one page and it should be our cursor page
        assert!(s.pages.pages.first == s.pages.pages.last);
        assert!(s.pages.pages.first == Some(cp(&s).node));

        let old_style = s.cursor.style;

        unsafe {
            let page = pg(last(&s));
            // 5 chars + cursor = 6 refs
            assert_eq!(6, page.styles.ref_count(page.memory, s.cursor.style_id));
        }

        // This forces the page to change via increaseCapacity.
        let new_node = s
            .increase_capacity(cp(&s).node, Some(IncreaseCapacity::GraphemeBytes))
            .unwrap();

        // Cursor's page_pin should now point to the new node
        assert!(cp(&s).node == new_node);

        // Verify cursor's page_cell and page_row are correctly reloaded from the pin
        let page_rac = cp(&s).row_and_cell();
        assert!(s.cursor.page_row == page_rac.row);
        assert!(s.cursor.page_cell == page_rac.cell);

        // Style should be preserved
        assert_eq!(old_style, s.cursor.style);
        assert!(s.cursor.style_id != style::DEFAULT_ID);

        // After increaseCapacity, the 5 chars are cloned (5 refs) and
        // the cursor's style is re-added (1 ref) = 6 total.
        unsafe {
            let page = pg(last(&s));
            assert_eq!(6, page.styles.ref_count(page.memory, s.cursor.style_id));
        }
    }

    #[test]
    fn screen_increase_capacity_cursor_hyperlink_ref_count_preserved() {
        let mut s = init(5, 5, Some(0));
        s.start_hyperlink(b"https://example.com/", None).unwrap();
        s.test_write_string("1ABCD").unwrap();

        // We should have one page and it should be our cursor page
        assert!(s.pages.pages.first == s.pages.pages.last);
        assert!(s.pages.pages.first == Some(cp(&s).node));

        {
            // Cursor has the hyperlink active = 1 count in hyperlink_set
            assert_eq!(1, pg(last(&s)).hyperlink_set.count());
            assert!(s.cursor.hyperlink_id != 0);
            assert!(s.cursor.hyperlink.is_some());
        }

        // This forces the page to change via increaseCapacity.
        let _ = s
            .increase_capacity(cp(&s).node, Some(IncreaseCapacity::GraphemeBytes))
            .unwrap();

        // Hyperlink should be preserved with correct URI
        assert!(s.cursor.hyperlink.is_some());
        assert!(s.cursor.hyperlink_id != 0);
        assert_eq!(b"https://example.com/", &*s.cursor.hyperlink.as_ref().unwrap().uri);

        // After increaseCapacity, the hyperlink is re-added to the new page.
        assert_eq!(1, pg(last(&s)).hyperlink_set.count());
    }

    #[test]
    fn screen_increase_capacity_cursor_with_both_style_and_hyperlink_preserved() {
        let mut s = init(5, 5, Some(0));

        // Set both a non-default style AND an active hyperlink.
        // Write one character first with bold to mark the row as styled,
        // then start the hyperlink and write more characters.
        s.set_attribute(bold()).unwrap();
        s.start_hyperlink(b"https://example.com/", None).unwrap();
        s.test_write_string("1ABCD").unwrap();

        // We should have one page and it should be our cursor page
        assert!(s.pages.pages.first == s.pages.pages.last);
        assert!(s.pages.pages.first == Some(cp(&s).node));

        let old_style = s.cursor.style;

        unsafe {
            let page = pg(last(&s));
            // 5 chars + cursor = 6 refs for bold style
            assert_eq!(6, page.styles.ref_count(page.memory, s.cursor.style_id));
            // Cursor has the hyperlink active = 1 count in hyperlink_set
            assert_eq!(1, page.hyperlink_set.count());
            assert!(s.cursor.style_id != style::DEFAULT_ID);
            assert!(s.cursor.hyperlink_id != 0);
            assert!(s.cursor.hyperlink.is_some());
        }

        // This forces the page to change via increaseCapacity.
        let _ = s
            .increase_capacity(cp(&s).node, Some(IncreaseCapacity::GraphemeBytes))
            .unwrap();

        // Style should be preserved
        assert_eq!(old_style, s.cursor.style);
        assert!(s.cursor.style_id != style::DEFAULT_ID);

        // Hyperlink should be preserved with correct URI
        assert!(s.cursor.hyperlink.is_some());
        assert!(s.cursor.hyperlink_id != 0);
        assert_eq!(b"https://example.com/", &*s.cursor.hyperlink.as_ref().unwrap().uri);

        // After increaseCapacity, both style and hyperlink are re-added to the new page.
        unsafe {
            let page = pg(last(&s));
            assert_eq!(6, page.styles.ref_count(page.memory, s.cursor.style_id));
            assert_eq!(1, page.hyperlink_set.count());
        }
    }

    #[test]
    fn screen_increase_capacity_non_cursor_page_returns_early() {
        // Test that calling increaseCapacity on a page that is NOT the cursor's
        // page properly delegates to pages.increaseCapacity without doing the
        // extra cursor accounting (style/hyperlink re-adding).
        let mut s = init(80, 24, Some(10000));

        // Set up a custom style and hyperlink on the cursor
        s.set_attribute(bold()).unwrap();
        s.start_hyperlink(b"https://example.com/", None).unwrap();
        s.test_write_string("Hello").unwrap();

        // Store cursor state before growing pages
        let old_style = s.cursor.style;
        let old_style_id = s.cursor.style_id;
        let old_hyperlink = s.cursor.hyperlink.as_ref().map(|h| &**h as *const Hyperlink<'static>);
        let old_hyperlink_id = s.cursor.hyperlink_id;

        // The cursor is on the first (and only) page
        assert!(s.pages.pages.first == s.pages.pages.last);
        assert!(cp(&s).node == first(&s));

        // Grow pages until we have multiple pages. The cursor's pin stays on
        // the first page since we're just adding rows.
        let first_page_node = first(&s);
        pg(first_page_node).pause_integrity_checks(true);
        for _ in 0..nd(first_page_node).capacity().rows - nd(first_page_node).rows() {
            let _ = s.pages.grow().unwrap();
        }
        pg(first_page_node).pause_integrity_checks(false);
        let _ = s.pages.grow().unwrap();

        // Now we have two pages
        assert!(s.pages.pages.first != s.pages.pages.last);
        let second_page = last(&s);

        // Cursor should still be on the first page (where it was created)
        assert!(cp(&s).node == first(&s));
        assert!(cp(&s).node != second_page);

        let second_page_styles_cap = nd(second_page).capacity().styles;
        let cursor_page_styles_cap = nd(cp(&s).node).capacity().styles;

        // Call increaseCapacity on the second page (NOT the cursor's page)
        let new_second_page = s
            .increase_capacity(second_page, Some(IncreaseCapacity::Styles))
            .unwrap();

        // The second page should have increased capacity
        assert_eq!(second_page_styles_cap * 2, nd(new_second_page).capacity().styles);

        // The cursor's page (first page) should be unchanged
        assert_eq!(cursor_page_styles_cap, nd(cp(&s).node).capacity().styles);

        // Cursor state should be completely unchanged since we didn't touch its page
        assert_eq!(old_style, s.cursor.style);
        assert_eq!(old_style_id, s.cursor.style_id);
        assert_eq!(
            old_hyperlink,
            s.cursor.hyperlink.as_ref().map(|h| &**h as *const Hyperlink<'static>)
        );
        assert_eq!(old_hyperlink_id, s.cursor.hyperlink_id);

        // Verify hyperlink is still valid
        assert!(s.cursor.hyperlink.is_some());
        assert_eq!(b"https://example.com/", &*s.cursor.hyperlink.as_ref().unwrap().uri);
    }

    /// Fill a page's style map with unique bg-color styles until it is full.
    fn fill_styles_until_full(page: &mut Page) {
        page.pause_integrity_checks(true);
        let mut n: u32 = 1;
        loop {
            let st = Style {
                bg_color: style::Color::Rgb(rgb(n as u8, (n >> 8) as u8, (n >> 16) as u8)),
                ..Style::default()
            };
            if unsafe { page.styles.add(page.memory, st) }.is_err() {
                break;
            }
            n += 1;
        }
        page.assert_integrity();
        page.pause_integrity_checks(false);
    }

    /// Fill a page's style map up to its layout capacity.
    fn fill_styles_to_layout_cap(page: &mut Page) {
        page.pause_integrity_checks(true);
        let max_items = page.styles.layout.cap;
        let mut n: usize = 1;
        while n < max_items {
            let st = Style {
                bg_color: style::Color::Rgb(rgb(n as u8, (n >> 8) as u8, (n >> 16) as u8)),
                ..Style::default()
            };
            if unsafe { page.styles.add(page.memory, st) }.is_err() {
                break;
            }
            n += 1;
        }
        page.assert_integrity();
        page.pause_integrity_checks(false);
    }

    #[test]
    fn screen_cursor_down_to_page_with_insufficient_capacity() {
        // Regression test for https://github.com/ghostty-org/ghostty/issues/10282
        //
        // This test exposes a use-after-realloc bug in cursorDown (and similar
        // cursor movement functions). The bug pattern:
        //
        // 1. cursorDown creates a by-value copy of the pin via page_pin.down(n)
        // 2. cursorChangePin is called, which may trigger increaseCapacity
        //    if the target page's style map is full
        // 3. increaseCapacity frees the old page and creates a new one
        // 4. The local pin copy still points to the freed page
        // 5. rowAndCell() on the stale pin accesses freed memory

        // Small screen to make page boundary crossing easy to set up
        let mut s = init(10, 3, Some(1));

        // Scroll down enough to create a second page
        let start_page = scroll_to_new_page(&mut s);

        // Cursor should now be on a new page
        let new_page: *mut Page = cpage(&s);
        assert!(start_page != new_page);

        // Fill new_page's style map to capacity. When we move INTO this page
        // with a style set, increaseCapacity will be triggered.
        fill_styles_until_full(unsafe { &mut *new_page });

        // Move cursor to start of active area and set a style
        s.cursor_absolute(0, 0);
        s.set_attribute(bold()).unwrap();
        assert!(s.cursor.style.flags.bold);
        assert!(s.cursor.style_id != style::DEFAULT_ID);

        // Find the row just before the page boundary
        let mut found = false;
        for row in 0..s.pages.rows - 1 {
            s.cursor_absolute(0, row);
            let cur_node = cp(&s).node;
            if let Some(next_pin) = cp(&s).down(1) {
                if next_pin.node != cur_node {
                    // Cursor is at 'row', moving down crosses to new_page
                    assert!(pg(next_pin.node) as *mut Page == new_page);

                    // This cursorDown triggers the bug: the local page_pin copy
                    // becomes stale after increaseCapacity, causing rowAndCell()
                    // to access freed memory.
                    s.cursor_down(1);

                    // If the fix is applied, verify correct state
                    assert!(s.cursor.y == row + 1);
                    assert!(s.cursor.style.flags.bold);

                    found = true;
                    break;
                }
            }
        }
        // Didn't find boundary
        assert!(found);
    }

    #[test]
    fn screen_set_attribute_increases_capacity_when_style_map_is_full() {
        // Tests that setAttribute succeeds when the style map is full by
        // increasing page capacity. When capacity is at max and increaseCapacity
        // returns OutOfSpace, manualStyleUpdate will split the page instead.

        // Use a small screen with multiple rows
        let mut s = init(10, 5, Some(10));

        // Write content to multiple rows
        s.test_write_string("line1\nline2\nline3\nline4\nline5").unwrap();

        // Get the page and fill its style map to capacity
        let page: *mut Page = cpage(&s);
        let original_styles_capacity = unsafe { (*page).capacity.styles };

        // Fill the style map to capacity using the StyleSet's layout capacity
        // which accounts for the load factor
        fill_styles_to_layout_cap(unsafe { &mut *page });

        // Now try to set a new unique attribute that would require a new style slot
        // This should succeed by increasing capacity (or splitting if at max capacity)
        s.set_attribute(bold()).unwrap();

        // The style should have been applied (bold flag set)
        assert!(s.cursor.style.flags.bold);

        // The cursor should have a valid non-default style_id
        assert!(s.cursor.style_id != style::DEFAULT_ID);

        // Either the capacity increased or the page was split/changed
        let current_page = cpage(&s);
        let capacity_increased = current_page.capacity.styles > original_styles_capacity;
        let page_changed = current_page as *mut Page != page;
        assert!(capacity_increased || page_changed);
    }

    #[test]
    fn screen_set_attribute_splits_page_on_out_of_space_at_max_styles() {
        let mut s = init(10, 10, Some(0));

        // Write content to multiple rows so we have something to split
        s.test_write_string("line1\nline2\nline3\nline4\nline5").unwrap();

        // Remember the original node
        let original_node = cp(&s).node;

        // Increase the page's style capacity to max by repeatedly calling increaseCapacity
        // Use Screen.increaseCapacity to properly maintain cursor state
        let max_styles = CellCountInt::MAX;
        while nd(cp(&s).node).capacity().styles < max_styles {
            if s.increase_capacity(cp(&s).node, Some(IncreaseCapacity::Styles)).is_err() {
                break;
            }
        }

        // Get the page reference after increaseCapacity - cursor may have moved
        let page = cpage(&s);
        assert_eq!(max_styles, page.capacity.styles);

        // Fill the style map to capacity using the StyleSet's layout capacity
        // which accounts for the load factor
        fill_styles_to_layout_cap(page);

        // Track the node before setAttribute
        let node_before_set = cp(&s).node;

        // Now try to set a new unique attribute that would require a new style slot
        // At max capacity, increaseCapacity will return OutOfSpace, triggering page split
        s.set_attribute(bold()).unwrap();

        // The style should have been applied (bold flag set)
        assert!(s.cursor.style.flags.bold);

        // The cursor should have a valid non-default style_id
        assert!(s.cursor.style_id != style::DEFAULT_ID);

        // The page should have been split
        let page_was_split = cp(&s).node != node_before_set
            || nd(node_before_set).next.is_some()
            || nd(node_before_set).prev.is_some()
            || cp(&s).node != original_node;
        assert!(page_was_split);
    }

    /// Enable line click mode and write "> " prompt + `input` input.
    fn prompt_input(s: &mut Screen, input: &str) {
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Initial));
        s.test_write_string("> ").unwrap();
        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string(input).unwrap();
    }

    fn cl_line(s: &mut Screen) {
        s.semantic_prompt.click = SemanticClick::Cl(osc_sp::Click::Line);
    }

    #[test]
    fn screen_prompt_click_move_line_right_basic() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write a prompt and input
        prompt_input(&mut s, "hello");

        // Move cursor back to start of input (column 2, the 'h')
        s.cursor_absolute(2, 0);

        // Click on first 'l' (column 4), should require 2 right movements (h->e->l)
        let result = s.prompt_click_move(pin(&s, act(4, 0)));

        assert_eq!(2, result.right);
        assert_eq!(0, result.left);
    }

    #[test]
    fn screen_prompt_click_move_line_right_cursor_not_on_input() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write a prompt and input
        prompt_input(&mut s, "hello");
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);

        // Move cursor back to prompt area (column 0, the '>')
        s.cursor_absolute(0, 0);

        // Cursor is on prompt, not input - should return zero
        let result = s.prompt_click_move(pin(&s, act(4, 0)));

        assert_eq!(PromptClickMove::ZERO, result);
    }

    #[test]
    fn screen_prompt_click_move_line_right_click_on_same_position() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write a prompt and input
        prompt_input(&mut s, "hello");

        // Move cursor to column 4
        s.cursor_absolute(4, 0);

        // Click on same position - no movement needed
        let result = s.prompt_click_move(pin(&s, act(4, 0)));

        assert_eq!(PromptClickMove::ZERO, result);
    }

    #[test]
    fn screen_prompt_click_move_line_right_skips_non_input_cells() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write: "> h" then output "X" then input "llo"
        prompt_input(&mut s, "h");
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("X").unwrap();
        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string("llo").unwrap();

        // Move cursor to column 2 (the 'h')
        s.cursor_absolute(2, 0);

        // Click on 'l' at column 5 - should skip the 'X' output cell
        // Movement: h (start) -> l (col 4) -> l (col 5) = 2 right movements
        let result = s.prompt_click_move(pin(&s, act(5, 0)));

        assert_eq!(2, result.right);
        assert_eq!(0, result.left);
    }

    #[test]
    fn screen_prompt_click_move_line_right_soft_wrapped_line() {
        let mut s = init(10, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write a prompt and input that wraps
        // Write 8 chars of input, first row has 2 for prompt + 8 input = 10 cols
        prompt_input(&mut s, "abcdefgh");
        // Continue on next row (soft-wrapped)
        s.test_write_string("ij").unwrap();

        // Verify soft wrap occurred
        assert_eq!("> abcdefgh\nij", dump(&s, Tag::Viewport));

        // Move cursor to column 2 (the 'a')
        s.cursor_absolute(2, 0);

        // Click on 'j' at column 1, row 1 - should count all input cells
        // Movement: a->b->c->d->e->f->g->h->i->j = 9 right movements
        let result = s.prompt_click_move(pin(&s, act(1, 1)));

        assert_eq!(9, result.right);
        assert_eq!(0, result.left);
    }

    #[test]
    fn screen_prompt_click_move_disabled_when_click_is_none() {
        let mut s = init(20, 5, Some(0));

        // Click mode is .none by default (disabled)
        assert_eq!(SemanticClick::None, s.semantic_prompt.click);

        // Write a prompt and input
        prompt_input(&mut s, "hello");

        // Move cursor to start of input
        s.cursor_absolute(2, 0);

        // Click should return zero since click mode is disabled
        let result = s.prompt_click_move(pin(&s, act(4, 0)));

        assert_eq!(PromptClickMove::ZERO, result);
    }

    #[test]
    fn screen_prompt_click_move_line_right_stops_at_hard_wrap() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write prompt and input on first line
        prompt_input(&mut s, "hello");
        // Hard wrap (newline)
        s.test_write_string("\n").unwrap();
        s.test_write_string("world").unwrap();

        // Move cursor to column 2 (the 'h')
        s.cursor_absolute(2, 0);

        // Click on 'w' at column 0, row 1 - but line mode stops at hard wrap
        // Should only move to end of first line: h->e->l->l->o = 4 movements
        let result = s.prompt_click_move(pin(&s, act(0, 1)));

        // Should stop at end of first line, not cross hard wrap
        assert_eq!(5, result.right);
        assert_eq!(0, result.left);
    }

    #[test]
    fn screen_prompt_click_move_line_right_stops_at_non_continuation_row() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Row 0: PROMPT "> hello"
        prompt_input(&mut s, "hello\n");

        // Row 1: CONTINUATION "world"
        s.cursor_set_semantic_content(prompt(osc_sp::PromptKind::Continuation));
        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string("world\n").unwrap();

        // Row 2: NEW PROMPT "> again"
        prompt_input(&mut s, "again");

        // Verify content
        assert_eq!("> hello\nworld\n> again", dump(&s, Tag::Viewport));

        // Move cursor to 'w' at column 0, row 1
        s.cursor_absolute(0, 1);

        // Click on 'a' at column 2, row 2 - but row 2 is a new prompt
        // Should stop at end of "world": w->o->r->l->d = 4 movements
        let result = s.prompt_click_move(pin(&s, act(2, 2)));

        // Should stop at 'd' (end of world), not cross to new prompt
        assert_eq!(5, result.right);
        assert_eq!(0, result.left);
    }

    #[test]
    fn screen_prompt_click_move_line_left_basic() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write a prompt and input
        prompt_input(&mut s, "hello");

        // Cursor is at column 7 (after 'o'), move it to column 6 (the 'o')
        s.cursor_absolute(6, 0);

        // Click on 'h' (column 2), should require 4 left movements (o->l->l->e->h)
        let result = s.prompt_click_move(pin(&s, act(2, 0)));

        assert_eq!(4, result.left);
        assert_eq!(0, result.right);
    }

    #[test]
    fn screen_prompt_click_move_line_left_skips_non_input_cells() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write: "> h" then output "X" then input "llo"
        prompt_input(&mut s, "h");
        s.cursor_set_semantic_content(SemanticContentUpdate::Output);
        s.test_write_string("X").unwrap();
        s.cursor_set_semantic_content(input_explicit());
        s.test_write_string("llo").unwrap();

        // Move cursor to column 6 (the 'o')
        s.cursor_absolute(6, 0);

        // Click on 'h' at column 2 - should skip the 'X' output cell
        // Movement: o->l->l->h = 3 left movements (skipping X)
        let result = s.prompt_click_move(pin(&s, act(2, 0)));

        assert_eq!(3, result.left);
        assert_eq!(0, result.right);
    }

    #[test]
    fn screen_prompt_click_move_line_left_soft_wrapped_line() {
        let mut s = init(10, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write a prompt and input that wraps
        // Write 8 chars of input, first row has 2 for prompt + 8 input = 10 cols
        prompt_input(&mut s, "abcdefgh");
        // Continue on next row (soft-wrapped)
        s.test_write_string("ij").unwrap();

        // Verify soft wrap occurred
        assert_eq!("> abcdefgh\nij", dump(&s, Tag::Viewport));

        // Cursor is at column 2, row 1 (after 'j'). Move to 'j' at column 1.
        s.cursor_absolute(1, 1);

        // Click on 'a' at column 2, row 0 - should count all input cells backwards
        // Movement: j->i->h->g->f->e->d->c->b->a = 9 left movements
        let result = s.prompt_click_move(pin(&s, act(2, 0)));

        assert_eq!(9, result.left);
        assert_eq!(0, result.right);
    }

    #[test]
    fn screen_prompt_click_move_line_left_stops_at_hard_wrap() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write prompt and input on first line, then hard wrap
        prompt_input(&mut s, "hello");
        // Hard wrap (newline)
        s.test_write_string("\n").unwrap();
        s.test_write_string("world").unwrap();

        // Move cursor to 'd' at column 4, row 1 (an actual input cell)
        s.cursor_absolute(4, 1);

        // Click on 'h' at column 2, row 0 - but line mode stops at hard wrap
        // Should only move to start of second line, not cross to row 0
        let result = s.prompt_click_move(pin(&s, act(2, 0)));

        // Should stop at start of second line: d->l->r->o->w = 4 movements
        assert_eq!(4, result.left);
        assert_eq!(0, result.right);
    }

    #[test]
    fn screen_prompt_click_move_click_right_of_input_same_line() {
        // Set up: "> hello" where "> " is prompt and "hello" is input
        // Clicking to the right of the 'o' should move cursor past the input

        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write a prompt and input
        prompt_input(&mut s, "hello");

        // Move cursor to start of input (column 2, the 'h')
        s.cursor_absolute(2, 0);

        // Click beyond the input (column 15) - should move to one past the 'o'
        let result = s.prompt_click_move(pin(&s, act(15, 0)));

        assert_eq!(5, result.right);
        assert_eq!(0, result.left);
    }

    #[test]
    fn screen_prompt_click_move_click_right_of_input_cursor_at_end() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write a prompt and input
        prompt_input(&mut s, "hello");

        // Cursor is already at column 7 (one past 'o') after writing
        // Click beyond the input (column 15) - no movement needed since
        // cursor is already at the end position
        let result = s.prompt_click_move(pin(&s, act(15, 0)));

        assert_eq!(0, result.right);
        assert_eq!(0, result.left);
    }

    #[test]
    fn screen_prompt_click_move_click_right_of_input_on_lower_line() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write a prompt and input
        prompt_input(&mut s, "hello");

        // Move cursor to start of input (column 2, the 'h')
        s.cursor_absolute(2, 0);

        // Click on a lower line (row 1) - should move to end of input
        // This is outside the prompt area so should clamp to end
        let result = s.prompt_click_move(pin(&s, act(5, 1)));

        // From 'h', we need to pass e, l, l, o (4 cells) + 1 past end = 5
        assert_eq!(5, result.right);
        assert_eq!(0, result.left);
    }

    #[test]
    fn screen_prompt_click_move_click_right_of_input_cursor_at_end_lower_line() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write a prompt and input
        prompt_input(&mut s, "hello");

        // Cursor is at column 7 after writing (one past 'o')
        // Click on a lower line (row 1) - cursor already at end, no movement needed
        let result = s.prompt_click_move(pin(&s, act(5, 1)));

        assert_eq!(0, result.right);
        assert_eq!(0, result.left);
    }

    #[test]
    fn screen_prompt_click_move_click_right_of_input_cursor_on_last_char() {
        let mut s = init(20, 5, Some(0));

        // Enable line click mode
        cl_line(&mut s);

        // Write a prompt and input
        prompt_input(&mut s, "hello");

        // Move cursor to last input char (column 6, the 'o')
        s.cursor_absolute(6, 0);

        // Click beyond the input (column 15)
        let result = s.prompt_click_move(pin(&s, act(15, 0)));

        assert_eq!(1, result.right);
        assert_eq!(0, result.left);
    }

    /// Mark a row as soft-wrapped, wrap continuation and prompt.
    fn dirty_row_metadata(s: &Screen, pt: Point) {
        let row = rw(s.pages.get_cell(pt).unwrap().row);
        row.set_wrap(true);
        row.set_wrap_continuation(true);
        row.set_semantic_prompt(RowSemanticPrompt::Prompt);
    }

    fn assert_default_row_metadata(s: &Screen, pt: Point) {
        let row = get_row(s, pt);
        assert!(!row.wrap());
        assert!(!row.wrap_continuation());
        assert_eq!(RowSemanticPrompt::None, row.semantic_prompt());
    }

    #[test]
    fn screen_cursor_scroll_region_up_recycled_row_has_default_metadata() {
        let mut s = init(5, 5, Some(0));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL\n4MNOP\n5QRST").unwrap();

        // Simulate the top region row being part of a soft-wrapped,
        // prompt-marked line. Its Row storage is recycled as the new
        // blank cursor row and must not retain the metadata.
        dirty_row_metadata(&s, act(0, 0));

        s.cursor_absolute(1, 2);
        s.cursor_scroll_region_up(2).unwrap();

        assert_default_row_metadata(&s, act(0, 2));
        assert_eq!("2EFGH\n3IJKL\n\n4MNOP\n5QRST", dump(&s, Tag::Screen));
    }

    #[test]
    fn screen_cursor_scroll_region_up_cross_page_recycled_row_has_default_metadata() {
        let mut s = init(10, 5, Some(10));

        // We need to get the active area to span two pages so that the
        // scroll region does too, exercising the slow path (eraseRowBounded).
        fill_first_page(&mut s, 3);
        s.test_write_string("1A\n2B\n3C\n4D\n5E").unwrap();

        // The first row of the last page is the Row storage that ends up
        // recycled as the blank region-bottom row: the erased row's
        // storage stays on the first page (receiving this row's content
        // via clone) while this storage is cleared for the blank row.
        dirty_row_metadata(&s, act(0, 3));

        // Region rows 1-3 with the cursor on the region bottom, which is
        // on the second page while the region top is on the first page.
        s.cursor_absolute(0, 3);
        s.cursor_scroll_region_up(2).unwrap();

        assert_default_row_metadata(&s, act(0, 3));
        assert_eq!("1A\n3C\n4D\n\n5E", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_cursor_scroll_above_cross_page_recycled_row_has_default_metadata() {
        let mut s = init(10, 5, Some(10));

        // Get the cursor page and the last page to differ so that
        // cursorScrollAbove takes the cross-page rotate path.
        fill_first_page(&mut s, 3);
        s.test_write_string("1A\n2B\n3C\n4D\n5E").unwrap();
        s.cursor_absolute(0, 1);
        assert!(cp(&s).node == first(&s));
        assert!(nd(first(&s)).next.is_some());

        // The last row of the cursor page is the Row storage that gets
        // recycled as the new blank row below the cursor after its content
        // is moved down to the next page.
        dirty_row_metadata(&s, act(0, 2));

        s.cursor_scroll_above().unwrap();

        // One row scrolled into history, so the blank row is at active
        // y=1 (just below the cursor's original row).
        assert_default_row_metadata(&s, act(0, 1));
        assert_eq!("2B\n\n3C\n4D\n5E", dump(&s, Tag::Viewport));
    }

    #[test]
    fn screen_cursor_down_scroll_no_scrollback_recycled_row_has_default_metadata() {
        let mut s = init(5, 3, Some(0));
        s.test_write_string("1ABCD\n2EFGH\n3IJKL").unwrap();

        dirty_row_metadata(&s, act(0, 0));

        s.cursor_absolute(0, 2);
        s.cursor_down_scroll().unwrap();

        assert_default_row_metadata(&s, act(0, 2));
    }

    #[test]
    fn screen_cursor_down_scroll_single_row_no_scrollback_resets_metadata() {
        let mut s = init(5, 1, Some(0));
        s.test_write_string("1ABCD").unwrap();

        dirty_row_metadata(&s, act(0, 0));

        s.cursor_down_scroll().unwrap();

        assert_default_row_metadata(&s, act(0, 0));
    }

    #[test]
    fn screen_select_line_does_not_join_lines_across_a_recycled_row() {
        let mut s = init(6, 5, Some(0));

        // A soft-wrapped line across rows 0 and 1: row 0 gets wrap=true.
        s.test_write_string("AAAAAAA").unwrap();

        // Scroll a region of rows 0-2 up by one: row 0 is discarded and
        // its Row storage recycled as the blank row 2.
        s.cursor_absolute(0, 2);
        s.cursor_scroll_region_up(2).unwrap();

        // Write unrelated single-line words on the recycled row and below.
        s.cursor_absolute(0, 2);
        s.test_write_string("world").unwrap();
        s.cursor_absolute(0, 3);
        s.test_write_string("hello").unwrap();

        // Selecting the line "world" must not extend into "hello": these
        // are separate hard lines. A stale wrap flag on the recycled row
        // would join them.
        let sel = select_line_at(&s, act(0, 2)).unwrap();
        assert_eq!("world", sel_str(&s, sel));
    }

    fn init_zero_lines() -> Screen {
        Screen::init(Options {
            cols: 10,
            rows: 3,
            max_scrollback_bytes: None,
            max_scrollback_lines: Some(0),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn screen_scroll_clear_cursor_page_pruned_keeps_per_page_style_id() {
        // A scrollback limit of 0 lines still keeps one page of rows, which
        // is the smallest amount allowed. This means the oldest page is
        // deleted as soon as all of its rows have scrolled into history.
        let mut s = init_zero_lines();

        // Fill the first page and put two more rows on a second page. Every
        // row has text so that scrollClear scrolls the whole active area.
        let page_rows = nd(first(&s)).capacity().rows;
        for _ in 0..page_rows + 1 {
            s.test_write_string("x\n").unwrap();
        }
        s.test_write_string("x").unwrap();
        assert!(first(&s) != last(&s));

        // Move the cursor to the top of the active area, which is the last
        // row of the first page. Making it bold gives the cursor a style ID
        // that only exists on the first page.
        s.cursor_absolute(0, 0);
        assert!(cp(&s).node == first(&s));
        s.set_attribute(bold()).unwrap();
        assert!(s.cursor.style_id != style::DEFAULT_ID);
        let first_node = first(&s);

        // Clearing scrolls all 3 active rows into history. The first page is
        // now entirely history, so the scrollback limit deletes it and the
        // cursor pin is moved to another page.
        s.scroll_clear().unwrap();
        assert!(cp(&s).node != first_node);

        // The cursor's style must be registered on the page it moved to.
        let page = cpage(&s);
        assert!(s.cursor.style_id != style::DEFAULT_ID);
        assert!(unsafe { page.styles.ref_count(page.memory, s.cursor.style_id) } > 0);
        assert!(!cp(&s).garbage);

        // Printing must work and produce a bold cell.
        s.test_write_string("y").unwrap();
        let list_cell = s.pages.get_cell(act(0, 0)).unwrap();
        let cell_style = list_cell.style();
        assert!(cell_style.flags.bold);
    }

    #[test]
    fn screen_scroll_clear_cursor_page_pruned_keeps_per_page_hyperlink_id() {
        // Same setup as the style test above, but with a hyperlink instead
        // of a style. Hyperlink IDs are also only valid on one page.
        let mut s = init_zero_lines();

        let page_rows = nd(first(&s)).capacity().rows;
        for _ in 0..page_rows + 1 {
            s.test_write_string("x\n").unwrap();
        }
        s.test_write_string("x").unwrap();
        assert!(first(&s) != last(&s));

        s.cursor_absolute(0, 0);
        assert!(cp(&s).node == first(&s));
        s.start_hyperlink(b"http://example.com", None).unwrap();
        assert!(s.cursor.hyperlink_id != 0);
        let first_node = first(&s);

        s.scroll_clear().unwrap();
        assert!(cp(&s).node != first_node);

        // The cursor's hyperlink must be registered on the page it moved to.
        let page = cpage(&s);
        assert!(s.cursor.hyperlink_id != 0);
        assert!(unsafe { page.hyperlink_set.ref_count(page.memory, s.cursor.hyperlink_id) } > 0);
        assert!(!cp(&s).garbage);

        // Printing must work and produce a cell with a hyperlink.
        s.test_write_string("y").unwrap();
        assert!(get_cell(&s, act(0, 0)).hyperlink());
    }

    // @tests-end
}
