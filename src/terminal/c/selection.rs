//! Port of `src/terminal/c/selection.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: enum arguments (`Adjustment`, `Order`) and the
//! `emit` format of [`FormatOptions`] cross the ABI as `c_int` and are
//! validated before use (Zig checks them with `std.enums.fromInt` in safe
//! builds). Codepoint arrays are borrowed as `&[u32]` (Zig reinterprets
//! them as `[]const u21`).

use std::ffi::c_int;

use super::grid_ref::CGridRef;
use super::result::Result;
use super::terminal as terminal_c;
use super::types::encode_to_buffer;
use crate::lib::allocator::{self as lib_alloc, Allocator as CAllocator, ZigAllocator};
use crate::terminal::formatter as formatterpkg;
use crate::terminal::point;
use crate::terminal::selection::{Adjustment as ZigAdjustment, Order as ZigOrder, Selection};
use crate::terminal::selection_codepoints;
use crate::terminal::terminal::Terminal as ZigTerminal;

const LOG_TARGET: &str = "selection_c";

pub type Adjustment = ZigAdjustment;
pub type Order = ZigOrder;
pub type Format = formatterpkg::Format;

/// C: GhosttySelection
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CSelection {
    pub size: usize,
    pub start: CGridRef,
    pub end: CGridRef,
    pub rectangle: bool,
}

impl CSelection {
    /// A selection with the `size` and `rectangle` defaults.
    pub fn new(start: CGridRef, end: CGridRef) -> CSelection {
        CSelection {
            size: std::mem::size_of::<CSelection>(),
            start,
            end,
            rectangle: false,
        }
    }

    pub fn to_zig(&self) -> Option<Selection> {
        let start_pin = self.start.to_pin()?;
        let end_pin = self.end.to_pin()?;
        Some(Selection::init(start_pin, end_pin, self.rectangle))
    }

    pub fn from_zig(sel: Selection) -> CSelection {
        CSelection {
            rectangle: sel.rectangle,
            ..CSelection::new(CGridRef::from_pin(sel.start()), CGridRef::from_pin(sel.end()))
        }
    }
}

/// C: GhosttySelectionBuffer
///
/// A caller-provided buffer of selections. This follows the same
/// conventions as GhosttyBuffer: ptr may be NULL with cap 0 to query
/// the required capacity, and len is set to the entries written on
/// success or to the required capacity on GHOSTTY_OUT_OF_SPACE.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CSelectionBuffer {
    pub ptr: *mut CSelection,
    pub cap: usize,
    pub len: usize,
}

impl Default for CSelectionBuffer {
    fn default() -> Self {
        CSelectionBuffer {
            ptr: std::ptr::null_mut(),
            cap: 0,
            len: 0,
        }
    }
}

/// C: GhosttyTerminalSelectWordOptions
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SelectWordOptions {
    pub size: usize,
    pub r#ref: CGridRef,
    pub boundary_codepoints: *const u32,
    pub boundary_codepoints_len: usize,
}

impl SelectWordOptions {
    pub fn new(r#ref: CGridRef) -> SelectWordOptions {
        SelectWordOptions {
            size: std::mem::size_of::<SelectWordOptions>(),
            r#ref,
            boundary_codepoints: std::ptr::null(),
            boundary_codepoints_len: 0,
        }
    }
}

/// C: GhosttyTerminalSelectWordBetweenOptions
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SelectWordBetweenOptions {
    pub size: usize,
    pub start: CGridRef,
    pub end: CGridRef,
    pub boundary_codepoints: *const u32,
    pub boundary_codepoints_len: usize,
}

impl SelectWordBetweenOptions {
    pub fn new(start: CGridRef, end: CGridRef) -> SelectWordBetweenOptions {
        SelectWordBetweenOptions {
            size: std::mem::size_of::<SelectWordBetweenOptions>(),
            start,
            end,
            boundary_codepoints: std::ptr::null(),
            boundary_codepoints_len: 0,
        }
    }
}

