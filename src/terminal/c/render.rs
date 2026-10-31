//! Port of `src/terminal/c/render.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - The iterator wrappers hold raw slice pointers into the render state
//!   (Zig: slices), valid until the next update, like Zig. The row cells
//!   wrapper holds a pointer to the row's `render::Cells` instead of the
//!   separate raw/grapheme/style slices because the Rust grapheme storage
//!   is an arena private to `Cells` (`Cells::grapheme`). Fields that Zig
//!   leaves `undefined` at creation start out empty/null.
//! - The image iterator holds a `render::ImagePlacementIterator` with its
//!   lifetime erased to `'static` (valid until the next update).
//! - Enum arguments and enum values read from C (`Dirty`) cross the ABI as
//!   `c_int` and are validated. The C enums that mirror core render enums
//!   (`Dirty`, `ImageProtocol`, `ImageLayer`, `KittyPlacementKind`) are
//!   declared here with `c_api_enum!` and converted at the boundary (Zig:
//!   aliases of the `lib.Enum` core types, C ints in C builds).
//! - `RenderState::update`/`begin_update` cannot fail in Rust (allocation
//!   failure aborts), so the `out_of_memory` results are unreachable.
//! - The sized-struct writers (`writeCursor`/`writeColors`) write through
//!   raw field pointers since the caller's struct may be truncated, and so
//!   does `row_cells_copy` for `GhosttyRenderStateRowCellsCopy`. It writes
//!   the caller's cell, style and grapheme buffers with `ptr::write`.
//! - `trimmedEnd` ORs each group of eight raw cells with a fold over a
//!   `[Cell; 8]` instead of `@Vector`/`@reduce`.
//! - `row_cells_copy` builds each copied cell from the raw cell bits
//!   (one 8-byte and one 4-byte store) instead of reading every field
//!   through its enum, gives unstyled codepoint cells and styled cells
//!   continuing the previous style ID their own short paths, and only
//!   compares styles when the style ID changes. The output is the same,
//!   style and grapheme indexes included. See `copy_run`.
//! - `row_get` answers `dirty` before the general dispatch.

use std::ffi::{c_int, c_void};
use std::mem::{offset_of, size_of};

use super::cell as cell_c;
use super::result::Result;
use super::row;
use super::style as style_c;
use super::terminal as terminal_c;
use super::types::HandleAllocator;
use crate::lib::allocator::Allocator as CAllocator;
use crate::lib::Buffer;
use crate::struct_sized_field_fits;
use crate::terminal::color::{self as colorpkg, RgbC};
use crate::terminal::cursor as cursorpkg;
use crate::terminal::page::{self, ContentTag};
use crate::terminal::page_list::Pin;
use crate::terminal::render as renderpkg;
use crate::terminal::size::CellCountInt;
use crate::terminal::style::{self, Color as StyleColor};

const LOG_TARGET: &str = "render_state_c";

pub struct RenderStateWrapper {
    alloc: HandleAllocator,
    state: renderpkg::RenderState,

    /// The overscan request set through the C API. This is copied into
    /// `state.overscan_request` at the start of each update rather than
    /// written there directly, because the render state reads its
    /// request to locate the rows of the last update. Changing it
    /// between an update and reading that update's rows would point the
    /// reads at the wrong entries.
    overscan_request: renderpkg::Overscan,

    /// The clip request set through the C API, applied like
    /// `overscan_request` and for the same reason.
    clip_request: renderpkg::Clip,
}

impl RenderStateWrapper {
    /// Apply any pending option changes to the render state. Called at
    /// the start of every update.
    fn apply_options(&mut self) {
        self.state.overscan_request = self.overscan_request;
        self.state.clip_request = self.clip_request;
    }
}

/// The "before the first element" position for the iterator wrappers
/// below. Represented as a sentinel index rather than an optional because
/// optional codegens into something that is less efficient than this.
const POSITION_NONE: usize = usize::MAX;

/// An empty raw slice pointer (the initial value of the wrapper slices).
fn empty_slice<T>() -> *mut [T] {
    std::ptr::slice_from_raw_parts_mut(std::ptr::NonNull::<T>::dangling().as_ptr(), 0)
}

pub struct RowIteratorWrapper {
    alloc: HandleAllocator,

    /// The current index into the row slices below, or `POSITION_NONE`
    /// if iteration hasn't started. Always validate against `raws.len`
    /// before use. Without overscan this is also the viewport y. With
    /// overscan, add `viewport_y_base` to get the viewport y.
    y: usize,

    /// These are the raw pointers into the render state data. They are
    /// sliced to the rows the last update captured
    /// (`RenderState.rowDataRange`), so every index below `raws.len` is
    /// valid and no other bounds checks are needed.
    raws: *const [page::Row],
    cells: *const [renderpkg::Cells],
    selection: *const [Option<[CellCountInt; 2]>],
    dirty: *mut [bool],

    /// Pins and serials for row identity, sliced like the other fields.
    pins: *const [Pin],
    serials: *const [u64],

    /// The viewport y of the first row in the slices. This is the
    /// negated count of overscan rows captured above the viewport, or
    /// zero without overscan, offset by the first row of any clip.
    viewport_y_base: i32,

    /// The global dirty state from the render state that populated this
    /// iterator. This has the same borrowed lifetime as the row slices.
    /// (Null until the iterator is populated.)
    state_dirty: *const renderpkg::Dirty,

    /// The color palette from the render state, needed to resolve
    /// palette-indexed background colors on cells.
    palette: *const colorpkg::Palette,
}

pub struct RowCellsWrapper {
    alloc: HandleAllocator,

    /// The current index (also x value) into the cell list, or
    /// `POSITION_NONE` if iteration hasn't started. Always validate
    /// against `raws.len` before use.
    x: usize,

    /// The row's cells (raw cells, graphemes and styles). Null until the
    /// wrapper is populated.
    cells: *const renderpkg::Cells,
    selection: Option<[CellCountInt; 2]>,

    /// The color palette, needed to resolve palette-indexed background colors.
    palette: *const colorpkg::Palette,
}

impl RowCellsWrapper {
    /// The raw cells (Zig: `raws`).
    fn raws(&self) -> &[page::Cell] {
        // SAFETY: a non-null pointer refers to the cells of the render
        // state that populated this wrapper (valid until its next update).
        match unsafe { self.cells.as_ref() } {
            Some(c) => &c.raw,
            None => &[],
        }
    }

    fn cells(&self) -> &renderpkg::Cells {
        // SAFETY: only called after `raws()` bounds-checked a position,
        // which requires a populated wrapper.
        unsafe { &*self.cells }
    }
}

/// C: GhosttyRenderState
pub type RenderState = *mut RenderStateWrapper;

/// C: GhosttyRenderStateRowIterator
pub type RowIterator = *mut RowIteratorWrapper;

/// C: GhosttyRenderStateRowCells
pub type RowCells = *mut RowCellsWrapper;

/// C: GhosttyRenderStateImageIterator
pub type ImageIterator = *mut ImageIteratorWrapper;

pub struct ImageIteratorWrapper {
    alloc: HandleAllocator,

    /// The iterator over the render state's image placements. It reads
    /// the render state's memory, so it is only valid until the next
    /// update.
    inner: renderpkg::ImagePlacementIterator<'static>,

    /// The placement returned by the last `next`, if any.
    current: Option<renderpkg::ImagePlacement>,
}

crate::c_api_enum! {
    /// C: GhosttyRenderStateDirty (Zig: `renderpkg.RenderState.Dirty`).
    pub enum Dirty {
        False = 0,
        Partial = 1,
        Full = 2,
    }
}

impl Dirty {
    pub fn from_zig(d: renderpkg::Dirty) -> Dirty {
        match d {
            renderpkg::Dirty::False => Dirty::False,
            renderpkg::Dirty::Partial => Dirty::Partial,
            renderpkg::Dirty::Full => Dirty::Full,
        }
    }

    pub fn to_zig(self) -> renderpkg::Dirty {
        match self {
            Dirty::False => renderpkg::Dirty::False,
            Dirty::Partial => renderpkg::Dirty::Partial,
            Dirty::Full => renderpkg::Dirty::Full,
        }
    }
}

/// C: GhosttyRenderStateRowSelection
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSelection {
    pub size: usize,
    pub start_x: u16,
    pub end_x: u16,
}

impl Default for RowSelection {
    fn default() -> Self {
        RowSelection {
            size: size_of::<RowSelection>(),
            start_x: 0,
            end_x: 0,
        }
    }
}

/// C: GhosttyRenderStateOverscan
///
/// This uses `u16` rather than `size.CellCountInt` so that the C layout
/// stays fixed even if the Zig type changes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Overscan {
    /// Rows above the viewport.
    pub above: u16,

    /// Rows below the viewport.
    pub below: u16,
}

impl Overscan {
    fn init(v: renderpkg::Overscan) -> Overscan {
        Overscan {
            above: v.above,
            below: v.below,
        }
    }
}

/// C: GhosttyRenderStateClip
///
/// This uses `u16` rather than `size.CellCountInt` so that the C layout
/// stays fixed even if the Zig type changes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Clip {
    /// The first viewport row.
    pub y: u16,

    /// The number of rows starting at `y`.
    pub rows: u16,

    /// The number of columns from the left edge.
    pub cols: u16,
}

impl Clip {
    fn init(v: renderpkg::Clip) -> Clip {
        Clip {
            y: v.y,
            rows: v.rows,
            cols: v.cols,
        }
    }
}

/// C: GhosttyRenderStateRowId
///
/// Opaque to C, where only equality is documented. The encoding is
/// internal and may change: `bits[0]` is the page serial and `bits[1]`
/// is the row index within the page plus one. The plus one guarantees
/// that the all-zero value is never a valid id, which the C docs promise.
/// The unused high bits of `bits[1]` are free for future use.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RowId {
    pub bits: [u64; 2],
}

