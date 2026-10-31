//! Port of `src/terminal/c/kitty_graphics.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - This port always builds Kitty graphics (`build_options::KITTY_GRAPHICS`),
//!   so the `*anyopaque` stand-ins of builds without it are not ported.
//! - The placement iterator holds a `hash_map::Iter` and the current entry
//!   with their lifetimes erased to `'static` (Zig: a hash map iterator and
//!   entry pointers into the live storage). Like Zig, the iterator is only
//!   valid until the storage changes.
//! - Enum arguments (`Data`, `ImageData`, `PlacementData`,
//!   `PlacementIteratorOption`) and the `GhosttyKittyPlacementLayer` value
//!   cross the ABI as `c_int` and are validated. A null `out` pointer
//!   returns `invalid_value` (Zig: undefined behavior).
//! - `GhosttyKittyImageFormat`/`GhosttyKittyImageCompression` are declared
//!   here as C enums mirroring `kitty_cmd::Format`/`Compression` (Zig:
//!   aliases of the `lib.Enum` types, which are C ints in C builds; the
//!   Rust core enums are `u8`).

use std::collections::hash_map;
use std::ffi::{c_int, c_void};

use super::grid_ref;
use super::result::Result;
use super::selection as selection_c;
use super::terminal as terminal_c;
use super::types::HandleAllocator;
use crate::lib::allocator::Allocator as CAllocator;
use crate::terminal::kitty::graphics_command as kitty_cmd;
use crate::terminal::kitty::graphics_image::Image;
use crate::terminal::kitty::graphics_storage::{
    self as kitty_storage, ImageStorage, Location, Placement, PlacementKey, TerminalInfo,
};
use crate::terminal::page_list::Pin;
use crate::terminal::point;
use crate::terminal::terminal::Terminal;

/// C: GhosttyKittyGraphics
pub type KittyGraphics = *mut ImageStorage;

/// C: GhosttyKittyGraphicsImage
pub type ImageHandle = *const Image;

/// C: GhosttyKittyGraphicsPlacementIterator
pub type PlacementIterator = *mut PlacementIteratorWrapper;

pub struct PlacementIteratorWrapper {
    alloc: HandleAllocator,
    /// (Zig: `undefined` until the iterator is reset with `get`.)
    inner: Option<hash_map::Iter<'static, PlacementKey, Placement>>,
    pub(crate) entry: Option<(&'static PlacementKey, &'static Placement)>,
    layer_filter: PlacementLayer,
}

crate::c_api_enum! {
    /// C: GhosttyKittyGraphicsData
    ///
    /// Output types: `placement_iterator` → `GhosttyKittyGraphicsPlacementIterator`
    /// (an existing iterator, reset in place), `generation` → `u64`.
    pub enum Data {
        Invalid = 0,
        PlacementIterator = 1,
        Generation = 2,
    }
}

crate::c_api_enum! {
    /// C: GhosttyKittyGraphicsPlacementData
    ///
    /// Output types: `is_virtual` → `bool`, `z` → `i32`, everything else
    /// → `u32`.
    pub enum PlacementData {
        Invalid = 0,
        ImageId = 1,
        PlacementId = 2,
        IsVirtual = 3,
        XOffset = 4,
        YOffset = 5,
        SourceX = 6,
        SourceY = 7,
        SourceWidth = 8,
        SourceHeight = 9,
        Columns = 10,
        Rows = 11,
        Z = 12,
    }
}

/// C: ghostty_kitty_graphics_get
///
/// # Safety
/// `graphics_` must be a live storage handle; `out` must be NULL or point
/// to the data kind's output type.
#[export_name = "ghostty_kitty_graphics_get"]
pub unsafe extern "C" fn get(graphics_: KittyGraphics, data: c_int, out: *mut c_void) -> Result {
    let Some(data) = Data::from_int(data) else {
        return Result::InvalidValue;
    };
    if out.is_null() {
        return Result::InvalidValue;
    }
    match data {
        Data::Invalid => Result::InvalidValue,
        _ => get_typed(graphics_, data, out),
    }
}

unsafe fn get_typed(graphics_: KittyGraphics, data: Data, out: *mut c_void) -> Result {
    let Some(storage) = graphics_.as_ref() else {
        return Result::InvalidValue;
    };
    match data {
        Data::Invalid => return Result::InvalidValue,
        Data::PlacementIterator => {
            let Some(it) = (*(out as *mut PlacementIterator)).as_mut() else {
                return Result::InvalidValue;
            };
            // SAFETY: the iterator is only valid until the storage
            // changes (C API contract, like Zig's hash map iterator).
            let inner: hash_map::Iter<'static, PlacementKey, Placement> =
                std::mem::transmute(storage.placements.iter());
            *it = PlacementIteratorWrapper {
                alloc: it.alloc,
                inner: Some(inner),
                entry: None,
                layer_filter: it.layer_filter,
            };
        }
        Data::Generation => *(out as *mut u64) = storage.generation,
    }
    Result::Success
}

crate::c_api_enum! {
    /// C: GhosttyKittyPlacementLayer
    pub enum PlacementLayer {
        All = 0,
        BelowBg = 1,
        BelowText = 2,
        AboveText = 3,
    }
}

