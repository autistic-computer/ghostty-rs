//! Port of `src/terminal/search/screen.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - The searched screen is a raw pointer like in Zig (`*Screen`): the
//!   caller guarantees it outlives the search (or calls
//!   [`ScreenSearch::deinit_screen_invalid`]) and holds the necessary locks
//!   for every call documented to access the screen.
//! - `deinit` stays explicit because it untracks pins in the screen's
//!   PageList, which must not happen when the screen is already gone.
//! - `matchAt`/`selectedMatch` return a borrowed highlight instead of a
//!   shallow copy; `matches` returns an owned `Vec` of cloned highlights.
//! - Zig's `hl.clone(alloc)` allocations are modeled with
//!   [`Allocator::check`] so failure injection sees them.

use core::ptr::NonNull;

use crate::terminal::highlight::{Flattened as FlattenedHighlight, Tracked as TrackedHighlight};
use crate::terminal::page_list::{Allocator, PageList, Pin, PinPtr};
use crate::terminal::point::Tag;
use crate::terminal::screen::Screen;
use crate::terminal::search::active::ActiveSearch;
use crate::terminal::search::pagelist::PageListSearch;
use crate::terminal::search::sliding_window::{Direction, SlidingWindow};
use crate::terminal::size::{AllocError, CellCountInt};
use crate::tripwire;

const LOG: &str = "search_screen";

/// Tripwire fail points for `ScreenSearch.reloadActive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReloadActiveFailPoint {
    HistoryAppendNew,
    HistoryAppendExisting,
}

static RELOAD_ACTIVE_TW: tripwire::Module<ReloadActiveFailPoint, AllocError> = tripwire::Module::new();

/// Searches for a needle within a Screen, handling active area updates,
/// pages being pruned from the screen (e.g. scrollback limits), and more.
///
/// Unlike our lower-level searchers (like ActiveSearch and PageListSearch),
/// this will cache and store all search results so the caller can re-access
/// them as needed. This structure does this because it is intended to help
/// the caller handle the case where the Screen is changing while the user
/// is searching.
///
/// An inactive screen can continue to be searched in the background, and when
/// screen state changes, the renderer/caller can access the existing search
/// results without needing to re-search everything. This prevents a particularly
/// nasty UX where going to alt screen (e.g. neovim) and then back would
/// restart the full scrollback search.
#[derive(Debug)]
pub struct ScreenSearch {
    /// The screen being searched.
    pub screen: NonNull<Screen>,

    /// The active area search state
    pub active: ActiveSearch,

    /// The history (scrollback) search state. May be null if there is
    /// no history yet.
    pub history: Option<HistorySearch>,

    /// Current state of the search, a state machine.
    pub state: State,

    /// The currently selected match, if any. As the screen contents
    /// change or get pruned, the screen search will do its best to keep
    /// this accurate.
    pub selected: Option<SelectedMatch>,

    /// The results found so far. These are stored separately because history
    /// is mostly immutable once found, while active area results may
    /// change. This lets us easily reset the active area results for a
    /// re-search scenario.
    pub history_results: Vec<FlattenedHighlight>,
    pub active_results: Vec<FlattenedHighlight>,

    /// The dimensions of the screen. When this changes we need to
    /// restart the whole search, currently.
    pub rows: CellCountInt,
    pub cols: CellCountInt,
}

// SAFETY: the raw pointers are only dereferenced under the caller's
// synchronization (see the type docs), exactly like the Zig version.
unsafe impl Send for ScreenSearch {}

/// (Zig: `ScreenSearch.SelectedMatch`.)
#[derive(Debug, Clone, Copy)]
pub struct SelectedMatch {
    /// Index from the end of the match list (0 = most recent match)
    pub idx: usize,

    /// Tracked highlight so we can detect movement.
    pub highlight: TrackedHighlight,
}

impl SelectedMatch {
    pub fn deinit(self, screen: &mut Screen) {
        self.highlight.deinit(&mut screen.pages);
    }
}

/// History search state. (Zig: `ScreenSearch.HistorySearch`.)
#[derive(Debug)]
pub struct HistorySearch {
    /// The actual searcher state.
    pub searcher: PageListSearch,

    /// The pin for the first node that this searcher is searching from.
    /// We use this when the active area changes to find the diff between
    /// the top of the new active area and the previous start point
    /// to determine if we need to search more history.
    pub start_pin: PinPtr,
}

impl HistorySearch {
    pub fn deinit(&mut self, screen: &mut Screen) {
        self.searcher.deinit();
        screen.pages.untrack_pin(self.start_pin);
    }
}

/// Search state machine. (Zig: `ScreenSearch.State`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Currently searching the active area
    Active,

    /// Currently searching the history area
    History,

    /// History search is waiting for more data to be fed before
    /// it can progress.
    HistoryFeed,

    /// Search is complete given the current terminal state.
    Complete,
}

impl State {
    pub fn is_complete(self) -> bool {
        matches!(self, State::Complete)
    }

    pub fn needs_feed(self) -> bool {
        match self {
            State::HistoryFeed => true,

            // Not obvious but complete search states will prune
            // stale history results on feed, and pick up pages
            // prepended below the searched frontier since the last
            // feed (incremental snapshot history restore).
            State::Complete => true,

            _ => false,
        }
    }
}

/// (Zig: `ScreenSearch.TickError`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickError {
    OutOfMemory,
    FeedRequired,
    SearchComplete,
}

impl From<AllocError> for TickError {
    fn from(_: AllocError) -> Self {
        TickError::OutOfMemory
    }
}

impl core::fmt::Display for TickError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            TickError::OutOfMemory => "OutOfMemory",
            TickError::FeedRequired => "FeedRequired",
            TickError::SearchComplete => "SearchComplete",
        };
        f.write_str(s)
    }
}

impl std::error::Error for TickError {}

/// (Zig: `ScreenSearch.Select`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select {
    /// Next selection, in reverse order (newest to oldest),
    /// non-wrapping.
    Next,

    /// Prev selection, in forward order (oldest to newest),
    /// non-wrapping.
    Prev,
}

impl ScreenSearch {
    /// Initialize a screen search for the given screen and needle.
    ///
    /// The screen must be live and outlive the search (see the type docs).
    pub fn init(alloc: Allocator, screen: *mut Screen, needle_unowned: &[u8]) -> Result<ScreenSearch, AllocError> {
        let screen = NonNull::new(screen).expect("screen search requires a screen");
        // SAFETY: caller contract.
        let (rows, cols) = unsafe { (screen.as_ref().pages.rows, screen.as_ref().pages.cols) };
        let mut result = ScreenSearch {
            screen,
            rows,
            cols,
            active: ActiveSearch::init(alloc, needle_unowned)?,
            history: None,
            state: State::Active,
            selected: None,
            active_results: Vec::new(),
            history_results: Vec::new(),
        };

        // Update our initial active area state
        if let Err(err) = result.reload_active() {
            result.deinit();
            return Err(err);
        }

        Ok(result)
    }

    /// Access the searched screen.
    ///
    /// The returned reference must not be held across calls that access the
    /// screen again (each use re-derives it from the raw pointer).
    #[allow(clippy::mut_from_ref)]
    fn scr<'a>(&self) -> &'a mut Screen {
        // SAFETY: the screen outlives the search and the caller holds the
        // necessary locks (type contract).
        unsafe { &mut *self.screen.as_ptr() }
    }

    pub fn deinit(&mut self) {
        self.deinit_internal(true);
    }

    /// Release owned search state after the underlying Screen has already
    /// been destroyed. Tracked pins belonged to that Screen's PageList pool
    /// and were freed with it, so only independently owned memory is freed.
    pub fn deinit_screen_invalid(&mut self) {
        self.deinit_internal(false);
    }

