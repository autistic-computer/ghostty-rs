//! Port of `src/terminal/search/viewport.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: `deinit` is `Drop`. The fingerprint's buffer is a
//! `Vec`; growing it is modeled as an allocation through the window's
//! allocator ([`Allocator::check`]) so failure injection sees it.

use crate::terminal::highlight::Flattened as FlattenedHighlight;
use crate::terminal::page_list::{Allocator, Direction as PageDirection, NodePtr, PageList};
use crate::terminal::point::{Coordinate, Point, Tag};
use crate::terminal::search::sliding_window::{AppendResult, Direction, SlidingWindow};
use crate::terminal::size::AllocError;

/// Searches for a substring within the viewport of a PageList.
///
/// This contains logic to efficiently detect when the viewport changes
/// and only re-search when necessary.
///
/// The specialization for "viewport" is because the viewport is the
/// only part of the search where the user can actively see the results,
/// usually. In that case, it is more efficient to re-search only the
/// viewport rather than store all the results for the entire screen.
///
/// Note that this searches all the pages that viewport covers, so
/// this can include extra matches outside the viewport if the data
/// lives in the same page.
#[derive(Debug)]
pub struct ViewportSearch {
    pub window: SlidingWindow,
    pub fingerprint: Fingerprint,

    /// If this is null, then active dirty tracking is disabled and if the
    /// viewport overlaps the active area we always re-search. If this is
    /// non-null, then we only re-search if the active area is dirty. Dirty
    /// marking is up to the caller.
    pub active_dirty: Option<bool>,
}

impl ViewportSearch {
    pub fn init(alloc: Allocator, needle_unowned: &[u8]) -> Result<ViewportSearch, AllocError> {
        // We just do a forward search since the viewport is usually
        // pretty small so search results are instant anyways. This avoids
        // a small amount of work to reverse things.
        let window = SlidingWindow::init(alloc, Direction::Forward, needle_unowned)?;
        Ok(ViewportSearch {
            window,
            fingerprint: Fingerprint::default(),
            active_dirty: None,
        })
    }

    /// Reset our fingerprint and results so that the next update will
    /// always re-search.
    pub fn reset(&mut self) {
        self.fingerprint.clear_and_retain_capacity();
        self.window.clear_and_retain_capacity();
    }

    /// The needle that this search is using.
    pub fn needle(&self) -> &[u8] {
        debug_assert!(self.window.direction == Direction::Forward);
        &self.window.needle
    }

    /// Update the sliding window to reflect the current viewport. This
    /// will do nothing if the viewport hasn't changed since the last
    /// search.
    ///
    /// The PageList must be safe to read throughout the lifetime of this
    /// function.
    ///
    /// Returns true if the viewport changed and a re-search is needed.
    /// Returns false if the viewport is unchanged.
    pub fn update(&mut self, list: &PageList) -> Result<bool, AllocError> {
        // See if our viewport changed. We retain the fingerprint buffer so
        // unchanged viewports don't allocate and changed viewports usually
        // reuse the previous capacity.
        'changed: {
            if self.fingerprint.update(self.window.alloc, list)? {
                break 'changed;
            }

            // Determine if we need to check if we overlap the active area.
            // If we have dirty tracking on we also set it to false here.
            let check_active: bool = match self.active_dirty {
                None => true,
                Some(false) => false,
                Some(true) => {
                    self.active_dirty = Some(false);
                    true
                }
            };

            if check_active {
                // If our fingerprint contains the active area, then we always
                // re-search since the active area is mutable.
                let active_tl = list.get_top_left(Tag::Active);
                let active_br = list.get_bottom_right(Tag::Active).unwrap();

                // If our viewport contains the start or end of the active area,
                // we are in the active area. We purposely do this first because
                // our viewport is always larger than the active area.
                for entry in &self.fingerprint.entries {
                    if entry.node == active_tl.node {
                        break 'changed;
                    }
                    if entry.node == active_br.node {
                        break 'changed;
                    }
                }
            }

            // No change
            return Ok(false);
        }

        // If our active area was set as dirty, we always unset it here
        // because we're re-searching now.
        if let Some(v) = &mut self.active_dirty {
            *v = false;
        }

        // Clear our previous sliding window
        self.window.clear_and_retain_capacity();

        // Add enough overlap to cover needle.len - 1 bytes (if it
        // exists) so we can cover the overlap. We only do this for the
        // soft-wrapped prior pages.
        let overlap_len = self.window.needle.len().saturating_sub(1);
        let entries = &self.fingerprint.entries;
        // SAFETY: fingerprint nodes were just captured from the live list.
        let mut node_ = unsafe { entries[0].node.as_ref().prev };
        let mut added: usize = 0;
        while let Some(node) = node_ {
            // We could be more accurate here and count bytes since the
            // last wrap but its complicated and unlikely multiple pages
            // wrap so this should be fine.
            let Some(appended) = self.window.append_if_wrapped(node)? else {
                break;
            };
            added += appended.content_len;
            if added >= overlap_len {
                break;
            }
            // SAFETY: live list node.
            node_ = unsafe { node.as_ref().prev };
        }