impl RowId {
    pub fn init(id: renderpkg::RowId) -> RowId {
        RowId {
            bits: [id.serial, id.y as u64 + 1],
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttyRenderStateCursorVisualStyle
    pub enum CursorVisualStyle {
        Bar = 0,
        Block = 1,
        Underline = 2,
        BlockHollow = 3,
    }
}

impl CursorVisualStyle {
    pub fn from_cursor_style(s: cursorpkg::Style) -> CursorVisualStyle {
        match s {
            cursorpkg::Style::Bar => CursorVisualStyle::Bar,
            cursorpkg::Style::Block => CursorVisualStyle::Block,
            cursorpkg::Style::Underline => CursorVisualStyle::Underline,
            cursorpkg::Style::BlockHollow => CursorVisualStyle::BlockHollow,
        }
    }
}

/// C: GhosttyRenderStateCursor
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub size: usize,
    pub viewport_has_value: bool,
    pub viewport_x: u16,
    pub viewport_y: u16,
    pub wide_tail: bool,
    pub visible: bool,
    pub blinking: bool,
    pub password_input: bool,
    pub visual_style: CursorVisualStyle,
}

crate::c_api_enum! {
    /// C: GhosttyRenderStateData
    ///
    /// Output types: `cols`/`rows`/`cursor_viewport_x`/`cursor_viewport_y`
    /// → `u16`, `dirty` → [`Dirty`], `row_iterator` → [`RowIterator`]
    /// (an existing iterator, populated in place), `color_*` →
    /// `GhosttyColorRgb` (`color_cursor_has_value` → `bool`),
    /// `color_palette` → `GhosttyColorRgb[256]`, `cursor_visual_style` →
    /// [`CursorVisualStyle`], the other `cursor_*` → `bool`, `cursor` →
    /// [`Cursor`], `colors` → [`Colors`], `overscan`/`overscan_request` →
    /// [`Overscan`], `image_iterator` → [`ImageIterator`],
    /// `image_placements_generation` → `u64`, `clip`/`clip_request` →
    /// [`Clip`].
    pub enum Data {
        Invalid = 0,
        Cols = 1,
        Rows = 2,
        Dirty = 3,
        RowIterator = 4,
        ColorBackground = 5,
        ColorForeground = 6,
        ColorCursor = 7,
        ColorCursorHasValue = 8,
        ColorPalette = 9,
        CursorVisualStyle = 10,
        CursorVisible = 11,
        CursorBlinking = 12,
        CursorPasswordInput = 13,
        CursorViewportHasValue = 14,
        CursorViewportX = 15,
        CursorViewportY = 16,
        CursorViewportWideTail = 17,
        Cursor = 18,
        Colors = 19,
        Overscan = 20,
        OverscanRequest = 21,
        ImageIterator = 22,
        ImagePlacementsGeneration = 23,
        Clip = 24,
        ClipRequest = 25,
    }
}

crate::c_api_enum! {
    /// C: GhosttyRenderStateOption
    ///
    /// Input types: `dirty` → [`Dirty`], `overscan` → [`Overscan`],
    /// `clip` → [`Clip`].
    pub enum SetOption {
        Dirty = 0,
        Overscan = 1,
        Clip = 2,
    }
}

/// C: GhosttyRenderStateColors
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colors {
    pub size: usize,
    pub background: RgbC,
    pub foreground: RgbC,
    pub cursor: RgbC,
    pub cursor_has_value: bool,
    pub palette: colorpkg::PaletteC,
}

/// C: ghostty_render_state_new
///
/// # Safety
/// The allocator (if any) must outlive the render state.
#[export_name = "ghostty_render_state_new"]
pub unsafe extern "C" fn new(alloc_: Option<&CAllocator>, result: &mut RenderState) -> Result {
    match new_(alloc_) {
        Some(ptr) => {
            *result = ptr;
            Result::Success
        }
        None => {
            *result = std::ptr::null_mut();
            Result::OutOfMemory
        }
    }
}

fn new_(alloc_: Option<&CAllocator>) -> Option<*mut RenderStateWrapper> {
    let alloc = HandleAllocator::new(alloc_);
    let ptr = alloc.create(RenderStateWrapper {
        alloc,
        state: renderpkg::RenderState::empty(),
        overscan_request: renderpkg::Overscan::default(),
        clip_request: renderpkg::Clip::default(),
    });
    if ptr.is_null() {
        None
    } else {
        Some(ptr)
    }
}

/// C: ghostty_render_state_free
///
/// # Safety
/// `state_` must be NULL or a render state not used afterwards.
#[export_name = "ghostty_render_state_free"]
pub unsafe extern "C" fn free(state_: RenderState) {
    let Some(state) = state_.as_ref() else {
        return;
    };
    let alloc = state.alloc;
    alloc.destroy(state_);
}

/// C: ghostty_render_state_update
///
/// # Safety
/// Handles must be NULL or live.
#[export_name = "ghostty_render_state_update"]
pub unsafe extern "C" fn update(state_: RenderState, terminal_: terminal_c::Terminal) -> Result {
    let Some(state) = state_.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(t) = terminal_c::zig_terminal(terminal_) else {
        return Result::InvalidValue;
    };

    state.apply_options();
    state.state.update(t);
    Result::Success
}

/// C: ghostty_render_state_begin_update
///
/// # Safety
/// Handles must be NULL or live.
#[export_name = "ghostty_render_state_begin_update"]
pub unsafe extern "C" fn begin_update(state_: RenderState, terminal_: terminal_c::Terminal) -> Result {
    let Some(state) = state_.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(t) = terminal_c::zig_terminal(terminal_) else {
        return Result::InvalidValue;
    };

    state.apply_options();
    state.state.begin_update(t);
    Result::Success
}

/// C: ghostty_render_state_end_update
///
/// # Safety
/// `state_` must be NULL or live.
#[export_name = "ghostty_render_state_end_update"]
pub unsafe extern "C" fn end_update(state_: RenderState) -> Result {
    let Some(state) = state_.as_mut() else {
        return Result::InvalidValue;
    };
    state.state.end_update();
    Result::Success
}

/// C: ghostty_render_state_clean
///
/// # Safety
/// `state_` must be NULL or live.
#[export_name = "ghostty_render_state_clean"]
pub unsafe extern "C" fn clean(state_: RenderState) -> Result {
    let Some(state) = state_.as_mut() else {
        return Result::InvalidValue;
    };
    state.state.clean();
    Result::Success
}

/// C: ghostty_render_state_get
///
/// # Safety
/// `state_` must be NULL or live; `out` must be NULL or point to the data
/// kind's output type.
#[export_name = "ghostty_render_state_get"]
pub unsafe extern "C" fn get(state_: RenderState, data: c_int, out: *mut c_void) -> Result {
    let Some(state) = state_.as_mut() else {
        return Result::InvalidValue;
    };
    get_dispatch(state, data, out)
}

/// C: ghostty_render_state_get_multi
///
/// # Safety
/// `keys` and `values` must be NULL or valid for `count` entries; see
/// [`get`].
#[export_name = "ghostty_render_state_get_multi"]
pub unsafe extern "C" fn get_multi(
    state_: RenderState,
    count: usize,
    keys: *const c_int,
    values: *const *mut c_void,
    out_written: *mut usize,
) -> Result {
    if keys.is_null() || values.is_null() {
        return Result::InvalidValue;
    }

    // Unwrap the handle once for the whole batch rather than per key.
    // A null handle fails on the first key, matching the per-call
    // behavior.
    let mut state = state_.as_mut();

    for i in 0..count {
        let result = match state.as_deref_mut() {
            Some(s) => get_dispatch(s, *keys.add(i), *values.add(i)),
            None => Result::InvalidValue,
        };
        if result != Result::Success {
            if let Some(w) = out_written.as_mut() {
                *w = i;
            }
            return result;
        }
    }
    if let Some(w) = out_written.as_mut() {
        *w = count;
    }
    Result::Success
}

#[inline]
unsafe fn get_dispatch(state: &mut RenderStateWrapper, data: c_int, out: *mut c_void) -> Result {
    let Some(data) = Data::from_int(data) else {
        log::warn!(target: LOG_TARGET, "render_state_get invalid data value={data}");
        return Result::InvalidValue;
    };

    if out.is_null() {
        return Result::InvalidValue;
    }
    match data {
        Data::Invalid => Result::InvalidValue,
        _ => get_typed(state, data, out),
    }
}

unsafe fn get_typed(state: &mut RenderStateWrapper, data: Data, out: *mut c_void) -> Result {
    macro_rules! write {
        ($ty:ty, $v:expr) => {
            *(out as *mut $ty) = $v
        };
    }

    let s = &mut state.state;
    match data {
        Data::Invalid => return Result::InvalidValue,
        Data::Cols => write!(CellCountInt, s.cols),
        Data::Rows => write!(CellCountInt, s.rows),
        Data::Dirty => write!(Dirty, Dirty::from_zig(s.dirty)),
        Data::RowIterator => {
            let Some(it) = (*(out as *mut RowIterator)).as_mut() else {
                return Result::InvalidValue;
            };

            // Only the captured rows hold data from the last update, so
            // slice to them. Without overscan this is the whole list.
            let range = s.row_data_range();
            let r = range.start..range.end;
            let row_data = &mut s.row_data;
            *it = RowIteratorWrapper {
                alloc: it.alloc,
                y: POSITION_NONE,
                raws: &row_data.raw[r.clone()],
                cells: &row_data.cells[r.clone()],
                selection: &row_data.selection[r.clone()],
                dirty: &mut row_data.dirty[r.clone()],
                pins: &row_data.pin[r.clone()],
                serials: &row_data.serial[r],
                state_dirty: &s.dirty,
                palette: &s.colors.palette,
                viewport_y_base: s.clip.y as i32 - s.overscan.above as i32,
            };
        }
        Data::ColorBackground => write!(RgbC, s.colors.background.cval()),
        Data::ColorForeground => write!(RgbC, s.colors.foreground.cval()),
        Data::ColorCursor => {
            let Some(cursor) = s.colors.cursor else {
                return Result::InvalidValue;
            };
            write!(RgbC, cursor.cval());
        }
        Data::ColorCursorHasValue => write!(bool, s.colors.cursor.is_some()),
        Data::ColorPalette => write!(colorpkg::PaletteC, colorpkg::palette_cval(&s.colors.palette)),
        Data::CursorVisualStyle => write!(
            CursorVisualStyle,
            CursorVisualStyle::from_cursor_style(s.cursor.visual_style)
        ),
        Data::CursorVisible => write!(bool, s.cursor.visible),
        Data::CursorBlinking => write!(bool, s.cursor.blinking),
        Data::CursorPasswordInput => write!(bool, s.cursor.password_input),
        Data::CursorViewportHasValue => write!(bool, s.cursor.viewport.is_some()),
        Data::CursorViewportX => {
            let Some(vp) = s.cursor.viewport else {
                return Result::InvalidValue;
            };
            write!(CellCountInt, vp.x);
        }
        Data::CursorViewportY => {
            let Some(vp) = s.cursor.viewport else {
                return Result::InvalidValue;
            };
            write!(CellCountInt, vp.y);
        }
        Data::CursorViewportWideTail => {
            let Some(vp) = s.cursor.viewport else {
                return Result::InvalidValue;
            };
            write!(bool, vp.wide_tail);
        }
        Data::Cursor => return write_cursor(state, out as *mut Cursor),
        Data::Colors => return write_colors(state, out as *mut Colors),
        Data::Overscan => write!(Overscan, Overscan::init(s.overscan)),
        Data::OverscanRequest => write!(Overscan, Overscan::init(state.overscan_request)),
        Data::ImageIterator => {
            let Some(it) = (*(out as *mut ImageIterator)).as_mut() else {
                return Result::InvalidValue;
            };
            // SAFETY: the iterator reads the render state's memory and
            // is only valid until the next update (C API contract).
            let inner: renderpkg::ImagePlacementIterator<'static> =
                std::mem::transmute(s.image_placement_iterator());
            *it = ImageIteratorWrapper {
                alloc: it.alloc,
                inner,
                current: None,
            };
        }
        Data::ImagePlacementsGeneration => write!(u64, s.image_placements_generation()),
        Data::Clip => write!(Clip, Clip::init(s.clip)),
        Data::ClipRequest => write!(Clip, Clip::init(state.clip_request)),
    }

    Result::Success
}

/// C: ghostty_render_state_set
///
/// # Safety
/// `state_` must be NULL or live; `value` must be NULL or point to the
/// option's input type.
#[export_name = "ghostty_render_state_set"]
pub unsafe extern "C" fn set(state_: RenderState, option: c_int, value: *const c_void) -> Result {
    let Some(option) = SetOption::from_int(option) else {
        log::warn!(target: LOG_TARGET, "render_state_set invalid option value={option}");
        return Result::InvalidValue;
    };

    if value.is_null() {
        return Result::InvalidValue;
    }
    set_typed(state_, option, value)
}

unsafe fn set_typed(state_: RenderState, option: SetOption, value: *const c_void) -> Result {
    let Some(state) = state_.as_mut() else {
        return Result::InvalidValue;
    };
    match option {
        SetOption::Dirty => {
            let Some(dirty) = Dirty::from_int(*(value as *const c_int)) else {
                return Result::InvalidValue;
            };
            state.state.dirty = dirty.to_zig();
        }

        // Applied by the next update. See the field docs for why.
        SetOption::Overscan => {
            let v = &*(value as *const Overscan);
            state.overscan_request = renderpkg::Overscan {
                above: v.above,
                below: v.below,
            };
        }
        SetOption::Clip => {
            let v = &*(value as *const Clip);
            state.clip_request = renderpkg::Clip {
                y: v.y,
                rows: v.rows,
                cols: v.cols,
            };
        }
    }

    Result::Success
}

unsafe fn write_cursor(state: &RenderStateWrapper, out_cursor: *mut Cursor) -> Result {
    use std::ptr::addr_of_mut;

    let out_size = std::ptr::addr_of!((*out_cursor).size).read();
    if out_size < size_of::<usize>() {
        return Result::InvalidValue;
    }

    let cursor = &state.state.cursor;
    if struct_sized_field_fits!(Cursor, out_size, viewport_has_value) {
        addr_of_mut!((*out_cursor).viewport_has_value).write(cursor.viewport.is_some());
    }

    if let Some(viewport) = cursor.viewport {
        if struct_sized_field_fits!(Cursor, out_size, viewport_x) {
            addr_of_mut!((*out_cursor).viewport_x).write(viewport.x);
        }

        if struct_sized_field_fits!(Cursor, out_size, viewport_y) {
            addr_of_mut!((*out_cursor).viewport_y).write(viewport.y);
        }

        if struct_sized_field_fits!(Cursor, out_size, wide_tail) {
            addr_of_mut!((*out_cursor).wide_tail).write(viewport.wide_tail);
        }
    }

    if struct_sized_field_fits!(Cursor, out_size, visible) {
        addr_of_mut!((*out_cursor).visible).write(cursor.visible);
    }

    if struct_sized_field_fits!(Cursor, out_size, blinking) {
        addr_of_mut!((*out_cursor).blinking).write(cursor.blinking);
    }

    if struct_sized_field_fits!(Cursor, out_size, password_input) {
        addr_of_mut!((*out_cursor).password_input).write(cursor.password_input);
    }

    if struct_sized_field_fits!(Cursor, out_size, visual_style) {
        addr_of_mut!((*out_cursor).visual_style).write(CursorVisualStyle::from_cursor_style(cursor.visual_style));
    }

    Result::Success
}

unsafe fn write_colors(state: &RenderStateWrapper, out_colors: *mut Colors) -> Result {
    use std::ptr::addr_of_mut;

    let out_size = std::ptr::addr_of!((*out_colors).size).read();
    if out_size < size_of::<usize>() {
        return Result::InvalidValue;
    }

    let colors = &state.state.colors;
    if struct_sized_field_fits!(Colors, out_size, background) {
        addr_of_mut!((*out_colors).background).write(colors.background.cval());
    }

    if struct_sized_field_fits!(Colors, out_size, foreground) {
        addr_of_mut!((*out_colors).foreground).write(colors.foreground.cval());
    }

    if let Some(cursor) = colors.cursor {
        if struct_sized_field_fits!(Colors, out_size, cursor) {
            addr_of_mut!((*out_colors).cursor).write(cursor.cval());
        }
    }

    if struct_sized_field_fits!(Colors, out_size, cursor_has_value) {
        addr_of_mut!((*out_colors).cursor_has_value).write(colors.cursor.is_some());
    }

    {
        let palette_offset = offset_of!(Colors, palette);
        if out_size > palette_offset {
            let available = out_size - palette_offset;
            let max_entries = colors.palette.len().min(available / size_of::<RgbC>());
            let dst = std::slice::from_raw_parts_mut(addr_of_mut!((*out_colors).palette) as *mut RgbC, max_entries);
            colorpkg::palette_cval_slice(&colors.palette[..max_entries], dst);
        }
    }

    Result::Success
}

/// C: ghostty_render_state_row_iterator_new
///
/// # Safety
/// The allocator (if any) must outlive the iterator.
#[export_name = "ghostty_render_state_row_iterator_new"]
pub unsafe extern "C" fn row_iterator_new(alloc_: Option<&CAllocator>, result: &mut RowIterator) -> Result {
    let alloc = HandleAllocator::new(alloc_);
    let ptr = alloc.create(RowIteratorWrapper {
        alloc,
        y: POSITION_NONE,
        raws: empty_slice::<page::Row>(),
        cells: empty_slice::<renderpkg::Cells>(),
        selection: empty_slice::<Option<[CellCountInt; 2]>>(),
        dirty: empty_slice::<bool>(),
        pins: empty_slice::<Pin>(),
        serials: empty_slice::<u64>(),
        state_dirty: std::ptr::null(),
        palette: std::ptr::null(),
        viewport_y_base: 0,
    });
    if ptr.is_null() {
        *result = std::ptr::null_mut();
        return Result::OutOfMemory;
    }
    *result = ptr;
    Result::Success
}

/// C: ghostty_render_state_row_iterator_free
///
/// # Safety
/// `iterator_` must be NULL or an iterator not used afterwards.
#[export_name = "ghostty_render_state_row_iterator_free"]
pub unsafe extern "C" fn row_iterator_free(iterator_: RowIterator) {
    let Some(iterator) = iterator_.as_ref() else {
        return;
    };
    let alloc = iterator.alloc;
    alloc.destroy(iterator_);
}

/// C: ghostty_render_state_row_iterator_next
///
/// # Safety
/// `iterator_` must be NULL or live.
#[export_name = "ghostty_render_state_row_iterator_next"]
pub unsafe extern "C" fn row_iterator_next(iterator_: RowIterator) -> bool {
    let Some(it) = iterator_.as_mut() else {
        return false;
    };
    // The none sentinel wraps to zero.
    let next_y = it.y.wrapping_add(1);
    if next_y >= it.raws.len() {
        return false;
    }
    it.y = next_y;
    true
}

/// C: ghostty_render_state_row_iterator_next_dirty
///
/// # Safety
/// `iterator_` must be NULL or live, populated from a live render state.
#[export_name = "ghostty_render_state_row_iterator_next_dirty"]
pub unsafe extern "C" fn row_iterator_next_dirty(iterator_: RowIterator, out_y: *mut CellCountInt) -> bool {
    let Some(it) = iterator_.as_mut() else {
        return false;
    };
    let Some(y_out) = out_y.as_mut() else {
        return false;
    };
    let Some(state_dirty) = it.state_dirty.as_ref() else {
        return false;
    };

    match *state_dirty {
        renderpkg::Dirty::False => false,
        renderpkg::Dirty::Full => {
            // The none sentinel wraps to zero.
            let next_y = it.y.wrapping_add(1);
            if next_y >= it.raws.len() {
                return false;
            }
            it.y = next_y;
            *y_out = next_y as CellCountInt;
            true
        }
        renderpkg::Dirty::Partial => {
            let dirty = &*it.dirty;
            let mut next_y = it.y.wrapping_add(1);
            while next_y < it.raws.len() {
                if dirty[next_y] {
                    it.y = next_y;
                    *y_out = next_y as CellCountInt;
                    return true;
                }
                next_y += 1;
            }
            false
        }
    }
}

/// C: ghostty_render_state_row_cells_new
///
/// # Safety
/// The allocator (if any) must outlive the wrapper.
#[export_name = "ghostty_render_state_row_cells_new"]
pub unsafe extern "C" fn row_cells_new(alloc_: Option<&CAllocator>, result: &mut RowCells) -> Result {
    let alloc = HandleAllocator::new(alloc_);
    let ptr = alloc.create(RowCellsWrapper {
        alloc,
        x: POSITION_NONE,
        cells: std::ptr::null(),
        selection: None,
        palette: std::ptr::null(),
    });
    if ptr.is_null() {
        *result = std::ptr::null_mut();
        return Result::OutOfMemory;
    }
    *result = ptr;
    Result::Success
}

/// C: ghostty_render_state_row_cells_next
///
/// # Safety
/// `cells_` must be NULL or live.
#[export_name = "ghostty_render_state_row_cells_next"]
pub unsafe extern "C" fn row_cells_next(cells_: RowCells) -> bool {
    let Some(cells) = cells_.as_mut() else {
        return false;
    };
    // The none sentinel wraps to zero.
    let next_x = cells.x.wrapping_add(1);
    if next_x >= cells.raws().len() {
        return false;
    }
    cells.x = next_x;
    true
}

/// C: ghostty_render_state_row_cells_select
///
/// # Safety
/// `cells_` must be NULL or live.
#[export_name = "ghostty_render_state_row_cells_select"]
pub unsafe extern "C" fn row_cells_select(cells_: RowCells, x: CellCountInt) -> Result {
    let Some(cells) = cells_.as_mut() else {
        return Result::InvalidValue;
    };
    if x as usize >= cells.raws().len() {
        return Result::InvalidValue;
    }
    cells.x = x as usize;
    Result::Success
}

/// C: ghostty_render_state_row_cells_free
///
/// # Safety
/// `cells_` must be NULL or a wrapper not used afterwards.
#[export_name = "ghostty_render_state_row_cells_free"]
pub unsafe extern "C" fn row_cells_free(cells_: RowCells) {
    let Some(cells) = cells_.as_ref() else {
        return;
    };
    let alloc = cells.alloc;
    alloc.destroy(cells_);
}

crate::c_api_enum! {
    /// C: GhosttyRenderStateRowCellsData
    ///
    /// Output types: `raw` → `GhosttyCell`, `style` → `GhosttyStyle`,
    /// `graphemes_len` → `u32`, `graphemes_buf` → `u32[]`,
    /// `bg_color`/`fg_color` → `GhosttyColorRgb`,
    /// `selected`/`has_styling` → `bool`, `graphemes_utf8` →
    /// `GhosttyBuffer`.
    pub enum RowCellsData {
        Invalid = 0,
        Raw = 1,
        Style = 2,
        GraphemesLen = 3,
        GraphemesBuf = 4,
        BgColor = 5,
        FgColor = 6,
        Selected = 7,
        HasStyling = 8,
        GraphemesUtf8 = 9,
    }
}

/// C: ghostty_render_state_row_cells_get
///
/// # Safety
/// `cells_` must be NULL or live; `out` must be NULL or point to the data
/// kind's output type.
#[export_name = "ghostty_render_state_row_cells_get"]
pub unsafe extern "C" fn row_cells_get(cells_: RowCells, data: c_int, out: *mut c_void) -> Result {
    let Some(cells) = cells_.as_ref() else {
        return Result::InvalidValue;
    };
    let x = cells.x;
    if x >= cells.raws().len() {
        return Result::InvalidValue;
    }
    row_cells_get_dispatch(cells, x, data, out)
}

/// C: ghostty_render_state_row_cells_get_multi
///
/// # Safety
/// `keys` and `values` must be NULL or valid for `count` entries; see
/// [`row_cells_get`].
#[export_name = "ghostty_render_state_row_cells_get_multi"]
pub unsafe extern "C" fn row_cells_get_multi(
    cells_: RowCells,
    count: usize,
    keys: *const c_int,
    values: *const *mut c_void,
    out_written: *mut usize,
) -> Result {
    if keys.is_null() || values.is_null() {
        return Result::InvalidValue;
    }

    // Unwrap the handle and position once for the whole batch rather
    // than per key. An invalid handle/position fails on the first
    // key, matching the per-call behavior.
    let unwrapped: Option<(&RowCellsWrapper, usize)> = 'valid: {
        let Some(cells) = cells_.as_ref() else {
            break 'valid None;
        };
        let x = cells.x;
        if x >= cells.raws().len() {
            break 'valid None;
        }
        Some((cells, x))
    };

    for i in 0..count {
        let result = match unwrapped {
            Some((c, x)) => row_cells_get_dispatch(c, x, *keys.add(i), *values.add(i)),
            None => Result::InvalidValue,
        };
        if result != Result::Success {
            if let Some(w) = out_written.as_mut() {
                *w = i;
            }
            return result;
        }
    }
    if let Some(w) = out_written.as_mut() {
        *w = count;
    }
    Result::Success
}

/// C: GhosttyRenderStateCell
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CopiedCell {
    pub content: u32,
    pub style: u16,
    pub grapheme: u16,
    pub content_tag: u8,
    pub wide: u8,
    pub flags: u8,
    pub semantic_content: u8,
}

impl CopiedCell {
    pub const HYPERLINK: u8 = 1 << 0;
    pub const PROTECTED: u8 = 1 << 1;
}

/// C: GhosttyRenderStateGrapheme
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CopiedGrapheme {
    pub offset: u32,
    pub len: u32,
}

/// C: GhosttyRenderStateRowCellsCopy
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RowCellsCopy {
    pub size: usize,
    pub cells: *mut CopiedCell,
    pub cells_cap: usize,
    pub cells_len: usize,
    pub styles: *mut style_c::Style,
    pub styles_cap: usize,
    pub styles_len: usize,
    pub graphemes: *mut CopiedGrapheme,
    pub graphemes_cap: usize,
    pub graphemes_len: usize,
    pub grapheme_bytes: *mut u8,
    pub grapheme_bytes_cap: usize,
    pub grapheme_bytes_len: usize,
    pub flags: u32,
}

impl Default for RowCellsCopy {
    fn default() -> Self {
        RowCellsCopy {
            size: size_of::<RowCellsCopy>(),
            cells: std::ptr::null_mut(),
            cells_cap: 0,
            cells_len: 0,
            styles: std::ptr::null_mut(),
            styles_cap: 0,
            styles_len: 0,
            graphemes: std::ptr::null_mut(),
            graphemes_cap: 0,
            graphemes_len: 0,
            grapheme_bytes: std::ptr::null_mut(),
            grapheme_bytes_cap: 0,
            grapheme_bytes_len: 0,
            flags: 0,
        }
    }
}

impl RowCellsCopy {
    pub const TRIM: u32 = 1 << 0;

    /// The size of the struct before `flags` was added. Callers built
    /// against it still copy, without flags.
    const SIZE_WITHOUT_FLAGS: usize = offset_of!(RowCellsCopy, flags);
}

/// Slots in the direct-mapped style cache of `row_cells_copy`. A
/// collision only costs a duplicate style table entry.
const COPY_STYLE_SLOTS: usize = 256;
const COPY_STYLE_SLOT_EMPTY: u32 = u32::MAX;

/// Reads a field of a raw cell straight from its bits.
#[inline(always)]
fn cell_field(bits: u64, f: page::BitField) -> u64 {
    (bits & f.mask()) >> f.offset
}

/// Bytes 8..12 of a copied cell after the content tag (`wide`, `flags`
/// and `semantic_content`, as a little-endian `u32`), by the six cell
/// bits from `wide` to `semantic_content`.
const COPY_HIGH: [u32; 64] = {
    let mut t = [0u32; 64];
    let mut v = 0;
    while v < 64 {
        let wide = v & 0b11;
        let protected = (v >> 2) & 1;
        let hyperlink = (v >> 3) & 1;
        let semantic = (v >> 4) & 0b11;
        let flags = hyperlink * CopiedCell::HYPERLINK as usize + protected * CopiedCell::PROTECTED as usize;
        t[v] = ((wide << 8) | (flags << 16) | (semantic << 24)) as u32;
        v += 1;
    }
    t
};

// `write_copied` stores a copied cell as one `u64` and one `u32`.
const _: () = {
    assert!(offset_of!(CopiedCell, content) == 0);
    assert!(offset_of!(CopiedCell, style) == 4);
    assert!(offset_of!(CopiedCell, grapheme) == 6);
    assert!(offset_of!(CopiedCell, content_tag) == 8);
    assert!(offset_of!(CopiedCell, wide) == 9);
    assert!(offset_of!(CopiedCell, flags) == 10);
    assert!(offset_of!(CopiedCell, semantic_content) == 11);
    assert!(size_of::<CopiedCell>() == 12);
    {
        use page::cell_fields as f;
        assert!(f::WIDE.offset == 42 && f::WIDE.bits == 2);
        assert!(f::PROTECTED.offset == 44 && f::HYPERLINK.offset == 45);
        assert!(f::SEMANTIC_CONTENT.offset == 46 && f::SEMANTIC_CONTENT.bits == 2);
    }
};

