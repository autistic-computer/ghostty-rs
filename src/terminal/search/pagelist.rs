//! Port of `src/terminal/search/pagelist.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - The PageList and the tracked pin are raw pointers like in Zig: the
//!   caller guarantees the PageList outlives the search (or calls
//!   [`PageListSearch::deinit_list_invalid`]) and holds the necessary locks
//!   for every call that accesses it.
//! - `deinit` stays explicit (it untracks the pin) because the
//!   list-invalid variant must not touch the PageList.

use core::ptr::NonNull;

use crate::terminal::highlight::Flattened as FlattenedHighlight;
use crate::terminal::page_list::{Allocator, NodePtr, PageList, Pin, PinPtr};
use crate::terminal::search::sliding_window::{Direction, SlidingWindow};
use crate::terminal::size::AllocError;

/// Searches for a term in a PageList structure.
///
/// This searches in reverse order starting from the given node.
///
/// This assumes that nodes do not change contents. For nodes that change
/// contents, look at ActiveSearch, which is designed to re-search the active
/// area since it assumed to change. When integrating ActiveSearch with
/// PageListSearch, the caller should start the PageListSearch from the
/// returned node from ActiveSearch.update().
///
/// Concurrent access to a PageList or nodes in a PageList are not allowed,
/// so the caller should ensure that necessary locks are held. Each function
/// documents whether it accesses the PageList or not. For example, you can
/// safely call `next()` without holding a lock, but you must hold a lock
/// while calling `feed()`.
#[derive(Debug)]
pub struct PageListSearch {
    /// The list we're searching.
    pub list: NonNull<PageList>,

    /// The sliding window of page contents and nodes to search.
    pub window: SlidingWindow,

    /// The tracked pin for our current position in the pagelist. This
    /// will always point to the CURRENT node we're searching from so that
    /// we can track if we move.
    pub pin: PinPtr,
}

// SAFETY: the raw pointers are only dereferenced under the caller's
// synchronization (see the type docs), exactly like the Zig version.
unsafe impl Send for PageListSearch {}

impl PageListSearch {
    /// Initialize the page list search. The needle is copied so it can
    /// be freed immediately.
    ///
    /// Accesses the PageList/Node so the caller must ensure it is safe
    /// to do so if there is any concurrent access.
    pub fn init(
        alloc: Allocator,
        needle: &[u8],
        list: &mut PageList,
        start: NodePtr,
    ) -> Result<PageListSearch, AllocError> {
        // We put a tracked pin into the node that we're starting from.
        // By using a tracked pin, we can keep our pagelist references safe
        // because if the pagelist prunes pages, the tracked pin will
        // be moved somewhere safe.
        // SAFETY: start is a live node of `list`.
        let (rows, cols) = unsafe { (start.as_ref().rows(), start.as_ref().cols()) };
        let pin = list.track_pin(Pin {
            y: rows - 1,
            x: cols - 1,
            ..Pin::new(start)
        })?;

        // Create our sliding window we'll use for searching.
        let mut window = match SlidingWindow::init(alloc, Direction::Reverse, needle) {
            Ok(w) => w,
            Err(e) => {
                list.untrack_pin(pin);
                return Err(e);
            }
        };

        // We always feed our initial page data into the window, because
        // we have the lock anyways and this lets our `pin` point to our
        // current node and feed to work properly.
        if let Err(e) = window.append(start) {
            list.untrack_pin(pin);
            return Err(e);
        }

        Ok(PageListSearch {
            list: NonNull::from(list),
            window,
            pin,
        })
    }

    /// Modifies the PageList (to untrack a pin) so the caller must ensure
    /// that it is safe to do so.
    pub fn deinit(&mut self) {
        self.window.clear_and_retain_capacity();
        // SAFETY: the list outlives the search (caller contract).
        unsafe { self.list.as_mut().untrack_pin(self.pin) };
    }

    /// Release owned search buffers after the PageList itself has already
    /// been destroyed. Its pin belonged to the PageList pool and was freed
    /// with that list, so it must not be untracked here.
    pub fn deinit_list_invalid(&mut self) {
        self.window.clear_and_retain_capacity();
    }