    fn deinit_internal(&mut self, screen_valid: bool) {
        if let Some(mut h) = self.history.take() {
            if screen_valid {
                h.deinit(self.scr());
            } else {
                h.searcher.deinit_list_invalid();
            }
        }
        if let Some(m) = self.selected.take() {
            if screen_valid {
                m.deinit(self.scr());
            }
        }
        self.active_results = Vec::new();
        self.history_results = Vec::new();
    }

    fn allocator(&self) -> Allocator {
        self.active.window.alloc
    }

    /// The needle that this search is using.
    pub fn needle(&self) -> &[u8] {
        debug_assert!(self.active.window.direction == Direction::Forward);
        &self.active.window.needle
    }

    /// Returns the total number of matches found so far.
    pub fn matches_len(&self) -> usize {
        self.active_results.len() + self.history_results.len()
    }

    /// Reinitialize cached search state after a screen resize. Reflow can
    /// replace every PageList node, so even flattened highlights that weren't
    /// selected contain invalid node pointers once either dimension changes.
    ///
    /// Returns true when the reset occurred. The replacement search has
    /// already loaded its active area through `init` in that case.
    fn reset_if_dimensions_changed(&mut self) -> Result<bool, AllocError> {
        {
            let screen = self.scr();
            if screen.pages.rows == self.rows && screen.pages.cols == self.cols {
                return Ok(false);
            }
        }

        let new = ScreenSearch::init(self.allocator(), self.screen.as_ptr(), self.needle())?;

        self.deinit();
        *self = new;

        debug_assert!(self.scr().pages.rows == self.rows);
        debug_assert!(self.scr().pages.cols == self.cols);
        Ok(true)
    }

    /// Returns all matches as an owned list.
    /// The matches are ordered from most recent to oldest (e.g. bottom
    /// of the screen to top of the screen).
    pub fn matches(&self, alloc: Allocator) -> Result<Vec<FlattenedHighlight>, AllocError> {
        alloc.check()?;
        let mut results: Vec<FlattenedHighlight> =
            Vec::with_capacity(self.active_results.len() + self.history_results.len());

        // Active does a forward search, so we add the active results then
        // reverse them. There are usually not many active results so this
        // is fast enough compared to adding them in reverse order.
        debug_assert!(self.active.window.direction == Direction::Forward);
        results.extend(self.active_results.iter().rev().cloned());

        // History does a backward search, so we can just append them
        // after.
        results.extend(self.history_results.iter().cloned());

        Ok(results)
    }

    /// Search the full screen state. This will block until the search
    /// is complete. For performance, it is recommended to use `tick` and
    /// `feed` to incrementally make progress on the search instead.
    pub fn search_all(&mut self) -> Result<(), AllocError> {
        loop {
            match self.tick() {
                Ok(()) => {}
                Err(TickError::OutOfMemory) => return Err(AllocError::OutOfMemory),
                Err(TickError::FeedRequired) => self.feed()?,
                Err(TickError::SearchComplete) => return Ok(()),
            }
        }
    }

    /// Make incremental progress on the search without accessing any
    /// screen state (so no lock is required).
    ///
    /// This will return error.FeedRequired if the search cannot make progress
    /// without being fed more data. In this case, the caller should call
    /// the `feed` function to provide more data to the searcher.
    ///
    /// This will return error.SearchComplete if the search is fully complete.
    /// This is to signal to the caller that it can move to a more efficient
    /// sleep/wait state until there is more work to do (e.g. new data to feed).
    pub fn tick(&mut self) -> Result<(), TickError> {
        match self.state {
            State::Active => self.tick_active()?,
            State::History => self.tick_history()?,
            State::HistoryFeed => return Err(TickError::FeedRequired),
            State::Complete => return Err(TickError::SearchComplete),
        }
        Ok(())
    }

    /// Feed more data to the searcher so it can continue searching. This
    /// accesses the screen state, so the caller must hold the necessary locks.
    ///
    /// Feed on a complete screen search will perform some cleanup of
    /// potentially stale history results (pruned) and reclaim some memory.
    ///
    /// If pages were prepended to the PageList since the search completed
    /// (incremental snapshot history restore), the feed picks them up and
    /// the search resumes, since "complete" only ever means caught up with
    /// the PageList as of the last feed.
    pub fn feed(&mut self) -> Result<(), AllocError> {
        // Resize/reflow invalidates every flattened result, not just search
        // state that needs another history feed.
        let _ = self.reset_if_dimensions_changed()?;

        let Some(h) = &mut self.history else {
            // No history to feed, search is complete.
            self.state = State::Complete;
            return Ok(());
        };

        // Future: we may want to feed multiple pages at once here to
        // lower the frequency of lock acquisitions.
        if !h.searcher.feed()? {
            // No more data to feed, search is complete.
            self.state = State::Complete;

            // We use this opportunity to also clean up older history
            // results that may be gone due to scrollback pruning, though.
            self.prune_history();

            return Ok(());
        }

        // Depending on our state handle where feed goes
        match self.state {
            // If we're searching active or history, then feeding doesn't
            // change the state.
            State::Active | State::History => {}

            // Feed goes back to searching history.
            State::HistoryFeed => self.state = State::History,

            // A complete search had exhausted the PageList as of its last
            // feed, but pages can still be prepended below that afterward
            // (incremental snapshot history restore). The history searcher's
            // tracked pin keeps its node identity through a prepend, so it
            // just discovered those older pages and loaded them. Resume
            // searching history.
            State::Complete => self.state = State::History,
        }

        Ok(())
    }

    fn history_result_is_valid(&self, hl: &FlattenedHighlight) -> bool {
        let pages = &self.scr().pages;
        hl.chunks
            .iter()
            .all(|chunk| pages.node_is_valid(chunk.node, chunk.serial))
    }

    /// Clear a selected history match if its flattened page coordinates are
    /// stale. reloadActive only inspects the selected history result, so this
    /// avoids validating every cached result on each active-area refresh.
    fn validate_selected_history(&mut self) {
        let Some(m) = self.selected else { return };
        let active_len = self.active_results.len();
        if m.idx < active_len {
            return;
        }

        let history_idx = m.idx - active_len;
        if history_idx < self.history_results.len()
            && self.history_result_is_valid(&self.history_results[history_idx])
        {
            return;
        }

        m.deinit(self.scr());
        self.selected = None;
    }

    fn prune_history(&mut self) {
        // Go through our history results in order (newest to oldest) to find
        // any result that contains an invalid serial.
        let mut i: usize = 0;
        while i < self.history_results.len() {
            let first_serial = self.history_results[i].chunks[0].serial;
            if first_serial < self.scr().pages.page_serial_epoch {
                // Only a whole-list reset advances the epoch. Results are
                // newest to oldest, so this result and the entire remaining
                // suffix predate that reset. Drop them without scanning the
                // live page list for every captured chunk.
                if let Some(m) = self.selected {
                    let first_pruned = self.active_results.len() + i;
                    if m.idx >= first_pruned {
                        m.deinit(self.scr());
                        self.selected = None;
                    }
                }
                self.history_results.truncate(i);
                self.history_results.shrink_to_fit();
                return;
            }

            // Ordinary pruning, layout changes, and replacements do not
            // advance the epoch. Validate their pointer-plus-generation pairs
            // against the live list.
            if self.history_result_is_valid(&self.history_results[i]) {
                i += 1;
                continue;
            }

            // Only this result is known to be invalid. Older results may live
            // on unrelated pages and remain usable, so remove it individually.
            let result_idx = self.active_results.len() + i;
            if let Some(m) = &mut self.selected {
                if m.idx == result_idx {
                    let m = *m;
                    self.selected = None;
                    m.deinit(self.scr());
                } else if m.idx > result_idx {
                    m.idx -= 1;
                }
            }

            self.history_results.remove(i);
        }
    }

