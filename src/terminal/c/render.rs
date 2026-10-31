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
//!   raw field pointers since the caller's struct may be truncated.

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
use crate::terminal::style::Color as StyleColor;

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
}

impl RenderStateWrapper {
    /// Apply any pending option changes to the render state. Called at
    /// the start of every update.
    fn apply_options(&mut self) {
        self.state.overscan_request = self.overscan_request;
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
    /// zero without overscan.
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
    /// `image_placements_generation` → `u64`.
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
    }
}

crate::c_api_enum! {
    /// C: GhosttyRenderStateOption
    ///
    /// Input types: `dirty` → [`Dirty`], `overscan` → [`Overscan`].
    pub enum SetOption {
        Dirty = 0,
        Overscan = 1,
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
                viewport_y_base: -(s.overscan.above as i32),
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
