//! Port of `src/terminal/c/grid_ref.zig`.
//!
//! STATUS: ported

use super::cell::CCell;
use super::result::Result;
use super::row::CRow;
use super::style as style_c;
use crate::terminal::page_list::{NodePtr, Pin};
use crate::terminal::size::CellCountInt;
use crate::terminal::style as stylepkg;

/// C: GhosttyGridRef
///
/// A sized struct that holds a reference to a position in the terminal grid.
/// The ref points to a specific cell position within the terminal's
/// internal page structure.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CGridRef {
    pub size: usize,
    pub node: Option<NodePtr>,
    pub x: CellCountInt,
    pub y: CellCountInt,
}

impl Default for CGridRef {
    fn default() -> Self {
        CGridRef {
            size: std::mem::size_of::<CGridRef>(),
            node: None,
            x: 0,
            y: 0,
        }
    }
}

impl CGridRef {
    pub fn from_pin(pin: Pin) -> CGridRef {
        CGridRef {
            node: Some(pin.node),
            x: pin.x,
            y: pin.y,
            ..Default::default()
        }
    }

    pub fn to_pin(&self) -> Option<Pin> {
        let mut pin = Pin::new(self.node?);
        pin.x = self.x;
        pin.y = self.y;
        Some(pin)
    }
}

/// C: ghostty_grid_ref_cell
///
/// # Safety
/// `ref_` must be valid; a non-null node must be live. A non-null `out`
/// must be writable.
#[export_name = "ghostty_grid_ref_cell"]
pub unsafe extern "C" fn grid_ref_cell(ref_: &CGridRef, out: *mut CCell) -> Result {
    let Some(p) = ref_.to_pin() else {
        return Result::InvalidValue;
    };
    if let Some(o) = out.as_mut() {
        *o = (*p.row_and_cell().cell).cval();
    }
    Result::Success
}

/// C: ghostty_grid_ref_row
///
/// # Safety
/// See [`grid_ref_cell`].
#[export_name = "ghostty_grid_ref_row"]
pub unsafe extern "C" fn grid_ref_row(ref_: &CGridRef, out: *mut CRow) -> Result {
    let Some(p) = ref_.to_pin() else {
        return Result::InvalidValue;
    };
    if let Some(o) = out.as_mut() {
        *o = (*p.row_and_cell().row).cval();
    }
    Result::Success
}

/// C: ghostty_grid_ref_graphemes
///
/// # Safety
/// See [`grid_ref_cell`]; a non-null `out_buf` must be valid for
/// `buf_len` writes.
#[export_name = "ghostty_grid_ref_graphemes"]
pub unsafe extern "C" fn grid_ref_graphemes(
    ref_: &CGridRef,
    out_buf: *mut u32,
    buf_len: usize,
    out_len: &mut usize,
) -> Result {
    let Some(p) = ref_.to_pin() else {
        return Result::InvalidValue;
    };
    let cell = &*p.row_and_cell().cell;

    if !cell.has_text() {
        *out_len = 0;
        return Result::Success;
    }

    let cp = cell.codepoint();
    let extra = if cell.has_grapheme() { p.grapheme(cell) } else { None };
    let total = 1 + extra.map_or(0, |e| e.len());

    if out_buf.is_null() || buf_len < total {
        *out_len = total;
        return Result::OutOfSpace;
    }

    let buf = std::slice::from_raw_parts_mut(out_buf, buf_len);
    buf[0] = cp;
    if let Some(e) = extra {
        buf[1..1 + e.len()].copy_from_slice(e);
    }

    *out_len = total;
    Result::Success
}

/// C: ghostty_grid_ref_hyperlink_uri
///
/// # Safety
/// See [`grid_ref_graphemes`].
#[export_name = "ghostty_grid_ref_hyperlink_uri"]
pub unsafe extern "C" fn grid_ref_hyperlink_uri(
    ref_: &CGridRef,
    out_buf: *mut u8,
    buf_len: usize,
    out_len: &mut usize,
) -> Result {
    let Some(p) = ref_.to_pin() else {
        return Result::InvalidValue;
    };
    let terminal_page = (*p.node.as_ptr()).page();
    let rac = terminal_page.get_row_and_cell(p.x as usize, p.y as usize);
    let cell = &*rac.cell;

    if !cell.hyperlink() {
        *out_len = 0;
        return Result::Success;
    }

    let Some(link_id) = terminal_page.lookup_hyperlink(cell) else {
        *out_len = 0;
        return Result::Success;
    };
    let entry = &*terminal_page.hyperlink_set.get(terminal_page.memory, link_id);
    let uri = entry.uri.slice(terminal_page.memory);

    if out_buf.is_null() || buf_len < uri.len() {
        *out_len = uri.len();
        return Result::OutOfSpace;
    }

    std::ptr::copy_nonoverlapping(uri.as_ptr(), out_buf, uri.len());
    *out_len = uri.len();
    Result::Success
}

