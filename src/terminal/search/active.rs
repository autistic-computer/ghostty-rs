//! Port of `src/terminal/search/active.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: `deinit` is `Drop` (the window owns only memory).

use crate::terminal::highlight::Flattened as FlattenedHighlight;
use crate::terminal::page_list::{Allocator, NodePtr, PageList};
use crate::terminal::search::sliding_window::{Direction, SlidingWindow};
use crate::terminal::size::AllocError;

/// Searches for a substring within the active area of a PageList.
///
/// The distinction for "active area" is important because it is the
/// only part of a PageList that is mutable. Therefore, its the only part
/// of the terminal that needs to be repeatedly searched as the contents
/// change.
///
/// This struct specializes in searching only within that active area,
/// and handling the active area moving as new lines are added to the bottom.
#[derive(Debug)]
pub struct ActiveSearch {
    pub window: SlidingWindow,
}

impl ActiveSearch {
    pub fn init(alloc: Allocator, needle: &[u8]) -> Result<ActiveSearch, AllocError> {
        // We just do a forward search since the active area is usually
        // pretty small so search results are instant anyways. This avoids
        // a small amount of work to reverse things.
        let window = SlidingWindow::init(alloc, Direction::Forward, needle)?;
        Ok(ActiveSearch { window })
    }

    /// Update the active area to reflect the current state of the PageList.
    ///
    /// This doesn't do the search, it only copies the necessary data
    /// to perform the search later. This lets the caller hold the lock
    /// on the PageList for a minimal amount of time.
    ///
    /// This returns the first page (in reverse order) covered by this
    /// search. This allows the history search to overlap and search history.
    /// There CAN BE duplicates, and this page CAN BE mutable, so the history
    /// search results should prune anything that's in the active area.
    ///
    /// If the return value is null it means the active area covers the entire
    /// PageList, currently.
    pub fn update(&mut self, list: &PageList) -> Result<Option<NodePtr>, AllocError> {
        // Clear our previous sliding window
        self.window.clear_and_retain_capacity();

        // An empty needle represents an inactive search and has no overlap
        // or history to load.
        if self.window.needle.is_empty() {
            return Ok(None);
        }

        // First up, add enough pages to cover the active area.
        let mut rem: usize = list.rows as usize;
        let mut node_ = list.pages.last;
        let mut last_node: Option<NodePtr> = None;
        while let Some(node) = node_ {
            self.window.append(node)?;
            last_node = Some(node);

            // SAFETY: nodes reachable from the list are live.
            let node_ref = unsafe { node.as_ref() };

            // If we reached our target amount, then this is the last
            // page that contains the active area. We go to the previous
            // page once more since its the first page of our required
            // overlap.
            if rem <= node_ref.rows() as usize {
                node_ = node_ref.prev;
                break;
            }

            rem -= node_ref.rows() as usize;
            node_ = node_ref.prev;
        }

        // Next, add enough overlap to cover needle.len - 1 bytes (if it
        // exists) so we can cover the overlap.
        while let Some(node) = node_ {
            // We could be more accurate here and count bytes since the
            // last wrap but its complicated and unlikely multiple pages
            // wrap so this should be fine.
            let Some(appended) = self.window.append_if_wrapped(node)? else {
                break;
            };
            if appended.content_len >= self.window.needle.len() - 1 {
                break;
            }
            // SAFETY: nodes reachable from the list are live.
            node_ = unsafe { node.as_ref().prev };
        }

        // Return the last node we added to our window.
        Ok(last_node)
    }

    /// Find the next match for the needle in the active area. This returns
    /// null when there are no more matches.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<FlattenedHighlight> {
        self.window.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::point::Tag;
    use crate::terminal::search::testing::{active, next_slice, terminal_default};

    #[test]
    fn simple_search() {
        let mut t = terminal_default(10, 10);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = ActiveSearch::init(Allocator::GLOBAL, b"Fizz").unwrap();
        search.update(&t.screens.active.pages).unwrap();

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
    fn clear_screen_and_search() {
        let mut t = terminal_default(10, 10);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = ActiveSearch::init(Allocator::GLOBAL, b"Fizz").unwrap();
        search.update(&t.screens.active.pages).unwrap();

        next_slice(&mut t, b"\x1b[2J"); // Clear screen
        next_slice(&mut t, b"\x1b[H"); // Move cursor home
        next_slice(&mut t, b"Buzz\r\nFizz\r\nBuzz");
        search.update(&t.screens.active.pages).unwrap();

        let pages = &t.screens.active.pages;
        {
            let sel = search.next().unwrap().untracked();
            assert_eq!(active(0, 1), pages.point_from_pin(Tag::Active, sel.start));
            assert_eq!(active(3, 1), pages.point_from_pin(Tag::Active, sel.end));
        }
        assert!(search.next().is_none());
    }
}