    fn tick_active(&mut self) -> Result<(), AllocError> {
        // For the active area, we consume the entire search in one go
        // because the active area is generally small.
        let alloc = self.allocator();
        while let Some(hl) = self.active.next() {
            // If this fails, then we miss a result since `active.next()`
            // moves forward and prunes data. In the future, we may want
            // to have some more robust error handling but the only
            // scenario this would fail is OOM and we're probably in
            // deeper trouble at that point anyways.
            alloc.check()?;
            self.active_results.push(hl);
        }

        // We've consumed the entire active area, move to history.
        self.state = State::History;
        Ok(())
    }

    fn tick_history(&mut self) -> Result<(), AllocError> {
        let alloc = self.allocator();
        let Some(history) = &mut self.history else {
            // No history to search, we're done.
            self.state = State::Complete;
            return Ok(());
        };

        // Try to consume all the loaded matches in one go, because
        // the search is generally fast for loaded data.
        // SAFETY: the start pin is tracked by the live screen.
        let start_node = unsafe { history.start_pin.as_ref().node };
        while let Some(hl) = history.searcher.next() {
            // Ignore selections that are found within the starting
            // node since those are covered by the active area search.
            if hl.chunks[0].node == start_node {
                continue;
            }

            // Same note as tickActive for error handling.
            alloc.check()?;
            self.history_results.push(hl);

            // Since history only appends to our results in reverse order,
            // we don't need to update any selected match state. The index
            // and prior results are unaffected.
        }

        // We need to be fed more data.
        self.state = State::HistoryFeed;
        Ok(())
    }

    /// Reload the active area because it has changed.
    ///
    /// Since it is very fast, this will also do the full active area
    /// search again, too. This avoids any complexity around the search
    /// state machine.
    ///
    /// The caller must hold the necessary locks to access the screen state.
    pub fn reload_active(&mut self) -> Result<(), AllocError> {
        // This check must precede all inspection of cached highlights and
        // searchers: column reflow may have freed every node they reference.
        if self.reset_if_dimensions_changed()? {
            return Ok(());
        }

        // reloadActive only inspects the selected history result, so validate
        // that result without scanning every cached match on this hot path.
        self.validate_selected_history();

        // If our selection pin became garbage it means we scrolled off
        // the end. Clear our selection and on exit of this function,
        // try to select the last match.
        let select_prev: bool = 'select_prev: {
            let Some(m) = self.selected else { break 'select_prev false };
            // SAFETY: tracked pins of the live screen.
            let garbage = unsafe { m.highlight.start.as_ref().garbage || m.highlight.end.as_ref().garbage };
            if !garbage {
                break 'select_prev false;
            }

            m.deinit(self.scr());
            self.selected = None;
            true
        };

        let result = self.reload_active_inner();

        if select_prev {
            if let Err(err) = self.select(Select::Prev) {
                log::info!(target: LOG, "reload failed to reset search selection err={err}");
            }
        }

        result
    }