/// The copied form of a raw cell as its first eight bytes (`content`,
/// `style`, `grapheme`) and last four (`content_tag`, `wide`, `flags`,
/// `semantic_content`), built from the cell bits with shifts, masks and
/// a table. Zig reads each field through its enum and stores it on its
/// own; here that costs a jump table per cell (`semantic_content` has a
/// panicking arm for its unused value) and eight stores.
#[inline(always)]
fn copied_cell(bits: u64, style: u16, grapheme: u16) -> (u64, u32) {
    use page::cell_fields as f;
    let tag = cell_field(bits, f::CONTENT_TAG) as u32;
    let raw = cell_field(bits, f::CONTENT) as u32;
    let content = if tag & 0b10 == 0 {
        // codepoint, codepoint_grapheme
        raw & 0x1F_FFFF
    } else if tag == ContentTag::BgColorPalette as u32 {
        raw & 0xFF
    } else {
        // The cell holds r, g, b from the low byte up; the copy holds
        // 0xRRGGBB.
        ((raw & 0xFF) << 16) | (raw & 0xFF00) | (raw >> 16)
    };
    let low = content as u64 | (style as u64) << 32 | (grapheme as u64) << 48;
    let high = tag | COPY_HIGH[(bits >> f::WIDE.offset) as usize & 0x3F];
    (low, high)
}

/// Writes a copied cell given as `copied_cell` returns it.
///
/// # Safety
/// `out` must be valid for a `CopiedCell` write.
#[inline(always)]
unsafe fn write_copied(out: *mut CopiedCell, (low, high): (u64, u32)) {
    if cfg!(target_endian = "little") {
        let p = out as *mut u8;
        (p as *mut u64).write_unaligned(low);
        (p.add(8) as *mut u32).write_unaligned(high);
    } else {
        out.write(CopiedCell {
            content: low as u32,
            style: (low >> 32) as u16,
            grapheme: (low >> 48) as u16,
            content_tag: high as u8,
            wide: (high >> 8) as u8,
            flags: (high >> 16) as u8,
            semantic_content: (high >> 24) as u8,
        });
    }
}

/// The style table that `row_cells_copy` fills.
struct CopyStyles {
    out: *mut style_c::Style,
    cap: usize,
    len: usize,
    /// The direct-mapped style cache, filled on first use so rows
    /// without styles don't pay for it.
    slots: std::mem::MaybeUninit<[u32; COPY_STYLE_SLOTS]>,
    slots_ready: bool,
}

impl CopyStyles {
    /// The style table index of the styled cell at `cx`: the previous
    /// styled cell's index if they have the same style, else the
    /// cached index for an equal style, else a new entry.
    #[inline(never)]
    fn index(&mut self, styles: &[style::Style], cx: usize, previous_x: usize, previous_style: u16) -> u16 {
        let s = &styles[cx];
        if previous_x != POSITION_NONE && styles[previous_x].eql(s) {
            return previous_style;
        }
        if !self.slots_ready {
            self.slots.write([COPY_STYLE_SLOT_EMPTY; COPY_STYLE_SLOTS]);
            self.slots_ready = true;
        }
        // SAFETY: initialized just above.
        let slots = unsafe { self.slots.assume_init_mut() };
        let slot = &mut slots[(s.hash() as usize) % COPY_STYLE_SLOTS];
        if *slot != COPY_STYLE_SLOT_EMPTY && styles[(*slot >> 16) as usize].eql(s) {
            return *slot as u16;
        }
        let index = self.len as u16;
        if self.len < self.cap && !self.out.is_null() {
            // SAFETY: the caller's style buffer holds `cap` entries.
            unsafe { self.out.add(self.len).write(style_c::Style::from_style(s)) };
        }
        self.len += 1;
        *slot = ((cx as u32) << 16) | index as u32;
        index
    }
}

/// The grapheme table and bytes that `row_cells_copy` fills.
struct CopyGraphemes {
    out: *mut CopiedGrapheme,
    cap: usize,
    len: usize,
    bytes: *mut u8,
    bytes_cap: usize,
    bytes_len: usize,
}

impl CopyGraphemes {
    /// Appends the grapheme of `cell` and returns its 1-based index.
    #[inline(never)]
    fn push(&mut self, cell: page::Cell, extra: &[u32]) -> std::result::Result<u16, Result> {
        let fits = self.bytes_len < self.bytes_cap && !self.bytes.is_null();
        let mut buffer = Buffer {
            // SAFETY: the caller's byte buffer holds `bytes_cap` bytes.
            ptr: if fits { unsafe { self.bytes.add(self.bytes_len) } } else { std::ptr::null_mut() },
            cap: if fits { self.bytes_cap - self.bytes_len } else { 0 },
            len: 0,
        };
        match row_cells_get_graphemes_utf8(cell, extra, &mut buffer) {
            Result::Success | Result::OutOfSpace => {}
            result => return Err(result),
        }
        if self.len < self.cap && self.bytes_len + buffer.len <= self.bytes_cap && !self.out.is_null() {
            // SAFETY: the caller's grapheme buffer holds `cap` entries.
            unsafe {
                self.out.add(self.len).write(CopiedGrapheme {
                    offset: self.bytes_len as u32,
                    len: buffer.len as u32,
                })
            };
        }
        self.len += 1;
        self.bytes_len += buffer.len;
        Ok(self.len as u16)
    }
}

/// C: ghostty_render_state_row_cells_copy
///
/// # Safety
/// `cells_` must be NULL or live; `out_` must be NULL or point to a
/// sized struct of at least `out_.size` bytes whose buffers hold their
/// capacities.
#[export_name = "ghostty_render_state_row_cells_copy"]
pub unsafe extern "C" fn row_cells_copy(
    cells_: RowCells,
    x: CellCountInt,
    len: CellCountInt,
    out_: *mut RowCellsCopy,
) -> Result {
    use std::ptr::{addr_of, addr_of_mut};

    let Some(cells) = cells_.as_ref() else {
        return Result::InvalidValue;
    };
    if out_.is_null() {
        return Result::InvalidValue;
    }

    // The caller's struct may end before `flags`, so its fields are read
    // and written through raw field pointers.
    let out_size = addr_of!((*out_).size).read();
    if out_size < RowCellsCopy::SIZE_WITHOUT_FLAGS {
        return Result::InvalidValue;
    }
    let raws = cells.raws();
    if x as usize > raws.len() {
        return Result::InvalidValue;
    }
    let flags: u32 = if out_size >= RowCellsCopy::SIZE_WITHOUT_FLAGS + size_of::<u32>() {
        addr_of!((*out_).flags).read()
    } else {
        0
    };
    let out_cells = addr_of!((*out_).cells).read();
    let cells_cap = addr_of!((*out_).cells_cap).read();

    let start: usize = x as usize;
    let end: usize = if flags & RowCellsCopy::TRIM != 0 {
        trimmed_end(raws, start, (start + len as usize).min(raws.len()))
    } else {
        (start + len as usize).min(raws.len())
    };

    let mut styles = CopyStyles {
        out: addr_of!((*out_).styles).read(),
        cap: addr_of!((*out_).styles_cap).read(),
        len: 1,
        slots: std::mem::MaybeUninit::uninit(),
        slots_ready: false,
    };
    if styles.cap >= 1 && !styles.out.is_null() {
        styles.out.write(style_c::Style::DEFAULT);
    }
    let mut graphemes = CopyGraphemes {
        out: addr_of!((*out_).graphemes).read(),
        cap: addr_of!((*out_).graphemes_cap).read(),
        len: 0,
        bytes: addr_of!((*out_).grapheme_bytes).read(),
        bytes_cap: addr_of!((*out_).grapheme_bytes_cap).read(),
        bytes_len: 0,
    };

    if end > start {
        // A non-empty range means the wrapper was populated.
        let row: &renderpkg::Cells = cells.cells();
        let cells_len = end - start;
        // The cells that fit the caller's buffer are written; the rest
        // are only counted (with their styles and graphemes) so the
        // caller learns the sizes it needs.
        let written = if out_cells.is_null() { 0 } else { cells_cap.min(cells_len) };
        let mut state = CopyRun {
            previous_x: POSITION_NONE,
            previous_style: 0,
            previous_key: COPY_NO_KEY,
        };
        if let Err(result) =
            copy_run::<true>(row, start, start + written, out_cells, &mut styles, &mut graphemes, &mut state)
        {
            return result;
        }
        if let Err(result) = copy_run::<false>(
            row,
            start + written,
            end,
            std::ptr::null_mut(),
            &mut styles,
            &mut graphemes,
            &mut state,
        ) {
            return result;
        }
    }

    let cells_len = end - start;
    let styles_len = styles.len;
    let graphemes_len = graphemes.len;
    let bytes_len = graphemes.bytes_len;
    addr_of_mut!((*out_).cells_len).write(cells_len);
    addr_of_mut!((*out_).styles_len).write(styles_len);
    addr_of_mut!((*out_).graphemes_len).write(graphemes_len);
    addr_of_mut!((*out_).grapheme_bytes_len).write(bytes_len);
    if (cells_len > 0 && out_cells.is_null())
        || cells_len > cells_cap
        || styles.out.is_null()
        || styles_len > styles.cap
        || (graphemes_len > 0 && graphemes.out.is_null())
        || graphemes_len > graphemes.cap
        || (bytes_len > 0 && graphemes.bytes.is_null())
        || bytes_len > graphemes.bytes_cap
    {
        return Result::OutOfSpace;
    }
    Result::Success
}

/// `CopyRun::previous_key` when there is no previous styled cell to
/// match. Real keys never have bit 0 set.
const COPY_NO_KEY: u64 = 1;

/// What `copy_run` carries from one cell to the next.
struct CopyRun {
    /// The x of the previous styled cell, or `POSITION_NONE`.
    previous_x: usize,
    /// The style table index of the previous styled cell.
    previous_style: u16,
    /// The style ID bits of the previous styled cell when it is a
    /// codepoint cell, else `COPY_NO_KEY`.
    previous_key: u64,
}

/// Copies `row.raw[from..to]`, writing each cell to `out` when `WRITE`
/// is true and only collecting styles and graphemes otherwise.
///
/// Zig compares every styled cell's style with the previous styled
/// cell's style. Here a styled codepoint cell whose style ID equals the
/// previous one's skips that compare: both styles were copied from the
/// same page entry when the row was built, so the compare would match.
/// Background color cells can't take this shortcut because their style
/// comes from their content.
#[inline(always)]
fn copy_run<const WRITE: bool>(
    row: &renderpkg::Cells,
    from: usize,
    to: usize,
    out: *mut CopiedCell,
    styles: &mut CopyStyles,
    graphemes: &mut CopyGraphemes,
    state: &mut CopyRun,
) -> std::result::Result<(), Result> {
    use page::cell_fields as f;
    const STYLE_ID: u64 = f::STYLE_ID.mask();
    // The high bit of the content tag, set for background color cells.
    const TAG_BG: u64 = 0b10;
    const TAG: u64 = f::CONTENT_TAG.mask();
    const CODEPOINT: u64 = f::CONTENT_CODEPOINT_DATA.mask();
    const CODEPOINT_DATA: u64 = CODEPOINT >> f::CONTENT_CODEPOINT_DATA.offset;
    const GRAPHEME: u64 = ContentTag::CodepointGrapheme as u64;

    let mut previous_x = state.previous_x;
    let mut previous_style = state.previous_style;
    let mut previous_key = state.previous_key;
    for (i, cell) in row.raw[from..to].iter().enumerate() {
        let bits = cell.to_bits();

        // Unstyled codepoint cells, the bulk of most rows: no style,
        // no grapheme, and the content is the codepoint.
        if bits & (STYLE_ID | TAG) == 0 {
            if WRITE {
                let low = (bits >> f::CONTENT.offset) & CODEPOINT_DATA;
                let high = COPY_HIGH[(bits >> f::WIDE.offset) as usize & 0x3F];
                // SAFETY: the caller's cell buffer holds the cells written.
                unsafe { write_copied(out.add(i), (low, high)) };
            }
            continue;
        }

        let cx = from + i;

        // Styled codepoint cells that continue the previous styled
        // cell's style, the bulk of styled rows.
        if bits & TAG == 0 && bits & STYLE_ID == previous_key {
            if WRITE {
                let low = (bits >> f::CONTENT.offset) & CODEPOINT_DATA | (previous_style as u64) << 32;
                let high = COPY_HIGH[(bits >> f::WIDE.offset) as usize & 0x3F];
                // SAFETY: the caller's cell buffer holds the cells written.
                unsafe { write_copied(out.add(i), (low, high)) };
            }
            previous_x = cx;
            continue;
        }

        let mut style_index: u16 = 0;
        if bits & STYLE_ID != 0 {
            let key = bits & (STYLE_ID | TAG_BG);
            if key == previous_key {
                style_index = previous_style;
            } else {
                style_index = styles.index(&row.style, cx, previous_x, previous_style);
                previous_key = if bits & TAG_BG == 0 { key } else { COPY_NO_KEY };
            }
            previous_x = cx;
            previous_style = style_index;
        }

        // A grapheme cell with text (has_grapheme and has_text).
        let mut grapheme: u16 = 0;
        if bits & TAG == GRAPHEME && bits & CODEPOINT != 0 {
            grapheme = graphemes.push(*cell, row.grapheme(cx))?;
        }

        if WRITE {
            // SAFETY: the caller's cell buffer holds the cells written.
            unsafe { write_copied(out.add(i), copied_cell(bits, style_index, grapheme)) };
        }
    }
    state.previous_x = previous_x;
    state.previous_style = previous_style;
    state.previous_key = previous_key;
    Ok(())
}

/// Returns the end of `raws[start..end]` without its trailing default
/// cells, the all-zero cells an erased or never written column holds.
fn trimmed_end(raws: &[page::Cell], start: usize, end: usize) -> usize {
    const GROUP: usize = 8;
    let mut i = end;
    while i - start >= GROUP {
        let group: &[page::Cell; GROUP] = raws[i - GROUP..i].try_into().unwrap();
        if group.iter().fold(0u64, |acc, c| acc | c.to_bits()) != 0 {
            break;
        }
        i -= GROUP;
    }
    while i > start && raws[i - 1].is_zero() {
        i -= 1;
    }
    i
}

#[inline]
unsafe fn row_cells_get_dispatch(cells: &RowCellsWrapper, x: usize, data: c_int, out: *mut c_void) -> Result {
    let Some(data) = RowCellsData::from_int(data) else {
        log::warn!(target: LOG_TARGET, "render_state_row_cells_get invalid data value={data}");
        return Result::InvalidValue;
    };
    if out.is_null() {
        return Result::InvalidValue;
    }

    match data {
        RowCellsData::Invalid => Result::InvalidValue,
        _ => row_cells_get_typed_inner(cells, x, data, out),
    }
}

unsafe fn row_cells_get_typed_inner(cells: &RowCellsWrapper, x: usize, data: RowCellsData, out: *mut c_void) -> Result {
    let cell = cells.raws()[x];
    let palette = &*cells.palette;
    let extra = || -> &[u32] {
        if cell.has_grapheme() {
            cells.cells().grapheme(x)
        } else {
            &[]
        }
    };
    let style = || &cells.cells().style[x];

    match data {
        RowCellsData::Invalid => return Result::InvalidValue,
        RowCellsData::Raw => *(out as *mut cell_c::CCell) = cell.cval(),
        RowCellsData::Style => {
            let out = &mut *(out as *mut style_c::Style);
            if cell.has_styling() {
                style_c::Style::write(style(), out);
            } else {
                *out = style_c::Style::DEFAULT;
            }
        }
        RowCellsData::GraphemesLen => {
            if !cell.has_text() {
                *(out as *mut u32) = 0;
                return Result::Success;
            }
            *(out as *mut u32) = (1 + extra().len()) as u32;
        }
        RowCellsData::GraphemesBuf => {
            if !cell.has_text() {
                return Result::Success;
            }
            let extra = extra();
            let buf = out as *mut u32;
            *buf = cell.codepoint();
            for (i, &cp) in extra.iter().enumerate() {
                *buf.add(i + 1) = cp;
            }
        }
        RowCellsData::BgColor => {
            let out = &mut *(out as *mut RgbC);
            // Avoid copying the full struct when only partial changes happen.
            match cell.content_tag() {
                ContentTag::BgColorPalette => {
                    *out = palette[cell.content_color_palette() as usize].cval();
                }

                ContentTag::BgColorRgb => {
                    let rgb = cell.content_color_rgb();
                    *out = RgbC {
                        r: rgb.r,
                        g: rgb.g,
                        b: rgb.b,
                    };
                }

                ContentTag::Codepoint | ContentTag::CodepointGrapheme => {
                    // The default style has no background.
                    if !cell.has_styling() {
                        return Result::InvalidValue;
                    }
                    match style().bg_color {
                        StyleColor::None => return Result::InvalidValue,
                        StyleColor::Palette(idx) => *out = palette[idx as usize].cval(),
                        StyleColor::Rgb(rgb) => *out = rgb.cval(),
                    }
                }
            }
        }
        RowCellsData::FgColor => {
            if !cell.has_styling() {
                return Result::InvalidValue;
            }
            let out = &mut *(out as *mut RgbC);
            match style().fg_color {
                StyleColor::None => return Result::InvalidValue,
                StyleColor::Palette(idx) => *out = palette[idx as usize].cval(),
                StyleColor::Rgb(rgb) => *out = rgb.cval(),
            }
        }
        RowCellsData::Selected => {
            *(out as *mut bool) = match cells.selection {
                Some(sel) => x >= sel[0] as usize && x <= sel[1] as usize,
                None => false,
            }
        }
        RowCellsData::HasStyling => *(out as *mut bool) = cell.has_styling(),
        RowCellsData::GraphemesUtf8 => {
            return row_cells_get_graphemes_utf8(cell, extra(), &mut *(out as *mut Buffer));
        }
    }

    Result::Success
}

/// `std.unicode.utf8CodepointSequenceLength`.
fn utf8_codepoint_sequence_length(cp: u32) -> Option<usize> {
    match cp {
        0..=0x7F => Some(1),
        0x80..=0x7FF => Some(2),
        0x800..=0xFFFF => Some(3),
        0x10000..=0x10FFFF => Some(4),
        _ => None,
    }
}

/// `std.unicode.utf8Encode` (fails for surrogates and out-of-range
/// codepoints).
fn utf8_encode(cp: u32, out: &mut [u8]) -> Option<usize> {
    let c = char::from_u32(cp)?;
    Some(c.encode_utf8(out).len())
}

fn row_cells_get_graphemes_utf8(cell: page::Cell, extra: &[u32], out: &mut Buffer) -> Result {
    if !cell.has_text() {
        out.len = 0;
        return Result::Success;
    }

    let first = cell.codepoint();

    // Fast path: a single ASCII codepoint written to an adequately
    // sized buffer. This is the overwhelmingly common case for
    // terminal content.
    if first < 0x80 && extra.is_empty() {
        if !out.ptr.is_null() && out.cap >= 1 {
            // SAFETY: the caller's buffer holds at least one byte.
            unsafe { *out.ptr = first as u8 };
            out.len = 1;
            return Result::Success;
        }
        out.len = 1;
        return Result::OutOfSpace;
    }

    out.len = 0;

    let Some(mut needed) = utf8_codepoint_sequence_length(first) else {
        return Result::InvalidValue;
    };
    for &cp in extra {
        let Some(n) = utf8_codepoint_sequence_length(cp) else {
            return Result::InvalidValue;
        };
        needed += n;
    }
    out.len = needed;

    if out.ptr.is_null() || out.cap < needed {
        return Result::OutOfSpace;
    }

    // SAFETY: the caller's buffer holds `cap` bytes.
    let buf = unsafe { std::slice::from_raw_parts_mut(out.ptr, out.cap) };
    let mut i: usize = 0;
    let Some(n) = utf8_encode(first, &mut buf[i..]) else {
        return Result::InvalidValue;
    };
    i += n;
    for &cp in extra {
        let Some(n) = utf8_encode(cp, &mut buf[i..]) else {
            return Result::InvalidValue;
        };
        i += n;
    }

    out.len = i;
    Result::Success
}