/// C: GhosttyTerminalSelectLineOptions
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SelectLineOptions {
    pub size: usize,
    pub r#ref: CGridRef,
    pub whitespace: *const u32,
    pub whitespace_len: usize,
    pub semantic_prompt_boundary: bool,
}

impl SelectLineOptions {
    pub fn new(r#ref: CGridRef) -> SelectLineOptions {
        SelectLineOptions {
            size: std::mem::size_of::<SelectLineOptions>(),
            r#ref,
            whitespace: std::ptr::null(),
            whitespace_len: 0,
            semantic_prompt_boundary: false,
        }
    }
}

/// C: GhosttyTerminalSelectionFormatOptions
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FormatOptions {
    pub size: usize,
    /// `GhosttyFormatterFormat`.
    pub emit: c_int,
    pub unwrap: bool,
    pub trim: bool,
    pub selection: *const CSelection,
}

impl FormatOptions {
    pub fn new(emit: Format, unwrap: bool, trim: bool) -> FormatOptions {
        FormatOptions {
            size: std::mem::size_of::<FormatOptions>(),
            emit: emit as c_int,
            unwrap,
            trim,
            selection: std::ptr::null(),
        }
    }
}

fn format_from_int(v: c_int) -> Option<Format> {
    [Format::Plain, Format::Vt, Format::Html]
        .into_iter()
        .find(|f| *f as c_int == v)
}

fn adjustment_from_int(v: c_int) -> Option<Adjustment> {
    use ZigAdjustment::*;
    [Left, Right, Up, Down, Home, End, PageUp, PageDown, BeginningOfLine, EndOfLine]
        .into_iter()
        .find(|a| *a as c_int == v)
}

fn order_from_int(v: c_int) -> Option<Order> {
    use ZigOrder::*;
    [Forward, Reverse, MirroredForward, MirroredReverse]
        .into_iter()
        .find(|o| *o as c_int == v)
}

/// C: ghostty_terminal_select_word
///
/// # Safety
/// Pointers must be NULL or valid; grid refs must reference live pages.
#[export_name = "ghostty_terminal_select_word"]
pub unsafe extern "C" fn word(
    terminal: terminal_c::Terminal,
    options: *const SelectWordOptions,
    out_selection: *mut CSelection,
) -> Result {
    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    let Some(opts) = options.as_ref() else {
        return Result::InvalidValue;
    };
    if opts.size < std::mem::size_of::<SelectWordOptions>() {
        return Result::InvalidValue;
    }
    let Some(out) = out_selection.as_mut() else {
        return Result::InvalidValue;
    };

    let Ok(boundary_codepoints) = codepoint_slice(opts.boundary_codepoints, opts.boundary_codepoints_len) else {
        return Result::InvalidValue;
    };

    let screen = &*t.screens.active;
    let Some(pin) = opts.r#ref.to_pin() else {
        return Result::InvalidValue;
    };
    let Some(sel) = screen.select_word(
        pin,
        boundary_codepoints.unwrap_or(&selection_codepoints::DEFAULT_WORD_BOUNDARIES),
    ) else {
        return Result::NoValue;
    };
    *out = CSelection::from_zig(sel);
    Result::Success
}

/// C: ghostty_terminal_select_word_between
///
/// # Safety
/// See [`word`].
#[export_name = "ghostty_terminal_select_word_between"]
pub unsafe extern "C" fn word_between(
    terminal: terminal_c::Terminal,
    options: *const SelectWordBetweenOptions,
    out_selection: *mut CSelection,
) -> Result {
    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    let Some(opts) = options.as_ref() else {
        return Result::InvalidValue;
    };
    if opts.size < std::mem::size_of::<SelectWordBetweenOptions>() {
        return Result::InvalidValue;
    }
    let Some(out) = out_selection.as_mut() else {
        return Result::InvalidValue;
    };

    let Ok(boundary_codepoints) = codepoint_slice(opts.boundary_codepoints, opts.boundary_codepoints_len) else {
        return Result::InvalidValue;
    };

    let screen = &*t.screens.active;
    let Some(start) = opts.start.to_pin() else {
        return Result::InvalidValue;
    };
    let Some(end) = opts.end.to_pin() else {
        return Result::InvalidValue;
    };
    let Some(sel) = screen.select_word_between(
        start,
        end,
        boundary_codepoints.unwrap_or(&selection_codepoints::DEFAULT_WORD_BOUNDARIES),
    ) else {
        return Result::NoValue;
    };
    *out = CSelection::from_zig(sel);
    Result::Success
}