        // We can use our fingerprint nodes to initialize our sliding
        // window, since we already traversed the viewport once.
        let mut end_append = AppendResult {
            content_len: 0,
            last_row_wrapped: false,
        };
        for entry in entries {
            end_append = self.window.append(entry.node)?;
        }

        // Add any trailing overlap as well.
        'trailing: {
            let end: NodePtr = entries[entries.len() - 1].node;
            if !end_append.last_row_wrapped {
                break 'trailing;
            }

            // SAFETY: live list node.
            node_ = unsafe { end.as_ref().next };
            added = 0;
            while let Some(node) = node_ {
                let appended = self.window.append(node)?;
                added += appended.content_len;
                if added >= overlap_len {
                    break;
                }

                // If this row doesn't wrap, then we can quit
                if !appended.last_row_wrapped {
                    break;
                }
                // SAFETY: live list node.
                node_ = unsafe { node.as_ref().next };
            }
        }

        Ok(true)
    }

    /// Find the next match for the needle in the active area. This returns
    /// null when there are no more matches.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<FlattenedHighlight> {
        self.window.next()
    }
}

/// Viewport fingerprint so we can detect when the viewport moves.
/// (Zig: `ViewportSearch.Fingerprint`.)
#[derive(Debug, Default)]
pub struct Fingerprint {
    /// The node addresses and generations that make up the viewport. We
    /// can't safely dereference cached node pointers because the nodes may
    /// be invalid, so equality only compares these captured values.
    pub entries: Vec<FingerprintEntry>,
}

/// (Zig: `Fingerprint.Entry`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FingerprintEntry {
    pub node: NodePtr,
    pub serial: u64,
}

impl Fingerprint {
    pub fn clear_and_retain_capacity(&mut self) {
        self.entries.clear();
    }

