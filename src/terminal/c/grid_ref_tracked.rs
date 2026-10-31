//! Port of `src/terminal/c/grid_ref_tracked.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: `alloc` is the terminal's handle allocator (Zig:
//! `t.gpa()`); point tags arrive as `c_int` and are validated.

use std::ffi::c_int;

use super::grid_ref as grid_ref_c;
use super::result::Result;
use super::terminal as terminal_c;
use super::types::HandleAllocator;
use crate::terminal::page_list::{PageList, PinPtr};
use crate::terminal::point;

/// C: GhosttyTrackedGridRef
///
/// An owned tracked reference to a position in the terminal grid. The
/// underlying PageList pin is automatically updated as the PageList changes.
pub type CTrackedGridRef = *mut TrackedGridRef;

pub struct TrackedGridRef {
    pub alloc: HandleAllocator,
    pub terminal: terminal_c::Terminal,
    pub screen_key: terminal_c::TerminalScreen,
    pub screen_generation: usize,
    pub pin: PinPtr,
}

impl TrackedGridRef {
    /// Return the PageList that owns this tracked ref's pin, or null if the
    /// owning screen has been removed/reinitialized since the ref was created.
    fn page_list(&self) -> Option<&mut PageList> {
        // SAFETY: a non-null terminal is live (the terminal detaches its
        // tracked refs before it is freed).
        let t = unsafe { terminal_c::zig_terminal(self.terminal) }?;
        let key = self.screen_key.to_zig();
        if t.screens.generation(key) != self.screen_generation {
            return None;
        }
        let screen = t.screens.get(key)?;
        // SAFETY: screens of a live terminal are live.
        Some(unsafe { &mut screen.as_mut().pages })
    }

    fn pin(&self) -> &mut crate::terminal::page_list::Pin {
        // SAFETY: the tracked pin is live while its page list is.
        unsafe { &mut *self.pin.as_ptr() }
    }
}

/// C: ghostty_tracked_grid_ref_free
///
/// # Safety
/// `ref_` must be NULL or a live tracked ref, not used afterwards.
#[export_name = "ghostty_tracked_grid_ref_free"]
pub unsafe extern "C" fn tracked_grid_ref_free(ref_: CTrackedGridRef) {
    let Some(r) = ref_.as_mut() else {
        return;
    };
    if let Some(wrapper) = r.terminal.as_mut() {
        if let Some(i) = wrapper.tracked_grid_refs.iter().position(|&x| x == ref_) {
            wrapper.tracked_grid_refs.swap_remove(i);
        }
    }
    if let Some(list) = r.page_list() {
        list.untrack_pin(r.pin);
    }
    let alloc = r.alloc;
    alloc.destroy(ref_);
}

/// C: ghostty_tracked_grid_ref_has_value
///
/// # Safety
/// `ref_` must be NULL or a live tracked ref.
#[export_name = "ghostty_tracked_grid_ref_has_value"]
pub unsafe extern "C" fn tracked_grid_ref_has_value(ref_: CTrackedGridRef) -> bool {
    let Some(r) = ref_.as_ref() else {
        return false;
    };
    if r.page_list().is_none() {
        return false;
    }
    !r.pin().garbage
}

/// C: ghostty_tracked_grid_ref_snapshot
///
/// # Safety
/// `ref_` must be NULL or a live tracked ref; a non-null `out_ref` must be
/// writable.
#[export_name = "ghostty_tracked_grid_ref_snapshot"]
pub unsafe extern "C" fn tracked_grid_ref_snapshot(ref_: CTrackedGridRef, out_ref: *mut grid_ref_c::CGridRef) -> Result {
    let Some(r) = ref_.as_ref() else {
        return Result::InvalidValue;
    };
    if r.page_list().is_none() {
        return Result::NoValue;
    }
    if r.pin().garbage {
        return Result::NoValue;
    }
    if let Some(out) = out_ref.as_mut() {
        *out = grid_ref_c::CGridRef::from_pin(*r.pin());
    }
    Result::Success
}

/// C: ghostty_tracked_grid_ref_point
///
/// # Safety
/// `ref_` must be NULL or a live tracked ref; a non-null `out` must be
/// writable.
#[export_name = "ghostty_tracked_grid_ref_point"]
pub unsafe extern "C" fn tracked_grid_ref_point(
    ref_: CTrackedGridRef,
    tag: c_int,
    out: *mut point::Coordinate,
) -> Result {
    let Some(r) = ref_.as_ref() else {
        return Result::InvalidValue;
    };
    let Some(tag) = terminal_c::point_tag_from_int(tag) else {
        return Result::InvalidValue;
    };
    let Some(list) = r.page_list() else {
        return Result::NoValue;
    };
    if r.pin().garbage {
        return Result::NoValue;
    }
    let Some(pt) = list.point_from_pin(tag, *r.pin()) else {
        return Result::NoValue;
    };
    if let Some(o) = out.as_mut() {
        *o = pt.coord();
    }
    Result::Success
}