/// C: ghostty_terminal_select_line
///
/// # Safety
/// See [`word`].
#[export_name = "ghostty_terminal_select_line"]
pub unsafe extern "C" fn line(
    terminal: terminal_c::Terminal,
    options: *const SelectLineOptions,
    out_selection: *mut CSelection,
) -> Result {
    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    let Some(opts) = options.as_ref() else {
        return Result::InvalidValue;
    };
    if opts.size < std::mem::size_of::<SelectLineOptions>() {
        return Result::InvalidValue;
    }
    let Some(out) = out_selection.as_mut() else {
        return Result::InvalidValue;
    };

    let Ok(whitespace) = codepoint_slice(opts.whitespace, opts.whitespace_len) else {
        return Result::InvalidValue;
    };

    let screen = &*t.screens.active;
    let Some(pin) = opts.r#ref.to_pin() else {
        return Result::InvalidValue;
    };
    let Some(sel) = screen.select_line(crate::terminal::screen::SelectLine {
        pin,
        whitespace: Some(whitespace.unwrap_or(&selection_codepoints::DEFAULT_LINE_WHITESPACE)),
        semantic_prompt_boundary: opts.semantic_prompt_boundary,
    }) else {
        return Result::NoValue;
    };
    *out = CSelection::from_zig(sel);
    Result::Success
}

/// C: ghostty_terminal_select_all
///
/// # Safety
/// See [`word`].
#[export_name = "ghostty_terminal_select_all"]
pub unsafe extern "C" fn all(terminal: terminal_c::Terminal, out_selection: *mut CSelection) -> Result {
    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    let Some(out) = out_selection.as_mut() else {
        return Result::InvalidValue;
    };

    let Some(sel) = t.screens.active.select_all() else {
        return Result::NoValue;
    };
    *out = CSelection::from_zig(sel);
    Result::Success
}

/// C: ghostty_terminal_select_output
///
/// # Safety
/// See [`word`].
#[export_name = "ghostty_terminal_select_output"]
pub unsafe extern "C" fn output(
    terminal: terminal_c::Terminal,
    r#ref: CGridRef,
    out_selection: *mut CSelection,
) -> Result {
    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    let Some(out) = out_selection.as_mut() else {
        return Result::InvalidValue;
    };

    let screen = &*t.screens.active;
    let Some(pin) = r#ref.to_pin() else {
        return Result::InvalidValue;
    };
    let Some(sel) = screen.select_output(pin) else {
        return Result::NoValue;
    };
    *out = CSelection::from_zig(sel);
    Result::Success
}

/// (Zig: the error set of `formatSelection`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatError {
    InvalidValue,
    NoValue,
}

/// C: ghostty_terminal_selection_format_buf
///
/// # Safety
/// See [`word`]; a non-null `out_` must be valid for `out_len` writes.
#[export_name = "ghostty_terminal_selection_format_buf"]
pub unsafe extern "C" fn format_buf(
    terminal: terminal_c::Terminal,
    opts: FormatOptions,
    out_: *mut u8,
    out_len: usize,
    out_written: &mut usize,
) -> Result {
    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };

    let mut formatter = match selection_formatter(t, opts) {
        Ok(f) => f,
        Err(FormatError::InvalidValue) => return Result::InvalidValue,
        Err(FormatError::NoValue) => return Result::NoValue,
    };

    if out_.is_null() {
        // A null buffer is the size query: it always reports out_of_space.
        let mut discarding: Vec<u8> = Vec::new();
        formatter.format(&mut discarding).expect("vec writer never fails");
        *out_written = discarding.len();
        return Result::OutOfSpace;
    }

    // Rust port: `encode_to_buffer` covers the too-small retry against a
    // counting writer.
    encode_to_buffer(out_, out_len, out_written, |w| formatter.format(w))
}