impl PlacementLayer {
    fn matches(self, z: i32) -> bool {
        match self {
            PlacementLayer::All => true,
            PlacementLayer::BelowBg => z < i32::MIN / 2,
            PlacementLayer::BelowText => z >= i32::MIN / 2 && z < 0,
            PlacementLayer::AboveText => z >= 0,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttyKittyGraphicsPlacementIteratorOption
    ///
    /// Value types: `layer` → [`PlacementLayer`].
    pub enum PlacementIteratorOption {
        Layer = 0,
    }
}

crate::c_api_enum! {
    /// C: GhosttyKittyImageFormat (Zig: `kitty_cmd.Transmission.Format`).
    pub enum ImageFormat {
        Rgb = 0,
        Rgba = 1,
        Png = 2,
        GrayAlpha = 3,
        Gray = 4,
    }
}

impl ImageFormat {
    pub fn from_zig(f: kitty_cmd::Format) -> ImageFormat {
        match f {
            kitty_cmd::Format::Rgb => ImageFormat::Rgb,
            kitty_cmd::Format::Rgba => ImageFormat::Rgba,
            kitty_cmd::Format::Png => ImageFormat::Png,
            kitty_cmd::Format::GrayAlpha => ImageFormat::GrayAlpha,
            kitty_cmd::Format::Gray => ImageFormat::Gray,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttyKittyImageCompression (Zig:
    /// `kitty_cmd.Transmission.Compression`).
    pub enum ImageCompression {
        None = 0,
        ZlibDeflate = 1,
    }
}

impl ImageCompression {
    pub fn from_zig(c: kitty_cmd::Compression) -> ImageCompression {
        match c {
            kitty_cmd::Compression::None => ImageCompression::None,
            kitty_cmd::Compression::ZlibDeflate => ImageCompression::ZlibDeflate,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttyKittyGraphicsImageData
    ///
    /// Output types: `id`/`number`/`width`/`height` → `u32`, `format` →
    /// [`ImageFormat`], `compression` → [`ImageCompression`], `data_ptr`
    /// → `const uint8_t*`, `data_len` → `usize`, `generation` → `u64`.
    pub enum ImageData {
        Invalid = 0,
        Id = 1,
        Number = 2,
        Width = 3,
        Height = 4,
        Format = 5,
        Compression = 6,
        DataPtr = 7,
        DataLen = 8,
        Generation = 9,
    }
}

/// C: ghostty_kitty_graphics_animation_tick
///
/// # Safety
/// `graphics_` must be a live storage handle; `next_ms` must be NULL or
/// valid.
#[export_name = "ghostty_kitty_graphics_animation_tick"]
pub unsafe extern "C" fn animation_tick(graphics_: KittyGraphics, now_ms: u64, next_ms: *mut u64) -> Result {
    let Some(out) = next_ms.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(storage) = graphics_.as_mut() else {
        return Result::InvalidValue;
    };

    // The tick needs an Io only to hand out new generation numbers.
    // (Rust: the generation counter needs no Io.)
    let Some(delay) = storage.animation_tick(now_ms) else {
        return Result::NoValue;
    };
    // The Zig tick returns a delay. The C API returns the absolute
    // time instead, so callers don't have to remember when they
    // called.
    *out = now_ms.saturating_add(delay);
    Result::Success
}

/// C: ghostty_kitty_graphics_image
///
/// # Safety
/// `graphics_` must be a live storage handle.
#[export_name = "ghostty_kitty_graphics_image"]
pub unsafe extern "C" fn image_get_handle(graphics_: KittyGraphics, image_id: u32) -> ImageHandle {
    let Some(storage) = graphics_.as_ref() else {
        return std::ptr::null();
    };
    match storage.images.get(&image_id) {
        Some(img) => img,
        None => std::ptr::null(),
    }
}

/// C: ghostty_kitty_graphics_image_get
///
/// # Safety
/// `image_` must be NULL or a live image handle; `out` must be NULL or
/// point to the data kind's output type.
#[export_name = "ghostty_kitty_graphics_image_get"]
pub unsafe extern "C" fn image_get(image_: ImageHandle, data: c_int, out: *mut c_void) -> Result {
    let Some(data) = ImageData::from_int(data) else {
        return Result::InvalidValue;
    };
    match data {
        ImageData::Invalid => Result::InvalidValue,
        _ => image_get_typed(image_, data, out),
    }
}

/// C: ghostty_kitty_graphics_image_get_multi
///
/// # Safety
/// `keys` and `values` must be NULL or valid for `count` entries; see
/// [`image_get`].
#[export_name = "ghostty_kitty_graphics_image_get_multi"]
pub unsafe extern "C" fn image_get_multi(
    image_: ImageHandle,
    count: usize,
    keys: *const c_int,
    values: *const *mut c_void,
    out_written: *mut usize,
) -> Result {
    if keys.is_null() || values.is_null() {
        return Result::InvalidValue;
    }

    for i in 0..count {
        let result = image_get(image_, *keys.add(i), *values.add(i));
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

unsafe fn image_get_typed(image_: ImageHandle, data: ImageData, out: *mut c_void) -> Result {
    let Some(image) = image_.as_ref() else {
        return Result::InvalidValue;
    };
    if out.is_null() {
        return Result::InvalidValue;
    }

    match data {
        ImageData::Invalid => return Result::InvalidValue,
        ImageData::Id => *(out as *mut u32) = image.id,
        ImageData::Number => *(out as *mut u32) = image.number,
        ImageData::Width => *(out as *mut u32) = image.width,
        ImageData::Height => *(out as *mut u32) = image.height,
        ImageData::Format => *(out as *mut ImageFormat) = ImageFormat::from_zig(image.format),
        ImageData::Compression => {
            *(out as *mut ImageCompression) = ImageCompression::from_zig(image.compression)
        }
        // For animated images this is the current animation frame's
        // data; the image generation changes whenever the current
        // frame does, so generation-keyed caches stay coherent.
        ImageData::DataPtr => {
            let Some(bytes) = image.render_data().bytes() else {
                return Result::NoValue;
            };
            *(out as *mut *const u8) = bytes.as_ptr();
        }
        ImageData::DataLen => *(out as *mut usize) = image.render_data().len(),
        ImageData::Generation => *(out as *mut u64) = image.generation,
    }

    Result::Success
}

/// C: ghostty_kitty_graphics_placement_iterator_new
///
/// # Safety
/// The allocator (if any) must outlive the iterator.
#[export_name = "ghostty_kitty_graphics_placement_iterator_new"]
pub unsafe extern "C" fn placement_iterator_new(
    alloc_: Option<&CAllocator>,
    out: &mut PlacementIterator,
) -> Result {
    let alloc = HandleAllocator::new(alloc_);
    let ptr = alloc.create(PlacementIteratorWrapper {
        alloc,
        inner: None,
        entry: None,
        layer_filter: PlacementLayer::All,
    });
    if ptr.is_null() {
        *out = std::ptr::null_mut();
        return Result::OutOfMemory;
    }
    *out = ptr;
    Result::Success
}

/// C: ghostty_kitty_graphics_placement_iterator_free
///
/// # Safety
/// `iter_` must be NULL or an iterator not used afterwards.
#[export_name = "ghostty_kitty_graphics_placement_iterator_free"]
pub unsafe extern "C" fn placement_iterator_free(iter_: PlacementIterator) {
    let Some(iter) = iter_.as_ref() else {
        return;
    };
    let alloc = iter.alloc;
    alloc.destroy(iter_);
}

/// C: ghostty_kitty_graphics_placement_iterator_set
///
/// # Safety
/// `iter_` must be NULL or live; `value` must be NULL or point to the
/// option's value type.
#[export_name = "ghostty_kitty_graphics_placement_iterator_set"]
pub unsafe extern "C" fn placement_iterator_set(
    iter_: PlacementIterator,
    option: c_int,
    value: *const c_void,
) -> Result {
    let Some(option) = PlacementIteratorOption::from_int(option) else {
        return Result::InvalidValue;
    };
    if value.is_null() {
        return Result::InvalidValue;
    }
    placement_iterator_set_typed(iter_, option, value)
}

unsafe fn placement_iterator_set_typed(
    iter_: PlacementIterator,
    option: PlacementIteratorOption,
    value: *const c_void,
) -> Result {
    let Some(iter) = iter_.as_mut() else {
        return Result::InvalidValue;
    };
    match option {
        PlacementIteratorOption::Layer => {
            let Some(layer) = PlacementLayer::from_int(*(value as *const c_int)) else {
                return Result::InvalidValue;
            };
            iter.layer_filter = layer;
        }
    }
    Result::Success
}

/// C: ghostty_kitty_graphics_placement_next
///
/// # Safety
/// `iter_` must be NULL or live, and its storage unchanged since the
/// iterator was reset.
#[export_name = "ghostty_kitty_graphics_placement_next"]
pub unsafe extern "C" fn placement_iterator_next(iter_: PlacementIterator) -> bool {
    let Some(iter) = iter_.as_mut() else {
        return false;
    };
    let Some(inner) = iter.inner.as_mut() else {
        return false;
    };
    for entry in inner.by_ref() {
        if iter.layer_filter.matches(entry.1.z) {
            iter.entry = Some(entry);
            return true;
        }
    }
    false
}

/// C: ghostty_kitty_graphics_placement_get
///
/// # Safety
/// `iter_` must be NULL or live; `out` must be NULL or point to the data
/// kind's output type.
#[export_name = "ghostty_kitty_graphics_placement_get"]
pub unsafe extern "C" fn placement_get(iter_: PlacementIterator, data: c_int, out: *mut c_void) -> Result {
    let Some(data) = PlacementData::from_int(data) else {
        return Result::InvalidValue;
    };
    match data {
        PlacementData::Invalid => Result::InvalidValue,
        _ => placement_get_typed(iter_, data, out),
    }
}

/// C: ghostty_kitty_graphics_placement_get_multi
///
/// # Safety
/// `keys` and `values` must be NULL or valid for `count` entries; see
/// [`placement_get`].
#[export_name = "ghostty_kitty_graphics_placement_get_multi"]
pub unsafe extern "C" fn placement_get_multi(
    iter_: PlacementIterator,
    count: usize,
    keys: *const c_int,
    values: *const *mut c_void,
    out_written: *mut usize,
) -> Result {
    if keys.is_null() || values.is_null() {
        return Result::InvalidValue;
    }

    for i in 0..count {
        let result = placement_get(iter_, *keys.add(i), *values.add(i));
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

unsafe fn placement_get_typed(iter_: PlacementIterator, data: PlacementData, out: *mut c_void) -> Result {
    let Some(iter) = iter_.as_ref() else {
        return Result::InvalidValue;
    };
    let Some((key, val)) = iter.entry else {
        return Result::InvalidValue;
    };
    if out.is_null() {
        return Result::InvalidValue;
    }

    match data {
        PlacementData::Invalid => return Result::InvalidValue,
        PlacementData::ImageId => *(out as *mut u32) = key.image_id,
        PlacementData::PlacementId => *(out as *mut u32) = key.placement_id.id,
        PlacementData::IsVirtual => *(out as *mut bool) = matches!(val.location, Location::Virtual),
        PlacementData::XOffset => *(out as *mut u32) = val.x_offset,
        PlacementData::YOffset => *(out as *mut u32) = val.y_offset,
        PlacementData::SourceX => *(out as *mut u32) = val.source_x,
        PlacementData::SourceY => *(out as *mut u32) = val.source_y,
        PlacementData::SourceWidth => *(out as *mut u32) = val.source_width,
        PlacementData::SourceHeight => *(out as *mut u32) = val.source_height,
        PlacementData::Columns => *(out as *mut u32) = val.columns,
        PlacementData::Rows => *(out as *mut u32) = val.rows,
        PlacementData::Z => *(out as *mut i32) = val.z,
    }

    Result::Success
}

/// The terminal, image and current placement of the placement geometry
/// functions, or `None` if any is missing.
unsafe fn placement_args<'a>(
    iter_: PlacementIterator,
    image_: ImageHandle,
    terminal_: terminal_c::Terminal,
) -> Option<(&'a mut Terminal, &'a Image, &'a Placement)> {
    let t = terminal_c::zig_terminal(terminal_)?;
    let image = image_.as_ref()?;
    let iter = iter_.as_ref()?;
    let (_, p) = iter.entry?;
    Some((t, image, p))
}

/// C: ghostty_kitty_graphics_placement_rect
///
/// # Safety
/// Handles must be NULL or live.
#[export_name = "ghostty_kitty_graphics_placement_rect"]
pub unsafe extern "C" fn placement_rect(
    iter_: PlacementIterator,
    image_: ImageHandle,
    terminal_: terminal_c::Terminal,
    out: &mut selection_c::CSelection,
) -> Result {
    let Some((t, image, p)) = placement_args(iter_, image_, terminal_) else {
        return Result::InvalidValue;
    };
    let Some(r) = p.rect(image, &TerminalInfo::from_terminal(t)) else {
        return Result::NoValue;
    };

    *out = selection_c::CSelection {
        rectangle: true,
        ..selection_c::CSelection::new(
            grid_ref::CGridRef::from_pin(r.top_left),
            grid_ref::CGridRef::from_pin(r.bottom_right),
        )
    };

    Result::Success
}

/// C: ghostty_kitty_graphics_placement_pixel_size
///
/// # Safety
/// Handles must be NULL or live.
#[export_name = "ghostty_kitty_graphics_placement_pixel_size"]
pub unsafe extern "C" fn placement_pixel_size(
    iter_: PlacementIterator,
    image_: ImageHandle,
    terminal_: terminal_c::Terminal,
    out_width: &mut u32,
    out_height: &mut u32,
) -> Result {
    let Some((t, image, p)) = placement_args(iter_, image_, terminal_) else {
        return Result::InvalidValue;
    };
    let s = p.pixel_size(image, &TerminalInfo::from_terminal(t));

    *out_width = s.width;
    *out_height = s.height;

    Result::Success
}

/// C: ghostty_kitty_graphics_placement_grid_size
///
/// # Safety
/// Handles must be NULL or live.
#[export_name = "ghostty_kitty_graphics_placement_grid_size"]
pub unsafe extern "C" fn placement_grid_size(
    iter_: PlacementIterator,
    image_: ImageHandle,
    terminal_: terminal_c::Terminal,
    out_cols: &mut u32,
    out_rows: &mut u32,
) -> Result {
    let Some((t, image, p)) = placement_args(iter_, image_, terminal_) else {
        return Result::InvalidValue;
    };
    let s = p.grid_size(image, &TerminalInfo::from_terminal(t));

    *out_cols = s.cols;
    *out_rows = s.rows;

    Result::Success
}

/// C: ghostty_kitty_graphics_placement_viewport_pos
///
/// # Safety
/// Handles must be NULL or live.
#[export_name = "ghostty_kitty_graphics_placement_viewport_pos"]
pub unsafe extern "C" fn placement_viewport_pos(
    iter_: PlacementIterator,
    image_: ImageHandle,
    terminal_: terminal_c::Terminal,
    out_col: &mut i32,
    out_row: &mut i32,
) -> Result {
    let Some((t, image, p)) = placement_args(iter_, image_, terminal_) else {
        return Result::InvalidValue;
    };

    let vp = compute_viewport_pos(p, image, t);
    if !vp.visible {
        return Result::NoValue;
    }

    *out_col = vp.col;
    *out_row = vp.row;

    Result::Success
}

/// C: ghostty_kitty_graphics_placement_source_rect
///
/// # Safety
/// Handles must be NULL or live.
#[export_name = "ghostty_kitty_graphics_placement_source_rect"]
pub unsafe extern "C" fn placement_source_rect(
    iter_: PlacementIterator,
    image_: ImageHandle,
    out_x: &mut u32,
    out_y: &mut u32,
    out_width: &mut u32,
    out_height: &mut u32,
) -> Result {
    let Some(image) = image_.as_ref() else {
        return Result::InvalidValue;
    };
    let Some(iter) = iter_.as_ref() else {
        return Result::InvalidValue;
    };
    let Some((_, p)) = iter.entry else {
        return Result::InvalidValue;
    };

    let source = p.source_rect(image);

    *out_x = source.x;
    *out_y = source.y;
    *out_width = source.width;
    *out_height = source.height;

    Result::Success
}

/// C: GhosttyKittyGraphicsPlacementRenderInfo
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementRenderInfo {
    pub size: usize,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub grid_cols: u32,
    pub grid_rows: u32,
    pub viewport_col: i32,
    pub viewport_row: i32,
    pub viewport_visible: bool,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
}

impl Default for PlacementRenderInfo {
    fn default() -> Self {
        PlacementRenderInfo {
            size: std::mem::size_of::<PlacementRenderInfo>(),
            pixel_width: 0,
            pixel_height: 0,
            grid_cols: 0,
            grid_rows: 0,
            viewport_col: 0,
            viewport_row: 0,
            viewport_visible: false,
            source_x: 0,
            source_y: 0,
            source_width: 0,
            source_height: 0,
        }
    }
}

/// C: ghostty_kitty_graphics_placement_render_info
///
/// # Safety
/// Handles must be NULL or live; `out_` must be NULL or valid.
#[export_name = "ghostty_kitty_graphics_placement_render_info"]
pub unsafe extern "C" fn placement_render_info(
    iter_: PlacementIterator,
    image_: ImageHandle,
    terminal_: terminal_c::Terminal,
    out_: *mut PlacementRenderInfo,
) -> Result {
    let Some((t, image, p)) = placement_args(iter_, image_, terminal_) else {
        return Result::InvalidValue;
    };
    let Some(out) = out_.as_mut() else {
        return Result::InvalidValue;
    };
    if out.size < std::mem::size_of::<PlacementRenderInfo>() {
        return Result::InvalidValue;
    }

    let info = TerminalInfo::from_terminal(t);
    let ps = p.pixel_size(image, &info);
    out.pixel_width = ps.width;
    out.pixel_height = ps.height;

    let gs = p.grid_size(image, &info);
    out.grid_cols = gs.cols;
    out.grid_rows = gs.rows;

    let vp = compute_viewport_pos(p, image, t);
    out.viewport_col = vp.col;
    out.viewport_row = vp.row;
    out.viewport_visible = vp.visible;

    let source = p.source_rect(image);
    out.source_x = source.x;
    out.source_y = source.y;
    out.source_width = source.width;
    out.source_height = source.height;

    Result::Success
}

/// The result of [`compute_viewport_pos`].
struct ViewportPos {
    col: i32,
    row: i32,
    visible: bool,
}

const NOT_VISIBLE: ViewportPos = ViewportPos {
    col: 0,
    row: 0,
    visible: false,
};

/// Compute viewport-relative position of a placement, for the C API
/// functions that inspect the storage. Drawing doesn't use this. The
/// render state positions placements on its own.
///
/// Converts the placement's internal pin to viewport-relative column
/// and row coordinates by getting screen-absolute coordinates for
/// both the pin and the viewport origin, then subtracting to get
/// viewport-relative coordinates. The row value can be negative when
/// the placement's origin has scrolled above the top of the viewport.
///
/// A placement is considered not visible if it is a virtual (unicode
/// placeholder) placement, its tracked content has been pruned, or it is
/// fully off-screen (its bottom edge is above the viewport or its top edge is
/// at or below the viewport's last row).
fn compute_viewport_pos(p: &Placement, image: &Image, t: &Terminal) -> ViewportPos {
    // Virtual placements use unicode placeholders and don't have a
    // screen position of their own. Relative placements are anchored at
    // the root of their parent chain, offset by the accumulated chain
    // offsets. A chain rooted at a virtual placement has no resolvable
    // position here: its origin is the parent's placeholder cells, which
    // the render state finds while it scans the captured rows.
    let (pin_ptr, col_offset, row_offset) = match p.location {
        Location::Pin(pin) => (pin, 0i32, 0i32),
        Location::Virtual => return NOT_VISIBLE,
        Location::Relative(rel) => {
            let storage: &kitty_storage::ImageStorage = &t.screens.active.kitty_images;
            let Some(chain) = storage.resolve_chain(rel) else {
                return NOT_VISIBLE;
            };
            match chain.root.location {
                Location::Pin(root_pin) => (root_pin, chain.horizontal_offset, chain.vertical_offset),
                Location::Virtual => return NOT_VISIBLE,
                // resolveChain roots are never relative
                Location::Relative(_) => unreachable!(),
            }
        }
    };

    // SAFETY: tracked pins stay valid while their placement exists.
    let pin: Pin = unsafe { *pin_ptr.as_ptr() };
    if pin.garbage {
        return NOT_VISIBLE;
    }

    // Convert both the placement's pin and the viewport's top-left
    // corner to screen-absolute coordinates so we can subtract them
    // to get viewport-relative coordinates.
    let pages = &t.screens.active.pages;
    let Some(pin_screen) = pages.point_from_pin(point::Tag::Screen, pin) else {
        return NOT_VISIBLE;
    };
    let vp_tl = pages.get_top_left(point::Tag::Viewport);
    let Some(vp_screen) = pages.point_from_pin(point::Tag::Screen, vp_tl) else {
        return NOT_VISIBLE;
    };

    // Subtracting viewport origin from the pin gives us viewport-
    // relative coordinates. The row can be negative when the
    // placement has partially scrolled above the viewport, and both
    // can be negative for relative placements with negative offsets.
    let vp_row: i32 = (pin_screen.coord().y as i32 - vp_screen.coord().y as i32).saturating_add(row_offset);
    let vp_col: i32 = (pin_screen.coord().x as i32).saturating_add(col_offset);

    // A placement is invisible if its bottom edge (row + height) is
    // above the viewport, or its top edge is at or below the viewport's
    // last row. The same applies horizontally: a pin's column is always
    // in bounds, but a relative placement's offsets can push it fully
    // off either side.
    let grid_size = p.grid_size(image, &TerminalInfo::from_terminal(t));
    let bottom_row = vp_row as i64 + grid_size.rows as i64;
    let right_col = vp_col as i64 + grid_size.cols as i64;
    let visible = bottom_row > 0 && vp_row < t.rows as i32 && right_col > 0 && vp_col < t.cols as i32;

    ViewportPos {
        col: vp_col,
        row: vp_row,
        visible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;
    use crate::terminal::c::terminal::TerminalData;
    use crate::terminal::kitty::graphics_image::Data as ImageDataPayload;

    fn new_t(cols: u16, rows: u16) -> terminal_c::Terminal {
        let mut t: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut t, cols, rows));
        t
    }

    fn write(t: terminal_c::Terminal, s: &[u8]) {
        unsafe { terminal_c::vt_write(t, s.as_ptr(), s.len()) };
    }

    fn resize(t: terminal_c::Terminal, cols: u16, rows: u16, cw: u32, ch: u32) {
        assert_eq!(Result::Success, unsafe { terminal_c::resize(t, cols, rows, cw, ch) });
    }

    fn vp<T>(v: &mut T) -> *mut c_void {
        v as *mut T as *mut c_void
    }

    fn graphics(t: terminal_c::Terminal) -> KittyGraphics {
        let mut g: KittyGraphics = std::ptr::null_mut();
        assert_eq!(Result::Success, unsafe {
            terminal_c::get(t, TerminalData::KittyGraphics as c_int, vp(&mut g))
        });
        g
    }

    fn new_iter() -> PlacementIterator {
        let mut iter: PlacementIterator = std::ptr::null_mut();
        assert_eq!(Result::Success, unsafe { placement_iterator_new(Some(&C_ALLOCATOR), &mut iter) });
        assert!(!iter.is_null());
        iter
    }

    unsafe fn reset_iter(g: KittyGraphics, iter: &mut PlacementIterator) {
        assert_eq!(Result::Success, get(g, Data::PlacementIterator as c_int, vp(iter)));
    }

    /// A fresh iterator positioned on the first placement.
    unsafe fn first_placement(g: KittyGraphics) -> PlacementIterator {
        let mut iter = new_iter();
        reset_iter(g, &mut iter);
        assert!(placement_iterator_next(iter));
        iter
    }

    unsafe fn pget<T>(iter: PlacementIterator, data: PlacementData, out: &mut T) -> Result {
        placement_get(iter, data as c_int, vp(out))
    }

    unsafe fn iget<T>(img: ImageHandle, data: ImageData, out: &mut T) -> Result {
        image_get(img, data as c_int, vp(out))
    }

    const TRANSMIT_1X2: &[u8] = b"\x1b_Ga=t,t=d,f=24,i=1,s=1,v=2;////////\x1b\\";
    const TRANSMIT_DISPLAY_1X2_C10: &[u8] = b"\x1b_Ga=T,t=d,f=24,i=1,p=1,s=1,v=2,c=10,r=1;////////\x1b\\";

    #[test]
    fn placement_iterator_new_free() {
        let iter = new_iter();
        unsafe { placement_iterator_free(iter) };
    }

    #[test]
    fn placement_iterator_free_null() {
        unsafe { placement_iterator_free(std::ptr::null_mut()) };
    }

    #[test]
    fn placement_iterator_next_on_empty_storage() {
        let t = new_t(80, 24);
        let g = graphics(t);
        unsafe {
            let mut iter = new_iter();
            reset_iter(g, &mut iter);
            assert!(!placement_iterator_next(iter));
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_iterator_get_before_next_returns_invalid() {
        let t = new_t(80, 24);
        let g = graphics(t);
        unsafe {
            let mut iter = new_iter();
            reset_iter(g, &mut iter);

            let mut image_id: u32 = 0;
            assert_eq!(Result::InvalidValue, pget(iter, PlacementData::ImageId, &mut image_id));
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_iterator_with_transmit_and_display() {
        let t = new_t(80, 24);

        // Transmit and display a 1x2 RGB image (image_id=1, placement_id=1).
        // a=T (transmit+display), t=d (direct), f=24 (RGB), i=1, p=1
        // s=1,v=2 (1x2 pixels), c=10,r=1 (10 cols, 1 row)
        // //////// = 8 base64 chars = 6 bytes = 1*2*3 RGB bytes
        write(t, TRANSMIT_DISPLAY_1X2_C10);

        let g = graphics(t);
        unsafe {
            let mut iter = new_iter();
            reset_iter(g, &mut iter);

            // Should have exactly one placement.
            assert!(placement_iterator_next(iter));

            let mut image_id: u32 = 0;
            assert_eq!(Result::Success, pget(iter, PlacementData::ImageId, &mut image_id));
            assert_eq!(1, image_id);

            let mut placement_id: u32 = 0;
            assert_eq!(Result::Success, pget(iter, PlacementData::PlacementId, &mut placement_id));
            assert_eq!(1, placement_id);

            let mut is_virtual = true;
            assert_eq!(Result::Success, pget(iter, PlacementData::IsVirtual, &mut is_virtual));
            assert!(!is_virtual);

            // No more placements.
            assert!(!placement_iterator_next(iter));
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_iterator_with_multiple_placements() {
        let t = new_t(80, 24);

        // Transmit image 1 then display it twice with different placement IDs.
        write(t, TRANSMIT_1X2);
        write(t, b"\x1b_Ga=p,i=1,p=1,c=10,r=1;\x1b\\");
        write(t, b"\x1b_Ga=p,i=1,p=2,c=5,r=1;\x1b\\");

        let g = graphics(t);
        unsafe {
            let mut iter = new_iter();
            reset_iter(g, &mut iter);

            // Count placements and collect image IDs.
            let mut count: usize = 0;
            let mut seen_p1 = false;
            let mut seen_p2 = false;
            while placement_iterator_next(iter) {
                count += 1;

                let mut image_id: u32 = 0;
                assert_eq!(Result::Success, pget(iter, PlacementData::ImageId, &mut image_id));
                assert_eq!(1, image_id);

                let mut placement_id: u32 = 0;
                assert_eq!(Result::Success, pget(iter, PlacementData::PlacementId, &mut placement_id));
                if placement_id == 1 {
                    seen_p1 = true;
                }
                if placement_id == 2 {
                    seen_p2 = true;
                }
            }

            assert_eq!(2, count);
            assert!(seen_p1);
            assert!(seen_p2);
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_iterator_set_layer_filter() {
        let t = new_t(80, 24);

        // Transmit image 1.
        write(t, TRANSMIT_1X2);

        // Display with z=5 (above text), z=-1 (below text), z=-1073741825 (below bg).
        // INT32_MIN/2 = -1073741824, so -1073741825 < INT32_MIN/2.
        write(t, b"\x1b_Ga=p,i=1,p=1,z=5;\x1b\\");
        write(t, b"\x1b_Ga=p,i=1,p=2,z=-1;\x1b\\");
        write(t, b"\x1b_Ga=p,i=1,p=3,z=-1073741825;\x1b\\");

        let g = graphics(t);
        unsafe {
            let mut iter = new_iter();

            let set_layer = |iter: PlacementIterator, layer: PlacementLayer| {
                let layer = layer as c_int;
                assert_eq!(
                    Result::Success,
                    placement_iterator_set(
                        iter,
                        PlacementIteratorOption::Layer as c_int,
                        &layer as *const c_int as *const c_void
                    )
                );
            };

            // Filter: above_text (z >= 0) — should yield only p=1.
            set_layer(iter, PlacementLayer::AboveText);
            reset_iter(g, &mut iter);
            let mut count: u32 = 0;
            while placement_iterator_next(iter) {
                let mut z: i32 = 0;
                assert_eq!(Result::Success, pget(iter, PlacementData::Z, &mut z));
                assert!(z >= 0);
                count += 1;
            }
            assert_eq!(1, count);

            // Filter: below_text (INT32_MIN/2 <= z < 0) — should yield only p=2.
            set_layer(iter, PlacementLayer::BelowText);
            reset_iter(g, &mut iter);
            count = 0;
            while placement_iterator_next(iter) {
                let mut z: i32 = 0;
                assert_eq!(Result::Success, pget(iter, PlacementData::Z, &mut z));
                assert!(z >= i32::MIN / 2 && z < 0);
                count += 1;
            }
            assert_eq!(1, count);

            // Filter: below_bg (z < INT32_MIN/2) — should yield only p=3.
            set_layer(iter, PlacementLayer::BelowBg);
            reset_iter(g, &mut iter);
            count = 0;
            while placement_iterator_next(iter) {
                let mut z: i32 = 0;
                assert_eq!(Result::Success, pget(iter, PlacementData::Z, &mut z));
                assert!(z < i32::MIN / 2);
                count += 1;
            }
            assert_eq!(1, count);

            // Filter: all — should yield all 3.
            set_layer(iter, PlacementLayer::All);
            reset_iter(g, &mut iter);
            count = 0;
            while placement_iterator_next(iter) {
                count += 1;
            }
            assert_eq!(3, count);
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn image_get_handle_returns_null_for_missing_id() {
        let t = new_t(80, 24);
        let g = graphics(t);
        unsafe {
            assert!(image_get_handle(g, 999).is_null());
            terminal_c::free(t);
        }
    }

    #[test]
    fn image_get_handle_and_image_get_with_transmitted_image() {
        let t = new_t(80, 24);

        // Transmit a 1x2 RGB image with image_id=1.
        write(t, b"\x1b_Ga=T,t=d,f=24,i=1,p=1,s=1,v=2;////////\x1b\\");

        let g = graphics(t);
        unsafe {
            let img = image_get_handle(g, 1);
            assert!(!img.is_null());

            let mut id: u32 = 0;
            assert_eq!(Result::Success, iget(img, ImageData::Id, &mut id));
            assert_eq!(1, id);

            let mut w: u32 = 0;
            assert_eq!(Result::Success, iget(img, ImageData::Width, &mut w));
            assert_eq!(1, w);

            let mut h: u32 = 0;
            assert_eq!(Result::Success, iget(img, ImageData::Height, &mut h));
            assert_eq!(2, h);

            let mut fmt = ImageFormat::Png;
            assert_eq!(Result::Success, iget(img, ImageData::Format, &mut fmt));
            assert_eq!(ImageFormat::Rgb, fmt);

            let mut comp = ImageCompression::ZlibDeflate;
            assert_eq!(Result::Success, iget(img, ImageData::Compression, &mut comp));
            assert_eq!(ImageCompression::None, comp);

            let mut data_len: usize = 0;
            assert_eq!(Result::Success, iget(img, ImageData::DataLen, &mut data_len));
            assert!(data_len > 0);
            terminal_c::free(t);
        }
    }

    #[test]
    fn image_get_exposes_pending_metadata_without_a_data_pointer() {
        let t = new_t(80, 24);
        let g = graphics(t);
        unsafe {
            let zt = terminal_c::zig_terminal(t).unwrap();
            let screen = &mut *zt.screens.active;
            let pending = screen
                .kitty_images
                .add_pending_image(
                    &mut screen.pages,
                    Image {
                        id: 42,
                        number: 7,
                        width: 1,
                        height: 4,
                        format: kitty_cmd::Format::Rgb,
                        data: ImageDataPayload::Pending(12),
                        ..Default::default()
                    },
                )
                .unwrap();

            let img = image_get_handle(g, 42);
            assert!(!img.is_null());

            let mut number: u32 = 0;
            assert_eq!(Result::Success, iget(img, ImageData::Number, &mut number));
            assert_eq!(7, number);

            let mut data_len: usize = 0;
            assert_eq!(Result::Success, iget(img, ImageData::DataLen, &mut data_len));
            assert_eq!(12, data_len);

            let mut data_ptr: *const u8 = std::ptr::null();
            assert_eq!(Result::NoValue, iget(img, ImageData::DataPtr, &mut data_ptr));

            let pixels = vec![b'*'; 12];
            assert!(pending.complete(&mut *g, pixels.clone()).is_ok());
            let completed_img = image_get_handle(g, 42);
            assert!(!completed_img.is_null());
            assert_eq!(Result::Success, iget(completed_img, ImageData::DataPtr, &mut data_ptr));
            assert_eq!(&pixels[..], std::slice::from_raw_parts(data_ptr, data_len));
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_rect_with_transmit_and_display() {
        let t = new_t(80, 24);

        // Set cell size so grid calculations are deterministic.
        // 80 cols * 10px = 800px, 24 rows * 20px = 480px.
        resize(t, 80, 24, 10, 20);

        // Transmit and display a 1x2 RGB image at cursor (0,0).
        // c=10,r=1 => 10 columns, 1 row.
        write(t, TRANSMIT_DISPLAY_1X2_C10);

        let g = graphics(t);
        unsafe {
            let img = image_get_handle(g, 1);
            assert!(!img.is_null());
            let iter = first_placement(g);

            let mut sel = selection_c::CSelection::new(Default::default(), Default::default());
            assert_eq!(Result::Success, placement_rect(iter, img, t, &mut sel));

            // Placement starts at cursor origin (0,0).
            assert_eq!(0, sel.start.x);
            assert_eq!(0, sel.start.y);

            // 10 columns wide, 1 row tall => bottom-right is (9, 0).
            assert_eq!(9, sel.end.x);
            assert_eq!(0, sel.end.y);

            assert!(sel.rectangle);
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_rect_null_args_return_invalid_value() {
        let mut sel = selection_c::CSelection::new(Default::default(), Default::default());
        assert_eq!(Result::InvalidValue, unsafe {
            placement_rect(std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut(), &mut sel)
        });
    }

    #[test]
    fn placement_pixel_size_with_transmit_and_display() {
        let t = new_t(80, 24);

        // 80 cols * 10px = 800px, 24 rows * 20px = 480px.
        resize(t, 80, 24, 10, 20);

        // Transmit and display a 1x2 RGB image with c=10,r=1.
        // 10 cols * 10px = 100px width, 1 row * 20px = 20px height.
        write(t, TRANSMIT_DISPLAY_1X2_C10);

        let g = graphics(t);
        unsafe {
            let img = image_get_handle(g, 1);
            assert!(!img.is_null());
            let iter = first_placement(g);

            let mut w: u32 = 0;
            let mut h: u32 = 0;
            assert_eq!(Result::Success, placement_pixel_size(iter, img, t, &mut w, &mut h));

            assert_eq!(100, w);
            assert_eq!(20, h);
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_pixel_size_null_args_return_invalid_value() {
        let mut w: u32 = 0;
        let mut h: u32 = 0;
        assert_eq!(Result::InvalidValue, unsafe {
            placement_pixel_size(std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut(), &mut w, &mut h)
        });
    }

    #[test]
    fn placement_grid_size_with_transmit_and_display() {
        let t = new_t(80, 24);

        // 80 cols * 10px = 800px, 24 rows * 20px = 480px.
        resize(t, 80, 24, 10, 20);

        // Transmit and display a 1x2 RGB image with c=10,r=1.
        write(t, TRANSMIT_DISPLAY_1X2_C10);

        let g = graphics(t);
        unsafe {
            let img = image_get_handle(g, 1);
            assert!(!img.is_null());
            let iter = first_placement(g);

            let mut cols: u32 = 0;
            let mut rows: u32 = 0;
            assert_eq!(Result::Success, placement_grid_size(iter, img, t, &mut cols, &mut rows));

            assert_eq!(10, cols);
            assert_eq!(1, rows);
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_grid_size_null_args_return_invalid_value() {
        let mut cols: u32 = 0;
        let mut rows: u32 = 0;
        assert_eq!(Result::InvalidValue, unsafe {
            placement_grid_size(std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut(), &mut cols, &mut rows)
        });
    }

    /// The viewport position of the only placement of image 1, or the
    /// result code.
    unsafe fn viewport_pos_of_first(t: terminal_c::Terminal) -> (Result, i32, i32) {
        let g = graphics(t);
        let img = image_get_handle(g, 1);
        assert!(!img.is_null());
        let iter = first_placement(g);
        let mut col: i32 = i32::MIN;
        let mut row: i32 = i32::MIN;
        let r = placement_viewport_pos(iter, img, t, &mut col, &mut row);
        placement_iterator_free(iter);
        (r, col, row)
    }

    #[test]
    fn placement_viewport_pos_with_transmit_and_display() {
        let t = new_t(80, 24);
        resize(t, 80, 24, 10, 20);

        // Transmit and display at cursor (0,0).
        write(t, TRANSMIT_DISPLAY_1X2_C10);

        unsafe {
            let (r, col, row) = viewport_pos_of_first(t);
            assert_eq!(Result::Success, r);
            assert_eq!(0, col);
            assert_eq!(0, row);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_viewport_pos_fully_off_screen_above() {
        let t = new_t(80, 5);
        resize(t, 80, 5, 10, 20);

        // Transmit image, then display at cursor (0,0) spanning 1 row.
        write(t, TRANSMIT_1X2);
        write(t, b"\x1b_Ga=p,i=1,p=1,c=1,r=1;\x1b\\");

        // Scroll the image completely off: 10 newlines in a 5-row terminal
        // scrolls by 5+ rows, so a 1-row image at row 0 is fully gone.
        write(t, b"\n\n\n\n\n\n\n\n\n\n");

        unsafe {
            let (r, _, _) = viewport_pos_of_first(t);
            assert_eq!(Result::NoValue, r);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_viewport_pos_top_off_screen() {
        let t = new_t(80, 5);
        resize(t, 80, 5, 10, 20);

        // Transmit image, display at cursor (0,0) spanning 4 rows.
        // C=1 prevents cursor movement after display.
        write(t, TRANSMIT_1X2);
        write(t, b"\x1b_Ga=p,i=1,p=1,c=1,r=4,C=1;\x1b\\");

        // Scroll by 2: cursor starts at row 0, 4 newlines to reach bottom,
        // then 2 more to scroll by 2. Image top-left moves to vp_row=-2,
        // but bottom rows -2+4=2 > 0 so it's still partially visible.
        write(t, b"\n\n\n\n\n\n");

        unsafe {
            let (r, col, row) = viewport_pos_of_first(t);
            assert_eq!(Result::Success, r);
            assert_eq!(0, col);
            assert_eq!(-2, row);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_viewport_pos_bottom_off_screen() {
        let t = new_t(80, 5);
        resize(t, 80, 5, 10, 20);

        // Transmit image, move cursor to row 3 (1-based: row 4), display spanning 4 rows.
        // C=1 prevents cursor movement after display.
        // Image occupies rows 3-6 but viewport only has rows 0-4, so bottom is clipped.
        write(t, TRANSMIT_1X2);
        write(t, b"\x1b[4;1H");
        write(t, b"\x1b_Ga=p,i=1,p=1,c=1,r=4,C=1;\x1b\\");

        unsafe {
            let (r, col, row) = viewport_pos_of_first(t);
            assert_eq!(Result::Success, r);
            assert_eq!(0, col);
            assert_eq!(3, row);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_viewport_pos_top_and_bottom_off_screen() {
        let t = new_t(80, 5);
        resize(t, 80, 5, 10, 20);

        // Transmit image, display at cursor (0,0) spanning 10 rows.
        // C=1 prevents cursor movement after display.
        // After scrolling by 3, image occupies vp rows -3..6, viewport is 0..4,
        // so both top and bottom are clipped but center is visible.
        write(t, TRANSMIT_1X2);
        write(t, b"\x1b_Ga=p,i=1,p=1,c=1,r=10,C=1;\x1b\\");

        // Scroll by 3: 4 newlines to reach bottom + 3 more to scroll.
        write(t, b"\n\n\n\n\n\n\n");

        unsafe {
            let (r, col, row) = viewport_pos_of_first(t);
            assert_eq!(Result::Success, r);
            assert_eq!(0, col);
            assert_eq!(-3, row);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_viewport_pos_null_args_return_invalid_value() {
        let mut col: i32 = 0;
        let mut row: i32 = 0;
        assert_eq!(Result::InvalidValue, unsafe {
            placement_viewport_pos(std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut(), &mut col, &mut row)
        });
    }

    /// The source rect of the only placement of image 1.
    unsafe fn source_rect_of_first(t: terminal_c::Terminal) -> (u32, u32, u32, u32) {
        let g = graphics(t);
        let img = image_get_handle(g, 1);
        assert!(!img.is_null());
        let iter = first_placement(g);
        let (mut x, mut y, mut w, mut h) = (0u32, 0u32, 0u32, 0u32);
        assert_eq!(Result::Success, placement_source_rect(iter, img, &mut x, &mut y, &mut w, &mut h));
        placement_iterator_free(iter);
        (x, y, w, h)
    }

    /// Transmit a 4x4 RGBA image (64 bytes = 4*4*4).
    /// Base64 of 64 zero bytes: 88 chars (21 full groups + AA== padding).
    const TRANSMIT_4X4: &[u8] = b"\x1b_Ga=t,t=d,f=32,i=1,s=4,v=4;AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==\x1b\\";

    #[test]
    fn placement_source_rect_defaults_to_full_image() {
        let t = new_t(80, 24);
        resize(t, 80, 24, 10, 20);

        // Transmit and display a 1x2 RGB image with no source rect specified.
        // source_width=0 and source_height=0 should resolve to full image (1x2).
        write(t, b"\x1b_Ga=T,t=d,f=24,i=1,p=1,s=1,v=2;////////\x1b\\");

        unsafe {
            assert_eq!((0, 0, 1, 2), source_rect_of_first(t));
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_source_rect_with_explicit_source_rect() {
        let t = new_t(80, 24);
        resize(t, 80, 24, 10, 20);

        write(t, TRANSMIT_4X4);
        // Display with explicit source rect: x=1, y=1, w=2, h=2.
        write(t, b"\x1b_Ga=p,i=1,p=1,x=1,y=1,w=2,h=2;\x1b\\");

        unsafe {
            assert_eq!((1, 1, 2, 2), source_rect_of_first(t));
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_source_rect_clamps_to_image_bounds() {
        let t = new_t(80, 24);
        resize(t, 80, 24, 10, 20);

        write(t, TRANSMIT_4X4);
        // Display with source rect that exceeds image bounds: x=3, y=3, w=10, h=10.
        // Should clamp to x=3, y=3, w=1, h=1.
        write(t, b"\x1b_Ga=p,i=1,p=1,x=3,y=3,w=10,h=10;\x1b\\");

        unsafe {
            assert_eq!((3, 3, 1, 1), source_rect_of_first(t));

            let g = graphics(t);
            let img = image_get_handle(g, 1);
            let iter = first_placement(g);
            let mut pixel_width: u32 = 0;
            let mut pixel_height: u32 = 0;
            assert_eq!(
                Result::Success,
                placement_pixel_size(iter, img, t, &mut pixel_width, &mut pixel_height)
            );
            assert_eq!(1, pixel_width);
            assert_eq!(1, pixel_height);
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_source_rect_null_args_return_invalid_value() {
        let (mut x, mut y, mut w, mut h) = (0u32, 0u32, 0u32, 0u32);
        assert_eq!(Result::InvalidValue, unsafe {
            placement_source_rect(std::ptr::null_mut(), std::ptr::null(), &mut x, &mut y, &mut w, &mut h)
        });
    }

    #[test]
    fn image_get_on_null_returns_invalid_value() {
        let mut id: u32 = 0;
        assert_eq!(Result::InvalidValue, unsafe { iget(std::ptr::null(), ImageData::Id, &mut id) });
    }

    #[test]
    fn placement_render_info_returns_all_fields() {
        let t = new_t(80, 24);
        resize(t, 80, 24, 10, 20);
        write(t, TRANSMIT_DISPLAY_1X2_C10);

        let g = graphics(t);
        unsafe {
            let img = image_get_handle(g, 1);
            assert!(!img.is_null());
            let iter = first_placement(g);

            let mut ri = PlacementRenderInfo::default();
            assert_eq!(Result::Success, placement_render_info(iter, img, t, &mut ri));
            assert!(ri.viewport_visible);
            assert_eq!(0, ri.viewport_col);
            assert_eq!(0, ri.viewport_row);
            assert_eq!(10, ri.grid_cols);
            assert_eq!(1, ri.grid_rows);
            assert_eq!(0, ri.source_x);
            assert_eq!(0, ri.source_y);
            assert_eq!(1, ri.source_width);
            assert_eq!(2, ri.source_height);

            let (_, entry) = (*iter).entry.unwrap();
            let pin = match entry.location {
                Location::Pin(pin) => pin,
                Location::Virtual | Location::Relative(_) => unreachable!(),
            };
            (*pin.as_ptr()).garbage = true;

            ri = PlacementRenderInfo::default();
            assert_eq!(Result::Success, placement_render_info(iter, img, t, &mut ri));
            assert!(!ri.viewport_visible);

            let mut rect = selection_c::CSelection::new(Default::default(), Default::default());
            assert_eq!(Result::NoValue, placement_rect(iter, img, t, &mut rect));
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_render_info_handles_maximum_grid_dimensions() {
        let t = new_t(80, 24);
        resize(t, 80, 24, 10, 20);
        write(t, b"\x1b_Ga=T,t=d,f=24,i=1,p=1,s=1,v=2,c=1,r=4294967295,C=1;////////\x1b\\");

        let g = graphics(t);
        unsafe {
            let img = image_get_handle(g, 1);
            assert!(!img.is_null());
            let iter = first_placement(g);

            let mut ri = PlacementRenderInfo::default();
            assert_eq!(Result::Success, placement_render_info(iter, img, t, &mut ri));
            assert!(ri.viewport_visible);
            assert_eq!(u32::MAX, ri.grid_rows);
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_render_info_off_screen_sets_viewport_visible_false() {
        let t = new_t(80, 5);
        resize(t, 80, 5, 10, 20);
        write(t, TRANSMIT_1X2);
        write(t, b"\x1b_Ga=p,i=1,p=1,c=1,r=1;\x1b\\");

        // Scroll the image completely off-screen.
        write(t, b"\n\n\n\n\n\n\n\n\n\n");

        let g = graphics(t);
        unsafe {
            let img = image_get_handle(g, 1);
            assert!(!img.is_null());
            let iter = first_placement(g);

            let mut ri = PlacementRenderInfo::default();
            assert_eq!(Result::Success, placement_render_info(iter, img, t, &mut ri));
            assert!(!ri.viewport_visible);
            // Other fields should still be populated.
            assert_eq!(1, ri.grid_cols);
            assert_eq!(1, ri.grid_rows);
            assert_eq!(1, ri.source_width);
            assert_eq!(2, ri.source_height);
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_render_info_null_returns_invalid_value() {
        let mut ri = PlacementRenderInfo::default();
        assert_eq!(Result::InvalidValue, unsafe {
            placement_render_info(std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut(), &mut ri)
        });
    }

    #[test]
    fn image_get_multi_success() {
        let t = new_t(80, 24);
        resize(t, 80, 24, 10, 20);
        write(t, b"\x1b_Ga=T,t=d,f=24,i=1,p=1,s=1,v=2;////////\x1b\\");

        let g = graphics(t);
        unsafe {
            let img = image_get_handle(g, 1);
            assert!(!img.is_null());

            let mut id: u32 = 0;
            let mut width: u32 = 0;
            let mut height: u32 = 0;
            let mut written: usize = 0;

            let keys = [ImageData::Id as c_int, ImageData::Width as c_int, ImageData::Height as c_int];
            let values = [vp(&mut id), vp(&mut width), vp(&mut height)];
            assert_eq!(
                Result::Success,
                image_get_multi(img, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);
            assert_eq!(1, id);
            assert_eq!(1, width);
            assert_eq!(2, height);
            terminal_c::free(t);
        }
    }

    #[test]
    fn image_get_multi_error_sets_out_written() {
        let mut id: u32 = 0;
        let mut written: usize = 99;

        let keys = [ImageData::Id as c_int, ImageData::Invalid as c_int];
        let values = [vp(&mut id), vp(&mut id)];
        assert_eq!(Result::InvalidValue, unsafe {
            image_get_multi(std::ptr::null(), keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
        });
        assert_eq!(0, written);
    }

    #[test]
    fn image_get_multi_null_keys_returns_invalid_value() {
        let mut id: u32 = 0;
        let values = [vp(&mut id)];
        assert_eq!(Result::InvalidValue, unsafe {
            image_get_multi(std::ptr::null(), 1, std::ptr::null(), values.as_ptr(), std::ptr::null_mut())
        });
    }

    #[test]
    fn placement_get_multi_success() {
        let t = new_t(80, 24);
        resize(t, 80, 24, 10, 20);
        write(t, TRANSMIT_DISPLAY_1X2_C10);

        let g = graphics(t);
        unsafe {
            let iter = first_placement(g);

            let mut image_id: u32 = 0;
            let mut columns: u32 = 0;
            let mut z: i32 = 99;
            let mut written: usize = 0;

            let keys = [
                PlacementData::ImageId as c_int,
                PlacementData::Columns as c_int,
                PlacementData::Z as c_int,
            ];
            let values = [vp(&mut image_id), vp(&mut columns), vp(&mut z)];
            assert_eq!(
                Result::Success,
                placement_get_multi(iter, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);
            assert_eq!(1, image_id);
            assert_eq!(10, columns);
            assert_eq!(0, z);
            placement_iterator_free(iter);
            terminal_c::free(t);
        }
    }

    #[test]
    fn placement_get_multi_error_sets_out_written() {
        let mut id: u32 = 0;
        let mut written: usize = 99;

        let keys = [PlacementData::ImageId as c_int, PlacementData::Invalid as c_int];
        let values = [vp(&mut id), vp(&mut id)];
        assert_eq!(Result::InvalidValue, unsafe {
            placement_get_multi(std::ptr::null_mut(), keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
        });
        assert_eq!(0, written);
    }

    #[test]
    fn placement_get_multi_null_keys_returns_invalid_value() {
        let mut id: u32 = 0;
        let values = [vp(&mut id)];
        assert_eq!(Result::InvalidValue, unsafe {
            placement_get_multi(std::ptr::null_mut(), 1, std::ptr::null(), values.as_ptr(), std::ptr::null_mut())
        });
    }

    unsafe fn generation(t: terminal_c::Terminal) -> u64 {
        let g = graphics(t);
        let mut gen: u64 = 99;
        assert_eq!(Result::Success, get(g, Data::Generation as c_int, vp(&mut gen)));
        gen
    }

    #[test]
    fn storage_generation_via_get() {
        let t = new_t(80, 24);
        unsafe {
            // Fresh storage: generation zero.
            let gen0 = generation(t);
            assert_eq!(0, gen0);

            // Transmit bumps the generation.
            write(t, TRANSMIT_1X2);
            let gen1 = generation(t);
            assert!(gen1 > gen0);

            // Unrelated terminal writes (plain text) do not bump it.
            write(t, b"hello world");
            let gen2 = generation(t);
            assert_eq!(gen1, gen2);

            // Placement bumps it.
            write(t, b"\x1b_Ga=p,i=1,p=1,c=1,r=1;\x1b\\");
            let gen3 = generation(t);
            assert!(gen3 > gen2);

            // Delete bumps it.
            write(t, b"\x1b_Ga=d,d=A\x1b\\");
            let gen4 = generation(t);
            assert!(gen4 > gen3);
            terminal_c::free(t);
        }
    }

    #[test]
    fn image_generation_detects_same_sized_retransmission() {
        let t = new_t(80, 24);

        // Transmit a 1x2 RGB image with id=1.
        write(t, TRANSMIT_1X2);

        unsafe {
            let read = |t: terminal_c::Terminal| -> (u64, u32, u32, usize) {
                let g = graphics(t);
                let img = image_get_handle(g, 1);
                assert!(!img.is_null());
                let (mut gen, mut w, mut h, mut len) = (0u64, 0u32, 0u32, 0usize);
                assert_eq!(Result::Success, iget(img, ImageData::Generation, &mut gen));
                assert_eq!(Result::Success, iget(img, ImageData::Width, &mut w));
                assert_eq!(Result::Success, iget(img, ImageData::Height, &mut h));
                assert_eq!(Result::Success, iget(img, ImageData::DataLen, &mut len));
                (gen, w, h, len)
            };

            let (gen1, w1, h1, len1) = read(t);
            assert!(gen1 > 0);

            // Retransmit the same ID with identical dimensions but different
            // pixel bytes. All size heuristics match; only the generation
            // reveals the change.
            write(t, b"\x1b_Ga=t,t=d,f=24,i=1,s=1,v=2;AAAAAAAA\x1b\\");

            let (gen2, w2, h2, len2) = read(t);

            // Size heuristics are identical...
            assert_eq!(w1, w2);
            assert_eq!(h1, h2);
            assert_eq!(len1, len2);

            // ...but the generation changed.
            assert!(gen2 > gen1);
            terminal_c::free(t);
        }
    }

    #[test]
    fn image_generation_via_image_get_multi() {
        let t = new_t(80, 24);
        write(t, TRANSMIT_1X2);

        let g = graphics(t);
        unsafe {
            let img = image_get_handle(g, 1);
            assert!(!img.is_null());

            let mut id: u32 = 0;
            let mut gen: u64 = 0;
            let mut written: usize = 0;
            let keys = [ImageData::Id as c_int, ImageData::Generation as c_int];
            let values = [vp(&mut id), vp(&mut gen)];
            assert_eq!(
                Result::Success,
                image_get_multi(img, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);
            assert_eq!(1, id);
            assert!(gen > 0);

            // The image stamp came from the same sequence as (and here, the
            // same event as) the storage stamp.
            assert_eq!(generation(t), gen);
            terminal_c::free(t);
        }
    }

    #[test]
    fn image_compression_and_format_always_report_decoded_data() {
        use base64::Engine as _;

        let t = new_t(80, 24);

        // Transmit a zlib-compressed 1x1 RGB image (o=z). The payload is
        // base64(zlib([0xFF, 0x00, 0x00])).
        // zlib stream for the 3 bytes FF 00 00 (stored block).
        // 78 01: zlib header; 01: final stored block; 03 00 len;
        // FC FF nlen; FF 00 00 data; adler32 = 0x03000100 (big endian).
        let raw = [0x78u8, 0x01, 0x01, 0x03, 0x00, 0xFC, 0xFF, 0xFF, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00];
        let compressed = base64::engine::general_purpose::STANDARD.encode(raw);
        let mut transmit = b"\x1b_Ga=t,t=d,f=24,o=z,i=1,s=1,v=1;".to_vec();
        transmit.extend_from_slice(compressed.as_bytes());
        transmit.extend_from_slice(b"\x1b\\");
        write(t, &transmit);

        let g = graphics(t);
        unsafe {
            let img = image_get_handle(g, 1);
            assert!(!img.is_null());

            // Compression must report NONE: the data was inflated at
            // transmission time.
            let mut comp = ImageCompression::ZlibDeflate;
            assert_eq!(Result::Success, iget(img, ImageData::Compression, &mut comp));
            assert_eq!(ImageCompression::None, comp);

            // Data is the decoded pixels: width * height * bpp.
            let mut data_len: usize = 0;
            assert_eq!(Result::Success, iget(img, ImageData::DataLen, &mut data_len));
            assert_eq!(3, data_len);

            let mut data_ptr: *const u8 = std::ptr::null();
            assert_eq!(Result::Success, iget(img, ImageData::DataPtr, &mut data_ptr));
            assert_eq!(&[0xFF, 0x00, 0x00], std::slice::from_raw_parts(data_ptr, data_len));
            terminal_c::free(t);
        }
    }

    #[test]
    fn generation_never_recurs_across_resets_and_screen_switches() {
        let t = new_t(80, 24);
        unsafe {
            // Transmit on the main screen.
            write(t, TRANSMIT_1X2);
            let gen_main = generation(t);
            assert!(gen_main > 0);

            // Switch to the alternate screen: its storage is untouched, so its
            // generation is zero (which always means "empty").
            write(t, b"\x1b[?1049h");
            assert_eq!(0, generation(t));

            // A transmit on the alt screen draws from the same global sequence,
            // so its stamp is strictly greater than anything seen on the main
            // screen: an embedder keying a cache on the generation value alone
            // can never confuse the two storages.
            write(t, TRANSMIT_1X2);
            let gen_alt = generation(t);
            assert!(gen_alt > gen_main);

            // Back to the main screen: its generation is unchanged.
            write(t, b"\x1b[?1049l");
            assert_eq!(gen_main, generation(t));

            // Reset zeroes the storage (empty), and the next mutation continues
            // from the global sequence: past values are never reused for
            // different content.
            terminal_c::reset(t);
            assert_eq!(0, generation(t));

            write(t, TRANSMIT_1X2);
            assert!(generation(t) > gen_alt);
            terminal_c::free(t);
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Snapshot {
        gen: u64,
        pixel: [u8; 4],
    }

    impl Snapshot {
        unsafe fn read(g: KittyGraphics) -> Snapshot {
            let img = image_get_handle(g, 1);
            assert!(!img.is_null());
            let mut gen: u64 = 0;
            let mut ptr: *const u8 = std::ptr::null();
            assert_eq!(Result::Success, iget(img, ImageData::Generation, &mut gen));
            assert_eq!(Result::Success, iget(img, ImageData::DataPtr, &mut ptr));
            Snapshot {
                gen,
                pixel: std::slice::from_raw_parts(ptr, 4).try_into().unwrap(),
            }
        }
    }

    #[test]
    fn animation_tick_advances_a_running_animation() {
        let t = new_t(80, 24);

        // Place a red 1x1 RGBA image and add a blue second frame that
        // shows for 40ms. The first frame starts with no display time,
        // so give it 30ms. Then start looping playback.
        write(
            t,
            b"\x1b_Ga=T,f=32,i=1,s=1,v=1,c=1,r=1;/wAA/w==\x1b\\\
              \x1b_Ga=f,f=32,i=1,s=1,v=1,z=40;AAD//w==\x1b\\\
              \x1b_Ga=a,i=1,r=1,z=30\x1b\\\
              \x1b_Ga=a,i=1,s=3\x1b\\",
        );

        let g = graphics(t);
        let red: [u8; 4] = [255, 0, 0, 255];
        let blue: [u8; 4] = [0, 0, 255, 255];
        unsafe {
            // The first tick starts the first frame's 30ms timer.
            let mut next: u64 = 0;
            let s0 = Snapshot::read(g);
            assert_eq!(red, s0.pixel);
            assert_eq!(Result::Success, animation_tick(g, 1000, &mut next));
            assert_eq!(1030, next);
            assert_eq!(s0, Snapshot::read(g));

            // An early tick changes nothing and reports the same time.
            assert_eq!(Result::Success, animation_tick(g, 1010, &mut next));
            assert_eq!(1030, next);
            assert_eq!(s0, Snapshot::read(g));

            // On time, the blue frame shows with a new generation.
            assert_eq!(Result::Success, animation_tick(g, 1030, &mut next));
            assert_eq!(1070, next);
            let s1 = Snapshot::read(g);
            assert_eq!(blue, s1.pixel);
            assert!(s1.gen > s0.gen);

            // It then loops back to the first frame.
            assert_eq!(Result::Success, animation_tick(g, 1070, &mut next));
            assert_eq!(1100, next);
            let s2 = Snapshot::read(g);
            assert_eq!(red, s2.pixel);
            assert!(s2.gen > s1.gen);

            // After a stop command (a=a,s=1) the tick reports nothing to
            // play and leaves next_ms and the image alone.
            write(t, b"\x1b_Ga=a,i=1,s=1\x1b\\");
            let g = graphics(t);
            next = 42;
            assert_eq!(Result::NoValue, animation_tick(g, 1100, &mut next));
            assert_eq!(42, next);
            assert_eq!(s2, Snapshot::read(g));
            terminal_c::free(t);
        }
    }

    #[test]
    fn animation_tick_without_animations_returns_no_value() {
        let t = new_t(80, 24);
        let mut next: u64 = 0;
        unsafe {
            // Empty storage.
            let g = graphics(t);
            assert_eq!(Result::NoValue, animation_tick(g, 0, &mut next));

            // A placed static image.
            write(t, b"\x1b_Ga=T,f=32,i=1,s=1,v=1,c=1,r=1;/wAA/w==\x1b\\");
            let g = graphics(t);
            assert_eq!(Result::NoValue, animation_tick(g, 0, &mut next));
            assert_eq!(Result::NoValue, animation_tick(g, 1000, &mut next));
            terminal_c::free(t);
        }
    }

    #[test]
    fn animation_tick_null_out_returns_invalid_value() {
        let t = new_t(80, 24);
        let g = graphics(t);
        unsafe {
            assert_eq!(Result::InvalidValue, animation_tick(g, 0, std::ptr::null_mut()));
            terminal_c::free(t);
        }
    }
}