/// C: ghostty_tracked_grid_ref_set
///
/// # Safety
/// `ref_` and `terminal_` must each be NULL or live.
#[export_name = "ghostty_tracked_grid_ref_set"]
pub unsafe extern "C" fn tracked_grid_ref_set(
    ref_: CTrackedGridRef,
    terminal_: terminal_c::Terminal,
    pt: point::PointC,
) -> Result {
    let Some(r) = ref_.as_mut() else {
        return Result::InvalidValue;
    };
    if terminal_.is_null() {
        return Result::InvalidValue;
    }
    if r.terminal != terminal_ {
        return Result::InvalidValue;
    }

    let t = (*terminal_).terminal_mut();
    let mut screen = t.screens.active;
    let list = &mut screen.pages;
    let Some(p) = list.pin(point::Point::from_c(pt)) else {
        return Result::InvalidValue;
    };
    let Ok(tracked_pin) = list.track_pin(p) else {
        return Result::OutOfMemory;
    };

    if let Some(old_list) = r.page_list() {
        old_list.untrack_pin(r.pin);
    }
    let t = (*terminal_).terminal_mut();
    r.screen_key = terminal_c::TerminalScreen::from_zig(t.screens.active_key);
    r.screen_generation = t.screens.generation(t.screens.active_key);
    r.pin = tracked_pin;
    Result::Success
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;
    use crate::terminal::point::{Coordinate, Point};

    fn active00() -> point::PointC {
        Point::Active(Coordinate { x: 0, y: 0 }).cval()
    }

    #[test]
    fn tracked_grid_ref_snapshots_after_terminal_scroll() {
        let mut terminal: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut terminal, 5, 2));
        unsafe {
            terminal_c::vt_write(terminal, b"A".as_ptr(), 1);

            let mut r: CTrackedGridRef = std::ptr::null_mut();
            assert_eq!(Result::Success, terminal_c::grid_ref_track(terminal, active00(), &mut r));

            terminal_c::vt_write(terminal, b"\r\nB\r\nC".as_ptr(), 6);
            assert!(tracked_grid_ref_has_value(r));

            let mut snapshot = grid_ref_c::CGridRef::default();
            assert_eq!(Result::Success, tracked_grid_ref_snapshot(r, &mut snapshot));

            let mut buf = [0u32; 1];
            let mut len: usize = 0;
            assert_eq!(
                Result::Success,
                grid_ref_c::grid_ref_graphemes(&snapshot, buf.as_mut_ptr(), buf.len(), &mut len)
            );
            assert_eq!(1, len);
            assert_eq!('A' as u32, buf[0]);

            tracked_grid_ref_free(r);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn tracked_grid_ref_reports_no_value_after_reset() {
        let mut terminal: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut terminal, 5, 2));
        unsafe {
            terminal_c::vt_write(terminal, b"A".as_ptr(), 1);

            let mut r: CTrackedGridRef = std::ptr::null_mut();
            assert_eq!(Result::Success, terminal_c::grid_ref_track(terminal, active00(), &mut r));

            terminal_c::reset(terminal);
            assert!(!tracked_grid_ref_has_value(r));

            let mut snapshot = grid_ref_c::CGridRef::default();
            assert_eq!(Result::NoValue, tracked_grid_ref_snapshot(r, &mut snapshot));

            tracked_grid_ref_free(r);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn tracked_grid_ref_reports_no_value_after_alternate_screen_reset() {
        let mut terminal: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut terminal, 5, 2));
        unsafe {
            terminal_c::vt_write(terminal, b"\x1b[?1049hA".as_ptr(), 9);

            let mut r: CTrackedGridRef = std::ptr::null_mut();
            assert_eq!(Result::Success, terminal_c::grid_ref_track(terminal, active00(), &mut r));

            terminal_c::vt_write(terminal, b"\x1bc".as_ptr(), 2);
            assert!(!tracked_grid_ref_has_value(r));

            let mut snapshot = grid_ref_c::CGridRef::default();
            assert_eq!(Result::NoValue, tracked_grid_ref_snapshot(r, &mut snapshot));

            let mut coord = Coordinate::default();
            assert_eq!(
                Result::NoValue,
                tracked_grid_ref_point(r, point::Tag::Active as c_int, &mut coord)
            );

            terminal_c::vt_write(terminal, b"\x1b[?1049h".as_ptr(), 8);
            assert!(!tracked_grid_ref_has_value(r));

            assert_eq!(Result::Success, tracked_grid_ref_set(r, terminal, active00()));
            assert!(tracked_grid_ref_has_value(r));
            assert_eq!(Result::Success, tracked_grid_ref_snapshot(r, &mut snapshot));

            tracked_grid_ref_free(r);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn tracked_grid_ref_reports_no_value_after_terminal_free() {
        let mut terminal: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut terminal, 5, 2));
        unsafe {
            terminal_c::vt_write(terminal, b"A".as_ptr(), 1);

            let mut r: CTrackedGridRef = std::ptr::null_mut();
            assert_eq!(Result::Success, terminal_c::grid_ref_track(terminal, active00(), &mut r));

            terminal_c::free(terminal);
            assert!(!tracked_grid_ref_has_value(r));

            let mut snapshot = grid_ref_c::CGridRef::default();
            assert_eq!(Result::NoValue, tracked_grid_ref_snapshot(r, &mut snapshot));

            let mut coord = Coordinate::default();
            assert_eq!(
                Result::NoValue,
                tracked_grid_ref_point(r, point::Tag::Active as c_int, &mut coord)
            );

            // Rust port: the freed handle is only compared, never
            // dereferenced (same as Zig).
            assert_eq!(Result::InvalidValue, tracked_grid_ref_set(r, terminal, active00()));

            tracked_grid_ref_free(r);
        }
    }
}