/// C: ghostty_terminal_selection_format_alloc
///
/// # Safety
/// See [`word`].
#[export_name = "ghostty_terminal_selection_format_alloc"]
pub unsafe extern "C" fn format_alloc(
    terminal: terminal_c::Terminal,
    alloc_: Option<&CAllocator>,
    opts: FormatOptions,
    out_ptr: &mut *mut u8,
    out_len: &mut usize,
) -> Result {
    *out_ptr = std::ptr::null_mut();
    *out_len = 0;

    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };

    let mut buf: Vec<u8> = Vec::new();
    match format_selection(t, opts, &mut buf) {
        Ok(()) => {}
        Err(Ok(FormatError::InvalidValue)) => return Result::InvalidValue,
        Err(Ok(FormatError::NoValue)) => return Result::NoValue,
        Err(Err(_)) => return Result::OutOfMemory,
    }

    // Do not expose Zig's empty slice sentinel through the C ABI.
    if buf.is_empty() {
        return Result::Success;
    }
    let alloc = lib_alloc::default(alloc_);
    let ptr = alloc.alloc(buf.len(), 0, 0);
    if ptr.is_null() {
        return Result::OutOfMemory;
    }
    std::ptr::copy_nonoverlapping(buf.as_ptr(), ptr, buf.len());
    *out_ptr = ptr;
    *out_len = buf.len();
    Result::Success
}

fn format_selection(
    t: &ZigTerminal,
    opts: FormatOptions,
    writer: &mut dyn std::io::Write,
) -> std::result::Result<(), std::result::Result<FormatError, std::io::Error>> {
    let mut formatter = selection_formatter(t, opts).map_err(Ok)?;
    formatter.format(writer).map_err(Err)
}

fn selection_formatter(
    t: &ZigTerminal,
    opts: FormatOptions,
) -> std::result::Result<formatterpkg::TerminalFormatter<'_>, FormatError> {
    if opts.size < std::mem::size_of::<FormatOptions>() {
        return Err(FormatError::InvalidValue);
    }
    let emit = format_from_int(opts.emit).ok_or(FormatError::InvalidValue)?;

    // SAFETY: a non-null selection pointer is valid (C API contract).
    let sel = match unsafe { opts.selection.as_ref() } {
        Some(sel) => sel.to_zig().ok_or(FormatError::InvalidValue)?,
        None => t.screens.active.selection.ok_or(FormatError::NoValue)?,
    };

    let mut fopts = formatterpkg::Options::new(emit);
    fopts.unwrap = opts.unwrap;
    fopts.trim = opts.trim;
    let mut formatter = formatterpkg::TerminalFormatter::init(t, fopts);
    formatter.content = formatterpkg::ScreenFormatterContent::Selection(Some(sel));
    Ok(formatter)
}

/// Return the borrowed C array of `uint32_t` codepoints.
///
/// `NULL + len 0` returns null, which callers treat as “use the API default
/// set.” A non-null pointer with `len 0` returns an empty slice, meaning “use an
/// explicitly empty set.” A non-zero length requires a non-null pointer.
///
/// This is intentionally zero-copy.
unsafe fn codepoint_slice<'a>(ptr: *const u32, len: usize) -> std::result::Result<Option<&'a [u32]>, ()> {
    if len == 0 {
        if ptr.is_null() {
            return Ok(None);
        }
        return Ok(Some(&[]));
    }

    if ptr.is_null() {
        return Err(());
    }
    Ok(Some(std::slice::from_raw_parts(ptr, len)))
}

/// C: ghostty_terminal_selection_adjust
///
/// # Safety
/// See [`word`].
#[export_name = "ghostty_terminal_selection_adjust"]
pub unsafe extern "C" fn adjust(
    terminal: terminal_c::Terminal,
    selection: *mut CSelection,
    adjustment: c_int,
) -> Result {
    let Some(adjustment) = adjustment_from_int(adjustment) else {
        log::warn!(target: LOG_TARGET, "terminal_selection_adjust invalid adjustment value={adjustment}");
        return Result::InvalidValue;
    };

    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    let Some(sel_ptr) = selection.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(mut sel) = sel_ptr.to_zig() else {
        return Result::InvalidValue;
    };
    sel.adjust(&t.screens.active, adjustment);
    *sel_ptr = CSelection::from_zig(sel);
    Result::Success
}