    fn reload_active_inner(&mut self) -> Result<(), AllocError> {
        self.reload_active_history()?;

        // Figure out if we need to fixup our selection later because
        // it was in the active area.
        let old_active_len = self.active_results.len();
        let old_selection_idx: Option<usize> = self.selected.map(|m| m.idx);

        if let Err(err) = self.reload_active_research() {
            // This is the error scenario. If something fails below,
            // our active area is probably gone, so we just go back
            // to the first result because our selection can't be trusted.
            if old_selection_idx.is_some_and(|idx| idx < old_active_len) {
                if let Some(m) = self.selected.take() {
                    m.deinit(self.scr());
                    if let Err(err) = self.select(Select::Next) {
                        log::info!(target: LOG, "reload failed to reset search selection err={err}");
                    }
                }
            }
            return Err(err);
        }

        // Now we have to fixup our selection if we had one.
        'fixup: {
            let Some(old_idx) = old_selection_idx else { break 'fixup };
            let Some(m) = &mut self.selected else { break 'fixup };

            // If our old selection wasn't in the active area, then we
            // need to fix up our offsets.
            if old_idx >= old_active_len {
                m.idx -= old_active_len;
                m.idx += self.active_results.len();
                break 'fixup;
            }

            // We search for the matching highlight in the new active results.
            // SAFETY: tracked pins of the live screen.
            let (start, end) = unsafe { (*m.highlight.start.as_ptr(), *m.highlight.end.as_ptr()) };
            let len = self.active_results.len();
            for (i, hl) in self.active_results.iter().enumerate() {
                let untracked = hl.untracked();
                if start.eql(untracked.start) && end.eql(untracked.end) {
                    // Found it! Update our index.
                    m.idx = len - 1 - i;
                    break 'fixup;
                }
            }

            // No match, just go back to the first match.
            let m = *m;
            self.selected = None;
            m.deinit(self.scr());
            if let Err(err) = self.select(Select::Next) {
                log::info!(target: LOG, "reload failed to reset search selection err={err}");
            }
        }

        Ok(())
    }

    /// The history part of `reloadActive`: update the active window and
    /// account for pages that moved into history.
    fn reload_active_history(&mut self) -> Result<(), AllocError> {
        let alloc = self.allocator();
        let tw = &RELOAD_ACTIVE_TW;

        let history_node = {
            let list: &PageList = &self.scr().pages;
            self.active.update(list)?
        };

        let Some(history_node) = history_node else {
            // No history node means we have no history
            if let Some(mut h) = self.history.take() {
                h.deinit(self.scr());
                self.history_results.clear();
            }

            // If we have a selection in the history area, we need to
            // move it to the end of the active area.
            if let Some(m) = self.selected {
                let active_len = self.active_results.len();
                if m.idx >= active_len {
                    m.deinit(self.scr());
                    self.selected = None;
                    if let Err(err) = self.select(Select::Prev) {
                        log::info!(target: LOG, "reload failed to reset search selection err={err}");
                    }
                }
            }

            return Ok(());
        };

        // We need to account for any active area growth that would
        // cause new pages to move into our history. If there are new
        // pages then we need to re-search the pages and add it to
        // our history results.

        // If our screen has no scrollback then we have no history.
        if self.scr().no_scrollback {
            debug_assert!(self.history.is_none());
            return Ok(());
        }

        if let Some(h) = &mut self.history {
            // If our start pin became garbage, it means we pruned all
            // the way up through it, so we have no history anymore.
            // Reset our history state.
            // SAFETY: tracked pin of the live screen.
            if unsafe { h.start_pin.as_ref().garbage } {
                let mut h = self.history.take().unwrap();
                h.deinit(self.scr());
                self.history_results.clear();
            }
        }

        let Some(history) = &self.history else {
            // No history search yet, but we now have history. So let's
            // initialize.
            let list: &mut PageList = &mut self.scr().pages;
            let mut search = PageListSearch::init(alloc, self.needle(), list, history_node)?;

            let pin = match list.track_pin(Pin::new(history_node)) {
                Ok(pin) => pin,
                Err(err) => {
                    search.deinit();
                    return Err(err);
                }
            };

            self.history = Some(HistorySearch {
                searcher: search,
                start_pin: pin,
            });

            // We don't need to update any history since we had no history
            // before, so we can break out of the whole conditional.
            return Ok(());
        };

        // SAFETY: tracked pin of the live screen.
        let start_pin = unsafe { &mut *history.start_pin.as_ptr() };
        if start_pin.node == history_node {
            // No change in the starting node, we're done.
            return Ok(());
        }

        // Do a forward search from our prior node to this one. We
        // collect all the results into a new list. We ASSUME that
        // reloadActive is being called frequently enough that there isn't
        // a massive amount of history to search here.
        let mut window = SlidingWindow::init(alloc, Direction::Forward, self.needle())?;
        loop {
            window.append(start_pin.node)?;
            if start_pin.node == history_node {
                break;
            }
            // SAFETY: live list node.
            let Some(next) = (unsafe { start_pin.node.as_ref().next }) else { break };
            start_pin.node = next;
        }
        debug_assert!(start_pin.node == history_node);

        let mut results: Vec<FlattenedHighlight> = Vec::with_capacity(self.history_results.len());
        while let Some(hl) = window.next() {
            if hl.chunks[0].node == history_node {
                continue;
            }

            alloc.check()?;
            tw.check(ReloadActiveFailPoint::HistoryAppendNew)?;
            results.push(hl);
        }

        // If we have no matches then there is nothing to change
        // in our history (fast path)
        if results.is_empty() {
            return Ok(());
        }

        // The number added to our history. Needed for updating
        // our selection if we have one.
        let added_len = results.len();

        // Matches! Reverse our list then append all the remaining
        // history items that didn't start on our original node.
        results.reverse();
        tw.check(ReloadActiveFailPoint::HistoryAppendExisting)?;
        results.append(&mut self.history_results);
        self.history_results = results;

        // If our prior selection was in the history area, update
        // the offset.
        if let Some(m) = &mut self.selected {
            let active_len = self.active_results.len();
            if m.idx >= active_len {
                m.idx += added_len;

                // Moving the idx should not change our targeted result
                // since the history is immutable.
                if cfg!(debug_assertions) {
                    let hl = &self.history_results[m.idx - active_len];
                    // SAFETY: tracked pin of the live screen.
                    debug_assert!(unsafe { m.highlight.start.as_ref() }.eql(hl.start_pin()));
                }
            }
        }

        Ok(())
    }

    /// The active re-search part of `reloadActive` (covered by its
    /// selection-reset `errdefer` in Zig).
    fn reload_active_research(&mut self) -> Result<(), AllocError> {
        // Reset our active search results and search again.
        self.active_results.clear();
        match self.state {
            // If we're in the active state we run a normal tick so
            // we can move into a better state.
            State::Active => self.tick_active()?,

            // Otherwise, just tick it and move back to whatever state
            // we were in.
            _ => {
                let old_state = self.state;
                let r = self.tick_active();
                self.state = old_state;
                r?;
            }
        }

        // If we have no scrollback, we need to prune any active results
        // that aren't in the actual active area. We only do this for the
        // no scrollback scenario because with scrollback we actually
        // rely on our active search searching by page to find history
        // items as well. This is all related to the fact that PageList
        // scrollback limits are discrete by page size except we special
        // case zero.
        if self.scr().no_scrollback && !self.active_results.is_empty() {
            let tl = self.scr().pages.get_top_left(Tag::Active);
            match self
                .active_results
                .iter()
                .position(|hl| tl.before(hl.end_pin()))
            {
                // In the active area! Since our results are sorted
                // that means everything after this is also in the active
                // area, so we prune up to this i.
                Some(i) => {
                    if i > 0 {
                        self.active_results.drain(0..i);
                    }
                }

                // None are in the active area...
                None => self.active_results.clear(),
            }
        }

        Ok(())
    }

    /// Return the match at the given index in newest-to-oldest order
    /// (0 = most recent match). Returns null if the index is out of
    /// range.
    ///
    /// This does not require read/write access to the underlying screen.
    pub fn match_at(&self, idx: usize) -> Option<&FlattenedHighlight> {
        let active_len = self.active_results.len();
        if idx < active_len {
            return Some(&self.active_results[active_len - 1 - idx]);
        }

        let history_len = self.history_results.len();
        if idx < active_len + history_len {
            return Some(&self.history_results[idx - active_len]);
        }

        None
    }

    /// Return the selected match.
    ///
    /// This does not require read/write access to the underlying screen.
    pub fn selected_match(&self) -> Option<&FlattenedHighlight> {
        let sel = self.selected?;
        self.match_at(sel.idx)
    }

    /// Select the next or previous search result. This requires read/write
    /// access to the underlying screen, since we utilize tracked pins to
    /// ensure our selection sticks with contents changing.
    pub fn select(&mut self, to: Select) -> Result<bool, AllocError> {
        // A resize can replace any page node, including nodes retained by
        // flattened history results. Reset before reloadActive or tracking a
        // selected result so no pre-resize pointer is ever dereferenced.
        let _ = self.reset_if_dimensions_changed()?;

        // Reload validates the selected history result before inspecting its
        // cached coordinates. Prune the remaining results afterward so every
        // candidate is valid before selection tracks it.
        self.reload_active()?;
        self.prune_history();

        match to {
            Select::Next => self.select_next(),
            Select::Prev => self.select_prev(),
        }
    }

    fn select_next(&mut self) -> Result<bool, AllocError> {
        // Get our previous match so we can change it. If we have no
        // prior match, we have the easy task of getting the first.
        let Some(prev) = self.selected else {
            // Get our highlight
            let hl: &FlattenedHighlight = if let Some(last) = self.active_results.last() {
                // Active is in forward order
                last
            } else if let Some(first) = self.history_results.first() {
                // History is in reverse order
                first
            } else {
                // No matches at all. Can't select anything.
                return Ok(false);
            };

            // Pin it so we can track any movement
            let tracked = hl.untracked().track(&mut self.scr().pages)?;

            // Our selection is index zero since we just started and
            // we store our selection.
            self.selected = Some(SelectedMatch {
                idx: 0,
                highlight: tracked,
            });
            return Ok(true);
        };

        let active_len = self.active_results.len();
        let history_len = self.history_results.len();
        let next_idx = if prev.idx + 1 >= active_len + history_len {
            0
        } else {
            prev.idx + 1
        };
        let hl: &FlattenedHighlight = if next_idx < active_len {
            &self.active_results[active_len - 1 - next_idx]
        } else {
            &self.history_results[next_idx - active_len]
        };

        // Pin it so we can track any movement
        let tracked = hl.untracked().track(&mut self.scr().pages)?;

        // Free our previous match and setup our new selection
        prev.deinit(self.scr());
        self.selected = Some(SelectedMatch {
            idx: next_idx,
            highlight: tracked,
        });

        Ok(true)
    }

    fn select_prev(&mut self) -> Result<bool, AllocError> {
        // Get our previous match so we can change it. If we have no
        // prior match, we have the easy task of getting the last.
        let Some(prev) = self.selected else {
            // Get our highlight (oldest match)
            let hl: &FlattenedHighlight = if let Some(last) = self.history_results.last() {
                // History is in reverse order, so last item is oldest
                last
            } else if let Some(first) = self.active_results.first() {
                // Active is in forward order, so first item is oldest
                first
            } else {
                // No matches at all. Can't select anything.
                return Ok(false);
            };

            // Pin it so we can track any movement
            let tracked = hl.untracked().track(&mut self.scr().pages)?;

            // Our selection is the last index since we just started
            // and we store our selection.
            let active_len = self.active_results.len();
            let history_len = self.history_results.len();
            self.selected = Some(SelectedMatch {
                idx: active_len + history_len - 1,
                highlight: tracked,
            });
            return Ok(true);
        };

        let active_len = self.active_results.len();
        let history_len = self.history_results.len();
        let next_idx = if prev.idx != 0 {
            prev.idx - 1
        } else {
            active_len + history_len - 1
        };

        let hl: &FlattenedHighlight = if next_idx < active_len {
            &self.active_results[active_len - 1 - next_idx]
        } else {
            &self.history_results[next_idx - active_len]
        };

        // Pin it so we can track any movement
        let tracked = hl.untracked().track(&mut self.scr().pages)?;

        // Free our previous match and setup our new selection
        prev.deinit(self.scr());
        self.selected = Some(SelectedMatch {
            idx: next_idx,
            highlight: tracked,
        });

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::csi::EraseDisplay;
    use crate::terminal::page::{Capacity, Cell};
    use crate::terminal::page_list::{FinalizeOptions, Location, NodePtr, Resize};
    use crate::terminal::point::{Coordinate, Point};
    use crate::terminal::screen::SelectionString;
    use crate::terminal::screen_set::Key as ScreenKey;
    use crate::terminal::search::testing::{active, next_slice, screen, terminal, terminal_default};
    use crate::terminal::selection::Selection;
    use crate::terminal::terminal::Terminal;
    use crate::tripwire::ResetMode;

    fn n<'a>(node: NodePtr) -> &'a mut crate::terminal::page_list::Node {
        // SAFETY: live test node with exclusive access.
        unsafe { &mut *node.as_ptr() }
    }

    fn list<'a>(t: &mut Terminal) -> &'a mut PageList {
        // SAFETY: the active screen outlives the test body.
        unsafe { &mut (*t.screens.active.as_ptr()).pages }
    }

    fn init(t: &mut Terminal, needle: &str) -> ScreenSearch {
        ScreenSearch::init(Allocator::GLOBAL, t.screens.active.as_ptr(), needle.as_bytes()).unwrap()
    }

    fn sp(t: &mut Terminal, p: Pin) -> Option<Point> {
        list(t).point_from_pin(Tag::Screen, p)
    }

    fn ap(t: &mut Terminal, p: Pin) -> Option<Point> {
        list(t).point_from_pin(Tag::Active, p)
    }

    /// Assert the selected match's screen coordinates.
    fn check_selected(t: &mut Terminal, search: &ScreenSearch, start: (u16, u32), end: (u16, u32)) {
        let sel = search.selected_match().unwrap().untracked();
        assert_eq!(screen(start.0, start.1), sp(t, sel.start));
        assert_eq!(screen(end.0, end.1), sp(t, sel.end));
    }

    fn history_terminal() -> Terminal {
        terminal(10, 2, Some(usize::MAX))
    }

    #[test]
    fn simple_search() {
        let mut t = terminal_default(10, 2);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = init(&mut t, "Fizz");
        search.search_all().unwrap();
        assert_eq!(2, search.active_results.len());
        // We don't test history results since there is overlap

        // Get all matches
        let matches = search.matches(Allocator::GLOBAL).unwrap();
        assert_eq!(2, matches.len());

        {
            let sel = matches[0].untracked();
            assert_eq!(screen(0, 2), sp(&mut t, sel.start));
            assert_eq!(screen(3, 2), sp(&mut t, sel.end));
        }
        {
            let sel = matches[1].untracked();
            assert_eq!(screen(0, 0), sp(&mut t, sel.start));
            assert_eq!(screen(3, 0), sp(&mut t, sel.end));
        }
        search.deinit();
    }

    #[test]
    fn simple_search_with_history() {
        let mut t = history_terminal();
        let list = list(&mut t);

        next_slice(&mut t, b"Fizz\r\n");
        while list.total_pages() < 3 {
            next_slice(&mut t, b"\r\n");
        }
        for _ in 0..list.rows {
            next_slice(&mut t, b"\r\n");
        }
        next_slice(&mut t, b"hello.");

        let mut search = init(&mut t, "Fizz");
        search.search_all().unwrap();
        assert_eq!(0, search.active_results.len());

        // Get all matches
        let matches = search.matches(Allocator::GLOBAL).unwrap();
        assert_eq!(1, matches.len());

        {
            let sel = matches[0].untracked();
            assert_eq!(screen(0, 0), sp(&mut t, sel.start));
            assert_eq!(screen(3, 0), sp(&mut t, sel.end));
        }
        search.deinit();
    }

    #[test]
    fn reload_active_with_history_change() {
        let mut t = history_terminal();
        let list = list(&mut t);
        next_slice(&mut t, b"Fizz\r\n");

        // Start up our search which will populate our initial active area.
        let mut search = init(&mut t, "Fizz");
        search.search_all().unwrap();
        {
            let matches = search.matches(Allocator::GLOBAL).unwrap();
            assert_eq!(1, matches.len());
        }

        // Grow into two pages so our history pin will move.
        while list.total_pages() < 2 {
            next_slice(&mut t, b"\r\n");
        }
        for _ in 0..list.rows {
            next_slice(&mut t, b"\r\n");
        }
        next_slice(&mut t, b"2Fizz");

        // Active area changed so reload
        search.reload_active().unwrap();
        search.search_all().unwrap();

        // Get all matches
        {
            let matches = search.matches(Allocator::GLOBAL).unwrap();
            assert_eq!(2, matches.len());
            {
                let sel = matches[1].untracked();
                assert_eq!(screen(0, 0), sp(&mut t, sel.start));
                assert_eq!(screen(3, 0), sp(&mut t, sel.end));
            }
            {
                let sel = matches[0].untracked();
                assert_eq!(active(1, 1), ap(&mut t, sel.start));
                assert_eq!(active(4, 1), ap(&mut t, sel.end));
            }
        }

        // Reset the screen which will make our pin garbage.
        t.full_reset();
        next_slice(&mut t, b"WeFizzing");
        search.reload_active().unwrap();
        search.search_all().unwrap();

        {
            let matches = search.matches(Allocator::GLOBAL).unwrap();
            assert_eq!(1, matches.len());
            {
                let sel = matches[0].untracked();
                assert_eq!(active(2, 0), ap(&mut t, sel.start));
                assert_eq!(active(5, 0), ap(&mut t, sel.end));
            }
        }
        search.deinit();
    }

    #[test]
    fn active_change_contents() {
        let mut t = terminal_default(10, 5);
        next_slice(&mut t, b"Fuzz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = init(&mut t, "Fizz");
        search.search_all().unwrap();
        assert_eq!(1, search.active_results.len());

        // Erase the screen, move our cursor to the top, and change contents.
        next_slice(&mut t, b"\x1b[2J\x1b[H"); // Clear screen and move home
        next_slice(&mut t, b"Bang\r\nFizz\r\nHello!");

        search.reload_active().unwrap();
        search.search_all().unwrap();
        assert_eq!(1, search.active_results.len());

        // Get all matches
        let matches = search.matches(Allocator::GLOBAL).unwrap();
        assert_eq!(1, matches.len());

        {
            let sel = matches[0].untracked();
            assert_eq!(screen(0, 1), sp(&mut t, sel.start));
            assert_eq!(screen(3, 1), sp(&mut t, sel.end));
        }
        search.deinit();
    }

    #[test]
    fn select_next() {
        let mut t = terminal_default(10, 2);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = init(&mut t, "Fizz");

        // Initially no selection
        assert!(search.selected_match().is_none());

        // Select our next match (first)
        search.search_all().unwrap();
        let _ = search.select(Select::Next).unwrap();
        check_selected(&mut t, &search, (0, 2), (3, 2));

        // Next match
        let _ = search.select(Select::Next).unwrap();
        check_selected(&mut t, &search, (0, 0), (3, 0));

        // Next match (wrap)
        let _ = search.select(Select::Next).unwrap();
        check_selected(&mut t, &search, (0, 2), (3, 2));
        search.deinit();
    }

    #[test]
    fn select_after_resize_resets_stale_flattened_results() {
        let mut t = terminal(10, 3, Some(usize::MAX));
        next_slice(&mut t, b"Fizz\r\nBuzz");

        let mut search = init(&mut t, "Fizz");
        search.search_all().unwrap();
        assert!(search.select(Select::Next).unwrap());
        assert!(search.selected_match().is_some());
        assert_eq!(10 as CellCountInt, search.cols);

        // Column reflow replaces the nodes referenced by the cached flattened
        // active result. select() calls reloadActive(), so dimension invalidation
        // must run before either path dereferences those old nodes.
        list(&mut t)
            .resize(Resize {
                cols: Some(5),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(10 as CellCountInt, search.cols);

        assert!(search.select(Select::Next).unwrap());
        assert_eq!(5 as CellCountInt, search.cols);

        let selected = search.selected_match().unwrap().untracked();
        assert_eq!(5 as CellCountInt, n(selected.start.node).cols());
        assert_eq!(5 as CellCountInt, n(selected.end.node).cols());
        search.deinit();
    }

    #[test]
    fn select_in_active_changes_contents_completely() {
        let mut t = terminal_default(10, 5);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = init(&mut t, "Fizz");
        search.search_all().unwrap();
        let _ = search.select(Select::Next).unwrap();
        let _ = search.select(Select::Next).unwrap();
        // Initial selection is the first fizz
        check_selected(&mut t, &search, (0, 0), (3, 0));

        // Erase the screen, move our cursor to the top, and change contents.
        next_slice(&mut t, b"\x1b[2J\x1b[H"); // Clear screen and move home
        next_slice(&mut t, b"Fuzz\r\nFizz\r\nHello!");

        search.reload_active().unwrap();
        // Our selection should move to the first
        check_selected(&mut t, &search, (0, 1), (3, 1));

        // Erase the screen, redraw with same contents.
        next_slice(&mut t, b"\x1b[2J\x1b[H"); // Clear screen and move home
        next_slice(&mut t, b"Fuzz\r\nFizz\r\nFizz");

        search.reload_active().unwrap();
        // Our selection should not move to the first
        check_selected(&mut t, &search, (0, 1), (3, 1));
        search.deinit();
    }

    #[test]
    fn select_into_history() {
        let mut t = history_terminal();
        let list = list(&mut t);

        next_slice(&mut t, b"Fizz\r\n");
        while list.total_pages() < 3 {
            next_slice(&mut t, b"\r\n");
        }
        for _ in 0..list.rows {
            next_slice(&mut t, b"\r\n");
        }
        next_slice(&mut t, b"hello.");

        let mut search = init(&mut t, "Fizz");
        search.search_all().unwrap();

        // Get all matches
        let _ = search.select(Select::Next).unwrap();
        check_selected(&mut t, &search, (0, 0), (3, 0));

        // Erase the screen, redraw with same contents.
        next_slice(&mut t, b"\x1b[2J\x1b[H"); // Clear screen and move home
        next_slice(&mut t, b"yo yo");

        search.reload_active().unwrap();
        // Our selection should not move since the history is still active.
        check_selected(&mut t, &search, (0, 0), (3, 0));

        // Create some new history by adding more lines.
        next_slice(&mut t, b"\r\nfizz\r\nfizz\r\nfizz"); // Clear screen and move home
        search.reload_active().unwrap();
        // Our selection should not move since the history is still not
        // pruned.
        check_selected(&mut t, &search, (0, 0), (3, 0));
        search.deinit();
    }

    #[test]
    fn select_prev() {
        let mut t = terminal_default(10, 2);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = init(&mut t, "Fizz");

        // Initially no selection
        assert!(search.selected_match().is_none());

        // Select prev (oldest first)
        search.search_all().unwrap();
        let _ = search.select(Select::Prev).unwrap();
        check_selected(&mut t, &search, (0, 0), (3, 0));

        // Prev match (towards newest)
        let _ = search.select(Select::Prev).unwrap();
        check_selected(&mut t, &search, (0, 2), (3, 2));

        // Prev match (wrap)
        let _ = search.select(Select::Prev).unwrap();
        check_selected(&mut t, &search, (0, 0), (3, 0));
        search.deinit();
    }

    #[test]
    fn select_prev_then_next() {
        let mut t = terminal_default(10, 2);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = init(&mut t, "Fizz");
        search.search_all().unwrap();

        // Select next (newest first)
        let _ = search.select(Select::Next).unwrap();
        {
            let sel = search.selected_match().unwrap().untracked();
            assert_eq!(screen(0, 2), sp(&mut t, sel.start));
        }

        // Select next (older)
        let _ = search.select(Select::Next).unwrap();
        {
            let sel = search.selected_match().unwrap().untracked();
            assert_eq!(screen(0, 0), sp(&mut t, sel.start));
        }

        // Select prev (back to newer)
        let _ = search.select(Select::Prev).unwrap();
        {
            let sel = search.selected_match().unwrap().untracked();
            assert_eq!(screen(0, 2), sp(&mut t, sel.start));
        }
        search.deinit();
    }

    #[test]
    fn select_prev_with_history() {
        let mut t = history_terminal();
        let list = list(&mut t);

        next_slice(&mut t, b"Fizz\r\n");
        while list.total_pages() < 3 {
            next_slice(&mut t, b"\r\n");
        }
        for _ in 0..list.rows {
            next_slice(&mut t, b"\r\n");
        }
        next_slice(&mut t, b"Fizz.");

        let mut search = init(&mut t, "Fizz");
        search.search_all().unwrap();

        // Select prev (oldest first, should be in history)
        let _ = search.select(Select::Prev).unwrap();
        check_selected(&mut t, &search, (0, 0), (3, 0));

        // Select prev (towards newer, should move to active area)
        let _ = search.select(Select::Prev).unwrap();
        {
            let sel = search.selected_match().unwrap().untracked();
            assert_eq!(active(0, 1), ap(&mut t, sel.start));
            assert_eq!(active(3, 1), ap(&mut t, sel.end));
        }
        search.deinit();
    }

    #[test]
    fn select_prev_wraps_when_all_matches_are_in_history() {
        // Regression test: when every match is in scrollback (the active area
        // has none, so active_len == 0), selecting prev from index 0 must wrap
        // to the last result without underflowing `active_len - 1`.
        let mut t = history_terminal();
        let list = list(&mut t);

        // Put the only match in scrollback, then scroll the active area to all
        // blank lines so it contains no match (active_len == 0, history_len == 1).
        next_slice(&mut t, b"Fizz\r\n");
        while list.total_pages() < 3 {
            next_slice(&mut t, b"\r\n");
        }
        for _ in 0..list.rows {
            next_slice(&mut t, b"\r\n");
        }

        let mut search = init(&mut t, "Fizz");
        search.search_all().unwrap();
        assert_eq!(0, search.active_results.len());

        // Select the first match (idx 0), then wrap backwards. This must not
        // panic and must keep a valid selection.
        let _ = search.select(Select::Next).unwrap();
        let _ = search.select(Select::Prev).unwrap();
        assert!(search.selected_match().is_some());
        search.deinit();
    }

    #[test]
    fn select_after_all_matches_disappear_drops_the_selection() {
        // The wrap arithmetic in selectPrev (active_len + history_len - 1) would
        // underflow if a selection were ever live while both result lists are
        // empty. This guards the invariant that makes that unreachable: when a
        // reload/prune empties the results, the selection is dropped, so the next
        // select() hits the "no matches" guard instead of the wrap arithmetic.
        let mut t = terminal_default(10, 2);
        next_slice(&mut t, b"Fizz");

        let mut search = init(&mut t, "Fizz");
        search.search_all().unwrap();
        assert_eq!(1, search.active_results.len());

        // Take a selection, then overwrite the only match so a reload finds none
        // (active and history both empty).
        let _ = search.select(Select::Next).unwrap();
        assert!(search.selected_match().is_some());
        next_slice(&mut t, b"\x1b[1;1H    ");

        // Must not underflow; the selection is dropped and nothing is selected.
        let _ = search.select(Select::Prev).unwrap();
        assert!(search.selected_match().is_none());
        assert_eq!(0, search.active_results.len());
        assert_eq!(0, search.history_results.len());
        search.deinit();
    }

    #[test]
    fn select_after_partial_history_erase_drops_a_pruned_selection() {
        let mut t = history_terminal();
        let list = list(&mut t);

        next_slice(&mut t, b"error\r\n");
        let first = list.pages.first.unwrap();
        while list.total_pages() < 3 {
            next_slice(&mut t, b"\r\n");
        }
        let first_rows = n(first).rows();

        let mut search = init(&mut t, "error");
        search.search_all().unwrap();
        assert_eq!(0, search.active_results.len());
        assert_eq!(1, search.history_results.len());
        assert!(search.select(Select::Next).unwrap());

        list.erase_history(Some(Point::History(Coordinate {
            x: 0,
            y: first_rows as u32 - 1,
        })));
        // SAFETY: tracked pin of the live screen.
        assert!(unsafe { search.selected.unwrap().highlight.start.as_ref().garbage });

        assert!(!search.select(Select::Next).unwrap());
        assert!(search.selected.is_none());
        search.deinit();
    }

    #[test]
    fn select_after_history_compaction_ignores_replaced_results() {
        let mut t = history_terminal();
        let list = list(&mut t);

        next_slice(&mut t, b"error\r\n");
        let first = list.pages.first.unwrap();
        while list.total_pages() < 3 {
            next_slice(&mut t, b"\r\n");
        }
        list.split(Pin {
            y: n(first).rows() / 2,
            x: 0,
            ..Pin::new(first)
        })
        .unwrap();

        let mut search = init(&mut t, "error");
        search.search_all().unwrap();
        assert_eq!(1, search.history_results.len());

        let replacement = list.compact(first).unwrap().unwrap();
        assert_ne!(replacement, first);

        assert!(!search.select(Select::Next).unwrap());
        assert!(search.selected.is_none());
        search.deinit();
    }

    /// Put "error" on the last row of the first page and push it into
    /// history (shared by several tests).
    fn error_in_history(t: &mut Terminal) -> NodePtr {
        let list = list(t);
        let first = list.pages.first.unwrap();
        while n(first).rows() < n(first).capacity().rows {
            next_slice(t, b"\r\n");
        }
        next_slice(t, b"error");
        for _ in 0..list.rows as usize + 1 {
            next_slice(t, b"\r\n");
        }
        first
    }

    #[test]
    fn select_after_partial_history_page_erase_ignores_shifted_results() {
        let mut t = history_terminal();
        let list = list(&mut t);

        let first = error_in_history(&mut t);
        assert_ne!(first, list.pages.last.unwrap());

        let mut search = init(&mut t, "error");
        search.search_all().unwrap();
        assert_eq!(0, search.active_results.len());
        assert_eq!(1, search.history_results.len());

        let old_rows = n(first).rows();
        list.erase_history(Some(Point::History(Coordinate { x: 0, y: 0 })));
        assert_eq!(old_rows - 1, n(first).rows());

        assert!(!search.select(Select::Next).unwrap());
        assert!(search.selected.is_none());
        search.deinit();
    }

    #[test]
    fn reload_defers_pruning_unselected_history_results() {
        let mut t = history_terminal();
        let list = list(&mut t);

        error_in_history(&mut t);

        let mut search = init(&mut t, "error");
        search.search_all().unwrap();
        assert_eq!(1, search.history_results.len());

        list.erase_history(Some(Point::History(Coordinate { x: 0, y: 0 })));

        // Routine active refreshes don't inspect unselected history results.
        search.reload_active().unwrap();
        assert_eq!(1, search.history_results.len());

        // Selection prunes every candidate before attempting to track it.
        assert!(!search.select(Select::Next).unwrap());
        assert_eq!(0, search.history_results.len());
        search.deinit();
    }

    #[test]
    fn reload_after_partial_history_page_erase_drops_shifted_selection_first() {
        let mut t = history_terminal();
        let list = list(&mut t);

        error_in_history(&mut t);

        let mut search = init(&mut t, "error");
        search.search_all().unwrap();
        assert!(search.select(Select::Next).unwrap());
        assert!(search.selected.is_some());

        list.erase_history(Some(Point::History(Coordinate { x: 0, y: 0 })));

        // Advance the active boundary into another node and add matches so
        // reloadActive updates the history result offsets. It must discard the
        // stale selected result before comparing any of its captured coordinates.
        let active_node = list.get_top_left(Tag::Active).node;
        while list.get_top_left(Tag::Active).node == active_node {
            next_slice(&mut t, b"error\r\n");
        }

        search.reload_active().unwrap();
        assert!(search.selected.is_none());
        search.deinit();
    }

    #[test]
    fn select_after_history_page_split_ignores_moved_results() {
        let mut t = history_terminal();
        let list = list(&mut t);

        let first = error_in_history(&mut t);

        let mut search = init(&mut t, "error");
        search.search_all().unwrap();
        assert_eq!(1, search.history_results.len());

        list.split(Pin {
            y: n(first).rows() / 2,
            x: 0,
            ..Pin::new(first)
        })
        .unwrap();

        assert!(!search.select(Select::Next).unwrap());
        assert!(search.selected.is_none());
        search.deinit();
    }

    #[test]
    fn screen_search_no_scrollback_has_no_history() {
        let mut t = terminal(10, 2, Some(0));

        // Alt screen has no scrollback
        let _ = t.switch_screen(ScreenKey::Alternate).unwrap();

        // This will probably stop working at some point and we'll have
        // no way to test it using public APIs, but at the time of writing
        // this test, CSI 22 J (scroll complete) pushes into scrollback
        // with alt screen.
        next_slice(&mut t, b"Fizz\r\n");
        next_slice(&mut t, b"\x1b[22J");
        next_slice(&mut t, b"hello.");

        let mut search = init(&mut t, "Fizz");
        search.search_all().unwrap();
        assert_eq!(0, search.active_results.len());

        // Get all matches
        let matches = search.matches(Allocator::GLOBAL).unwrap();
        assert_eq!(0, matches.len());
        search.deinit();
    }

    /// Shared setup of the reloadActive tripwire tests.
    fn partial_history_setup(t: &mut Terminal) -> ScreenSearch {
        let list = list(t);

        // Write multiple "Fizz" matches that will end up in history.
        // We need enough content to push "Fizz" entries into scrollback.
        next_slice(t, b"Fizz\r\nFizz\r\n");
        while list.total_pages() < 3 {
            next_slice(t, b"\r\n");
        }
        for _ in 0..list.rows {
            next_slice(t, b"\r\n");
        }
        next_slice(t, b"Fizz.");

        // Complete initial search
        let mut search = init(t, "Fizz");
        search.search_all().unwrap();

        // Now trigger reloadActive by adding more content that changes the
        // active/history boundary. First add more "Fizz" entries to history.
        next_slice(t, b"\r\nFizz\r\nFizz\r\n");
        while list.total_pages() < 4 {
            next_slice(t, b"\r\n");
        }
        for _ in 0..list.rows {
            next_slice(t, b"\r\n");
        }

        search
    }

    #[test]
    fn reload_active_partial_history_cleanup_on_append_slice_error() {
        // This test verifies that when reloadActive fails at appendSlice (after
        // the loop), all FlattenedHighlight items are properly cleaned up.
        let mut t = history_terminal();
        let mut search = partial_history_setup(&mut t);

        // Arm the tripwire to fail at appendSlice (after the loop completes).
        // At this point, there are FlattenedHighlight items in the results list
        // that need cleanup.
        let tw = &RELOAD_ACTIVE_TW;
        tw.error_always(ReloadActiveFailPoint::HistoryAppendExisting, AllocError::OutOfMemory);

        // reloadActive is called by select(), which should trigger the error path.
        assert_eq!(Err(AllocError::OutOfMemory), search.select(Select::Next));
        tw.end(ResetMode::Reset).unwrap();
        search.deinit();
    }

    #[test]
    fn reload_active_partial_history_cleanup_on_loop_append_error() {
        // This test verifies that when reloadActive fails inside the loop
        // (after some items have been appended), all FlattenedHighlight items
        // are properly cleaned up.
        let mut t = history_terminal();
        let mut search = partial_history_setup(&mut t);

        // Arm the tripwire to fail after the first loop append succeeds.
        // This leaves at least one FlattenedHighlight in the results list
        // that needs cleanup.
        let tw = &RELOAD_ACTIVE_TW;
        tw.error_after(ReloadActiveFailPoint::HistoryAppendNew, AllocError::OutOfMemory, 1);

        // reloadActive is called by select(), which should trigger the error path.
        assert_eq!(Err(AllocError::OutOfMemory), search.select(Select::Next));
        tw.end(ResetMode::Reset).unwrap();
        search.deinit();
    }

    #[test]
    fn select_after_clearing_scrollback() {
        // Regression test for: https://github.com/ghostty-org/ghostty/issues/11957
        // After clearing scrollback (CSI 3J), selecting next/prev should not crash.
        let mut t = history_terminal();
        let list = list(&mut t);

        // Write enough content to push matches into scrollback history.
        next_slice(&mut t, b"error\r\n");
        while list.total_pages() < 3 {
            next_slice(&mut t, b"error\r\n");
        }
        for _ in 0..list.rows {
            next_slice(&mut t, b"\r\n");
        }
        next_slice(&mut t, b"error.");

        // Start search and find all matches.
        let mut search = init(&mut t, "error");
        search.search_all().unwrap();

        // Should have matches in both history and active areas.
        assert!(!search.history_results.is_empty());
        assert!(!search.active_results.is_empty());

        // Select a match first (so we have a selection).
        let _ = search.select(Select::Next).unwrap();
        assert!(search.selected.is_some());

        // Clear scrollback (equivalent to CSI 3J / Cmd+K erasing scrollback).
        t.erase_display(EraseDisplay::Scrollback, false);

        // Selecting next/prev after clearing scrollback should not crash.
        // Before the fix, this would hit an assertion in trackPin because
        // the FlattenedHighlight contained dangling node pointers.
        let _ = search.select(Select::Next).unwrap();
        let _ = search.select(Select::Prev).unwrap();
        search.deinit();
    }

    fn set_cells(page: &mut crate::terminal::page::Page, y: usize, text: &str) {
        for (x, c) in text.bytes().enumerate() {
            // SAFETY: in-bounds cell of a detached page.
            unsafe { *page.get_row_and_cell(x, y).cell = Cell::init(c as u32) };
        }
    }

    #[test]
    fn feed_after_complete_discovers_prepended_history_pages() {
        let mut t = history_terminal();
        let list = list(&mut t);

        // The screen begins with the tail of the needle so a restored page which
        // soft-wraps into it can complete a match across the restore boundary.
        next_slice(&mut t, b"st\r\nTest\r\n");
        while list.total_pages() < 2 {
            next_slice(&mut t, b"\r\n");
        }
        for _ in 0..list.rows {
            next_slice(&mut t, b"\r\n");
        }
        next_slice(&mut t, b"Test");
        let old_first = list.pages.first.unwrap();

        // Search to completion, then select the oldest match so we can verify
        // that restored (older) results never move an existing selection.
        let mut search = init(&mut t, "Test");
        search.search_all().unwrap();
        assert!(search.state.is_complete());
        assert_eq!(1, search.active_results.len());
        assert_eq!(1, search.history_results.len());
        assert!(search.select(Select::Prev).unwrap());
        let selected_idx = search.selected.unwrap().idx;
        assert_eq!(1, selected_idx);
        let selected_before = search.selected_match().unwrap().untracked();
        assert_eq!(old_first, selected_before.start.node);

        // Restore the newest history page: a full match on its first row and a
        // soft-wrapped last row ending in "Te" which joins the screen's "st".
        {
            let mut allocation = list.allocate_page(Capacity::new(10, 2)).unwrap();
            let page = allocation.page();
            page.size.rows = 2;
            set_cells(page, 0, "Test");
            set_cells(page, 1, "xxxxxxxxTe");
            // SAFETY: in-bounds rows.
            unsafe {
                (*page.get_row(1)).set_wrap(true);
                (*n(old_first).page().get_row(0)).set_wrap_continuation(true);
            }
            allocation.finalize(Location::Prepend, FinalizeOptions::default()).unwrap();
        }
        let newest = list.pages.first.unwrap();

        // The completed search is fed like any periodic refresh. It must pick
        // the page up and go back to searching rather than staying complete.
        search.feed().unwrap();
        assert!(!search.state.is_complete());
        search.search_all().unwrap();
        assert_eq!(1, search.active_results.len());
        assert_eq!(3, search.history_results.len());

        // New results are older than everything cached, so they append in
        // newest-to-oldest order: the boundary match first, then the row above.
        {
            let boundary = search.history_results[1].untracked();
            assert_eq!(newest, boundary.start.node);
            assert_eq!(old_first, boundary.end.node);
            assert_eq!(1 as CellCountInt, boundary.start.y);
            assert_eq!(8 as CellCountInt, boundary.start.x);
            assert_eq!(0 as CellCountInt, boundary.end.y);
            assert_eq!(1 as CellCountInt, boundary.end.x);
            let str = t
                .screens
                .active
                .selection_string(SelectionString::new(Selection::init(boundary.start, boundary.end, false)));
            assert_eq!("Test", str);
        }
        {
            let oldest = search.history_results[2].untracked();
            assert_eq!(newest, oldest.start.node);
            assert_eq!(0 as CellCountInt, oldest.start.y);
            assert_eq!(0 as CellCountInt, oldest.start.x);
        }

        // The selection kept both its index and its target.
        assert_eq!(selected_idx, search.selected.unwrap().idx);
        {
            let selected_after = search.selected_match().unwrap().untracked();
            assert!(selected_before.start.eql(selected_after.start));
            assert!(selected_before.end.eql(selected_after.end));
        }

        // Restore an even older page. Each incremental step is discovered from
        // the frontier left behind by the previous one.
        {
            let mut allocation = list.allocate_page(Capacity::new(10, 2)).unwrap();
            let page = allocation.page();
            page.size.rows = 2;
            set_cells(page, 0, "Test");
            allocation.finalize(Location::Prepend, FinalizeOptions::default()).unwrap();
        }
        let oldest_node = list.pages.first.unwrap();
        search.feed().unwrap();
        search.search_all().unwrap();
        assert_eq!(4, search.history_results.len());
        assert_eq!(oldest_node, search.history_results[3].untracked().start.node);
        assert_eq!(selected_idx, search.selected.unwrap().idx);
        {
            let selected_after = search.selected_match().unwrap().untracked();
            assert!(selected_before.start.eql(selected_after.start));
            assert!(selected_before.end.eql(selected_after.end));
        }

        // With nothing new, a feed on the complete search stays complete.
        search.feed().unwrap();
        assert!(search.state.is_complete());
        assert_eq!(5, search.matches_len());
        search.deinit();
    }
}