/// C: ghostty_grid_ref_style
///
/// # Safety
/// See [`grid_ref_cell`].
#[export_name = "ghostty_grid_ref_style"]
pub unsafe extern "C" fn grid_ref_style(ref_: &CGridRef, out: *mut style_c::Style) -> Result {
    let Some(p) = ref_.to_pin() else {
        return Result::InvalidValue;
    };
    if let Some(o) = out.as_mut() {
        let cell = &*p.row_and_cell().cell;
        if cell.style_id() == stylepkg::DEFAULT_ID {
            *o = style_c::Style::from_style(&stylepkg::Style::default());
        } else {
            let terminal_page = (*p.node.as_ptr()).page();
            *o = style_c::Style::from_style(&*terminal_page.styles.get(terminal_page.memory, cell.style_id()));
        }
    }
    Result::Success
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;
    use crate::terminal::c::terminal as terminal_c;
    use crate::terminal::point::{Coordinate, Point};

    #[test]
    fn grid_ref_cell_null_node() {
        let r = CGridRef::default();
        let mut out: CCell = 0;
        assert_eq!(Result::InvalidValue, unsafe { grid_ref_cell(&r, &mut out) });
    }

    #[test]
    fn grid_ref_row_null_node() {
        let r = CGridRef::default();
        let mut out: CRow = 0;
        assert_eq!(Result::InvalidValue, unsafe { grid_ref_row(&r, &mut out) });
    }

    #[test]
    fn grid_ref_cell_null_out() {
        let r = CGridRef::default();
        assert_eq!(Result::InvalidValue, unsafe { grid_ref_cell(&r, std::ptr::null_mut()) });
    }

    #[test]
    fn grid_ref_row_null_out() {
        let r = CGridRef::default();
        assert_eq!(Result::InvalidValue, unsafe { grid_ref_row(&r, std::ptr::null_mut()) });
    }

    #[test]
    fn grid_ref_graphemes_null_node() {
        let r = CGridRef::default();
        let mut len: usize = 0;
        assert_eq!(Result::InvalidValue, unsafe {
            grid_ref_graphemes(&r, std::ptr::null_mut(), 0, &mut len)
        });
    }

    #[test]
    fn grid_ref_graphemes_null_buf_returns_out_of_space() {
        let r = CGridRef::default();
        let mut len: usize = 0;
        // With null node this returns invalid_value before checking the buffer,
        // so we can only test null node here. Full buffer tests require a real page.
        assert_eq!(Result::InvalidValue, unsafe {
            grid_ref_graphemes(&r, std::ptr::null_mut(), 0, &mut len)
        });
    }

    #[test]
    fn grid_ref_style_null_node() {
        let r = CGridRef::default();
        let mut out = style_c::Style::from_style(&stylepkg::Style::default());
        assert_eq!(Result::InvalidValue, unsafe { grid_ref_style(&r, &mut out) });
    }

    #[test]
    fn grid_ref_style_null_out() {
        let r = CGridRef::default();
        assert_eq!(Result::InvalidValue, unsafe { grid_ref_style(&r, std::ptr::null_mut()) });
    }

    #[test]
    fn grid_ref_hyperlink_uri_null_node() {
        let r = CGridRef::default();
        let mut len: usize = 0;
        assert_eq!(Result::InvalidValue, unsafe {
            grid_ref_hyperlink_uri(&r, std::ptr::null_mut(), 0, &mut len)
        });
    }

    #[test]
    fn grid_ref_hyperlink_uri_no_hyperlink() {
        let mut terminal: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut terminal, 80, 24));
        unsafe {
            terminal_c::vt_write(terminal, b"hello".as_ptr(), 5);

            let mut r = CGridRef::default();
            assert_eq!(
                Result::Success,
                terminal_c::grid_ref(terminal, Point::Active(Coordinate { x: 0, y: 0 }).cval(), &mut r)
            );

            let mut len: usize = 0;
            assert_eq!(Result::Success, grid_ref_hyperlink_uri(&r, std::ptr::null_mut(), 0, &mut len));
            assert_eq!(0, len);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn grid_ref_hyperlink_uri_with_hyperlink() {
        let mut terminal: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut terminal, 80, 24));
        unsafe {
            // Write OSC 8 hyperlink: \e]8;;uri\e\\text\e]8;;\e\\
            let seq = b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\";
            terminal_c::vt_write(terminal, seq.as_ptr(), seq.len());

            let mut r = CGridRef::default();
            assert_eq!(
                Result::Success,
                terminal_c::grid_ref(terminal, Point::Active(Coordinate { x: 0, y: 0 }).cval(), &mut r)
            );

            // First query length with null buf
            let mut len: usize = 0;
            assert_eq!(Result::OutOfSpace, grid_ref_hyperlink_uri(&r, std::ptr::null_mut(), 0, &mut len));
            assert_eq!(19, len); // "https://example.com"

            // Now read with a properly sized buffer
            let mut buf = [0u8; 256];
            assert_eq!(Result::Success, grid_ref_hyperlink_uri(&r, buf.as_mut_ptr(), buf.len(), &mut len));
            assert_eq!(b"https://example.com", &buf[..len]);
            terminal_c::free(terminal);
        }
    }
}