crate::c_api_enum! {
    /// C: GhosttyRenderStateRowData
    ///
    /// Output types: `dirty` → `bool`, `raw` → `GhosttyRow`, `cells` →
    /// [`RowCells`] (an existing wrapper, populated in place), `selection`
    /// → [`RowSelection`], `cells_raw` → `GhosttyCellsView`, `viewport_y`
    /// → `i32`, `id` → [`RowId`].
    pub enum RowData {
        Invalid = 0,
        Dirty = 1,
        Raw = 2,
        Cells = 3,
        Selection = 4,
        CellsRaw = 5,
        ViewportY = 6,
        Id = 7,
    }
}

crate::c_api_enum! {
    /// C: GhosttyRenderStateRowOption
    ///
    /// Input types: `dirty` → `bool`.
    pub enum RowOption {
        Dirty = 0,
    }
}

/// C: ghostty_render_state_row_get
///
/// # Safety
/// `iterator_` must be NULL or live; `out` must be NULL or point to the
/// data kind's output type.
#[export_name = "ghostty_render_state_row_get"]
pub unsafe extern "C" fn row_get(iterator_: RowIterator, data: c_int, out: *mut c_void) -> Result {
    let Some(it) = iterator_.as_mut() else {
        return Result::InvalidValue;
    };
    let y = it.y;
    if y >= it.raws.len() {
        return Result::InvalidValue;
    }
    // A renderer asks every row whether it is dirty on every frame, so
    // that read skips the general dispatch. Zig's dispatch is a bare
    // jump table; ours validates the data value first.
    if data == RowData::Dirty as c_int && !out.is_null() {
        // SAFETY: the row slices share one length, checked above.
        *(out as *mut bool) = *(&*it.dirty).get_unchecked(y);
        return Result::Success;
    }
    row_get_dispatch(it, y, data, out)
}

/// C: ghostty_render_state_row_get_multi
///
/// # Safety
/// `keys` and `values` must be NULL or valid for `count` entries; see
/// [`row_get`].
#[export_name = "ghostty_render_state_row_get_multi"]
pub unsafe extern "C" fn row_get_multi(
    iterator_: RowIterator,
    count: usize,
    keys: *const c_int,
    values: *const *mut c_void,
    out_written: *mut usize,
) -> Result {
    if keys.is_null() || values.is_null() {
        return Result::InvalidValue;
    }

    // Unwrap the handle and position once for the whole batch rather
    // than per key. An invalid handle/position fails on the first
    // key, matching the per-call behavior.
    let mut unwrapped: Option<(&mut RowIteratorWrapper, usize)> = 'valid: {
        let Some(it) = iterator_.as_mut() else {
            break 'valid None;
        };
        let y = it.y;
        if y >= it.raws.len() {
            break 'valid None;
        }
        Some((it, y))
    };

    for i in 0..count {
        let result = match unwrapped.as_mut() {
            Some((it, y)) => row_get_dispatch(it, *y, *keys.add(i), *values.add(i)),
            None => Result::InvalidValue,
        };
        if result != Result::Success {
            if let Some(w) = out_written.as_mut() {
                *w = i;
            }
            return result;
        }
    }
    if let Some(w) = out_written.as_mut() {
        *w = count;
    }
    Result::Success
}

#[inline]
unsafe fn row_get_dispatch(it: &mut RowIteratorWrapper, y: usize, data: c_int, out: *mut c_void) -> Result {
    let Some(data) = RowData::from_int(data) else {
        log::warn!(target: LOG_TARGET, "render_state_row_get invalid data value={data}");
        return Result::InvalidValue;
    };

    match data {
        RowData::Invalid => Result::InvalidValue,
        _ => {
            if out.is_null() {
                return Result::InvalidValue;
            }
            row_get_typed(it, y, data, out)
        }
    }
}

unsafe fn row_get_typed(it: &mut RowIteratorWrapper, y: usize, data: RowData, out: *mut c_void) -> Result {
    match data {
        RowData::Invalid => return Result::InvalidValue,
        RowData::Dirty => *(out as *mut bool) = (&*it.dirty)[y],
        RowData::Raw => *(out as *mut row::CRow) = (&*it.raws)[y].cval(),
        RowData::Cells => {
            let Some(cells) = (*(out as *mut RowCells)).as_mut() else {
                return Result::InvalidValue;
            };
            *cells = RowCellsWrapper {
                alloc: cells.alloc,
                x: POSITION_NONE,
                cells: &(&*it.cells)[y],
                selection: (&*it.selection)[y],
                palette: it.palette,
            };
        }
        RowData::Selection => {
            let out = out as *mut RowSelection;
            let out_size = std::ptr::addr_of!((*out).size).read();
            if out_size < size_of::<RowSelection>() {
                return Result::InvalidValue;
            }

            let Some(sel) = (&*it.selection)[y] else {
                return Result::NoValue;
            };
            (*out).start_x = sel[0];
            (*out).end_x = sel[1];
        }
        RowData::CellsRaw => {
            let raws: &[page::Cell] = &(&*it.cells)[y].raw;
            *(out as *mut cell_c::CellsView) = cell_c::CellsView {
                ptr: raws.as_ptr() as *const cell_c::CCell,
                len: raws.len(),
            };
        }
        RowData::ViewportY => *(out as *mut i32) = it.viewport_y_base + y as i32,
        RowData::Id => {
            *(out as *mut RowId) = RowId::init(renderpkg::RowId {
                serial: (&*it.serials)[y],
                y: (&*it.pins)[y].y,
            })
        }
    }

    Result::Success
}

/// C: ghostty_render_state_row_set
///
/// # Safety
/// `iterator_` must be NULL or live; `value` must be NULL or point to the
/// option's input type.
#[export_name = "ghostty_render_state_row_set"]
pub unsafe extern "C" fn row_set(iterator_: RowIterator, option: c_int, value: *const c_void) -> Result {
    let Some(option) = RowOption::from_int(option) else {
        log::warn!(target: LOG_TARGET, "render_state_row_set invalid option value={option}");
        return Result::InvalidValue;
    };

    if value.is_null() {
        return Result::InvalidValue;
    }
    row_set_typed(iterator_, option, value)
}

unsafe fn row_set_typed(iterator_: RowIterator, option: RowOption, value: *const c_void) -> Result {
    let Some(it) = iterator_.as_mut() else {
        return Result::InvalidValue;
    };
    let y = it.y;
    if y >= it.raws.len() {
        return Result::InvalidValue;
    }
    match option {
        RowOption::Dirty => (&mut *it.dirty)[y] = *(value as *const u8) != 0,
    }

    Result::Success
}

crate::c_api_enum! {
    /// C: GhosttyImageProtocol (Zig: `renderpkg.RenderState.ImageProtocol`).
    pub enum ImageProtocol {
        Kitty = 0,
    }
}

