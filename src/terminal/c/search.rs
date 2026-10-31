//! Port of `src/terminal/c/search.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - Enum arguments (`Option`, `Data`) and the `GhosttySearchScroll` value
//!   cross the ABI as `c_int` and are validated (Zig checks them with
//!   `std.enums.fromInt` in safe builds).
//! - The search engine's internal state uses the global allocator
//!   (`page_list::Allocator::GLOBAL`); the wrapper itself is allocated
//!   with the caller's allocator like Zig.
//! - The terminal's search registry is a `Vec` (Zig: an array hash map
//!   with `putNoClobber`/`swapRemove`).

use std::ffi::{c_int, c_void};

use super::result::Result;
use super::selection as selection_c;
use super::terminal::{self as terminal_c, Terminal};
use super::types::HandleAllocator;
use crate::lib::allocator::Allocator as CAllocator;
use crate::lib::String as LibString;
use crate::terminal::highlight::Flattened as FlattenedHighlight;
use crate::terminal::page_list::Allocator as PageAllocator;
use crate::terminal::search::screen::Select;
use crate::terminal::search::terminal::{
    SelectScroll, Status as ZigStatus, TerminalSearch,
};
use crate::terminal::terminal::Terminal as ZigTerminal;

const LOG_TARGET: &str = "search_c";

/// C: GhosttySearch
pub type Search = *mut SearchWrapper;

pub struct SearchWrapper {
    pub alloc: HandleAllocator,

    /// The terminal this search is bound to. This is borrowed, so the
    /// search never frees it. If the terminal is freed first, it sets
    /// this to null to detach the search: calls that need the terminal
    /// then fail cleanly and free releases only search-owned memory.
    pub terminal: Terminal,

    /// The search state. Null when no needle is set: the search
    /// starts idle and returns to idle when the needle is cleared.
    pub search: std::option::Option<TerminalSearch>,

    /// The scroll policy applied by the select options. Persistent
    /// across selects, set via the select_scroll option.
    pub select_scroll: SelectScroll,
}

impl SearchWrapper {
    /// The Zig terminal this search is bound to, or null if the
    /// terminal was freed before the search.
    pub fn zig_terminal<'a>(&self) -> std::option::Option<&'a mut ZigTerminal> {
        // SAFETY: a non-null terminal is live (the terminal detaches its
        // searches before it is freed).
        unsafe { terminal_c::zig_terminal(self.terminal) }
    }