    /// Return the next match in the loaded page nodes. If this returns
    /// null then the PageList search needs to be fed the next node(s).
    /// Call, `feed` to do this.
    ///
    /// Beware that the selection returned may point to a node that
    /// is freed if the caller does not hold necessary locks on the
    /// PageList while searching. The pins should be validated prior to
    /// final use.
    ///
    /// This does NOT access the PageList, so it can be called without
    /// a lock held.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<FlattenedHighlight> {
        self.window.next()
    }

    /// Feed more data to the sliding window from the pagelist. This will
    /// feed enough data to cover at least one match (needle length) if it
    /// exists; this doesn't perform the search, it only feeds data.
    ///
    /// This accesses nodes in the PageList, so the caller must ensure
    /// it is safe to do so (i.e. hold necessary locks).
    ///
    /// This returns false if there is no more data to feed. This essentially
    /// means we've searched the entire pagelist.
    pub fn feed(&mut self) -> Result<bool, AllocError> {
        // SAFETY: the pin is tracked by the live list (caller contract).
        let pin = unsafe { &mut *self.pin.as_ptr() };

        // If our pin becomes garbage it means wherever we were next
        // was reused and we can't make sense of our progress anymore.
        // It is effectively equivalent to reaching the end of the PageList.
        if pin.garbage {
            return Ok(false);
        }

        // Add at least enough data to find a single match.
        let mut rem = self.window.needle.len();

        // Start at our previous node and then continue adding until we
        // get our desired amount of data.
        // SAFETY: pin nodes are live.
        let mut node_ = unsafe { pin.node.as_ref().prev };
        while let Some(node) = node_ {
            rem = rem.saturating_sub(self.window.append(node)?.content_len);

            // Move our tracked pin to the new node.
            // SAFETY: node is live.
            let (rows, cols) = unsafe { (node.as_ref().rows(), node.as_ref().cols()) };
            pin.node = node;
            pin.y = rows - 1;
            pin.x = cols - 1;
            pin.garbage = false;

            if rem == 0 {
                break;
            }
            // SAFETY: node is live.
            node_ = unsafe { node.as_ref().prev };
        }

        // True if we fed any data.
        Ok(rem < self.window.needle.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::page::{Capacity, Cell};
    use crate::terminal::page_list::{CompressMode, FinalizeOptions, Location, Options as PageListOptions, Storage};
    use crate::terminal::point::Tag;
    use crate::terminal::screen::SelectionString;
    use crate::terminal::search::testing::{active, next_slice, terminal, terminal_default};
    use crate::terminal::selection::Selection;
    use crate::terminal::size::CellCountInt;
    use crate::terminal::terminal::Terminal;

    fn n<'a>(node: NodePtr) -> &'a mut crate::terminal::page_list::Node {
        // SAFETY: live test node with exclusive access.
        unsafe { &mut *node.as_ptr() }
    }

    fn pages(t: &mut Terminal) -> &mut PageList {
        &mut t.screens.active.pages
    }

    fn search_last(t: &mut Terminal, needle: &[u8]) -> PageListSearch {
        let list = pages(t);
        let last = list.pages.last.unwrap();
        PageListSearch::init(Allocator::GLOBAL, needle, list, last).unwrap()
    }

    fn first_page_rows(t: &mut Terminal) -> CellCountInt {
        n(pages(t).pages.first.unwrap()).capacity().rows
    }

    fn single_page(t: &mut Terminal) -> bool {
        let list = pages(t);
        list.pages.first == list.pages.last
    }

    #[test]
    fn simple_search() {
        let mut t = terminal_default(10, 10);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = search_last(&mut t, b"Fizz");

        {
            let sel = search.next().unwrap().untracked();
            assert_eq!(active(0, 2), pages(&mut t).point_from_pin(Tag::Active, sel.start));
            assert_eq!(active(3, 2), pages(&mut t).point_from_pin(Tag::Active, sel.end));
        }
        {
            let sel = search.next().unwrap().untracked();
            assert_eq!(active(0, 0), pages(&mut t).point_from_pin(Tag::Active, sel.start));
            assert_eq!(active(3, 0), pages(&mut t).point_from_pin(Tag::Active, sel.end));
        }
        assert!(search.next().is_none());

        // We should not be able to feed since we have one page
        assert!(!search.feed().unwrap());
        search.deinit();
    }

    #[test]
    fn feed_multiple_pages_with_matches() {
        let mut t = terminal_default(10, 10);

        // Fill up first page
        let rows = first_page_rows(&mut t);
        for _ in 0..rows - 1 {
            next_slice(&mut t, b"\r\n");
        }
        next_slice(&mut t, b"Fizz");
        assert!(single_page(&mut t));

        // Create second page
        next_slice(&mut t, b"\r\n");
        assert!(!single_page(&mut t));
        next_slice(&mut t, b"Buzz\r\nFizz");

        let mut search = search_last(&mut t, b"Fizz");

        // First match on the last page
        assert!(search.next().is_some());
        assert!(search.next().is_none());

        // Feed should succeed and load the first page
        assert!(search.feed().unwrap());

        // Now we should find the match on the first page
        assert!(search.next().is_some());
        assert!(search.next().is_none());

        // No more pages to feed
        assert!(!search.feed().unwrap());
        search.deinit();
    }

    #[test]
    fn feed_multiple_pages_no_matches() {
        let mut t = terminal_default(10, 10);

        // Fill up first page
        let rows = first_page_rows(&mut t);
        for _ in 0..rows - 1 {
            next_slice(&mut t, b"\r\n");
        }
        next_slice(&mut t, b"Hello");

        // Create second page
        next_slice(&mut t, b"\r\n");
        assert!(!single_page(&mut t));
        next_slice(&mut t, b"World");

        let mut search = search_last(&mut t, b"Nope");

        // No matches on last page
        assert!(search.next().is_none());

        // Feed first page
        assert!(search.feed().unwrap());

        // Still no matches
        assert!(search.next().is_none());

        // No more pages
        assert!(!search.feed().unwrap());
        search.deinit();
    }

    #[test]
    fn feed_iteratively_through_multiple_matches() {
        let mut t = terminal_default(80, 24);

        let rows = first_page_rows(&mut t);

        // Fill first page with a match at the end
        for _ in 0..rows - 1 {
            next_slice(&mut t, b"\r\n");
        }
        next_slice(&mut t, b"Page1Test");
        assert!(single_page(&mut t));

        // Create second page with a match
        next_slice(&mut t, b"\r\n");
        assert!(!single_page(&mut t));
        next_slice(&mut t, b"Page2Test");

        let mut search = search_last(&mut t, b"Test");

        // Match on page 2
        assert!(search.next().is_some());
        assert!(search.next().is_none());

        // Feed page 1
        assert!(search.feed().unwrap());
        assert!(search.next().is_some());
        assert!(search.next().is_none());

        // No more pages
        assert!(!search.feed().unwrap());
        search.deinit();
    }

    #[test]
    fn feed_with_match_spanning_page_boundary() {
        let mut t = terminal_default(80, 24);

        let rows = first_page_rows(&mut t);

        // Fill first page ending with "Te"
        for _ in 0..rows - 1 {
            next_slice(&mut t, b"\r\n");
        }
        for _ in 0..pages(&mut t).cols - 2 {
            next_slice(&mut t, b"x");
        }
        next_slice(&mut t, b"Te");
        assert!(single_page(&mut t));

        // Second page starts with "st"
        next_slice(&mut t, b"st");
        assert!(!single_page(&mut t));

        let mut search = search_last(&mut t, b"Test");

        // No complete match on last page alone (only has "st")
        assert!(search.next().is_none());

        // Feed first page - this should give us enough data to find "Test"
        assert!(search.feed().unwrap());

        // Should find the spanning match
        let sel = search.next().unwrap().untracked();
        assert_ne!(sel.start.node, sel.end.node);
        {
            let str = t
                .screens
                .active
                .selection_string(SelectionString::new(Selection::init(sel.start, sel.end, false)));
            assert_eq!(str, "Test");
        }

        // No more matches
        assert!(search.next().is_none());

        // No more pages
        assert!(!search.feed().unwrap());
        search.deinit();
    }

    #[test]
    fn compressed_history_match_spanning_page_boundary_remains_compressed() {
        let mut t = terminal(80, 24, Some(10 * 1024 * 1024));

        let first = pages(&mut t).pages.first.unwrap();
        let rows = n(first).capacity().rows;

        // Put half the needle on either side of a soft-wrapped page boundary.
        for _ in 0..rows - 1 {
            next_slice(&mut t, b"\r\n");
        }
        for _ in 0..pages(&mut t).cols - 2 {
            next_slice(&mut t, b"x");
        }
        next_slice(&mut t, b"Test");
        let second = n(first).next.unwrap();
        assert_eq!(second, pages(&mut t).pages.last.unwrap());

        // Move both matching pages completely into history. The third page holds
        // the active area, making the first two eligible for compression.
        while pages(&mut t).pages.last.unwrap() == second {
            next_slice(&mut t, b"\r\n");
        }
        for _ in 0..pages(&mut t).rows {
            next_slice(&mut t, b"\r\n");
        }
        assert_ne!(pages(&mut t).get_top_left(Tag::Active).node, second);

        let _ = pages(&mut t).compress(CompressMode::Full);
        assert_eq!(Storage::Compressed, n(first).storage());
        assert_eq!(Storage::Compressed, n(second).storage());
        let compressed = pages(&mut t).memory_stats();
        assert!(compressed.compressed_pages >= 2);

        let mut search = search_last(&mut t, b"Test");

        // Drain and feed incrementally until the boundary match is available. The
        // exact number of feeds depends on how much blank-page text the formatter
        // trims, so bound progress by the number of pages rather than encoding
        // that implementation detail in the test.
        let mut found: Option<FlattenedHighlight> = None;
        for _ in 0..pages(&mut t).total_pages() + 1 {
            if let Some(m) = search.next() {
                found = Some(m);
                break;
            }
            if !search.feed().unwrap() {
                break;
            }
        }
        assert!(found.is_some());

        let m = found.unwrap().untracked();
        assert_eq!(first, m.start.node);
        assert_eq!(second, m.end.node);
        assert_eq!(pages(&mut t).cols - 2, m.start.x);
        assert_eq!(1 as CellCountInt, m.end.x);

        // Search owns formatted text and coordinate maps, not page snapshots. The
        // original raw mappings therefore remain discarded after matching.
        assert_eq!(compressed, pages(&mut t).memory_stats());
        assert_eq!(Storage::Compressed, n(first).storage());
        assert_eq!(Storage::Compressed, n(second).storage());
        search.deinit();
    }

    #[test]
    fn feed_with_match_spanning_page_boundary_with_newline() {
        let mut t = terminal_default(80, 24);

        let rows = first_page_rows(&mut t);

        // Fill first page ending with "Te"
        for _ in 0..rows - 1 {
            next_slice(&mut t, b"\r\n");
        }
        for _ in 0..pages(&mut t).cols - 2 {
            next_slice(&mut t, b"x");
        }
        next_slice(&mut t, b"Te");
        assert!(single_page(&mut t));

        // Second page starts with "st"
        next_slice(&mut t, b"\r\n");
        assert!(!single_page(&mut t));
        next_slice(&mut t, b"st");

        let mut search = search_last(&mut t, b"Test");

        // Should not find any matches since we broke with an explicit newline.
        assert!(search.next().is_none());
        assert!(search.feed().unwrap());
        assert!(search.next().is_none());
        assert!(!search.feed().unwrap());
        search.deinit();
    }

    #[test]
    fn feed_with_pruned_page() {
        // Zero here forces minimum max size to effectively two pages.
        let mut p = PageList::init(PageListOptions {
            cols: 80,
            rows: 24,
            max_size: Some(0),
            max_lines: None,
        })
        .unwrap();

        // Grow to capacity
        let page1_node = p.pages.last.unwrap();
        let (cap, size) = {
            let page1 = n(page1_node).page();
            (page1.capacity.rows, page1.size.rows)
        };
        for _ in 0..cap - size {
            assert!(p.grow().unwrap().is_none());
        }

        // Grow and allocate one more page. Then fill that page up.
        let page2_node = p.grow().unwrap().unwrap();
        let (cap, size) = {
            let page2 = n(page2_node).page();
            (page2.capacity.rows, page2.size.rows)
        };
        for _ in 0..cap - size {
            assert!(p.grow().unwrap().is_none());
        }

        // Setup search and feed until we can't
        let last = p.pages.last.unwrap();
        let mut search = PageListSearch::init(Allocator::GLOBAL, b"Test", &mut p, last).unwrap();
        assert!(search.feed().unwrap());
        assert!(!search.feed().unwrap());

        // Next should create a new page, but it should reuse our first
        // page since we're at max size.
        let new = p.grow().unwrap().unwrap();
        assert!(p.pages.last.unwrap() == new);

        // Our first should now be page2 and our last should be page1
        assert_eq!(page2_node, p.pages.first.unwrap());
        assert_eq!(page1_node, p.pages.last.unwrap());

        // Feed should still do nothing
        assert!(!search.feed().unwrap());
        search.deinit();
    }

    #[test]
    fn feed_keeps_its_tracked_pin_within_a_shorter_page() {
        let mut pages = PageList::init(PageListOptions {
            cols: 10,
            rows: 2,
            max_size: None,
            max_lines: None,
        })
        .unwrap();

        let first = pages.pages.first.unwrap();
        while n(first).rows() < n(first).capacity().rows {
            let _ = pages.grow().unwrap();
        }

        let second = pages.grow().unwrap().unwrap();
        while n(second).rows() < n(second).capacity().rows {
            let _ = pages.grow().unwrap();
        }

        pages
            .split(Pin {
                y: 1,
                x: 0,
                ..Pin::new(first)
            })
            .unwrap();
        let shorter = n(first).next.unwrap();
        assert!(n(shorter).rows() < n(second).rows());

        let mut search = PageListSearch::init(Allocator::GLOBAL, b"x", &mut pages, second).unwrap();

        assert!(search.feed().unwrap());
        // SAFETY: the pin is tracked by the live list.
        let pin = unsafe { *search.pin.as_ptr() };
        assert_eq!(shorter, pin.node);
        assert!(pages.pin_is_valid(pin));
        search.deinit();
    }

    #[test]
    fn feed_discovers_pages_prepended_after_exhaustion() {
        let mut t = terminal(10, 10, Some(usize::MAX));
        next_slice(&mut t, b"Fizz");

        // Exhaust the list: one match on the only page and nothing left to feed.
        let mut search = search_last(&mut t, b"Fizz");
        assert!(search.next().is_some());
        assert!(search.next().is_none());
        assert!(!search.feed().unwrap());

        // Prepend an older page the way incremental snapshot history restore
        // does. The frontier pin names the page that used to be first, and
        // finalize keeps that node's identity, so the pin now has a `prev`.
        let old_first = pages(&mut t).pages.first.unwrap();
        {
            let mut allocation = pages(&mut t).allocate_page(Capacity::new(10, 2)).unwrap();
            let page = allocation.page();
            page.size.rows = 2;
            for (x, c) in "Fizz".bytes().enumerate() {
                // SAFETY: in-bounds cell of the detached page.
                unsafe { *page.get_row_and_cell(x, 1).cell = Cell::init(c as u32) };
            }
            allocation.finalize(Location::Prepend, FinalizeOptions::default()).unwrap();
        }
        let prepended = pages(&mut t).pages.first.unwrap();
        assert_ne!(prepended, old_first);
        // SAFETY: the pin is tracked by the live list.
        assert_eq!(old_first, unsafe { search.pin.as_ref().node });

        // Feeding resumes into the restored history and finds its match.
        assert!(search.feed().unwrap());
        assert_eq!(prepended, unsafe { search.pin.as_ref().node });
        let sel = search.next().unwrap().untracked();
        assert_eq!(prepended, sel.start.node);
        assert_eq!(prepended, sel.end.node);
        assert_eq!(1 as CellCountInt, sel.start.y);
        assert_eq!(0 as CellCountInt, sel.start.x);
        assert_eq!(3 as CellCountInt, sel.end.x);
        assert!(search.next().is_none());
        assert!(!search.feed().unwrap());
        search.deinit();
    }
}