/// C: ghostty_terminal_selection_order
///
/// # Safety
/// See [`word`].
#[export_name = "ghostty_terminal_selection_order"]
pub unsafe extern "C" fn order(
    terminal: terminal_c::Terminal,
    selection: *const CSelection,
    out_order: *mut c_int,
) -> Result {
    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    let Some(sel) = selection.as_ref().and_then(|s| s.to_zig()) else {
        return Result::InvalidValue;
    };
    let Some(out) = out_order.as_mut() else {
        return Result::InvalidValue;
    };

    *out = sel.order(&t.screens.active) as c_int;
    Result::Success
}

/// C: ghostty_terminal_selection_ordered
///
/// # Safety
/// See [`word`].
#[export_name = "ghostty_terminal_selection_ordered"]
pub unsafe extern "C" fn ordered(
    terminal: terminal_c::Terminal,
    selection: *const CSelection,
    desired: c_int,
    out_selection: *mut CSelection,
) -> Result {
    let Some(desired) = order_from_int(desired) else {
        log::warn!(target: LOG_TARGET, "terminal_selection_ordered invalid desired value={desired}");
        return Result::InvalidValue;
    };

    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    let Some(sel) = selection.as_ref().and_then(|s| s.to_zig()) else {
        return Result::InvalidValue;
    };
    let Some(out) = out_selection.as_mut() else {
        return Result::InvalidValue;
    };

    *out = CSelection::from_zig(sel.ordered(&t.screens.active, desired));
    Result::Success
}

/// C: ghostty_terminal_selection_contains
///
/// # Safety
/// See [`word`].
#[export_name = "ghostty_terminal_selection_contains"]
pub unsafe extern "C" fn contains(
    terminal: terminal_c::Terminal,
    selection: *const CSelection,
    pt: point::PointC,
    out_contains: *mut bool,
) -> Result {
    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    let Some(sel) = selection.as_ref().and_then(|s| s.to_zig()) else {
        return Result::InvalidValue;
    };
    let Some(out) = out_contains.as_mut() else {
        return Result::InvalidValue;
    };

    let screen = &*t.screens.active;
    let Some(pin) = screen.pages.pin(point::Point::from_c(pt)) else {
        return Result::InvalidValue;
    };
    *out = sel.contains(screen, pin);
    Result::Success
}