    /// The status of the search. A search with no needle reports
    /// complete since there is nothing to look for.
    fn status(&self) -> Status {
        match &self.search {
            Some(s) => Status::from_zig(s.status()),
            None => Status::Complete,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttySearchStatus
    pub enum Status {
        Running = 0,
        FeedRequired = 1,
        Complete = 2,
    }
}

impl Status {
    fn from_zig(status: ZigStatus) -> Status {
        match status {
            ZigStatus::Running => Status::Running,
            ZigStatus::FeedRequired => Status::FeedRequired,
            ZigStatus::Complete => Status::Complete,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttySearchScroll
    pub enum Scroll {
        IfNeeded = 0,
        None = 1,
    }
}

impl Scroll {
    fn to_zig(self) -> SelectScroll {
        match self {
            Scroll::IfNeeded => SelectScroll::IfNeeded,
            Scroll::None => SelectScroll::None,
        }
    }

    fn from_zig(scroll: SelectScroll) -> Scroll {
        match scroll {
            SelectScroll::IfNeeded => Scroll::IfNeeded,
            SelectScroll::None => Scroll::None,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttySearchData
    ///
    /// Output types: `status` → [`Status`], `needle` → `GhosttyString`,
    /// `total_matches`/`selected_index` → `usize`, `selected_match` →
    /// `GhosttySelection`, `matches`/`viewport_matches` →
    /// `GhosttySelectionBuffer`, `select_scroll` → [`Scroll`].
    pub enum Data {
        Status = 0,
        Needle = 1,
        TotalMatches = 2,
        SelectedIndex = 3,
        SelectedMatch = 4,
        Matches = 5,
        ViewportMatches = 6,
        SelectScroll = 7,
    }
}

crate::c_api_enum! {
    /// C: GhosttySearchOption
    ///
    /// Value types: `needle` → `GhosttyString`, `select_next`/`select_prev`
    /// → none (the value must be NULL; reserved for future use),
    /// `select_scroll` → [`Scroll`].
    pub enum Option {
        Needle = 0,
        SelectNext = 1,
        SelectPrev = 2,
        SelectScroll = 3,
    }
}

/// C: ghostty_search_new
///
/// # Safety
/// `terminal` must be NULL or live; `out_search` must be NULL or valid.
#[export_name = "ghostty_search_new"]
pub unsafe extern "C" fn new(
    alloc_: std::option::Option<&CAllocator>,
    out_search: *mut Search,
    terminal: Terminal,
) -> Result {
    let Some(out) = out_search.as_mut() else {
        return Result::InvalidValue;
    };
    *out = std::ptr::null_mut();

    let Some(terminal_wrapper) = terminal.as_mut() else {
        return Result::InvalidValue;
    };
    let alloc = HandleAllocator::new(alloc_);
    let wrapper = alloc.create(SearchWrapper {
        alloc,
        terminal,
        search: None,
        select_scroll: SelectScroll::IfNeeded,
    });
    if wrapper.is_null() {
        return Result::OutOfMemory;
    }

    // Store the search in the terminal so that when the terminal is
    // freed the search can be detached safely.
    debug_assert!(!terminal_wrapper.searches.contains(&wrapper));
    terminal_wrapper.searches.push(wrapper);

    *out = wrapper;
    Result::Success
}

/// C: ghostty_search_free
///
/// # Safety
/// `search_` must be NULL or a search not used afterwards.
#[export_name = "ghostty_search_free"]
pub unsafe extern "C" fn free(search_: Search) {
    let Some(wrapper) = search_.as_mut() else {
        return;
    };
    if let Some(terminal_wrapper) = wrapper.terminal.as_mut() {
        if let Some(i) = terminal_wrapper.searches.iter().position(|&s| s == search_) {
            terminal_wrapper.searches.swap_remove(i);
        }
        if let Some(s) = wrapper.search.as_mut() {
            s.deinit(Some(terminal_wrapper.terminal()));
        }
    } else {
        // The terminal was freed first. Tracked state died with it,
        // so only search-owned memory is freed.
        if let Some(s) = wrapper.search.as_mut() {
            s.deinit(None);
        }
    }
    let alloc = wrapper.alloc;
    alloc.destroy(search_);
}

/// C: ghostty_search_tick
///
/// # Safety
/// `search_` must be NULL or live; `out_status` must be NULL or valid.
#[export_name = "ghostty_search_tick"]
pub unsafe extern "C" fn tick(search_: Search, out_status: *mut Status) -> Result {
    let Some(wrapper) = search_.as_mut() else {
        return Result::InvalidValue;
    };
    // History ticks read a pin tracked in the terminal's page list.
    if wrapper.zig_terminal().is_none() {
        return Result::InvalidValue;
    }
    if let Some(s) = wrapper.search.as_mut() {
        let _ = s.tick();
    }
    if let Some(out) = out_status.as_mut() {
        *out = wrapper.status();
    }
    Result::Success
}

/// C: ghostty_search_feed
///
/// # Safety
/// `search_` must be NULL or live.
#[export_name = "ghostty_search_feed"]
pub unsafe extern "C" fn feed(search_: Search) -> Result {
    let Some(wrapper) = search_.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(t) = wrapper.zig_terminal() else {
        return Result::InvalidValue;
    };
    let Some(s) = wrapper.search.as_mut() else {
        return Result::Success;
    };

    // The C API has no renderer cooperation to know whether the active
    // area changed, so it is always re-scanned. This is correct without
    // any dirty tracking and cheap because the active area search was
    // built for exactly this.
    s.feed(t, true);
    Result::Success
}

/// C: ghostty_search_run
///
/// # Safety
/// `search_` must be NULL or live.
#[export_name = "ghostty_search_run"]
pub unsafe extern "C" fn run(search_: Search) -> Result {
    let Some(wrapper) = search_.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(t) = wrapper.zig_terminal() else {
        return Result::InvalidValue;
    };
    let Some(s) = wrapper.search.as_mut() else {
        return Result::Success;
    };

    // Always start with a feed: complete only means caught up as of
    // the last feed, so run doubles as the "terminal changed, catch
    // up" convenience for one-shot embedders.
    s.feed(t, true);
    loop {
        match s.status() {
            ZigStatus::Complete => return Result::Success,
            ZigStatus::FeedRequired => s.feed(t, true),
            ZigStatus::Running => {
                let _ = s.tick();
            }
        }
    }
}

/// C: ghostty_search_set
///
/// # Safety
/// `search_` must be NULL or live; `value` must be NULL or point to the
/// option's value type.
#[export_name = "ghostty_search_set"]
pub unsafe extern "C" fn set(search_: Search, option: c_int, value: *const c_void) -> Result {
    let Some(option) = Option::from_int(option) else {
        log::warn!(target: LOG_TARGET, "search_set invalid option value={option}");
        return Result::InvalidValue;
    };

    set_typed(search_, option, value)
}

unsafe fn set_typed(search_: Search, option: Option, value: *const c_void) -> Result {
    let Some(wrapper) = search_.as_mut() else {
        return Result::InvalidValue;
    };
    match option {
        Option::Needle => {
            // The needle touches the terminal (replacing or clearing
            // an active search releases tracked pins through it), so
            // it requires the terminal to still be alive.
            let Some(t) = wrapper.zig_terminal() else {
                return Result::InvalidValue;
            };

            // NULL and empty both clear the needle, returning the
            // search to idle. This matches the internal engine, where
            // an empty needle stops the search.
            let Some(v) = (value as *const LibString).as_ref() else {
                return clear_needle(wrapper, t);
            };
            if v.len == 0 {
                return clear_needle(wrapper, t);
            }
            if v.ptr.is_null() {
                return Result::InvalidValue;
            }
            let bytes = std::slice::from_raw_parts(v.ptr, v.len);

            // Setting the current needle again keeps existing results,
            // using the same ASCII case-insensitive comparison as
            // matching. This is what a find bar wants on resubmit.
            if let Some(s) = wrapper.search.as_ref() {
                if s.needle().eq_ignore_ascii_case(bytes) {
                    return Result::Success;
                }
            }

            // Create the replacement before dropping the current
            // search so an allocation failure keeps existing state.
            let Ok(replacement) = TerminalSearch::init(PageAllocator::GLOBAL, bytes) else {
                return Result::OutOfMemory;
            };
            if let Some(s) = wrapper.search.as_mut() {
                s.deinit(Some(t));
            }
            wrapper.search = Some(replacement);
        }

        Option::SelectNext | Option::SelectPrev => {
            // The value is reserved for future use and must be NULL.
            if !value.is_null() {
                return Result::InvalidValue;
            }

            let Some(t) = wrapper.zig_terminal() else {
                return Result::InvalidValue;
            };
            let select_scroll = wrapper.select_scroll;
            let Some(s) = wrapper.search.as_mut() else {
                return Result::NoValue;
            };
            let to = match option {
                Option::SelectNext => Select::Next,
                Option::SelectPrev => Select::Prev,
                _ => unreachable!(),
            };
            match s.select(t, to, select_scroll) {
                Ok(true) => {}
                Ok(false) => return Result::NoValue,
                Err(_) => return Result::OutOfMemory,
            }
        }

        Option::SelectScroll => {
            let Some(v) = (value as *const c_int).as_ref() else {
                wrapper.select_scroll = SelectScroll::IfNeeded;
                return Result::Success;
            };
            let Some(scroll) = Scroll::from_int(*v) else {
                return Result::InvalidValue;
            };
            wrapper.select_scroll = scroll.to_zig();
        }
    }

    Result::Success
}

fn clear_needle(wrapper: &mut SearchWrapper, t: &mut ZigTerminal) -> Result {
    if let Some(s) = wrapper.search.as_mut() {
        s.deinit(Some(t));
        wrapper.search = None;
    }
    Result::Success
}

/// C: ghostty_search_get
///
/// # Safety
/// `search_` must be NULL or live; `value` must be NULL or point to the
/// data kind's output type.
#[export_name = "ghostty_search_get"]
pub unsafe extern "C" fn get(search_: Search, data: c_int, value: *mut c_void) -> Result {
    let Some(data) = Data::from_int(data) else {
        log::warn!(target: LOG_TARGET, "search_get invalid data value={data}");
        return Result::InvalidValue;
    };

    if value.is_null() {
        return Result::InvalidValue;
    }
    get_typed(search_, data, value)
}

/// C: ghostty_search_get_multi
///
/// # Safety
/// `keys` and `values` must be NULL or valid for `count` entries; see
/// [`get`].
#[export_name = "ghostty_search_get_multi"]
pub unsafe extern "C" fn get_multi(
    search_: Search,
    count: usize,
    keys: *const c_int,
    values: *const *mut c_void,
    out_written: *mut usize,
) -> Result {
    if keys.is_null() || values.is_null() {
        return Result::InvalidValue;
    }

    for i in 0..count {
        let result = get(search_, *keys.add(i), *values.add(i));
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

unsafe fn get_typed(search_: Search, data: Data, out: *mut c_void) -> Result {
    let Some(wrapper) = search_.as_mut() else {
        return Result::InvalidValue;
    };

    match data {
        Data::Status => *(out as *mut Status) = wrapper.status(),

        Data::Needle => {
            let Some(s) = wrapper.search.as_ref() else {
                return Result::NoValue;
            };
            *(out as *mut LibString) = LibString::init(s.needle());
        }

        Data::TotalMatches => {
            let total = match wrapper.search.as_mut().and_then(|s| s.active_screen_search()) {
                Some(ss) => ss.matches_len(),
                None => 0,
            };
            *(out as *mut usize) = total;
        }

        Data::SelectedIndex => {
            let Some(s) = wrapper.search.as_mut() else {
                return Result::NoValue;
            };
            let Some(ss) = s.active_screen_search() else {
                return Result::NoValue;
            };
            let Some(selected) = ss.selected.as_ref() else {
                return Result::NoValue;
            };
            *(out as *mut usize) = selected.idx;
        }

        Data::SelectedMatch => {
            let Some(s) = wrapper.search.as_mut() else {
                return Result::NoValue;
            };
            let Some(ss) = s.active_screen_search() else {
                return Result::NoValue;
            };
            let Some(hl) = ss.selected_match() else {
                return Result::NoValue;
            };
            *(out as *mut selection_c::CSelection) = selection_from_highlight(hl);
        }

        Data::Matches => {
            let out = &mut *(out as *mut selection_c::CSelectionBuffer);
            let ss = wrapper.search.as_mut().and_then(|s| s.active_screen_search());
            let total = ss.as_ref().map_or(0, |v| v.matches_len());
            if out.cap < total {
                out.len = total;
                return Result::OutOfSpace;
            }
            if total > 0 {
                if out.ptr.is_null() {
                    return Result::InvalidValue;
                }
                let dst = std::slice::from_raw_parts_mut(out.ptr, total);
                let ss = ss.unwrap();
                for (i, d) in dst.iter_mut().enumerate() {
                    *d = selection_from_highlight(ss.match_at(i).unwrap());
                }
            }
            out.len = total;
        }

        Data::ViewportMatches => {
            let out = &mut *(out as *mut selection_c::CSelectionBuffer);
            let matches: &[FlattenedHighlight] = match wrapper.search.as_mut() {
                Some(s) => match s.viewport_matches() {
                    Ok(m) => m,
                    Err(_) => return Result::OutOfMemory,
                },
                None => &[],
            };
            if out.cap < matches.len() {
                out.len = matches.len();
                return Result::OutOfSpace;
            }
            if !matches.is_empty() {
                if out.ptr.is_null() {
                    return Result::InvalidValue;
                }
                let dst = std::slice::from_raw_parts_mut(out.ptr, matches.len());
                for (d, hl) in dst.iter_mut().zip(matches) {
                    *d = selection_from_highlight(hl);
                }
            }
            out.len = matches.len();
        }

        Data::SelectScroll => *(out as *mut Scroll) = Scroll::from_zig(wrapper.select_scroll),
    }

    Result::Success
}

fn selection_from_highlight(hl: &FlattenedHighlight) -> selection_c::CSelection {
    let untracked = hl.untracked();
    selection_c::CSelection {
        rectangle: false,
        ..selection_c::CSelection::new(
            super::grid_ref::CGridRef::from_pin(untracked.start),
            super::grid_ref::CGridRef::from_pin(untracked.end),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;
    use selection_c::{CSelection, CSelectionBuffer};

    fn test_string(s: &[u8]) -> LibString {
        LibString::init(s)
    }

    fn new_terminal(cols: u16, rows: u16) -> Terminal {
        let mut t: Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut t, cols, rows));
        t
    }

    fn write(t: Terminal, s: &[u8]) {
        unsafe { terminal_c::vt_write(t, s.as_ptr(), s.len()) };
    }

    fn vp<T>(v: &mut T) -> *mut c_void {
        v as *mut T as *mut c_void
    }

    fn cp<T>(v: &T) -> *const c_void {
        v as *const T as *const c_void
    }

    unsafe fn get_(search: Search, data: Data, out: *mut c_void) -> Result {
        get(search, data as c_int, out)
    }

    unsafe fn set_(search: Search, option: Option, value: *const c_void) -> Result {
        set(search, option as c_int, value)
    }

    fn test_new_search(terminal: Terminal, needle_str: &[u8]) -> Search {
        let mut search: Search = std::ptr::null_mut();
        unsafe {
            assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut search, terminal));
            let needle_value = test_string(needle_str);
            assert_eq!(Result::Success, set_(search, Option::Needle, cp(&needle_value)));
        }
        search
    }

    unsafe fn needle_str<'a>(s: LibString) -> &'a [u8] {
        std::slice::from_raw_parts(s.ptr, s.len)
    }

    #[test]
    fn search_lifecycle_and_run_to_complete() {
        let terminal = new_terminal(10, 4);
        write(terminal, b"Fizz\r\nBuzz\r\nFizz\r\nBang");
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            // Right after the needle is set the search must report
            // feed_required, not complete. The internal completion check is
            // trivially true before the search has seen the terminal.
            let mut status = Status::Complete;
            assert_eq!(Result::Success, get_(search, Data::Status, vp(&mut status)));
            assert_eq!(Status::FeedRequired, status);

            assert_eq!(Result::Success, run(search));
            assert_eq!(Result::Success, get_(search, Data::Status, vp(&mut status)));
            assert_eq!(Status::Complete, status);

            let mut total: usize = 0;
            assert_eq!(Result::Success, get_(search, Data::TotalMatches, vp(&mut total)));
            assert_eq!(2, total);

            // The needle is borrowed and matches what we searched for.
            let mut needle = LibString::init(b"");
            assert_eq!(Result::Success, get_(search, Data::Needle, vp(&mut needle)));
            assert_eq!(b"Fizz", needle_str(needle));

            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_tick_reports_feed_required_before_first_feed() {
        let terminal = new_terminal(10, 4);
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            let mut status = Status::Complete;
            assert_eq!(Result::Success, tick(search, &mut status));
            assert_eq!(Status::FeedRequired, status);

            // NULL status out is allowed.
            assert_eq!(Result::Success, tick(search, std::ptr::null_mut()));
            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_feed_after_write_updates_results() {
        let terminal = new_terminal(10, 4);
        write(terminal, b"Fizz");
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            assert_eq!(Result::Success, run(search));
            let mut total: usize = 0;
            assert_eq!(Result::Success, get_(search, Data::TotalMatches, vp(&mut total)));
            assert_eq!(1, total);

            // Write another match: the caller-driven feed is the "terminal
            // changed" signal.
            write(terminal, b"\r\nFizz");
            assert_eq!(Result::Success, run(search));
            assert_eq!(Result::Success, get_(search, Data::TotalMatches, vp(&mut total)));
            assert_eq!(2, total);
            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_alt_screen_flip_and_return_retains_results() {
        let terminal = new_terminal(10, 4);
        write(terminal, b"Fizz\r\nFizz");
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            assert_eq!(Result::Success, run(search));
            let mut total: usize = 0;
            assert_eq!(Result::Success, get_(search, Data::TotalMatches, vp(&mut total)));
            assert_eq!(2, total);

            // Flip to the alternate screen: reads now reflect that screen.
            write(terminal, b"\x1b[?1049h");
            assert_eq!(Result::Success, run(search));
            assert_eq!(Result::Success, get_(search, Data::TotalMatches, vp(&mut total)));
            assert_eq!(0, total);

            write(terminal, b"Fizz");
            assert_eq!(Result::Success, run(search));
            assert_eq!(Result::Success, get_(search, Data::TotalMatches, vp(&mut total)));
            assert_eq!(1, total);

            // Return to the primary screen: results are retained and restored.
            write(terminal, b"\x1b[?1049l");
            assert_eq!(Result::Success, run(search));
            assert_eq!(Result::Success, get_(search, Data::TotalMatches, vp(&mut total)));
            assert_eq!(2, total);
            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_select_wraps_in_both_directions() {
        let terminal = new_terminal(10, 4);
        write(terminal, b"Fizz\r\nBuzz\r\nFizz");
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            assert_eq!(Result::Success, run(search));

            // No selection yet.
            let mut idx: usize = 999;
            assert_eq!(Result::NoValue, get_(search, Data::SelectedIndex, vp(&mut idx)));

            // Next: newest match first (index 0), then older (1), then wrap
            // back to 0.
            assert_eq!(Result::Success, set_(search, Option::SelectNext, std::ptr::null()));
            assert_eq!(Result::Success, get_(search, Data::SelectedIndex, vp(&mut idx)));
            assert_eq!(0, idx);

            let mut m = CSelection::new(Default::default(), Default::default());
            assert_eq!(Result::Success, get_(search, Data::SelectedMatch, vp(&mut m)));
            assert!(m.start.to_pin().is_some());

            assert_eq!(Result::Success, set_(search, Option::SelectNext, std::ptr::null()));
            assert_eq!(Result::Success, get_(search, Data::SelectedIndex, vp(&mut idx)));
            assert_eq!(1, idx);

            assert_eq!(Result::Success, set_(search, Option::SelectNext, std::ptr::null()));
            assert_eq!(Result::Success, get_(search, Data::SelectedIndex, vp(&mut idx)));
            assert_eq!(0, idx);

            // Prev: wrap backward to the oldest match.
            assert_eq!(Result::Success, set_(search, Option::SelectPrev, std::ptr::null()));
            assert_eq!(Result::Success, get_(search, Data::SelectedIndex, vp(&mut idx)));
            assert_eq!(1, idx);
            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_select_with_no_matches_returns_no_value() {
        let terminal = new_terminal(10, 4);
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            assert_eq!(Result::NoValue, set_(search, Option::SelectNext, std::ptr::null()));
            assert_eq!(Result::NoValue, set_(search, Option::SelectPrev, std::ptr::null()));
            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_select_rejects_a_non_null_reserved_value() {
        let terminal = new_terminal(10, 4);
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            let bogus: c_int = 0;
            assert_eq!(Result::InvalidValue, set_(search, Option::SelectNext, cp(&bogus)));
            assert_eq!(Result::InvalidValue, set_(search, Option::SelectPrev, cp(&bogus)));
            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_select_scroll_option() {
        let terminal = new_terminal(10, 4);
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            let mut scroll = Scroll::None;
            assert_eq!(Result::Success, get_(search, Data::SelectScroll, vp(&mut scroll)));
            assert_eq!(Scroll::IfNeeded, scroll);

            let none = Scroll::None;
            assert_eq!(Result::Success, set_(search, Option::SelectScroll, cp(&none)));
            assert_eq!(Result::Success, get_(search, Data::SelectScroll, vp(&mut scroll)));
            assert_eq!(Scroll::None, scroll);

            // NULL resets to the default.
            assert_eq!(Result::Success, set_(search, Option::SelectScroll, std::ptr::null()));
            assert_eq!(Result::Success, get_(search, Data::SelectScroll, vp(&mut scroll)));
            assert_eq!(Scroll::IfNeeded, scroll);

            // Invalid enum values are rejected.
            let bogus: c_int = 42;
            assert_eq!(Result::InvalidValue, set_(search, Option::SelectScroll, cp(&bogus)));
            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_selection_dropped_when_reset_prunes_results() {
        let terminal = new_terminal(10, 4);
        write(terminal, b"Fizz\r\nBuzz");
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            assert_eq!(Result::Success, run(search));
            assert_eq!(Result::Success, set_(search, Option::SelectNext, std::ptr::null()));

            // A selection survives unrelated writes...
            write(terminal, b"\r\nBang");
            assert_eq!(Result::Success, run(search));
            let mut m = CSelection::new(Default::default(), Default::default());
            assert_eq!(Result::Success, get_(search, Data::SelectedMatch, vp(&mut m)));

            // ...but a full reset prunes every result, dropping the selection.
            write(terminal, b"\x1bc");
            assert_eq!(Result::Success, run(search));
            let mut total: usize = 999;
            assert_eq!(Result::Success, get_(search, Data::TotalMatches, vp(&mut total)));
            assert_eq!(0, total);
            assert_eq!(Result::NoValue, get_(search, Data::SelectedMatch, vp(&mut m)));
            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_matches_buffer_semantics() {
        let terminal = new_terminal(10, 4);
        write(terminal, b"Fizz\r\nBuzz\r\nFizz");
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            assert_eq!(Result::Success, run(search));

            // NULL ptr with cap 0 queries the required capacity.
            let mut buf = CSelectionBuffer::default();
            assert_eq!(Result::OutOfSpace, get_(search, Data::Matches, vp(&mut buf)));
            assert_eq!(2, buf.len);

            // Too-small buffers report the required capacity.
            let empty = CSelection::new(Default::default(), Default::default());
            let mut one = [empty; 1];
            buf = CSelectionBuffer {
                ptr: one.as_mut_ptr(),
                cap: one.len(),
                len: 0,
            };
            assert_eq!(Result::OutOfSpace, get_(search, Data::Matches, vp(&mut buf)));
            assert_eq!(2, buf.len);

            // A large-enough buffer is filled newest to oldest.
            let mut storage = [empty; 4];
            buf = CSelectionBuffer {
                ptr: storage.as_mut_ptr(),
                cap: storage.len(),
                len: 0,
            };
            assert_eq!(Result::Success, get_(search, Data::Matches, vp(&mut buf)));
            assert_eq!(2, buf.len);
            let first = storage[0].start.to_pin().unwrap();
            let second = storage[1].start.to_pin().unwrap();
            assert!(first.y != second.y || first.node != second.node);

            // Viewport matches use the same conventions and are cached across
            // reads.
            buf = CSelectionBuffer::default();
            assert_eq!(Result::OutOfSpace, get_(search, Data::ViewportMatches, vp(&mut buf)));
            assert_eq!(2, buf.len);
            buf = CSelectionBuffer {
                ptr: storage.as_mut_ptr(),
                cap: storage.len(),
                len: 0,
            };
            assert_eq!(Result::Success, get_(search, Data::ViewportMatches, vp(&mut buf)));
            assert_eq!(2, buf.len);
            buf = CSelectionBuffer {
                ptr: storage.as_mut_ptr(),
                cap: storage.len(),
                len: 0,
            };
            assert_eq!(Result::Success, get_(search, Data::ViewportMatches, vp(&mut buf)));
            assert_eq!(2, buf.len);
            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_get_multi_returns_first_failing_index() {
        let terminal = new_terminal(10, 4);
        write(terminal, b"Fizz");
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            assert_eq!(Result::Success, run(search));

            // No selection: selected_index fails at index 1.
            let keys = [Data::TotalMatches as c_int, Data::SelectedIndex as c_int, Data::Status as c_int];
            let mut total: usize = 0;
            let mut idx: usize = 0;
            let mut status = Status::Running;
            let values = [vp(&mut total), vp(&mut idx), vp(&mut status)];
            let mut written: usize = 999;
            assert_eq!(
                Result::NoValue,
                get_multi(search, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(1, written);
            assert_eq!(1, total);

            // After selecting, the whole batch succeeds.
            assert_eq!(Result::Success, set_(search, Option::SelectNext, std::ptr::null()));
            assert_eq!(
                Result::Success,
                get_multi(search, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);
            assert_eq!(0, idx);
            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_new_validates_options() {
        let terminal = new_terminal(10, 4);
        unsafe {
            let mut search: Search = std::ptr::null_mut();

            // NULL out and NULL terminal are invalid.
            assert_eq!(Result::InvalidValue, new(Some(&C_ALLOCATOR), std::ptr::null_mut(), terminal));
            assert_eq!(Result::InvalidValue, new(Some(&C_ALLOCATOR), &mut search, std::ptr::null_mut()));
            assert!(search.is_null());
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_without_a_needle_is_idle() {
        let terminal = new_terminal(10, 4);
        write(terminal, b"Fizz");
        unsafe {
            let mut search: Search = std::ptr::null_mut();
            assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut search, terminal));

            // With nothing to look for, the search is complete and empty, and
            // driving it is a harmless no-op.
            let mut status = Status::Running;
            assert_eq!(Result::Success, get_(search, Data::Status, vp(&mut status)));
            assert_eq!(Status::Complete, status);
            assert_eq!(Result::Success, feed(search));
            assert_eq!(Result::Success, run(search));
            assert_eq!(Result::Success, tick(search, &mut status));
            assert_eq!(Status::Complete, status);

            let mut needle_out = LibString::init(b"");
            assert_eq!(Result::NoValue, get_(search, Data::Needle, vp(&mut needle_out)));
            let mut total: usize = 999;
            assert_eq!(Result::Success, get_(search, Data::TotalMatches, vp(&mut total)));
            assert_eq!(0, total);
            assert_eq!(Result::NoValue, set_(search, Option::SelectNext, std::ptr::null()));

            let mut buf = CSelectionBuffer::default();
            assert_eq!(Result::Success, get_(search, Data::Matches, vp(&mut buf)));
            assert_eq!(0, buf.len);
            buf = CSelectionBuffer::default();
            assert_eq!(Result::Success, get_(search, Data::ViewportMatches, vp(&mut buf)));
            assert_eq!(0, buf.len);

            // Clearing an already-clear needle is fine.
            assert_eq!(Result::Success, set_(search, Option::Needle, std::ptr::null()));
            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_needle_change_and_clear() {
        let terminal = new_terminal(10, 4);
        write(terminal, b"Fizz\r\nBuzz");
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            assert_eq!(Result::Success, run(search));
            assert_eq!(Result::Success, set_(search, Option::SelectNext, std::ptr::null()));

            let mut total: usize = 0;
            assert_eq!(Result::Success, get_(search, Data::TotalMatches, vp(&mut total)));
            assert_eq!(1, total);

            // Setting the same needle again (any ASCII case) keeps existing
            // results and the selection.
            let same = test_string(b"fIZZ");
            assert_eq!(Result::Success, set_(search, Option::Needle, cp(&same)));
            let mut needle_out = LibString::init(b"");
            assert_eq!(Result::Success, get_(search, Data::Needle, vp(&mut needle_out)));
            assert_eq!(b"Fizz", needle_str(needle_out));
            let mut idx: usize = 999;
            assert_eq!(Result::Success, get_(search, Data::SelectedIndex, vp(&mut idx)));
            assert_eq!(0, idx);

            // Changing the needle restarts the search from scratch.
            let changed = test_string(b"Buzz");
            assert_eq!(Result::Success, set_(search, Option::Needle, cp(&changed)));
            let mut status = Status::Complete;
            assert_eq!(Result::Success, get_(search, Data::Status, vp(&mut status)));
            assert_eq!(Status::FeedRequired, status);
            assert_eq!(Result::NoValue, get_(search, Data::SelectedIndex, vp(&mut idx)));
            assert_eq!(Result::Success, run(search));
            assert_eq!(Result::Success, get_(search, Data::TotalMatches, vp(&mut total)));
            assert_eq!(1, total);
            assert_eq!(Result::Success, get_(search, Data::Needle, vp(&mut needle_out)));
            assert_eq!(b"Buzz", needle_str(needle_out));

            // An empty needle clears the search, same as NULL.
            let empty = test_string(b"");
            assert_eq!(Result::Success, set_(search, Option::Needle, cp(&empty)));
            assert_eq!(Result::NoValue, get_(search, Data::Needle, vp(&mut needle_out)));
            assert_eq!(Result::Success, get_(search, Data::Status, vp(&mut status)));
            assert_eq!(Status::Complete, status);
            assert_eq!(Result::Success, get_(search, Data::TotalMatches, vp(&mut total)));
            assert_eq!(0, total);
            free(search);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_free_after_terminal_free() {
        let terminal = new_terminal(10, 4);
        write(terminal, b"Fizz\r\nFizz");
        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            // Run and select so the search holds tracked pins within the
            // terminal's page storage.
            assert_eq!(Result::Success, run(search));
            assert_eq!(Result::Success, set_(search, Option::SelectNext, std::ptr::null()));

            // Free the terminal first. The search detaches: calls that need
            // the terminal fail cleanly instead of touching freed memory.
            terminal_c::free(terminal);
            assert_eq!(Result::InvalidValue, feed(search));
            assert_eq!(Result::InvalidValue, run(search));
            assert_eq!(Result::InvalidValue, tick(search, std::ptr::null_mut()));
            assert_eq!(Result::InvalidValue, set_(search, Option::SelectNext, std::ptr::null()));
            let needle_value = test_string(b"Buzz");
            assert_eq!(Result::InvalidValue, set_(search, Option::Needle, cp(&needle_value)));

            // Reads that only touch search-owned state still answer.
            let mut needle_out = LibString::init(b"");
            assert_eq!(Result::Success, get_(search, Data::Needle, vp(&mut needle_out)));
            assert_eq!(b"Fizz", needle_str(needle_out));
            let scroll = Scroll::None;
            assert_eq!(Result::Success, set_(search, Option::SelectScroll, cp(&scroll)));

            // The search can still be freed, releasing only its own memory.
            free(search);
        }
    }

    #[test]
    fn search_tick_after_terminal_free_with_history() {
        let terminal = new_terminal(10, 3);

        // Push matches into scrollback so the search has history.
        write(terminal, b"Fizz\r\nFizz\r\nFizz\r\nFizz\r\n");

        let search = test_new_search(terminal, b"Fizz");
        unsafe {
            assert_eq!(Result::Success, feed(search));
            let mut status = Status::Complete;
            assert_eq!(Result::Success, get_(search, Data::Status, vp(&mut status)));
            assert_eq!(Status::Running, status);

            // The history search's tracked pin is freed with the terminal.
            terminal_c::free(terminal);
            assert_eq!(Result::InvalidValue, tick(search, &mut status));
            free(search);
        }
    }

    #[test]
    fn search_freed_before_terminal_detaches_from_the_registry() {
        let terminal = new_terminal(10, 4);

        // Create two searches and free one while the terminal is alive.
        // The freed search must be unregistered so the later terminal free
        // only detaches the survivor.
        let a = test_new_search(terminal, b"Fizz");
        let b = test_new_search(terminal, b"Fizz");
        unsafe {
            assert_eq!(Result::Success, run(a));
            free(a);
            assert_eq!(Result::Success, run(b));
            free(b);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn search_free_null() {
        unsafe { free(std::ptr::null_mut()) };
    }
}