    /// Update this fingerprint to match the current viewport.
    ///
    /// Returns true if the viewport changed. The viewport can have at
    /// most one entry per visible row, so we reserve that upper bound
    /// before mutating the retained buffer. This lets us rebuild in one
    /// pass while still preserving the old fingerprint if allocation
    /// fails.
    pub fn update(&mut self, alloc: Allocator, pages: &PageList) -> Result<bool, AllocError> {
        let rows = pages.rows as usize;
        if self.entries.capacity() < rows {
            alloc.check()?;
            self.entries.reserve_exact(rows - self.entries.len());
        }

        let old_len = self.entries.len();
        let mut changed = false;

        let mut i: usize = 0;
        let it = pages.page_iterator(PageDirection::RightDown, Point::Viewport(Coordinate::default()), None);
        for chunk in it {
            let entry = FingerprintEntry {
                node: chunk.node,
                // SAFETY: chunk nodes are live list nodes.
                serial: unsafe { chunk.node.as_ref().serial },
            };
            if !changed {
                changed = i >= old_len
                    || self.entries[i].node != entry.node
                    || self.entries[i].serial != entry.serial;
            }

            if i < self.entries.len() {
                self.entries[i] = entry;
            } else {
                self.entries.push(entry);
            }
            i += 1;
        }

        self.entries.truncate(i);
        Ok(changed || i != old_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::page_list::testing::FailingAllocator;
    use crate::terminal::search::testing::{active, next_slice, screen, terminal, terminal_default};
    use crate::terminal::terminal::ScrollViewport;

    #[test]
    fn simple_search() {
        let mut t = terminal_default(10, 10);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = ViewportSearch::init(Allocator::GLOBAL, b"Fizz").unwrap();
        assert!(search.update(&t.screens.active.pages).unwrap());

        // Viewport contains active so update should always re-search.
        assert!(search.update(&t.screens.active.pages).unwrap());

        let pages = &t.screens.active.pages;
        {
            let sel = search.next().unwrap().untracked();
            assert_eq!(active(0, 0), pages.point_from_pin(Tag::Active, sel.start));
            assert_eq!(active(3, 0), pages.point_from_pin(Tag::Active, sel.end));
        }
        {
            let sel = search.next().unwrap().untracked();
            assert_eq!(active(0, 2), pages.point_from_pin(Tag::Active, sel.start));
            assert_eq!(active(3, 2), pages.point_from_pin(Tag::Active, sel.end));
        }
        assert!(search.next().is_none());
    }

    #[test]
    fn fingerprint_detects_node_generation_changes() {
        let t = terminal_default(10, 10);

        let mut search = ViewportSearch::init(Allocator::GLOBAL, b"needle").unwrap();
        search.active_dirty = Some(false);

        assert!(search.update(&t.screens.active.pages).unwrap());
        assert!(!search.update(&t.screens.active.pages).unwrap());

        let old_entry = search.fingerprint.entries[0];
        let node = t.screens.active.pages.pages.first.unwrap();
        assert_eq!(node, old_entry.node);
        // SAFETY: live test node with exclusive access.
        let original_serial = unsafe { node.as_ref().serial };
        unsafe { (*node.as_ptr()).serial += 1 };

        // The viewport still contains the same pointer, but its generation has
        // changed, so the cached window must be rebuilt.
        assert!(search.update(&t.screens.active.pages).unwrap());
        let new_entry = search.fingerprint.entries[0];
        assert_eq!(old_entry.node, new_entry.node);
        assert_ne!(old_entry.serial, new_entry.serial);

        // SAFETY: as above.
        unsafe { (*node.as_ptr()).serial = original_serial };
    }

    #[test]
    fn fingerprint_only_allocates_on_changes() {
        let t = terminal_default(10, 10);

        let failing = FailingAllocator::leak(usize::MAX);
        let failing_alloc = failing.allocator();

        let mut search = ViewportSearch::init(failing_alloc, b"needle").unwrap();
        search.active_dirty = Some(false);

        assert!(search.update(&t.screens.active.pages).unwrap());

        // Fail the next allocation. An unchanged viewport with active dirty
        // tracking disabled should reuse the existing fingerprint and return
        // unchanged before rebuilding the search window, so this update should
        // succeed without reporting a viewport change.
        failing.set_fail_index(failing.alloc_index());
        assert!(!search.update(&t.screens.active.pages).unwrap());
    }

    #[test]
    fn clear_screen_and_search() {
        let mut t = terminal_default(10, 10);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = ViewportSearch::init(Allocator::GLOBAL, b"Fizz").unwrap();
        assert!(search.update(&t.screens.active.pages).unwrap());

        next_slice(&mut t, b"\x1b[2J"); // Clear screen
        next_slice(&mut t, b"\x1b[H"); // Move cursor home
        next_slice(&mut t, b"Buzz\r\nFizz\r\nBuzz");
        assert!(search.update(&t.screens.active.pages).unwrap());

        let pages = &t.screens.active.pages;
        {
            let sel = search.next().unwrap().untracked();
            assert_eq!(active(0, 1), pages.point_from_pin(Tag::Active, sel.start));
            assert_eq!(active(3, 1), pages.point_from_pin(Tag::Active, sel.end));
        }
        assert!(search.next().is_none());
    }

    #[test]
    fn clear_screen_and_search_dirty_tracking() {
        let mut t = terminal_default(10, 10);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = ViewportSearch::init(Allocator::GLOBAL, b"Fizz").unwrap();

        // Turn on dirty tracking
        search.active_dirty = Some(false);

        // Should update since we've never searched before
        assert!(search.update(&t.screens.active.pages).unwrap());

        // Should not update since nothing changed
        assert!(!search.update(&t.screens.active.pages).unwrap());

        next_slice(&mut t, b"\x1b[2J"); // Clear screen
        next_slice(&mut t, b"\x1b[H"); // Move cursor home
        next_slice(&mut t, b"Buzz\r\nFizz\r\nBuzz");

        // Should still not update since active area isn't dirty
        assert!(!search.update(&t.screens.active.pages).unwrap());

        // Mark
        search.active_dirty = Some(true);
        assert!(search.update(&t.screens.active.pages).unwrap());

        let pages = &t.screens.active.pages;
        {
            let sel = search.next().unwrap().untracked();
            assert_eq!(active(0, 1), pages.point_from_pin(Tag::Active, sel.start));
            assert_eq!(active(3, 1), pages.point_from_pin(Tag::Active, sel.end));
        }
        assert!(search.next().is_none());
    }

    #[test]
    fn history_search_no_active_area() {
        let mut t = terminal(10, 2, Some(10_000));

        // Fill up first page
        // SAFETY: live test node.
        let first_page_rows = unsafe { t.screens.active.pages.pages.first.unwrap().as_ref().capacity().rows };
        next_slice(&mut t, b"Fizz\r\n");
        for _ in 1..first_page_rows - 1 {
            next_slice(&mut t, b"\r\n");
        }
        assert!(t.screens.active.pages.pages.first == t.screens.active.pages.pages.last);

        // Create second page
        next_slice(&mut t, b"\r\n");
        assert!(t.screens.active.pages.pages.first != t.screens.active.pages.pages.last);
        next_slice(&mut t, b"Buzz\r\nFizz");

        t.scroll_viewport(ScrollViewport::Top);

        let mut search = ViewportSearch::init(Allocator::GLOBAL, b"Fizz").unwrap();
        assert!(search.update(&t.screens.active.pages).unwrap());

        let pages = &t.screens.active.pages;
        {
            let sel = search.next().unwrap().untracked();
            assert_eq!(screen(0, 0), pages.point_from_pin(Tag::Screen, sel.start));
            assert_eq!(screen(3, 0), pages.point_from_pin(Tag::Screen, sel.end));
        }
        assert!(search.next().is_none());

        // Viewport doesn't contain active
        assert!(!search.update(&t.screens.active.pages).unwrap());
        assert!(search.next().is_none());
    }
}