/// C: ghostty_terminal_selection_equal
///
/// # Safety
/// See [`word`].
#[export_name = "ghostty_terminal_selection_equal"]
pub unsafe extern "C" fn equal(
    terminal: terminal_c::Terminal,
    a: *const CSelection,
    b: *const CSelection,
    out_equal: *mut bool,
) -> Result {
    if terminal_c::zig_terminal(terminal).is_none() {
        return Result::InvalidValue;
    }
    let Some(sel_a) = a.as_ref().and_then(|s| s.to_zig()) else {
        return Result::InvalidValue;
    };
    let Some(sel_b) = b.as_ref().and_then(|s| s.to_zig()) else {
        return Result::InvalidValue;
    };
    let Some(out) = out_equal.as_mut() else {
        return Result::InvalidValue;
    };

    *out = sel_a.eql(sel_b);
    Result::Success
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;
    use crate::terminal::c::allocator as allocator_c;
    use crate::terminal::c::terminal::{tests::FAILING_C_ALLOCATOR, Option as TermOption};
    use crate::terminal::point::{Coordinate, Point};
    use std::ffi::c_void;

    fn active(x: u16, y: u32) -> point::PointC {
        Point::Active(Coordinate { x, y }).cval()
    }

    fn new_terminal() -> terminal_c::Terminal {
        let mut t: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut t, 80, 24));
        t
    }

    #[test]
    fn selection_format_alloc_empty_output() {
        let failing = &FAILING_C_ALLOCATOR;
        let t = new_terminal();
        unsafe {
            let mut r = CGridRef::default();
            assert_eq!(Result::Success, terminal_c::grid_ref(t, active(0, 0), &mut r));
            let sel = CSelection::new(r, r);
            assert_eq!(
                Result::Success,
                terminal_c::set(t, TermOption::Selection as c_int, &sel as *const _ as *const c_void)
            );

            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut len: usize = 123;
            assert_eq!(
                Result::Success,
                format_alloc(t, Some(failing), FormatOptions::new(Format::Plain, true, true), &mut ptr, &mut len)
            );
            assert_eq!(0, len);
            assert!(ptr.is_null());
            allocator_c::free(Some(failing), ptr, len);
            terminal_c::free(t);
        }
    }

    #[test]
    fn selection_format_alloc_uses_active_selection() {
        let t = new_terminal();
        unsafe {
            terminal_c::vt_write(t, b"Hello World".as_ptr(), 11);

            let mut start_ref = CGridRef::default();
            assert_eq!(Result::Success, terminal_c::grid_ref(t, active(6, 0), &mut start_ref));

            let mut end_ref = CGridRef::default();
            assert_eq!(Result::Success, terminal_c::grid_ref(t, active(10, 0), &mut end_ref));

            let sel = CSelection::new(start_ref, end_ref);
            assert_eq!(
                Result::Success,
                terminal_c::set(t, TermOption::Selection as c_int, &sel as *const _ as *const c_void)
            );

            let opts = FormatOptions::new(Format::Plain, true, true);

            let mut required: usize = 0;
            assert_eq!(Result::OutOfSpace, format_buf(t, opts, std::ptr::null_mut(), 0, &mut required));
            assert_eq!(5, required);

            let mut out_ptr: *mut u8 = std::ptr::null_mut();
            let mut out_len: usize = 0;
            assert_eq!(
                Result::Success,
                format_alloc(t, Some(&C_ALLOCATOR), opts, &mut out_ptr, &mut out_len)
            );
            assert!(!out_ptr.is_null());
            assert_eq!(b"World", std::slice::from_raw_parts(out_ptr, out_len));
            allocator_c::free(Some(&C_ALLOCATOR), out_ptr, out_len);
            terminal_c::free(t);
        }
    }

    #[test]
    fn selection_format_buf_uses_provided_selection() {
        let t = new_terminal();
        unsafe {
            terminal_c::vt_write(t, b"Hello World".as_ptr(), 11);

            let mut start_ref = CGridRef::default();
            assert_eq!(Result::Success, terminal_c::grid_ref(t, active(0, 0), &mut start_ref));

            let mut end_ref = CGridRef::default();
            assert_eq!(Result::Success, terminal_c::grid_ref(t, active(4, 0), &mut end_ref));

            let sel = CSelection::new(start_ref, end_ref);
            let mut opts = FormatOptions::new(Format::Plain, true, true);
            opts.selection = &sel;

            let mut small = [0u8; 2];
            let mut written: usize = 0;
            assert_eq!(
                Result::OutOfSpace,
                format_buf(t, opts, small.as_mut_ptr(), small.len(), &mut written)
            );
            assert_eq!(5, written);

            let mut buf = [0u8; 32];
            assert_eq!(Result::Success, format_buf(t, opts, buf.as_mut_ptr(), buf.len(), &mut written));
            assert_eq!(b"Hello", &buf[..written]);
            terminal_c::free(t);
        }
    }

    #[test]
    fn selection_format_alloc_returns_no_value_without_active_selection() {
        let t = new_terminal();
        unsafe {
            let mut out_ptr: *mut u8 = 1 as *mut u8;
            let mut out_len: usize = 123;
            assert_eq!(
                Result::NoValue,
                format_alloc(
                    t,
                    Some(&C_ALLOCATOR),
                    FormatOptions::new(Format::Plain, true, true),
                    &mut out_ptr,
                    &mut out_len
                )
            );
            assert!(out_ptr.is_null());
            assert_eq!(0, out_len);
            terminal_c::free(t);
        }
    }
}