impl ImageProtocol {
    pub fn from_zig(p: renderpkg::ImageProtocol) -> ImageProtocol {
        match p {
            renderpkg::ImageProtocol::Kitty => ImageProtocol::Kitty,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttyRenderStateImageLayer (Zig:
    /// `renderpkg.RenderState.ImageLayer`).
    pub enum ImageLayer {
        BelowBackground = 0,
        BelowText = 1,
        AboveText = 2,
    }
}

impl ImageLayer {
    pub fn from_zig(l: renderpkg::ImageLayer) -> ImageLayer {
        match l {
            renderpkg::ImageLayer::BelowBackground => ImageLayer::BelowBackground,
            renderpkg::ImageLayer::BelowText => ImageLayer::BelowText,
            renderpkg::ImageLayer::AboveText => ImageLayer::AboveText,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttyRenderStateKittyPlacementKind (Zig:
    /// `renderpkg.RenderState.KittyPlacement.Kind`).
    pub enum KittyPlacementKind {
        Virtual = 0,
        Relative = 1,
        Fixed = 2,
    }
}

impl KittyPlacementKind {
    pub fn from_zig(k: renderpkg::KittyPlacementKind) -> KittyPlacementKind {
        match k {
            renderpkg::KittyPlacementKind::Virtual => KittyPlacementKind::Virtual,
            renderpkg::KittyPlacementKind::Relative => KittyPlacementKind::Relative,
            renderpkg::KittyPlacementKind::Fixed => KittyPlacementKind::Fixed,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttyRenderStateImageData
    ///
    /// Output types: `protocol` → [`ImageProtocol`], `layer` →
    /// [`ImageLayer`], `kitty_kind` → [`KittyPlacementKind`],
    /// `image_generation` → `u64`, `viewport_col`/`viewport_row`/`z` →
    /// `i32`, everything else → `u32`.
    pub enum ImageData {
        Invalid = 0,
        Protocol = 1,
        ImageId = 2,
        ImageGeneration = 3,
        Layer = 4,
        Z = 5,
        ViewportCol = 6,
        ViewportRow = 7,
        GridCols = 8,
        GridRows = 9,
        CellOffsetX = 10,
        CellOffsetY = 11,
        PixelWidth = 12,
        PixelHeight = 13,
        SourceX = 14,
        SourceY = 15,
        SourceWidth = 16,
        SourceHeight = 17,
        KittyPlacementId = 18,
        KittyKind = 19,
    }
}

/// C: ghostty_render_state_image_iterator_new
///
/// # Safety
/// The allocator (if any) must outlive the iterator.
#[export_name = "ghostty_render_state_image_iterator_new"]
pub unsafe extern "C" fn image_iterator_new(alloc_: Option<&CAllocator>, result: &mut ImageIterator) -> Result {
    let alloc = HandleAllocator::new(alloc_);
    let ptr = alloc.create(ImageIteratorWrapper {
        alloc,
        inner: renderpkg::ImagePlacementIterator {
            rows: &[],
            viewport_y_base: 0,
            placed: &[],
            y: 0,
            i: 0,
        },
        current: None,
    });
    if ptr.is_null() {
        *result = std::ptr::null_mut();
        return Result::OutOfMemory;
    }
    *result = ptr;
    Result::Success
}

/// C: ghostty_render_state_image_iterator_free
///
/// # Safety
/// `iterator_` must be NULL or an iterator not used afterwards.
#[export_name = "ghostty_render_state_image_iterator_free"]
pub unsafe extern "C" fn image_iterator_free(iterator_: ImageIterator) {
    let Some(iterator) = iterator_.as_ref() else {
        return;
    };
    let alloc = iterator.alloc;
    alloc.destroy(iterator_);
}

/// C: ghostty_render_state_image_iterator_next
///
/// # Safety
/// `iterator_` must be NULL or live, and its render state not updated
/// since it was populated.
#[export_name = "ghostty_render_state_image_iterator_next"]
pub unsafe extern "C" fn image_iterator_next(iterator_: ImageIterator) -> bool {
    let Some(it) = iterator_.as_mut() else {
        return false;
    };
    it.current = it.inner.next();
    it.current.is_some()
}

/// C: ghostty_render_state_image_get
///
/// # Safety
/// `iterator_` must be NULL or live; `out` must be NULL or point to the
/// data kind's output type.
#[export_name = "ghostty_render_state_image_get"]
pub unsafe extern "C" fn image_get(iterator_: ImageIterator, data: c_int, out: *mut c_void) -> Result {
    let Some(data) = ImageData::from_int(data) else {
        log::warn!(target: LOG_TARGET, "render_state_image_get invalid data value={data}");
        return Result::InvalidValue;
    };

    match data {
        ImageData::Invalid => Result::InvalidValue,
        _ => {
            if out.is_null() {
                return Result::InvalidValue;
            }
            image_get_typed(iterator_, data, out)
        }
    }
}

/// C: ghostty_render_state_image_get_multi
///
/// # Safety
/// `keys` and `values` must be NULL or valid for `count` entries; see
/// [`image_get`].
#[export_name = "ghostty_render_state_image_get_multi"]
pub unsafe extern "C" fn image_get_multi(
    iterator_: ImageIterator,
    count: usize,
    keys: *const c_int,
    values: *const *mut c_void,
    out_written: *mut usize,
) -> Result {
    if keys.is_null() || values.is_null() {
        return Result::InvalidValue;
    }

    for i in 0..count {
        let result = image_get(iterator_, *keys.add(i), *values.add(i));
        if result != Result::Success {
            if let Some(w) = out_written.as_mut() {
                *w = i;
            }
            return result;
        }
    }
    if let Some(w) = out_written.as_mut() {
        *w = count;
    }
    Result::Success
}

unsafe fn image_get_typed(iterator_: ImageIterator, data: ImageData, out: *mut c_void) -> Result {
    let Some(it) = iterator_.as_ref() else {
        return Result::InvalidValue;
    };
    let Some(p) = it.current else {
        return Result::InvalidValue;
    };

    macro_rules! write {
        ($ty:ty, $v:expr) => {
            *(out as *mut $ty) = $v
        };
    }

    match data {
        ImageData::Invalid => return Result::InvalidValue,
        ImageData::Protocol => write!(ImageProtocol, ImageProtocol::from_zig(p.protocol.tag())),
        ImageData::ImageId => write!(u32, p.image_id),
        ImageData::ImageGeneration => write!(u64, p.image_generation),
        ImageData::Layer => write!(ImageLayer, ImageLayer::from_zig(p.layer)),
        ImageData::Z => write!(i32, p.z),
        ImageData::ViewportCol => write!(i32, p.x),
        ImageData::ViewportRow => write!(i32, p.y),
        ImageData::GridCols => write!(u32, p.cols),
        ImageData::GridRows => write!(u32, p.rows),
        ImageData::CellOffsetX => write!(u32, p.offset_x),
        ImageData::CellOffsetY => write!(u32, p.offset_y),
        ImageData::PixelWidth => write!(u32, p.width),
        ImageData::PixelHeight => write!(u32, p.height),
        ImageData::SourceX => write!(u32, p.source_x),
        ImageData::SourceY => write!(u32, p.source_y),
        ImageData::SourceWidth => write!(u32, p.source_width),
        ImageData::SourceHeight => write!(u32, p.source_height),

        // Protocol-specific values. Other protocols have no value. The
        // switches are exhaustive so that adding a protocol makes this
        // fail to compile until it is handled here.
        ImageData::KittyPlacementId => match p.protocol {
            renderpkg::ImagePlacementProtocol::Kitty(kitty) => write!(u32, kitty.placement_id),
        },
        ImageData::KittyKind => match p.protocol {
            renderpkg::ImagePlacementProtocol::Kitty(kitty) => {
                write!(KittyPlacementKind, KittyPlacementKind::from_zig(kitty.kind))
            }
        },
    }

    Result::Success
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;
    use crate::terminal::point::{Coordinate, Point};
    use crate::terminal::selection::Selection;
    use crate::terminal::terminal::ScrollViewport;

    fn vp<T>(v: &mut T) -> *mut c_void {
        v as *mut T as *mut c_void
    }

    fn cp<T>(v: &T) -> *const c_void {
        v as *const T as *const c_void
    }

    fn new_terminal(cols: u16, rows: u16) -> terminal_c::Terminal {
        let mut t: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut t, cols, rows));
        t
    }

    fn write(t: terminal_c::Terminal, s: &[u8]) {
        unsafe { terminal_c::vt_write(t, s.as_ptr(), s.len()) };
    }

    fn zt<'a>(t: terminal_c::Terminal) -> &'a mut crate::terminal::terminal::Terminal {
        unsafe { terminal_c::zig_terminal(t).unwrap() }
    }

    fn new_state() -> RenderState {
        let mut state: RenderState = std::ptr::null_mut();
        assert_eq!(Result::Success, unsafe { new(Some(&C_ALLOCATOR), &mut state) });
        assert!(!state.is_null());
        state
    }

    fn st<'a>(state: RenderState) -> &'a mut renderpkg::RenderState {
        unsafe { &mut (*state).state }
    }

    fn new_row_iter() -> RowIterator {
        let mut it: RowIterator = std::ptr::null_mut();
        assert_eq!(Result::Success, unsafe { row_iterator_new(Some(&C_ALLOCATOR), &mut it) });
        it
    }

    fn new_row_cells() -> RowCells {
        let mut cells: RowCells = std::ptr::null_mut();
        assert_eq!(Result::Success, unsafe { row_cells_new(Some(&C_ALLOCATOR), &mut cells) });
        cells
    }

    unsafe fn get_<T>(state: RenderState, data: Data, out: &mut T) -> Result {
        get(state, data as c_int, vp(out))
    }

    unsafe fn set_<T>(state: RenderState, option: SetOption, value: &T) -> Result {
        set(state, option as c_int, cp(value))
    }

    unsafe fn row_get_<T>(it: RowIterator, data: RowData, out: &mut T) -> Result {
        row_get(it, data as c_int, vp(out))
    }

    unsafe fn cells_get_<T>(cells: RowCells, data: RowCellsData, out: &mut T) -> Result {
        row_cells_get(cells, data as c_int, vp(out))
    }

    unsafe fn image_get_<T>(it: ImageIterator, data: ImageData, out: &mut T) -> Result {
        image_get(it, data as c_int, vp(out))
    }

    /// A populated row iterator for `state`.
    unsafe fn row_iter(state: RenderState) -> RowIterator {
        let mut it = new_row_iter();
        assert_eq!(Result::Success, get_(state, Data::RowIterator, &mut it));
        it
    }

    /// Row cells for the first row of `state`, positioned on the first
    /// cell, plus the iterator that owns them.
    unsafe fn first_row_first_cell(state: RenderState) -> (RowIterator, RowCells) {
        let it = row_iter(state);
        assert!(row_iterator_next(it));
        let mut cells = new_row_cells();
        assert_eq!(Result::Success, row_get_(it, RowData::Cells, &mut cells));
        assert!(row_cells_next(cells));
        (it, cells)
    }

    fn zeroed_colors() -> Colors {
        // SAFETY: all-zero is valid for every field.
        let mut c: Colors = unsafe { std::mem::zeroed() };
        c.size = size_of::<Colors>();
        c
    }

    fn zeroed_cursor() -> Cursor {
        // SAFETY: all-zero is valid for every field (visual_style 0 = bar).
        let mut c: Cursor = unsafe { std::mem::zeroed() };
        c.size = size_of::<Cursor>();
        c
    }

    fn select(t: terminal_c::Terminal, x0: u16, y0: u32, x1: u16, y1: u32) {
        let screen = &mut *zt(t).screens.active;
        let start = screen.pages.pin(Point::Active(Coordinate { x: x0, y: y0 })).unwrap();
        let end = screen.pages.pin(Point::Active(Coordinate { x: x1, y: y1 })).unwrap();
        screen.select(Some(Selection::init(start, end, false))).unwrap();
    }

    unsafe fn free_all(terminal: terminal_c::Terminal, state: RenderState, it: RowIterator, cells: RowCells) {
        row_cells_free(cells);
        row_iterator_free(it);
        free(state);
        terminal_c::free(terminal);
    }

    #[test]
    fn render_new_free() {
        let state = new_state();
        unsafe { free(state) };
    }

    #[test]
    fn render_free_null() {
        unsafe { free(std::ptr::null_mut()) };
    }

    #[test]
    fn render_update_invalid_value() {
        let state = new_state();
        unsafe {
            assert_eq!(Result::InvalidValue, update(std::ptr::null_mut(), std::ptr::null_mut()));
            assert_eq!(Result::InvalidValue, update(state, std::ptr::null_mut()));
            free(state);
        }
    }

    #[test]
    fn render_begin_end_update_invalid_value() {
        let state = new_state();
        unsafe {
            assert_eq!(Result::InvalidValue, begin_update(std::ptr::null_mut(), std::ptr::null_mut()));
            assert_eq!(Result::InvalidValue, begin_update(state, std::ptr::null_mut()));
            assert_eq!(Result::InvalidValue, end_update(std::ptr::null_mut()));

            // End without a begin is safe.
            assert_eq!(Result::Success, end_update(state));
            free(state);
        }
    }

    #[test]
    fn render_begin_end_update() {
        let terminal = new_terminal(10, 3);

        // Write some styled text so that the update has deferred work.
        // (Zig writes through a separate `vtStream`.)
        write(terminal, b"\x1b[1mAB"); // Bold

        let state = new_state();
        unsafe {
            // Begin should record pending work, end should complete it.
            assert_eq!(Result::Success, begin_update(state, terminal));
            assert!(!st(state).pending_styles.is_empty());
            assert_eq!(Result::Success, end_update(state));
            assert_eq!(0, st(state).pending_styles.len());

            // The cell styles should be complete.
            let cells = &st(state).row_data.cells;
            assert!(cells[0].get(0).style.flags.bold);
            assert!(cells[0].get(1).style.flags.bold);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_get_invalid_value() {
        unsafe {
            let mut cols: CellCountInt = 0;
            assert_eq!(Result::InvalidValue, get_(std::ptr::null_mut(), Data::Cols, &mut cols));

            let state = new_state();
            assert_eq!(Result::InvalidValue, get(state, Data::Cols as c_int, std::ptr::null_mut()));
            free(state);
        }
    }

    #[test]
    fn render_get_invalid_data() {
        let state = new_state();
        unsafe {
            assert_eq!(Result::InvalidValue, get(state, Data::Invalid as c_int, std::ptr::null_mut()));
            free(state);
        }
    }

    #[test]
    fn render_aggregate_get_invalid_value() {
        let state = new_state();
        unsafe {
            let mut colors = zeroed_colors();

            assert_eq!(Result::InvalidValue, get_(std::ptr::null_mut(), Data::Colors, &mut colors));
            assert_eq!(Result::InvalidValue, get(state, Data::Colors as c_int, std::ptr::null_mut()));

            colors.size = size_of::<usize>() - 1;
            assert_eq!(Result::InvalidValue, get_(state, Data::Colors, &mut colors));

            let mut cursor = zeroed_cursor();

            assert_eq!(Result::InvalidValue, get_(std::ptr::null_mut(), Data::Cursor, &mut cursor));
            assert_eq!(Result::InvalidValue, get(state, Data::Cursor as c_int, std::ptr::null_mut()));

            cursor.size = size_of::<usize>() - 1;
            assert_eq!(Result::InvalidValue, get_(state, Data::Cursor, &mut cursor));
            free(state);
        }
    }

    #[test]
    fn render_get_set_dirty_invalid_value() {
        unsafe {
            let mut dirty = Dirty::False;
            assert_eq!(Result::InvalidValue, get_(std::ptr::null_mut(), Data::Dirty, &mut dirty));
            let dirty_full = Dirty::Full;
            assert_eq!(Result::InvalidValue, set_(std::ptr::null_mut(), SetOption::Dirty, &dirty_full));
        }
    }

    #[test]
    fn render_get_set_dirty() {
        let state = new_state();
        unsafe {
            let mut dirty = Dirty::Full;
            assert_eq!(Result::Success, get_(state, Data::Dirty, &mut dirty));
            assert_eq!(Dirty::False, dirty);

            assert_eq!(Result::Success, set_(state, SetOption::Dirty, &Dirty::Partial));
            assert_eq!(Result::Success, get_(state, Data::Dirty, &mut dirty));
            assert_eq!(Dirty::Partial, dirty);

            assert_eq!(Result::Success, set_(state, SetOption::Dirty, &Dirty::Full));
            assert_eq!(Result::Success, get_(state, Data::Dirty, &mut dirty));
            assert_eq!(Dirty::Full, dirty);
            free(state);
        }
    }

    #[test]
    fn render_clean() {
        let terminal = new_terminal(10, 3);
        let state = new_state();
        unsafe {
            assert_eq!(Result::InvalidValue, clean(std::ptr::null_mut()));
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(renderpkg::Dirty::Full, st(state).dirty);

            assert_eq!(Result::Success, clean(state));
            assert_eq!(renderpkg::Dirty::False, st(state).dirty);
            for &dirty in &st(state).row_data.dirty {
                assert!(!dirty);
            }

            // Cleaning is idempotent, and an unchanged update remains clean.
            assert_eq!(Result::Success, clean(state));
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(renderpkg::Dirty::False, st(state).dirty);
            for &dirty in &st(state).row_data.dirty {
                assert!(!dirty);
            }
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_set_null_value() {
        let state = new_state();
        unsafe {
            assert_eq!(Result::InvalidValue, set(state, SetOption::Dirty as c_int, std::ptr::null()));
            free(state);
        }
    }

    #[test]
    fn render_row_iterator_get_invalid_value() {
        let mut iterator = new_row_iter();
        unsafe {
            assert_eq!(Result::InvalidValue, get_(std::ptr::null_mut(), Data::RowIterator, &mut iterator));
            row_iterator_free(iterator);
        }
    }

    #[test]
    fn render_row_iterator_new_free() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));

            let iterator = row_iter(state);
            assert!(!iterator.is_null());

            let it = &*iterator;
            let row_data = &st(state).row_data;

            assert_eq!(POSITION_NONE, it.y);
            assert_eq!(row_data.raw.len(), it.raws.len());
            assert_eq!(row_data.cells.len(), it.cells.len());
            assert_eq!(row_data.selection.len(), it.selection.len());
            assert_eq!(row_data.dirty.len(), it.dirty.len());
            row_iterator_free(iterator);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_iterator_free_null() {
        unsafe { row_iterator_free(std::ptr::null_mut()) };
    }

    #[test]
    fn render_row_iterator_next_null() {
        assert!(!unsafe { row_iterator_next(std::ptr::null_mut()) });
    }

    #[test]
    fn render_row_get_null() {
        let mut dirty = false;
        assert_eq!(Result::InvalidValue, unsafe { row_get_(std::ptr::null_mut(), RowData::Dirty, &mut dirty) });
    }

    #[test]
    fn render_row_get_invalid_data() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let iterator = row_iter(state);
            assert!(row_iterator_next(iterator));
            assert_eq!(Result::InvalidValue, row_get(iterator, RowData::Invalid as c_int, std::ptr::null_mut()));
            row_iterator_free(iterator);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_set_null() {
        let dirty = false;
        assert_eq!(Result::InvalidValue, unsafe {
            row_set(std::ptr::null_mut(), RowOption::Dirty as c_int, cp(&dirty))
        });
    }

    #[test]
    fn render_row_set_before_iteration() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let iterator = row_iter(state);
            let dirty = false;
            assert_eq!(Result::InvalidValue, row_set(iterator, RowOption::Dirty as c_int, cp(&dirty)));
            row_iterator_free(iterator);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_get_before_iteration() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let iterator = row_iter(state);
            let mut dirty = false;
            assert_eq!(Result::InvalidValue, row_get_(iterator, RowData::Dirty, &mut dirty));
            row_iterator_free(iterator);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_get_set_dirty() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));

            // Dirty the first row so the iterator has at least one dirty row to observe.
            write(terminal, b"hello");
            assert_eq!(Result::Success, update(state, terminal));

            // Create an iterator and verify it is dirty.
            let it = row_iter(state);
            assert!(row_iterator_next(it));
            let mut dirty = false;
            assert_eq!(Result::Success, row_get_(it, RowData::Dirty, &mut dirty));
            assert!(dirty);

            // Clear dirty on this row.
            let dirty_false = false;
            assert_eq!(Result::Success, row_set(it, RowOption::Dirty as c_int, cp(&dirty_false)));

            // It should not be dirty anymore.
            let it2 = row_iter(state);
            assert!(row_iterator_next(it2));
            assert_eq!(Result::Success, row_get_(it2, RowData::Dirty, &mut dirty));
            assert!(!dirty);
            row_iterator_free(it);
            row_iterator_free(it2);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_get_selection() {
        let terminal = new_terminal(10, 3);
        select(terminal, 2, 1, 4, 1);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let it = row_iter(state);

            let mut sel = RowSelection::default();
            assert!(row_iterator_next(it));
            assert_eq!(Result::NoValue, row_get_(it, RowData::Selection, &mut sel));

            assert!(row_iterator_next(it));
            sel = RowSelection::default();
            assert_eq!(Result::Success, row_get_(it, RowData::Selection, &mut sel));
            assert_eq!(2, sel.start_x);
            assert_eq!(4, sel.end_x);

            assert!(row_iterator_next(it));
            sel = RowSelection::default();
            assert_eq!(Result::NoValue, row_get_(it, RowData::Selection, &mut sel));
            row_iterator_free(it);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_get_cells_raw() {
        let terminal = new_terminal(10, 3);
        write(terminal, b"AB");
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let it = row_iter(state);

            // Not positioned on a row yet.
            let mut view = cell_c::CellsView {
                ptr: std::ptr::null(),
                len: 0,
            };
            assert_eq!(Result::InvalidValue, row_get_(it, RowData::CellsRaw, &mut view));

            assert!(row_iterator_next(it));
            assert_eq!(Result::Success, row_get_(it, RowData::CellsRaw, &mut view));
            assert_eq!(10, view.len);

            // The view must match the per-cell raw reads.
            let mut cells = new_row_cells();
            assert_eq!(Result::Success, row_get_(it, RowData::Cells, &mut cells));
            let ptr = view.ptr;
            assert!(!ptr.is_null());
            for x in 0..view.len as u16 {
                let mut raw: cell_c::CCell = 0;
                assert_eq!(Result::Success, row_cells_select(cells, x));
                assert_eq!(Result::Success, cells_get_(cells, RowCellsData::Raw, &mut raw));
                assert_eq!(raw, *ptr.add(x as usize));
            }

            // Contents sanity: first two cells hold our text.
            let first = &*(ptr as *const page::Cell);
            let second = &*(ptr.add(1) as *const page::Cell);
            assert_eq!('A' as u32, first.codepoint());
            assert_eq!('B' as u32, second.codepoint());
            row_cells_free(cells);
            row_iterator_free(it);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_cells_get_selected() {
        let terminal = new_terminal(10, 3);
        select(terminal, 2, 1, 4, 1);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let mut cells = new_row_cells();
            let it = row_iter(state);

            assert!(row_iterator_next(it));
            assert_eq!(Result::Success, row_get_(it, RowData::Cells, &mut cells));

            let mut selected = true;
            assert_eq!(Result::Success, row_cells_select(cells, 0));
            assert_eq!(Result::Success, cells_get_(cells, RowCellsData::Selected, &mut selected));
            assert!(!selected);

            assert!(row_iterator_next(it));
            assert_eq!(Result::Success, row_get_(it, RowData::Cells, &mut cells));

            for (x, expected) in [(1, false), (2, true), (4, true), (5, false)] {
                assert_eq!(Result::Success, row_cells_select(cells, x));
                assert_eq!(Result::Success, cells_get_(cells, RowCellsData::Selected, &mut selected));
                assert_eq!(expected, selected);
            }

            assert_eq!(Result::Success, row_cells_select(cells, 3));
            selected = false;
            let mut written: usize = 0;
            let keys = [RowCellsData::Selected as c_int];
            let values = [vp(&mut selected)];
            assert_eq!(
                Result::Success,
                row_cells_get_multi(cells, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);
            assert!(selected);
            row_cells_free(cells);
            row_iterator_free(it);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_cells_get_has_styling() {
        let terminal = new_terminal(10, 3);
        write(terminal, b"A\x1b[31mB");
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let mut cells = new_row_cells();
            let it = row_iter(state);
            assert!(row_iterator_next(it));
            assert_eq!(Result::Success, row_get_(it, RowData::Cells, &mut cells));

            let mut has_styling = true;
            assert_eq!(Result::Success, row_cells_select(cells, 0));
            assert_eq!(Result::Success, cells_get_(cells, RowCellsData::HasStyling, &mut has_styling));
            assert!(!has_styling);

            assert_eq!(Result::Success, row_cells_select(cells, 1));
            assert_eq!(Result::Success, cells_get_(cells, RowCellsData::HasStyling, &mut has_styling));
            assert!(has_styling);

            has_styling = false;
            let mut written: usize = 0;
            let keys = [RowCellsData::HasStyling as c_int];
            let values = [vp(&mut has_styling)];
            assert_eq!(
                Result::Success,
                row_cells_get_multi(cells, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);
            assert!(has_styling);
            row_cells_free(cells);
            row_iterator_free(it);
            free(state);
            terminal_c::free(terminal);
        }
    }

    /// The first row's cells of a fresh 10x3 terminal with `input`.
    unsafe fn cells_for(input: &[u8]) -> (terminal_c::Terminal, RenderState, RowIterator, RowCells) {
        let terminal = new_terminal(10, 3);
        write(terminal, input);
        let state = new_state();
        assert_eq!(Result::Success, update(state, terminal));
        let mut cells = new_row_cells();
        let it = row_iter(state);
        assert!(row_iterator_next(it));
        assert_eq!(Result::Success, row_get_(it, RowData::Cells, &mut cells));
        (terminal, state, it, cells)
    }

    #[test]
    fn render_row_cells_get_graphemes_utf8() {
        let cases: [(&str, &str); 3] = [
            ("e\u{301}", "e\u{301}"),
            ("\x1b[?2027h\u{1F1FA}\u{1F1F8}", "\u{1F1FA}\u{1F1F8}"),
            (
                "\x1b[?2027h\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
                "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            ),
        ];

        unsafe {
            for (terminal_input, expected) in cases {
                let (terminal, state, it, cells) = cells_for(terminal_input.as_bytes());

                assert_eq!(Result::Success, row_cells_select(cells, 0));

                let mut text = Buffer::default();
                assert_eq!(Result::OutOfSpace, cells_get_(cells, RowCellsData::GraphemesUtf8, &mut text));
                assert_eq!(expected.len(), text.len);

                let mut small = [b'x'; 32];
                let small_cap = expected.len() - 1;
                text = Buffer {
                    ptr: small.as_mut_ptr(),
                    cap: small_cap,
                    len: 0,
                };
                assert_eq!(Result::OutOfSpace, cells_get_(cells, RowCellsData::GraphemesUtf8, &mut text));
                assert_eq!(expected.len(), text.len);
                assert_eq!([b'x'; 32], small);

                let mut buf = [0u8; 32];
                text = Buffer {
                    ptr: buf.as_mut_ptr(),
                    cap: expected.len(),
                    len: 0,
                };
                assert_eq!(Result::Success, cells_get_(cells, RowCellsData::GraphemesUtf8, &mut text));
                assert_eq!(expected.len(), text.len);
                assert_eq!(expected.as_bytes(), &buf[..text.len]);
                free_all(terminal, state, it, cells);
            }

            let (terminal, state, it, cells) = cells_for("e\u{301}".as_bytes());
            assert_eq!(Result::Success, row_cells_select(cells, 1));
            let mut buf = [0u8; 8];
            let mut text = Buffer {
                ptr: buf.as_mut_ptr(),
                cap: buf.len(),
                len: 0,
            };
            assert_eq!(Result::Success, cells_get_(cells, RowCellsData::GraphemesUtf8, &mut text));
            assert_eq!(0, text.len);
            free_all(terminal, state, it, cells);
        }
    }

    fn expect_copied_style(expected: &style_c::Style, actual: &style_c::Style) {
        let colors = [
            (expected.fg_color, actual.fg_color),
            (expected.bg_color, actual.bg_color),
            (expected.underline_color, actual.underline_color),
        ];
        for (e, a) in colors {
            assert_eq!(e.tag, a.tag);
            // SAFETY: every arm of the union is plain data.
            assert_eq!(unsafe { e.value._padding }, unsafe { a.value._padding });
        }
        assert_eq!(expected.bold, actual.bold);
        assert_eq!(expected.italic, actual.italic);
        assert_eq!(expected.faint, actual.faint);
        assert_eq!(expected.blink, actual.blink);
        assert_eq!(expected.inverse, actual.inverse);
        assert_eq!(expected.invisible, actual.invisible);
        assert_eq!(expected.strikethrough, actual.strikethrough);
        assert_eq!(expected.overline, actual.overline);
        assert_eq!(expected.underline, actual.underline);
    }

    #[test]
    fn render_row_cells_copy_matches_the_per_cell_getters() {
        let terminal = new_terminal(12, 4);
        write(
            terminal,
            "\x1b[1;31mab\x1b[0mc\x1b[1;31md\x1b[0me\u{301}\u{4E2D}\
             \x1b]8;;http://x\x1b\\L\x1b]8;;\x1b\\\x1b[4;38;2;1;2;3mu\x1b[0m\
             \r\n\x1b[48;5;4m\x1b[K\x1b[0m\r\n\x1b[48;2;7;8;9m\x1b[K\x1b[0mz"
                .as_bytes(),
        );

        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));

            let it = row_iter(state);
            let mut cells = new_row_cells();

            let mut copied = [CopiedCell::default(); 12];
            let mut styles = [style_c::Style::DEFAULT; 13];
            let mut graphemes = [CopiedGrapheme::default(); 12];
            let mut bytes = [0u8; 96];
            let mut rows: usize = 0;
            while row_iterator_next(it) {
                assert_eq!(Result::Success, row_get_(it, RowData::Cells, &mut cells));
                let mut out = RowCellsCopy {
                    cells: copied.as_mut_ptr(),
                    cells_cap: copied.len(),
                    styles: styles.as_mut_ptr(),
                    styles_cap: styles.len(),
                    graphemes: graphemes.as_mut_ptr(),
                    graphemes_cap: graphemes.len(),
                    grapheme_bytes: bytes.as_mut_ptr(),
                    grapheme_bytes_cap: bytes.len(),
                    ..Default::default()
                };
                assert_eq!(Result::Success, row_cells_copy(cells, 0, 12, &mut out));
                assert_eq!(12, out.cells_len);
                expect_copied_style(&style_c::Style::DEFAULT, &styles[0]);
                if rows == 0 {
                    assert_eq!(3, out.styles_len);
                    assert_eq!(copied[0].style, copied[3].style);
                    assert_eq!(1, out.graphemes_len);
                    assert!(copied[7].flags & CopiedCell::HYPERLINK != 0);
                }

                for (x, cell) in copied.iter().enumerate() {
                    assert_eq!(Result::Success, row_cells_select(cells, x as CellCountInt));
                    let mut raw_value: cell_c::CCell = 0;
                    assert_eq!(Result::Success, cells_get_(cells, RowCellsData::Raw, &mut raw_value));
                    let raw = page::Cell::from_bits(raw_value);
                    assert_eq!(raw.content_tag() as u8, cell.content_tag);
                    assert_eq!(raw.wide() as u8, cell.wide);
                    assert_eq!(raw.hyperlink(), cell.flags & CopiedCell::HYPERLINK != 0);
                    assert_eq!(raw.semantic_content() as u8, cell.semantic_content);

                    let mut expected_style = style_c::Style::DEFAULT;
                    assert_eq!(Result::Success, cells_get_(cells, RowCellsData::Style, &mut expected_style));
                    assert!((cell.style as usize) < out.styles_len);
                    expect_copied_style(&expected_style, &styles[cell.style as usize]);

                    let mut text_buf = [0u8; 32];
                    let mut text = Buffer {
                        ptr: text_buf.as_mut_ptr(),
                        cap: text_buf.len(),
                        len: 0,
                    };
                    assert_eq!(Result::Success, cells_get_(cells, RowCellsData::GraphemesUtf8, &mut text));
                    if cell.grapheme != 0 {
                        let span = graphemes[cell.grapheme as usize - 1];
                        let offset = span.offset as usize;
                        assert_eq!(&text_buf[..text.len], &bytes[offset..offset + span.len as usize]);
                    } else if text.len > 0 {
                        let mut encoded = [0u8; 4];
                        let n = char::from_u32(cell.content).unwrap().encode_utf8(&mut encoded).len();
                        assert_eq!(&text_buf[..text.len], &encoded[..n]);
                    }

                    let mut bg = RgbC { r: 0, g: 0, b: 0 };
                    let bg_result = cells_get_(cells, RowCellsData::BgColor, &mut bg);
                    match raw.content_tag() {
                        ContentTag::BgColorPalette => {
                            assert_eq!(Result::Success, bg_result);
                            assert_eq!(4, cell.content);
                        }
                        ContentTag::BgColorRgb => {
                            assert_eq!(Result::Success, bg_result);
                            assert_eq!(((bg.r as u32) << 16) | ((bg.g as u32) << 8) | bg.b as u32, cell.content);
                        }
                        ContentTag::Codepoint | ContentTag::CodepointGrapheme => {
                            assert_eq!(raw.codepoint(), cell.content);
                        }
                    }
                }
                rows += 1;
            }
            assert_eq!(4, rows);
            free_all(terminal, state, it, cells);
        }
    }

    #[test]
    fn render_row_cells_copy_reports_short_buffers_and_ranges() {
        let terminal = new_terminal(8, 2);
        write(terminal, "\x1b[1ma\x1b[3mb\x1b[0mce\u{301}".as_bytes());

        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let it = row_iter(state);
            let mut cells = new_row_cells();
            assert!(row_iterator_next(it));
            assert_eq!(Result::Success, row_get_(it, RowData::Cells, &mut cells));

            let mut query = RowCellsCopy::default();
            assert_eq!(Result::OutOfSpace, row_cells_copy(cells, 0, 8, &mut query));
            assert_eq!(8, query.cells_len);
            assert_eq!(3, query.styles_len);
            assert_eq!(1, query.graphemes_len);
            assert_eq!(3, query.grapheme_bytes_len);

            let mut copied = [CopiedCell::default(); 8];
            let mut styles = [style_c::Style::DEFAULT; 3];
            let mut graphemes = [CopiedGrapheme::default(); 1];
            let mut bytes = [0u8; 2];
            let mut short = RowCellsCopy {
                cells: copied.as_mut_ptr(),
                cells_cap: copied.len(),
                styles: styles.as_mut_ptr(),
                styles_cap: styles.len(),
                graphemes: graphemes.as_mut_ptr(),
                graphemes_cap: graphemes.len(),
                grapheme_bytes: bytes.as_mut_ptr(),
                grapheme_bytes_cap: bytes.len(),
                ..Default::default()
            };
            assert_eq!(Result::OutOfSpace, row_cells_copy(cells, 0, 8, &mut short));
            assert_eq!(3, short.grapheme_bytes_len);

            let mut tail = RowCellsCopy {
                cells: copied.as_mut_ptr(),
                cells_cap: copied.len(),
                styles: styles.as_mut_ptr(),
                styles_cap: styles.len(),
                ..Default::default()
            };
            assert_eq!(Result::Success, row_cells_copy(cells, 1, 2, &mut tail));
            assert_eq!(2, tail.cells_len);
            assert_eq!(2, tail.styles_len);
            assert_eq!('b' as u32, copied[0].content);
            assert_eq!(1, copied[0].style);
            assert_eq!(0, copied[1].style);

            assert_eq!(Result::Success, row_cells_copy(cells, 6, 100, &mut tail));
            assert_eq!(2, tail.cells_len);
            assert_eq!(Result::Success, row_cells_copy(cells, 8, 1, &mut tail));
            assert_eq!(0, tail.cells_len);
            assert_eq!(Result::InvalidValue, row_cells_copy(cells, 9, 1, &mut tail));
            assert_eq!(Result::InvalidValue, row_cells_copy(std::ptr::null_mut(), 0, 1, &mut tail));
            assert_eq!(Result::InvalidValue, row_cells_copy(cells, 0, 1, std::ptr::null_mut()));
            free_all(terminal, state, it, cells);
        }
    }

    #[test]
    fn render_row_cells_copy_trims_trailing_default_cells() {
        let terminal = new_terminal(20, 4);
        write(terminal, "ab\x1b[5Gc\r\n\x1b[48;5;4m\x1b[K\x1b[0m\r\n\r\n\u{4E2D}".as_bytes());

        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let mut it = row_iter(state);
            let mut cells = new_row_cells();

            let mut full = [CopiedCell::default(); 20];
            let mut trimmed = [CopiedCell::default(); 20];
            let mut styles = [style_c::Style::DEFAULT; 21];
            let expected: [usize; 4] = [5, 20, 0, 2];
            let mut y: usize = 0;
            while row_iterator_next(it) {
                assert_eq!(Result::Success, row_get_(it, RowData::Cells, &mut cells));
                let mut all = RowCellsCopy {
                    cells: full.as_mut_ptr(),
                    cells_cap: full.len(),
                    styles: styles.as_mut_ptr(),
                    styles_cap: styles.len(),
                    ..Default::default()
                };
                assert_eq!(Result::Success, row_cells_copy(cells, 0, 20, &mut all));
                assert_eq!(20, all.cells_len);

                let mut trim = RowCellsCopy {
                    cells: trimmed.as_mut_ptr(),
                    cells_cap: trimmed.len(),
                    styles: styles.as_mut_ptr(),
                    styles_cap: styles.len(),
                    flags: RowCellsCopy::TRIM,
                    ..Default::default()
                };
                assert_eq!(Result::Success, row_cells_copy(cells, 0, 20, &mut trim));
                assert_eq!(expected[y], trim.cells_len);
                for (a, b) in full[..trim.cells_len].iter().zip(trimmed[..trim.cells_len].iter()) {
                    assert_eq!(a, b);
                }
                for cell in &full[trim.cells_len..] {
                    assert_eq!(CopiedCell::default(), *cell);
                }
                y += 1;
            }
            assert_eq!(4, y);

            assert_eq!(Result::Success, get_(state, Data::RowIterator, &mut it));
            assert!(row_iterator_next(it));
            assert_eq!(Result::Success, row_get_(it, RowData::Cells, &mut cells));
            let mut range = RowCellsCopy {
                cells: trimmed.as_mut_ptr(),
                cells_cap: trimmed.len(),
                styles: styles.as_mut_ptr(),
                styles_cap: styles.len(),
                flags: RowCellsCopy::TRIM,
                ..Default::default()
            };
            assert_eq!(Result::Success, row_cells_copy(cells, 3, 10, &mut range));
            assert_eq!(2, range.cells_len);
            assert_eq!('c' as u32, trimmed[1].content);
            assert_eq!(Result::Success, row_cells_copy(cells, 5, 10, &mut range));
            assert_eq!(0, range.cells_len);

            let mut old = RowCellsCopy {
                size: offset_of!(RowCellsCopy, flags),
                cells: trimmed.as_mut_ptr(),
                cells_cap: trimmed.len(),
                styles: styles.as_mut_ptr(),
                styles_cap: styles.len(),
                flags: RowCellsCopy::TRIM,
                ..Default::default()
            };
            assert_eq!(Result::Success, row_cells_copy(cells, 0, 20, &mut old));
            assert_eq!(20, old.cells_len);
            old.size -= 1;
            assert_eq!(Result::InvalidValue, row_cells_copy(cells, 0, 20, &mut old));
            free_all(terminal, state, it, cells);
        }
    }

    /// The copied form of a cell read field by field through the cell
    /// accessors, as Zig writes it.
    fn reference_copied_cell(cell: page::Cell, style: u16, grapheme: u16) -> CopiedCell {
        CopiedCell {
            content: match cell.content_tag() {
                ContentTag::Codepoint | ContentTag::CodepointGrapheme => cell.content_codepoint(),
                ContentTag::BgColorPalette => cell.content_color_palette() as u32,
                ContentTag::BgColorRgb => {
                    let rgb = cell.content_color_rgb();
                    ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32
                }
            },
            style,
            grapheme,
            content_tag: cell.content_tag() as u8,
            wide: cell.wide() as u8,
            flags: (if cell.hyperlink() { CopiedCell::HYPERLINK } else { 0 })
                | (if cell.protected() { CopiedCell::PROTECTED } else { 0 }),
            semantic_content: cell.semantic_content() as u8,
        }
    }

    #[test]
    fn render_copied_cell_matches_the_cell_fields() {
        use page::cell_fields as f;
        let contents: [u64; 5] = [0, 0x41, 0x1F_FFFF, 0x12_3456, 0xFF_FFFF];
        let mut out = [CopiedCell::default(); 2];
        for tag in 0..4u64 {
            for high in 0..64u64 {
                // The fourth semantic content value is unused.
                if high >> 4 == 3 {
                    continue;
                }
                for &content in &contents {
                    for (style, grapheme) in [(0u16, 0u16), (7, 0), (0, 3), (0xFFFF, 0xFFFF)] {
                        let bits = tag | content << f::CONTENT.offset | 0x1234 << f::STYLE_ID.offset | high << 42;
                        let cell = page::Cell::from_bits(bits);
                        unsafe { write_copied(&mut out[1], copied_cell(bits, style, grapheme)) };
                        assert_eq!(reference_copied_cell(cell, style, grapheme), out[1], "{bits:#x}");
                    }
                }
            }
        }
    }

    /// `row_cells_copy` as Zig writes it: field by field, comparing
    /// every styled cell's style with the previous one's.
    fn reference_copy(
        row: &renderpkg::Cells,
        start: usize,
        end: usize,
    ) -> (Vec<CopiedCell>, Vec<style_c::Style>, Vec<CopiedGrapheme>, Vec<u8>) {
        let mut cells = Vec::new();
        let mut styles = vec![style_c::Style::DEFAULT];
        let mut graphemes = Vec::new();
        let mut bytes = Vec::new();
        let mut slots = [COPY_STYLE_SLOT_EMPTY; COPY_STYLE_SLOTS];
        let mut previous_x = POSITION_NONE;
        let mut previous_style = 0u16;
        for cx in start..end {
            let cell = row.raw[cx];
            let mut style_index = 0u16;
            if cell.has_styling() {
                let s = &row.style[cx];
                if previous_x != POSITION_NONE && row.style[previous_x].eql(s) {
                    style_index = previous_style;
                } else {
                    let slot = &mut slots[(s.hash() as usize) % COPY_STYLE_SLOTS];
                    if *slot != COPY_STYLE_SLOT_EMPTY && row.style[(*slot >> 16) as usize].eql(s) {
                        style_index = *slot as u16;
                    } else {
                        style_index = styles.len() as u16;
                        styles.push(style_c::Style::from_style(s));
                        *slot = ((cx as u32) << 16) | style_index as u32;
                    }
                }
                previous_x = cx;
                previous_style = style_index;
            }
            let mut grapheme = 0u16;
            if cell.has_grapheme() && cell.has_text() {
                let mut buf = [0u8; 64];
                let mut b = Buffer {
                    ptr: buf.as_mut_ptr(),
                    cap: buf.len(),
                    len: 0,
                };
                assert_eq!(Result::Success, row_cells_get_graphemes_utf8(cell, row.grapheme(cx), &mut b));
                graphemes.push(CopiedGrapheme {
                    offset: bytes.len() as u32,
                    len: b.len as u32,
                });
                bytes.extend_from_slice(&buf[..b.len]);
                grapheme = graphemes.len() as u16;
            }
            cells.push(reference_copied_cell(cell, style_index, grapheme));
        }
        (cells, styles, graphemes, bytes)
    }

    #[test]
    fn render_row_cells_copy_matches_a_field_by_field_copy() {
        // Styles that repeat apart and many distinct ones (slot
        // collisions), background color cells, protected, hyperlinked,
        // prompt and input cells, wide characters and graphemes.
        let tokens: [&str; 22] = [
            "abc ",
            "\x1b[1;31m",
            "\x1b[0m",
            "\x1b[4m",
            "\x1b[38;2;%;%;%m",
            "\x1b[48;5;%m",
            "\x1b[48;5;%m\x1b[K",
            "\x1b[48;2;%;%;%m\x1b[2X",
            "\x1b[1\"q",
            "\x1b[0\"q",
            "\x1b]8;;http://x\x1b\\",
            "\x1b]8;;\x1b\\",
            "\x1b]133;A\x07",
            "\x1b]133;B\x07",
            "\x1b]133;C\x07",
            "\u{4E2D}\u{6587}",
            "e\u{301}",
            "\u{1F469}\u{200D}\u{1F4BB}",
            "\r\n",
            "\x1b[%G",
            "xyz",
            "\x1b[3;9H",
        ];
        let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        for _ in 0..24 {
            let cols: u16 = 41;
            let terminal = new_terminal(cols, 6);
            let mut input = String::new();
            for _ in 0..300 {
                for part in tokens[next(tokens.len() as u64) as usize].split('%').enumerate() {
                    if part.0 > 0 {
                        input.push_str(&next(256).to_string());
                    }
                    input.push_str(part.1);
                }
            }
            write(terminal, input.as_bytes());

            let state = new_state();
            unsafe {
                assert_eq!(Result::Success, update(state, terminal));
                let it = row_iter(state);
                let mut cells = new_row_cells();
                let mut copied = vec![CopiedCell::default(); cols as usize];
                let mut styles = vec![style_c::Style::DEFAULT; cols as usize + 1];
                let mut graphemes = vec![CopiedGrapheme::default(); cols as usize];
                let mut bytes = vec![0u8; 64 * cols as usize];
                while row_iterator_next(it) {
                    assert_eq!(Result::Success, row_get_(it, RowData::Cells, &mut cells));
                    let row = (*cells).cells();
                    for (x, flags) in [(0u16, 0), (0, RowCellsCopy::TRIM), (5, RowCellsCopy::TRIM)] {
                        let mut out = RowCellsCopy {
                            cells: copied.as_mut_ptr(),
                            cells_cap: copied.len(),
                            styles: styles.as_mut_ptr(),
                            styles_cap: styles.len(),
                            graphemes: graphemes.as_mut_ptr(),
                            graphemes_cap: graphemes.len(),
                            grapheme_bytes: bytes.as_mut_ptr(),
                            grapheme_bytes_cap: bytes.len(),
                            flags,
                            ..Default::default()
                        };
                        assert_eq!(Result::Success, row_cells_copy(cells, x, cols, &mut out));
                        let mut end = cols as usize;
                        if flags & RowCellsCopy::TRIM != 0 {
                            while end > x as usize && row.raw[end - 1].is_zero() {
                                end -= 1;
                            }
                        }
                        let (want_cells, want_styles, want_graphemes, want_bytes) =
                            reference_copy(row, x as usize, end);
                        assert_eq!(want_cells.as_slice(), &copied[..out.cells_len]);
                        assert_eq!(want_styles.len(), out.styles_len);
                        for (want, got) in want_styles.iter().zip(&styles) {
                            expect_copied_style(want, got);
                        }
                        assert_eq!(want_graphemes.as_slice(), &graphemes[..out.graphemes_len]);
                        assert_eq!(want_bytes.as_slice(), &bytes[..out.grapheme_bytes_len]);
                    }
                }
                free_all(terminal, state, it, cells);
            }
        }
    }

    #[test]
    fn render_clip_limits_rows_columns_and_the_cursor() {
        let terminal = new_terminal(10, 6);
        write(terminal, b"r0\r\nr1\r\nr2\r\nr3\r\nr4\r\nr5");

        let state = new_state();
        unsafe {
            let mut out = Clip::default();
            assert_eq!(Result::Success, get_(state, Data::ClipRequest, &mut out));
            assert_eq!(Clip::default(), out);
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(Result::Success, get_(state, Data::Clip, &mut out));
            assert_eq!(Clip { y: 0, rows: 6, cols: 10 }, out);

            let req = Clip { y: 2, rows: 3, cols: 4 };
            assert_eq!(Result::Success, set_(state, SetOption::Clip, &req));
            assert_eq!(Result::Success, get_(state, Data::ClipRequest, &mut out));
            assert_eq!(req, out);

            // The request applies on the next update.
            assert_eq!(Result::Success, get_(state, Data::Clip, &mut out));
            assert_eq!(Clip { y: 0, rows: 6, cols: 10 }, out);
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(Result::Success, get_(state, Data::Clip, &mut out));
            assert_eq!(req, out);

            let mut rows: CellCountInt = 0;
            let mut cols: CellCountInt = 0;
            assert_eq!(Result::Success, get_(state, Data::Rows, &mut rows));
            assert_eq!(Result::Success, get_(state, Data::Cols, &mut cols));
            assert_eq!(6, rows);
            assert_eq!(10, cols);

            // The cursor is on row 5, below the clip.
            let mut has_cursor = true;
            assert_eq!(Result::Success, get_(state, Data::CursorViewportHasValue, &mut has_cursor));
            assert!(!has_cursor);

            let it = row_iter(state);
            let mut cells = new_row_cells();

            let mut copied = [CopiedCell::default(); 10];
            let mut styles = [style_c::Style::DEFAULT; 11];
            let mut n: usize = 0;
            while row_iterator_next(it) {
                let mut vy: i32 = 0;
                assert_eq!(Result::Success, row_get_(it, RowData::ViewportY, &mut vy));
                assert_eq!((n + 2) as i32, vy);

                let mut view = cell_c::CellsView {
                    ptr: std::ptr::null(),
                    len: 0,
                };
                assert_eq!(Result::Success, row_get_(it, RowData::CellsRaw, &mut view));
                assert_eq!(4, view.len);

                assert_eq!(Result::Success, row_get_(it, RowData::Cells, &mut cells));
                let mut copy = RowCellsCopy {
                    cells: copied.as_mut_ptr(),
                    cells_cap: copied.len(),
                    styles: styles.as_mut_ptr(),
                    styles_cap: styles.len(),
                    ..Default::default()
                };
                assert_eq!(Result::Success, row_cells_copy(cells, 0, 10, &mut copy));
                assert_eq!(4, copy.cells_len);
                assert_eq!('r' as u32, copied[0].content);
                assert_eq!('0' as u32 + (n + 2) as u32, copied[1].content);
                assert_eq!(Result::InvalidValue, row_cells_select(cells, 4));
                n += 1;
            }
            assert_eq!(3, n);

            // A clip that holds the cursor reports it in viewport coordinates.
            let bottom = Clip { y: 4, rows: 0, cols: 0 };
            assert_eq!(Result::Success, set_(state, SetOption::Clip, &bottom));
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(Result::Success, get_(state, Data::Clip, &mut out));
            assert_eq!(Clip { y: 4, rows: 2, cols: 10 }, out);
            let mut cursor_y: CellCountInt = 0;
            assert_eq!(Result::Success, get_(state, Data::CursorViewportY, &mut cursor_y));
            assert_eq!(5, cursor_y);

            assert_eq!(Result::InvalidValue, set(state, SetOption::Clip as c_int, std::ptr::null()));
            assert_eq!(Result::InvalidValue, get(state, Data::Clip as c_int, std::ptr::null_mut()));
            free_all(terminal, state, it, cells);
        }
    }

    #[test]
    fn render_row_iterator_next() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let iterator = row_iter(state);

            let rows = st(state).rows;
            if rows == 0 {
                assert!(!row_iterator_next(iterator));
            } else {
                assert!(row_iterator_next(iterator));
                assert_eq!(0, (*iterator).y);

                for i in 1..rows {
                    assert!(row_iterator_next(iterator));
                    assert_eq!(i as usize, (*iterator).y);
                }

                assert!(!row_iterator_next(iterator));
                assert_eq!(rows as usize - 1, (*iterator).y);
            }
            row_iterator_free(iterator);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_iterator_next_dirty() {
        let terminal = new_terminal(10, 4);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let mut iterator = new_row_iter();

            // Invalid arguments neither write the output nor advance the iterator.
            assert_eq!(Result::Success, get_(state, Data::RowIterator, &mut iterator));
            let mut out_y: CellCountInt = 0xCAFE;
            assert!(!row_iterator_next_dirty(std::ptr::null_mut(), &mut out_y));
            assert_eq!(0xCAFE, out_y);
            assert!(!row_iterator_next_dirty(iterator, std::ptr::null_mut()));
            assert_eq!(POSITION_NONE, (*iterator).y);

            // A full redraw returns every row, even if its row flag was cleared.
            st(state).row_data.dirty.fill(false);
            let mut expected_y: CellCountInt = 0;
            while row_iterator_next_dirty(iterator, &mut out_y) {
                assert_eq!(expected_y, out_y);
                expected_y += 1;
            }
            assert_eq!(4, expected_y);
            assert_eq!(3, out_y);

            // A partial redraw skips clean rows without consuming dirty flags.
            st(state).dirty = renderpkg::Dirty::Partial;
            st(state).row_data.dirty.fill(false);
            st(state).row_data.dirty[1] = true;
            st(state).row_data.dirty[3] = true;
            assert_eq!(Result::Success, get_(state, Data::RowIterator, &mut iterator));
            assert!(row_iterator_next_dirty(iterator, &mut out_y));
            assert_eq!(1, out_y);
            assert!(row_iterator_next_dirty(iterator, &mut out_y));
            assert_eq!(3, out_y);
            assert!(!row_iterator_next_dirty(iterator, &mut out_y));
            assert_eq!(3, out_y);
            assert!(st(state).row_data.dirty[1]);
            assert!(st(state).row_data.dirty[3]);

            // The global clean state is authoritative, even if stale row flags exist.
            st(state).dirty = renderpkg::Dirty::False;
            assert_eq!(Result::Success, get_(state, Data::RowIterator, &mut iterator));
            out_y = 0xCAFE;
            assert!(!row_iterator_next_dirty(iterator, &mut out_y));
            assert_eq!(0xCAFE, out_y);

            // Existing iterators observe cleaning through their borrowed state.
            st(state).dirty = renderpkg::Dirty::Partial;
            st(state).row_data.dirty[1] = true;
            assert_eq!(Result::Success, get_(state, Data::RowIterator, &mut iterator));
            assert_eq!(Result::Success, clean(state));
            assert!(!row_iterator_next_dirty(iterator, &mut out_y));
            row_iterator_free(iterator);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_update() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));

            write(terminal, b"hello");
            assert_eq!(Result::Success, update(state, terminal));

            let mut cols: CellCountInt = 0;
            let mut rows_val: CellCountInt = 0;
            assert_eq!(Result::Success, get_(state, Data::Cols, &mut cols));
            assert_eq!(Result::Success, get_(state, Data::Rows, &mut rows_val));
            assert_eq!(80, cols);
            assert_eq!(24, rows_val);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_colors_data_get() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));

            let mut colors = zeroed_colors();
            assert_eq!(Result::Success, get_(state, Data::Colors, &mut colors));

            let state_colors = &st(state).colors;
            assert_eq!(state_colors.background.cval(), colors.background);
            assert_eq!(state_colors.foreground.cval(), colors.foreground);

            if let Some(cursor) = state_colors.cursor {
                assert!(colors.cursor_has_value);
                assert_eq!(cursor.cval(), colors.cursor);
            } else {
                assert!(!colors.cursor_has_value);
            }

            for (expected, actual) in state_colors.palette.iter().zip(colors.palette.iter()) {
                assert_eq!(expected.cval(), *actual);
            }
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_cursor_data_get_matches_scalar_getters() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));

            let mut cursor = zeroed_cursor();
            assert_eq!(Result::Success, get_(state, Data::Cursor, &mut cursor));

            let mut viewport_has_value = false;
            let mut viewport_x: u16 = 0;
            let mut viewport_y: u16 = 0;
            let mut wide_tail = false;
            let mut visible = false;
            let mut blinking = false;
            let mut password_input = false;
            let mut visual_style = CursorVisualStyle::Bar;
            assert_eq!(Result::Success, get_(state, Data::CursorViewportHasValue, &mut viewport_has_value));
            assert_eq!(Result::Success, get_(state, Data::CursorViewportX, &mut viewport_x));
            assert_eq!(Result::Success, get_(state, Data::CursorViewportY, &mut viewport_y));
            assert_eq!(Result::Success, get_(state, Data::CursorViewportWideTail, &mut wide_tail));
            assert_eq!(Result::Success, get_(state, Data::CursorVisible, &mut visible));
            assert_eq!(Result::Success, get_(state, Data::CursorBlinking, &mut blinking));
            assert_eq!(Result::Success, get_(state, Data::CursorPasswordInput, &mut password_input));
            assert_eq!(Result::Success, get_(state, Data::CursorVisualStyle, &mut visual_style));

            assert_eq!(viewport_has_value, cursor.viewport_has_value);
            assert_eq!(viewport_x, cursor.viewport_x);
            assert_eq!(viewport_y, cursor.viewport_y);
            assert_eq!(wide_tail, cursor.wide_tail);
            assert_eq!(visible, cursor.visible);
            assert_eq!(blinking, cursor.blinking);
            assert_eq!(password_input, cursor.password_input);
            assert_eq!(visual_style, cursor.visual_style);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_cursor_data_get_without_viewport() {
        let state = new_state();
        unsafe {
            st(state).cursor.viewport = None;
            st(state).cursor.visible = true;
            st(state).cursor.blinking = true;
            st(state).cursor.password_input = true;
            st(state).cursor.visual_style = cursorpkg::Style::Underline;

            let mut cursor = zeroed_cursor();
            cursor.viewport_x = 0xAAAA;
            cursor.viewport_y = 0xBBBB;
            cursor.wide_tail = true;
            assert_eq!(Result::Success, get_(state, Data::Cursor, &mut cursor));

            assert!(!cursor.viewport_has_value);
            assert_eq!(0xAAAA, cursor.viewport_x);
            assert_eq!(0xBBBB, cursor.viewport_y);
            assert!(cursor.wide_tail);
            assert!(cursor.visible);
            assert!(cursor.blinking);
            assert!(cursor.password_input);
            assert_eq!(CursorVisualStyle::Underline, cursor.visual_style);
            free(state);
        }
    }

    #[test]
    fn render_cursor_data_get_supports_truncated_sized_struct() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let expected = st(state).cursor;
            let viewport = expected.viewport.unwrap();

            let mut cursor = zeroed_cursor();
            cursor.size = offset_of!(Cursor, viewport_y) + size_of::<u16>();
            cursor.wide_tail = !viewport.wide_tail;
            cursor.visible = !expected.visible;
            assert_eq!(Result::Success, get_(state, Data::Cursor, &mut cursor));

            assert!(cursor.viewport_has_value);
            assert_eq!(viewport.x, cursor.viewport_x);
            assert_eq!(viewport.y, cursor.viewport_y);
            assert_eq!(!viewport.wide_tail, cursor.wide_tail);
            assert_eq!(!expected.visible, cursor.visible);
            free(state);
            terminal_c::free(terminal);
        }
    }

    /// The first cell of the first row of an 80x24 terminal with `input`.
    unsafe fn first_cell_for(input: &[u8]) -> (terminal_c::Terminal, RenderState, RowIterator, RowCells) {
        let terminal = new_terminal(80, 24);
        write(terminal, input);
        let state = new_state();
        assert_eq!(Result::Success, update(state, terminal));
        let (it, cells) = first_row_first_cell(state);
        (terminal, state, it, cells)
    }

    fn rgb0() -> RgbC {
        RgbC { r: 0, g: 0, b: 0 }
    }

    #[test]
    fn render_row_cells_bg_color_no_background() {
        unsafe {
            // Write plain text (no background color set).
            let (terminal, state, it, cells) = first_cell_for(b"hello");

            // No background set, should return invalid_value.
            let mut bg = rgb0();
            assert_eq!(Result::InvalidValue, cells_get_(cells, RowCellsData::BgColor, &mut bg));
            free_all(terminal, state, it, cells);
        }
    }

    #[test]
    fn render_row_cells_bg_color_from_style() {
        unsafe {
            // Set an RGB background via SGR 48;2;R;G;B and write text.
            let (terminal, state, it, cells) = first_cell_for(b"\x1b[48;2;10;20;30mA");

            let mut bg = rgb0();
            assert_eq!(Result::Success, cells_get_(cells, RowCellsData::BgColor, &mut bg));
            assert_eq!(10, bg.r);
            assert_eq!(20, bg.g);
            assert_eq!(30, bg.b);
            free_all(terminal, state, it, cells);
        }
    }

    #[test]
    fn render_row_cells_bg_color_from_content_tag() {
        unsafe {
            // Set an RGB background and then erase the line. The erased cells
            // should carry the background color via the content tag (bg_color_rgb)
            // rather than through the style.
            let (terminal, state, it, cells) = first_cell_for(b"\x1b[48;2;10;20;30m\x1b[2K");

            let mut bg = rgb0();
            assert_eq!(Result::Success, cells_get_(cells, RowCellsData::BgColor, &mut bg));
            assert_eq!(10, bg.r);
            assert_eq!(20, bg.g);
            assert_eq!(30, bg.b);
            free_all(terminal, state, it, cells);
        }
    }

    #[test]
    fn render_row_cells_fg_color_no_foreground() {
        unsafe {
            // Write plain text (no foreground color set).
            let (terminal, state, it, cells) = first_cell_for(b"hello");

            // No foreground set, should return invalid_value.
            let mut fg = rgb0();
            assert_eq!(Result::InvalidValue, cells_get_(cells, RowCellsData::FgColor, &mut fg));
            free_all(terminal, state, it, cells);
        }
    }

    #[test]
    fn render_row_cells_fg_color_from_style() {
        unsafe {
            // Set an RGB foreground via SGR 38;2;R;G;B and write text.
            let (terminal, state, it, cells) = first_cell_for(b"\x1b[38;2;10;20;30mA");

            let mut fg = rgb0();
            assert_eq!(Result::Success, cells_get_(cells, RowCellsData::FgColor, &mut fg));
            assert_eq!(10, fg.r);
            assert_eq!(20, fg.g);
            assert_eq!(30, fg.b);
            free_all(terminal, state, it, cells);
        }
    }

    #[test]
    fn render_colors_data_get_supports_truncated_sized_struct() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));

            let mut colors = zeroed_colors();
            let sentinel = RgbC {
                r: 0xAA,
                g: 0xBB,
                b: 0xCC,
            };
            for entry in colors.palette.iter_mut() {
                *entry = sentinel;
            }

            colors.size = offset_of!(Colors, palette) + size_of::<RgbC>() * 2;
            assert_eq!(Result::Success, get_(state, Data::Colors, &mut colors));

            let state_colors = &st(state).colors;
            assert_eq!(state_colors.palette[0].cval(), colors.palette[0]);
            assert_eq!(state_colors.palette[1].cval(), colors.palette[1]);
            assert_eq!(sentinel, colors.palette[2]);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_get_multi_success() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));

            let mut cols: u16 = 0;
            let mut rows: u16 = 0;
            let mut written: usize = 0;

            let keys = [Data::Cols as c_int, Data::Rows as c_int];
            let values = [vp(&mut cols), vp(&mut rows)];
            assert_eq!(
                Result::Success,
                get_multi(state, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);
            assert_eq!(80, cols);
            assert_eq!(24, rows);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_get_multi_null_returns_invalid_value() {
        let mut cols: u16 = 0;
        let values = [vp(&mut cols)];
        assert_eq!(Result::InvalidValue, unsafe {
            get_multi(std::ptr::null_mut(), 1, std::ptr::null(), values.as_ptr(), std::ptr::null_mut())
        });
    }

    #[test]
    fn render_row_get_multi_success() {
        let terminal = new_terminal(80, 24);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let it = row_iter(state);
            assert!(row_iterator_next(it));

            let mut dirty = true;
            let mut written: usize = 0;

            let keys = [RowData::Dirty as c_int];
            let values = [vp(&mut dirty)];
            assert_eq!(
                Result::Success,
                row_get_multi(it, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);
            row_iterator_free(it);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_get_multi_null_returns_invalid_value() {
        let mut dirty = false;
        let values = [vp(&mut dirty)];
        assert_eq!(Result::InvalidValue, unsafe {
            row_get_multi(std::ptr::null_mut(), 1, std::ptr::null(), values.as_ptr(), std::ptr::null_mut())
        });
    }

    #[test]
    fn render_row_cells_get_multi_success() {
        unsafe {
            let (terminal, state, it, cells) = first_cell_for(b"A");

            let mut raw: row::CRow = 0;
            let mut written: usize = 0;

            let keys = [RowCellsData::Raw as c_int];
            let values = [vp(&mut raw)];
            assert_eq!(
                Result::Success,
                row_cells_get_multi(cells, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);
            free_all(terminal, state, it, cells);
        }
    }

    #[test]
    fn render_row_cells_get_multi_null_returns_invalid_value() {
        let mut raw: row::CRow = 0;
        let values = [vp(&mut raw)];
        assert_eq!(Result::InvalidValue, unsafe {
            row_cells_get_multi(std::ptr::null_mut(), 1, std::ptr::null(), values.as_ptr(), std::ptr::null_mut())
        });
    }

    /// Test helper: a terminal of the given size with `lines` numbered lines
    /// written, so everything above the last `rows` lines is scrollback.
    fn test_terminal_with_lines(cols: CellCountInt, rows: CellCountInt, lines: usize) -> terminal_c::Terminal {
        let terminal = new_terminal(cols, rows);
        for i in 0..lines {
            let line = format!("{}line {}", if i == 0 { "" } else { "\r\n" }, i);
            write(terminal, line.as_bytes());
        }
        terminal
    }

    /// Test helper: a row id and its viewport y from one iteration.
    #[derive(Debug, Clone, Copy)]
    struct TestRowEntry {
        id: RowId,
        viewport_y: i32,
    }

    /// Test helper: collect the id and viewport y of every captured row.
    unsafe fn test_collect_rows(state: RenderState) -> Vec<TestRowEntry> {
        let it = row_iter(state);
        let mut list = Vec::new();
        while row_iterator_next(it) {
            let mut entry = TestRowEntry {
                id: RowId::default(),
                viewport_y: 0,
            };
            assert_eq!(Result::Success, row_get_(it, RowData::Id, &mut entry.id));
            assert_eq!(Result::Success, row_get_(it, RowData::ViewportY, &mut entry.viewport_y));
            list.push(entry);
        }
        row_iterator_free(it);
        list
    }

    #[test]
    fn render_overscan_request_applies_on_the_next_update() {
        let rows: usize = 10;
        let terminal = test_terminal_with_lines(10, rows as u16, 50);
        zt(terminal).scroll_viewport(ScrollViewport::Delta(-5));

        let state = new_state();
        unsafe {
            let req = Overscan { above: 3, below: 2 };
            assert_eq!(Result::Success, set_(state, SetOption::Overscan, &req));
            assert_eq!(Result::Success, update(state, terminal));

            // Changing the request after an update must not affect reading the
            // rows of that update.
            let none = Overscan::default();
            assert_eq!(Result::Success, set_(state, SetOption::Overscan, &none));
            let mut out = Overscan::default();
            assert_eq!(Result::Success, get_(state, Data::OverscanRequest, &mut out));
            assert_eq!(none, out);
            assert_eq!(Result::Success, get_(state, Data::Overscan, &mut out));
            assert_eq!(req, out);

            let list = test_collect_rows(state);
            assert_eq!(3 + rows + 2, list.len());
            assert_eq!(-3, list[0].viewport_y);

            // The next update uses the new request.
            assert_eq!(Result::Success, update(state, terminal));
            let list = test_collect_rows(state);
            assert_eq!(rows, list.len());
            assert_eq!(0, list[0].viewport_y);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_overscan_option_roundtrip() {
        let state = new_state();
        unsafe {
            let req = Overscan { above: 4, below: 1 };
            assert_eq!(Result::Success, set_(state, SetOption::Overscan, &req));

            let mut out = Overscan { above: 99, below: 99 };
            assert_eq!(Result::Success, get_(state, Data::OverscanRequest, &mut out));
            assert_eq!(Overscan { above: 4, below: 1 }, out);

            // Nothing is captured before an update.
            assert_eq!(Result::Success, get_(state, Data::Overscan, &mut out));
            assert_eq!(Overscan::default(), out);
            free(state);
        }
    }

    #[test]
    fn render_overscan_clamps_to_existing_rows() {
        let terminal = test_terminal_with_lines(10, 10, 50);
        let t = zt(terminal);
        let state = new_state();
        unsafe {
            let req = Overscan { above: 3, below: 2 };
            assert_eq!(Result::Success, set_(state, SetOption::Overscan, &req));

            let mut out = Overscan::default();

            // At the bottom there is nothing below the viewport.
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(Result::Success, get_(state, Data::Overscan, &mut out));
            assert_eq!(Overscan { above: 3, below: 0 }, out);

            // Scrolled up, both sides are available.
            t.scroll_viewport(ScrollViewport::Delta(-5));
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(Result::Success, get_(state, Data::Overscan, &mut out));
            assert_eq!(Overscan { above: 3, below: 2 }, out);

            // At the top there is nothing above the viewport.
            t.scroll_viewport(ScrollViewport::Top);
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(Result::Success, get_(state, Data::Overscan, &mut out));
            assert_eq!(Overscan { above: 0, below: 2 }, out);

            // The request is reported unchanged.
            assert_eq!(Result::Success, get_(state, Data::OverscanRequest, &mut out));
            assert_eq!(req, out);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_iterator_unchanged_without_overscan() {
        let rows: usize = 10;
        let terminal = test_terminal_with_lines(10, rows as u16, 50);
        zt(terminal).scroll_viewport(ScrollViewport::Delta(-5));

        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let it = row_iter(state);

            // The first update is a full redraw so next_dirty visits every row.
            let mut count: usize = 0;
            let mut y: CellCountInt = 0;
            while row_iterator_next_dirty(it, &mut y) {
                assert_eq!(count, y as usize);
                let mut vy: i32 = 0;
                assert_eq!(Result::Success, row_get_(it, RowData::ViewportY, &mut vy));
                assert_eq!(y as i32, vy);
                count += 1;
            }
            assert_eq!(rows, count);
            row_iterator_free(it);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_iterator_covers_overscan() {
        let rows: usize = 10;
        let terminal = test_terminal_with_lines(10, rows as u16, 50);
        zt(terminal).scroll_viewport(ScrollViewport::Delta(-5));

        unsafe {
            // A reference state with no overscan for comparison.
            let plain = new_state();
            assert_eq!(Result::Success, update(plain, terminal));
            let plain_it = row_iter(plain);
            assert!(row_iterator_next(plain_it));
            let mut plain_raw: row::CRow = 0;
            let mut plain_id = RowId::default();
            assert_eq!(Result::Success, row_get_(plain_it, RowData::Raw, &mut plain_raw));
            assert_eq!(Result::Success, row_get_(plain_it, RowData::Id, &mut plain_id));

            let state = new_state();
            let req = Overscan { above: 3, below: 2 };
            assert_eq!(Result::Success, set_(state, SetOption::Overscan, &req));
            assert_eq!(Result::Success, update(state, terminal));

            let it = row_iter(state);
            let mut count: usize = 0;
            let mut found_top = false;
            while row_iterator_next(it) {
                let mut vy: i32 = 0;
                assert_eq!(Result::Success, row_get_(it, RowData::ViewportY, &mut vy));
                assert_eq!(count as i32 - 3, vy);

                if vy == 0 {
                    found_top = true;
                    let mut raw: row::CRow = 0;
                    let mut id = RowId::default();
                    assert_eq!(Result::Success, row_get_(it, RowData::Raw, &mut raw));
                    assert_eq!(Result::Success, row_get_(it, RowData::Id, &mut id));
                    assert_eq!(plain_raw, raw);
                    assert_eq!(plain_id, id);
                }
                count += 1;
            }
            assert_eq!(3 + rows + 2, count);
            assert!(found_top);
            row_iterator_free(it);
            row_iterator_free(plain_it);
            free(state);
            free(plain);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_iterator_overscan_at_the_bottom() {
        let rows: usize = 10;
        let terminal = test_terminal_with_lines(10, rows as u16, 50);
        let state = new_state();
        unsafe {
            let req = Overscan { above: 3, below: 2 };
            assert_eq!(Result::Success, set_(state, SetOption::Overscan, &req));
            assert_eq!(Result::Success, update(state, terminal));

            let list = test_collect_rows(state);
            assert_eq!(3 + rows, list.len());
            assert_eq!(-3, list[0].viewport_y);
            assert_eq!(rows as i32 - 1, list[list.len() - 1].viewport_y);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_ids_stable_across_scroll() {
        let rows: i32 = 10;
        let terminal = test_terminal_with_lines(10, rows as u16, 50);
        let t = zt(terminal);
        t.scroll_viewport(ScrollViewport::Delta(-5));

        let state = new_state();
        unsafe {
            let req = Overscan { above: 3, below: 2 };
            assert_eq!(Result::Success, set_(state, SetOption::Overscan, &req));
            assert_eq!(Result::Success, update(state, terminal));

            let before = test_collect_rows(state);

            t.scroll_viewport(ScrollViewport::Delta(-1));
            assert_eq!(Result::Success, update(state, terminal));

            let after = test_collect_rows(state);
            assert_eq!(before.len(), after.len());

            // Every row except the last one (which scrolled out of the captured
            // rows at the bottom) is still present, one row further down.
            for old in &before {
                if old.viewport_y == rows + 1 {
                    continue;
                }
                let new_vy = after.iter().find(|cur| cur.id == old.id).map(|cur| cur.viewport_y);
                assert_eq!(old.viewport_y + 1, new_vy.unwrap());
            }
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_row_id_without_overscan() {
        let terminal = test_terminal_with_lines(10, 5, 20);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));
            let first = test_collect_rows(state);

            assert_eq!(Result::Success, update(state, terminal));
            let second = test_collect_rows(state);

            assert_eq!(5, first.len());
            assert_eq!(first.len(), second.len());
            for (a, b) in first.iter().zip(second.iter()) {
                assert_eq!(a.id, b.id);
                assert_ne!(a.id, RowId::default());
            }
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_overscan_invalid_values() {
        let state = new_state();
        unsafe {
            assert_eq!(Result::InvalidValue, get(state, Data::Overscan as c_int, std::ptr::null_mut()));
            assert_eq!(Result::InvalidValue, set(state, SetOption::Overscan as c_int, std::ptr::null()));

            let terminal = test_terminal_with_lines(10, 5, 1);
            assert_eq!(Result::Success, update(state, terminal));

            let it = row_iter(state);

            // Before the first next, there is no current row.
            let mut id = RowId::default();
            assert_eq!(Result::InvalidValue, row_get_(it, RowData::Id, &mut id));
            row_iterator_free(it);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_overscan_get_multi_and_row_get_multi() {
        let terminal = test_terminal_with_lines(10, 5, 20);
        zt(terminal).scroll_viewport(ScrollViewport::Delta(-5));

        let state = new_state();
        unsafe {
            let req = Overscan { above: 1, below: 1 };
            assert_eq!(Result::Success, set_(state, SetOption::Overscan, &req));
            assert_eq!(Result::Success, update(state, terminal));

            let mut rows: u16 = 0;
            let mut overscan = Overscan::default();
            let mut overscan_req = Overscan::default();
            let mut written: usize = 0;
            let keys = [Data::Rows as c_int, Data::Overscan as c_int, Data::OverscanRequest as c_int];
            let values = [vp(&mut rows), vp(&mut overscan), vp(&mut overscan_req)];
            assert_eq!(
                Result::Success,
                get_multi(state, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);
            assert_eq!(5, rows);
            assert_eq!(Overscan { above: 1, below: 1 }, overscan);
            assert_eq!(req, overscan_req);

            let it = row_iter(state);
            assert!(row_iterator_next(it));

            let mut dirty = false;
            let mut vy: i32 = 0;
            let mut id = RowId::default();
            let row_keys = [RowData::Dirty as c_int, RowData::ViewportY as c_int, RowData::Id as c_int];
            let row_values = [vp(&mut dirty), vp(&mut vy), vp(&mut id)];
            assert_eq!(
                Result::Success,
                row_get_multi(it, row_keys.len(), row_keys.as_ptr(), row_values.as_ptr(), &mut written)
            );
            assert_eq!(row_keys.len(), written);
            assert_eq!(-1, vy);
            assert_ne!(id, RowId::default());
            row_iterator_free(it);
            free(state);
            terminal_c::free(terminal);
        }
    }

    fn new_image_iter() -> ImageIterator {
        let mut it: ImageIterator = std::ptr::null_mut();
        assert_eq!(Result::Success, unsafe { image_iterator_new(Some(&C_ALLOCATOR), &mut it) });
        assert!(!it.is_null());
        it
    }

    #[test]
    fn render_image_iterator_new_free() {
        let it = new_image_iter();
        unsafe {
            // A new iterator is empty until it is filled from a render state.
            assert!(!image_iterator_next(it));
            let mut image_id: u32 = 0;
            assert_eq!(Result::InvalidValue, image_get_(it, ImageData::ImageId, &mut image_id));
            image_iterator_free(it);

            image_iterator_free(std::ptr::null_mut());
            assert!(!image_iterator_next(std::ptr::null_mut()));
        }
    }

    #[test]
    fn render_image_iterator_kitty_virtual_placement() {
        let terminal = new_terminal(10, 5);
        assert_eq!(Result::Success, unsafe { terminal_c::resize(terminal, 10, 5, 10, 10) });

        // Transmit a 1x2 RGB image with a 2x1 virtual placement (U=1), then
        // write two placeholder cells (U+10EEEE) for it at row 1, column 3.
        write(terminal, b"\x1b_Ga=T,t=d,f=24,i=1,p=1,s=1,v=2,U=1,c=2,r=1,q=2;////////\x1b\\");
        write(terminal, "\x1b[2;4H\x1b[38;5;1;58;5;1m\u{10EEEE}\u{10EEEE}".as_bytes());

        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));

            let mut it = new_image_iter();
            assert_eq!(Result::Success, get_(state, Data::ImageIterator, &mut it));
            assert!(image_iterator_next(it));

            let mut protocol = ImageProtocol::Kitty;
            let mut layer = ImageLayer::AboveText;
            let mut kind = KittyPlacementKind::Relative;
            let mut image_id: u32 = 0;
            let mut image_generation: u64 = 0;
            let mut placement_id: u32 = 0;
            let mut col: i32 = 0;
            let mut row_: i32 = 0;
            let mut cols: u32 = 0;
            let mut rows: u32 = 0;
            let mut source_height: u32 = 0;
            let mut z: i32 = 0;
            let mut written: usize = 0;
            let keys = [
                ImageData::Protocol,
                ImageData::Layer,
                ImageData::KittyKind,
                ImageData::ImageId,
                ImageData::ImageGeneration,
                ImageData::KittyPlacementId,
                ImageData::ViewportCol,
                ImageData::ViewportRow,
                ImageData::GridCols,
                ImageData::GridRows,
                ImageData::SourceHeight,
                ImageData::Z,
            ]
            .map(|k| k as c_int);
            let values = [
                vp(&mut protocol),
                vp(&mut layer),
                vp(&mut kind),
                vp(&mut image_id),
                vp(&mut image_generation),
                vp(&mut placement_id),
                vp(&mut col),
                vp(&mut row_),
                vp(&mut cols),
                vp(&mut rows),
                vp(&mut source_height),
                vp(&mut z),
            ];
            assert_eq!(
                Result::Success,
                image_get_multi(it, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);
            assert_eq!(ImageProtocol::Kitty, protocol);
            assert_eq!(ImageLayer::BelowText, layer);
            assert_eq!(KittyPlacementKind::Virtual, kind);
            assert_eq!(1, image_id);
            assert_eq!(1, placement_id);
            assert_eq!(3, col);
            assert_eq!(1, row_);
            assert_eq!(2, cols);
            assert_eq!(1, rows);
            assert_eq!(2, source_height);
            assert_eq!(-1, z);

            // The generation matches the image in the terminal's storage.
            assert_eq!(
                zt(terminal).screens.active.kitty_images.image_by_id(1).unwrap().generation,
                image_generation
            );

            // The 1x2 pixel image is scaled to fit 2x1 cells of 10x10 pixels.
            // That makes it 5 pixels wide, centered in the 20 pixel width.
            let mut width: u32 = 0;
            let mut offset_x: u32 = 0;
            assert_eq!(Result::Success, image_get_(it, ImageData::PixelWidth, &mut width));
            assert_eq!(Result::Success, image_get_(it, ImageData::CellOffsetX, &mut offset_x));
            assert_eq!(5, width);
            assert_eq!(8, offset_x);

            assert!(!image_iterator_next(it));
            assert_eq!(Result::InvalidValue, image_get_(it, ImageData::ImageId, &mut image_id));
            image_iterator_free(it);
            free(state);
            terminal_c::free(terminal);
        }
    }

    /// Test helper: a terminal with 10x10 pixel cells that holds a 1x1 RGB
    /// Kitty image with ID 1 and no placements.
    fn test_kitty_image_terminal(cols: CellCountInt, rows: CellCountInt) -> terminal_c::Terminal {
        let terminal = new_terminal(cols, rows);
        assert_eq!(Result::Success, unsafe { terminal_c::resize(terminal, cols, rows, 10, 10) });
        write(terminal, b"\x1b_Ga=t,t=d,f=24,i=1,s=1,v=1,q=2;AAAA\x1b\\");
        terminal
    }

    #[test]
    fn render_image_iterator_kitty_fixed_placement() {
        use crate::terminal::kitty::graphics_storage::{PlacementId, PlacementKey, PlacementTag, TerminalInfo};

        let terminal = test_kitty_image_terminal(10, 10);

        // Place image 1 at column 2, row 1 with the same values as the Zig
        // test "fixed: pin placement in viewport".
        write(terminal, b"\x1b[2;3H\x1b_Ga=p,i=1,p=7,c=3,r=2,X=4,Y=3,z=-5,C=1,q=2\x1b\\");

        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));

            let mut it = new_image_iter();
            assert_eq!(Result::Success, get_(state, Data::ImageIterator, &mut it));
            assert!(image_iterator_next(it));

            let mut protocol = ImageProtocol::Kitty;
            let mut layer = ImageLayer::AboveText;
            let mut kind = KittyPlacementKind::Relative;
            let mut image_id: u32 = 0;
            let mut image_generation: u64 = 0;
            let mut placement_id: u32 = 0;
            let mut col: i32 = 0;
            let mut row_: i32 = 0;
            let mut cols: u32 = 0;
            let mut rows: u32 = 0;
            let mut offset_x: u32 = 0;
            let mut offset_y: u32 = 0;
            let mut width: u32 = 0;
            let mut height: u32 = 0;
            let mut source_x: u32 = 0;
            let mut source_y: u32 = 0;
            let mut source_width: u32 = 0;
            let mut source_height: u32 = 0;
            let mut z: i32 = 0;
            let mut written: usize = 0;
            let keys = [
                ImageData::Protocol,
                ImageData::Layer,
                ImageData::KittyKind,
                ImageData::ImageId,
                ImageData::ImageGeneration,
                ImageData::KittyPlacementId,
                ImageData::ViewportCol,
                ImageData::ViewportRow,
                ImageData::GridCols,
                ImageData::GridRows,
                ImageData::CellOffsetX,
                ImageData::CellOffsetY,
                ImageData::PixelWidth,
                ImageData::PixelHeight,
                ImageData::SourceX,
                ImageData::SourceY,
                ImageData::SourceWidth,
                ImageData::SourceHeight,
                ImageData::Z,
            ]
            .map(|k| k as c_int);
            let values = [
                vp(&mut protocol),
                vp(&mut layer),
                vp(&mut kind),
                vp(&mut image_id),
                vp(&mut image_generation),
                vp(&mut placement_id),
                vp(&mut col),
                vp(&mut row_),
                vp(&mut cols),
                vp(&mut rows),
                vp(&mut offset_x),
                vp(&mut offset_y),
                vp(&mut width),
                vp(&mut height),
                vp(&mut source_x),
                vp(&mut source_y),
                vp(&mut source_width),
                vp(&mut source_height),
                vp(&mut z),
            ];
            assert_eq!(
                Result::Success,
                image_get_multi(it, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);

            let t = zt(terminal);
            let storage = &t.screens.active.kitty_images;
            let image = storage.image_by_id(1).unwrap();
            let sp = storage
                .placements
                .get(&PlacementKey {
                    image_id: 1,
                    placement_id: PlacementId {
                        tag: PlacementTag::External,
                        id: 7,
                    },
                })
                .unwrap();
            let dest = sp.pixel_size(image, &TerminalInfo::from_terminal(t));
            let source = sp.source_rect(image);
            assert_eq!(ImageProtocol::Kitty, protocol);
            assert_eq!(ImageLayer::BelowText, layer);
            assert_eq!(KittyPlacementKind::Fixed, kind);
            assert_eq!(1, image_id);
            assert_eq!(image.generation, image_generation);
            assert_eq!(7, placement_id);
            assert_eq!(2, col);
            assert_eq!(1, row_);
            assert_eq!(3, cols);
            assert_eq!(2, rows);
            assert_eq!(4, offset_x);
            assert_eq!(3, offset_y);
            assert_eq!(dest.width, width);
            assert_eq!(dest.height, height);
            assert_eq!(source.x, source_x);
            assert_eq!(source.y, source_y);
            assert_eq!(source.width, source_width);
            assert_eq!(source.height, source_height);
            assert_eq!(-5, z);
            assert!(!image_iterator_next(it));

            // A placement the program gave no ID gets an internal one, which is
            // reported as 0.
            write(terminal, b"\x1b[6;1H\x1b_Ga=p,i=1,C=1,q=2\x1b\\");
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(Result::Success, get_(state, Data::ImageIterator, &mut it));
            let mut found_internal = false;
            let mut count: usize = 0;
            while image_iterator_next(it) {
                assert_eq!(Result::Success, image_get_(it, ImageData::KittyPlacementId, &mut placement_id));
                assert_eq!(Result::Success, image_get_(it, ImageData::ViewportRow, &mut row_));
                if row_ == 5 {
                    assert_eq!(0, placement_id);
                    found_internal = true;
                } else {
                    assert_eq!(7, placement_id);
                }
                count += 1;
            }
            assert_eq!(2, count);
            assert!(found_internal);
            image_iterator_free(it);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_image_iterator_fixed_placement_above_the_viewport() {
        let terminal = test_kitty_image_terminal(10, 5);

        // A placement 2 columns by 3 rows on the top row, then two lines of
        // output so that it starts 2 rows above the viewport.
        write(terminal, b"\x1b[1;2H\x1b_Ga=p,i=1,p=1,c=2,r=3,C=1,q=2\x1b\\");
        write(terminal, b"\x1b[5;1H\n\n");

        let state = new_state();
        unsafe {
            let req = Overscan { above: 1, below: 0 };
            assert_eq!(Result::Success, set_(state, SetOption::Overscan, &req));
            assert_eq!(Result::Success, update(state, terminal));

            let mut out = Overscan::default();
            assert_eq!(Result::Success, get_(state, Data::Overscan, &mut out));
            assert_eq!(1, out.above);

            let mut it = new_image_iter();
            assert_eq!(Result::Success, get_(state, Data::ImageIterator, &mut it));
            assert!(image_iterator_next(it));

            // Its top row is above the one overscan row, and it is still reported
            // at its full size.
            let mut col: i32 = 0;
            let mut row_: i32 = 0;
            let mut cols: u32 = 0;
            let mut rows: u32 = 0;
            let mut width: u32 = 0;
            let mut height: u32 = 0;
            assert_eq!(Result::Success, image_get_(it, ImageData::ViewportCol, &mut col));
            assert_eq!(Result::Success, image_get_(it, ImageData::ViewportRow, &mut row_));
            assert_eq!(Result::Success, image_get_(it, ImageData::GridCols, &mut cols));
            assert_eq!(Result::Success, image_get_(it, ImageData::GridRows, &mut rows));
            assert_eq!(Result::Success, image_get_(it, ImageData::PixelWidth, &mut width));
            assert_eq!(Result::Success, image_get_(it, ImageData::PixelHeight, &mut height));
            assert_eq!(1, col);
            assert_eq!(-2, row_);
            assert_eq!(2, cols);
            assert_eq!(3, rows);
            assert_eq!(20, width);
            assert_eq!(30, height);
            assert!(!image_iterator_next(it));
            image_iterator_free(it);
            free(state);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn render_image_placements_generation() {
        let terminal = test_kitty_image_terminal(10, 5);
        let state = new_state();
        unsafe {
            assert_eq!(Result::Success, update(state, terminal));

            let mut generation: u64 = 0;
            assert_eq!(Result::Success, get_(state, Data::ImagePlacementsGeneration, &mut generation));

            let mut current: u64 = 0;

            // A quiet update leaves it alone.
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(Result::Success, get_(state, Data::ImagePlacementsGeneration, &mut current));
            assert_eq!(generation, current);

            // Adding a placement changes it.
            write(terminal, b"\x1b[1;1H\x1b_Ga=p,i=1,p=1,c=2,r=2,C=1,q=2\x1b\\");
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(Result::Success, get_(state, Data::ImagePlacementsGeneration, &mut current));
            assert_ne!(generation, current);
            generation = current;

            // Dirtying a row without images leaves it alone.
            write(terminal, b"\x1b[4;1Hhello");
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(Result::Success, get_(state, Data::ImagePlacementsGeneration, &mut current));
            assert_eq!(generation, current);

            // Scrolling the screen moves the fixed placement, which changes it.
            write(terminal, b"\x1b[5;1H\n");
            assert_eq!(Result::Success, update(state, terminal));
            assert_eq!(Result::Success, get_(state, Data::ImagePlacementsGeneration, &mut current));
            assert_ne!(generation, current);
            free(state);
            terminal_c::free(terminal);
        }
    }
}
