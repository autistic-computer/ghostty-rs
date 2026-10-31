//! Port of `src/terminal/search/terminal.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - `std.EnumMap(ScreenSet.Key, T)` is a `[Option<T>; Key::COUNT]` array
//!   indexed by the key.
//! - `deinit` stays explicit because it needs the (possibly already
//!   destroyed) terminal.
//! - `viewportMatches` returns a slice borrowed from the search (Zig: valid
//!   until the next call/feed/deinit, which the borrow enforces).

use crate::terminal::highlight::Flattened as FlattenedHighlight;
use crate::terminal::page_list::{Allocator, Chunk, Direction as PageDirection};
use crate::terminal::point::{Coordinate, Point};
use crate::terminal::screen::Scroll;
use crate::terminal::screen_set::{Key as ScreenKey, ScreenSet};
use crate::terminal::search::screen::{ScreenSearch, Select, State as ScreenState, TickError};
use crate::terminal::search::viewport::ViewportSearch;
use crate::terminal::size::AllocError;
use crate::terminal::terminal::Terminal;

const LOG: &str = "search_terminal";

/// Searches for a needle within an entire Terminal, orchestrating a
/// ScreenSearch per live screen plus a viewport search for the active
/// screen.
///
/// This reconciles per-screen searchers against the terminal's active
/// ScreenSet, retains results across primary/alt screen switches, and
/// recovers from resize/reflow, resets, scrollback pruning, etc.
///
/// The main ownership contract is that this is safe to call concurrently
/// with Terminal IO as long as you're not calling a function that takes
/// a terminal as an argument. Each function has more specific details about
/// its behavior.
#[derive(Debug)]
pub struct TerminalSearch {
    /// Allocator used for all search state.
    pub alloc: Allocator,

    /// Active viewport search for the active screen.
    pub viewport: ViewportSearch,

    /// The searchers for all the screens.
    pub screens: [Option<ScreenSearch>; ScreenKey::COUNT],

    /// ScreenSet generations captured when each searcher was initialized.
    /// Allocators may reuse a destroyed Screen address, so pointer equality
    /// alone cannot distinguish replacement screens from stale handles.
    pub screen_generations: [Option<usize>; ScreenKey::COUNT],

    /// The active screen key as of the last feed. All single-screen
    /// reads (total matches, selection, etc.) resolve against this.
    pub active_key: ScreenKey,

    /// True when the cached viewport matches are stale and must be
    /// recollected from the viewport searcher on the next
    /// `viewportMatches` call.
    pub stale_viewport_matches: bool,

    /// Cached viewport matches. The viewport search's sliding window
    /// drains on read, so results are collected once per viewport
    /// change and cached here.
    pub viewport_matches: Vec<FlattenedHighlight>,
}

// SAFETY: the raw pointers inside are only dereferenced under the
// caller's synchronization (see the type docs), exactly like Zig.
unsafe impl Send for TerminalSearch {}

/// Overall status of the search. See `status`. (Zig: `TerminalSearch.Status`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// `tick` can make progress without terminal access.
    Running,

    /// Blocked until the next `feed`. This is also the initial
    /// state, since a search that has never been fed has never
    /// seen the terminal.
    FeedRequired,

    /// Caught up with the terminal state as of the last feed.
    Complete,
}

/// Viewport scroll behavior applied by `select` when a match
/// becomes selected. (Zig: `TerminalSearch.SelectScroll`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectScroll {
    /// Scroll so the match is visible, only if it is not already.
    IfNeeded,

    /// Never scroll.
    None,
}

/// (Zig: `TerminalSearch.Tick`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    /// All searches are complete.
    Complete,

    /// Progress was made on at least one screen.
    Progress,

    /// All incomplete searches are blocked on feed.
    Blocked,
}

impl TerminalSearch {
    /// Initialize a search for the given needle. The needle is copied.
    ///
    /// This doesn't read any terminal state. The first `feed` does.
    pub fn init(alloc: Allocator, needle_unowned: &[u8]) -> Result<TerminalSearch, AllocError> {
        let mut vp = ViewportSearch::init(alloc, needle_unowned)?;

        // We use dirty tracking for active area changes. Start with it
        // dirty so the first change is re-searched.
        vp.active_dirty = Some(true);

        Ok(TerminalSearch {
            alloc,
            viewport: vp,
            screens: [None, None],
            screen_generations: [None, None],
            active_key: ScreenKey::Primary,
            stale_viewport_matches: true,
            viewport_matches: Vec::new(),
        })
    }

    /// Release all state. The terminal must be the same one given to
    /// every other call, or null if the terminal has already been
    /// deinitialized. When the terminal is alive this releases tracked
    /// pins held within it. When it is null, those pins died with the
    /// terminal's page storage, so only search-owned memory is freed.
    pub fn deinit(&mut self, t_: Option<&Terminal>) {
        self.clear_viewport_matches();
        self.viewport_matches = Vec::new();
        self.viewport.reset();
        for key in ScreenKey::ALL {
            let Some(mut search) = self.screens[key as usize].take() else { continue };
            let valid = match t_ {
                Some(t) => self.screen_is_valid(&t.screens, key, &search),
                None => false,
            };
            if valid {
                search.deinit();
            } else {
                search.deinit_screen_invalid();
            }
            self.screen_generations[key as usize] = None;
        }
    }

    fn screen_is_valid(&self, screens: &ScreenSet, key: ScreenKey, search: &ScreenSearch) -> bool {
        let Some(generation) = self.screen_generations[key as usize] else { return false };
        if generation != screens.generation(key) {
            return false;
        }
        let Some(actual) = screens.get(key) else { return false };
        actual.as_ptr() == search.screen.as_ptr()
    }

    /// The needle that this search is using, borrowed.
    pub fn needle(&self) -> &[u8] {
        self.viewport.needle()
    }

    /// The searcher for the active screen as of the last feed. Null if
    /// the search has never been fed (or the screen failed to
    /// initialize).
    pub fn active_screen_search(&mut self) -> Option<&mut ScreenSearch> {
        self.screens[self.active_key as usize].as_mut()
    }

    /// Returns true if all searches on all screens are complete.
    pub fn is_complete(&self) -> bool {
        self.screens.iter().flatten().all(|s| s.state.is_complete())
    }

    /// The overall search status derived from the per-screen search
    /// states.
    ///
    /// Unlike `isComplete`, a search that has never been fed reports
    /// `feed_required` rather than `complete`. An empty screen map
    /// means the search has never seen the terminal, so reporting it
    /// as complete would be technically true but useless to a caller
    /// deciding what to do next.
    pub fn status(&self) -> Status {
        let mut saw_any = false;
        let mut result = Status::Complete;
        for search in self.screens.iter().flatten() {
            saw_any = true;
            match search.state {
                // Progress is possible without a feed.
                ScreenState::Active | ScreenState::History => return Status::Running,

                // Blocked until fed.
                ScreenState::HistoryFeed => result = Status::FeedRequired,

                ScreenState::Complete => {}
            }
        }

        if !saw_any {
            return Status::FeedRequired;
        }
        result
    }

    /// Tick the search forward as much as possible without reading
    /// any terminal state, so this is safe to call concurrently with
    /// terminal IO. Returns the overall tick progress.
    pub fn tick(&mut self) -> Tick {
        let mut result = Tick::Complete;
        for key in ScreenKey::ALL {
            let Some(search) = &mut self.screens[key as usize] else { continue };
            match search.tick() {
                Ok(()) => result = Tick::Progress,

                // Ignore... nothing we can do.
                Err(err @ TickError::OutOfMemory) => {
                    log::warn!(target: LOG, "error ticking screen search key={} err={err}", key.as_str());
                }

                // Ignore, good for us. State remains whatever it is.
                Err(TickError::SearchComplete) => {}

                // Ignore, too, progressed
                Err(TickError::FeedRequired) => match result {
                    // If we think we're complete, we're not because we're
                    // blocked now (nothing made progress).
                    Tick::Complete => result = Tick::Blocked,

                    // If we made some progress, we remain in progress
                    // since blocked means no progress at all.
                    Tick::Progress => {}

                    // If we're blocked already then we remain blocked.
                    Tick::Blocked => {}
                },
            }
        }

        result
    }

    /// Read the terminal to update any search state that requires it,
    /// such as reconciling screens, feeding more data to the
    /// searchers, and detecting viewport changes.
    ///
    /// This reads the terminal, so the caller must ensure the terminal
    /// isn't modified for the duration of this call (e.g. by holding a
    /// lock). Feeding is also the only way the search learns about
    /// terminal changes, so callers should feed periodically while a
    /// search is in use, even after it reports complete.
    ///
    /// `active_dirty` should be true when the active area may have
    /// changed since the last feed, which triggers a re-scan of the
    /// active area. Callers that know when the active area changes
    /// (e.g. Ghostty's renderer-maintained dirty flag) can pass false
    /// to skip the re-scan. Callers without that knowledge should
    /// always pass true. That is always correct, just slightly more
    /// work, and the active area search is cheap by design.
    pub fn feed(&mut self, t: &mut Terminal, active_dirty: bool) {
        let alloc = self.alloc;

        // Update our active screen
        if t.screens.active_key != self.active_key {
            self.active_key = t.screens.active_key;
        }

        // Reconcile our screens with the terminal screens. Remove
        // searchers for screens that no longer exist and add searchers
        // for screens that do exist but we don't have yet.
        {
            // Remove screens we have that no longer exist or changed.
            for key in ScreenKey::ALL {
                let Some(search) = &self.screens[key as usize] else { continue };
                let remove = !self.screen_is_valid(&t.screens, key, search);

                if remove {
                    let mut search = self.screens[key as usize].take().unwrap();
                    search.deinit_screen_invalid();
                    self.screen_generations[key as usize] = None;
                }
            }
        }
        {
            // Add screens that exist but we don't have yet.
            for key in ScreenKey::ALL {
                let Some(screen) = t.screens.all[key as usize] else { continue };
                if self.screens[key as usize].is_some() {
                    continue;
                }
                let screen_search = match ScreenSearch::init(alloc, screen.as_ptr(), self.viewport.needle()) {
                    Ok(s) => s,
                    Err(err) => {
                        // OOM is probably going to sink the entire ship but
                        // we can just ignore it and wait on the next
                        // reconciliation to try again.
                        log::warn!(
                            target: LOG,
                            "error initializing screen search for key={} err={err:?}",
                            key.as_str()
                        );
                        continue;
                    }
                };
                self.screens[key as usize] = Some(screen_search);
                self.screen_generations[key as usize] = Some(t.screens.generation(key));
            }
        }

        // The caller told us the active area may have changed, so mark
        // our viewport searcher dirty (forcing a re-search) and reload
        // the active area for the active screen.
        if active_dirty {
            // Mark our viewport dirty so it researches the active
            self.viewport.active_dirty = Some(true);

            // Reload our active area for our active screen
            let active_key = t.screens.active_key;
            if let Some(screen_search) = &mut self.screens[active_key as usize] {
                if let Err(err) = screen_search.reload_active() {
                    log::warn!(
                        target: LOG,
                        "error reloading active area for screen key={} err={err:?}",
                        active_key.as_str()
                    );
                }
            }
        }

        // Check our viewport for changes.
        match self.viewport.update(&t.screens.active.pages) {
            Ok(updated) => {
                if updated {
                    self.stale_viewport_matches = true;
                }
            }
            Err(err) => log::warn!(target: LOG, "error updating viewport search err={err:?}"),
        }

        // Feed data
        for key in ScreenKey::ALL {
            let Some(search) = &mut self.screens[key as usize] else { continue };
            if search.state.needs_feed() {
                if let Err(err) = search.feed() {
                    log::warn!(target: LOG, "error feeding screen search key={} err={err:?}", key.as_str());
                }
            }
        }
    }

    /// Return the matches on the pages covering the viewport, as of
    /// the last feed. Note this can include matches slightly outside
    /// the visible viewport when they share a page with it.
    ///
    /// The results are cached, since the underlying viewport search
    /// drains as it is read. Matches are collected once after a feed
    /// notices a viewport change and the cached results are returned
    /// otherwise. The returned slice is owned by the search and valid
    /// until the next call to this function, the next feed, or deinit.
    ///
    /// This doesn't read any terminal state, so it is safe to call
    /// concurrently with terminal IO.
    pub fn viewport_matches(&mut self) -> Result<&[FlattenedHighlight], AllocError> {
        if !self.stale_viewport_matches {
            return Ok(&self.viewport_matches);
        }

        // We always mark the cache fresh, even if collection fails
        // below: a failed collection isn't retried until the next feed
        // marks the cache stale again.
        self.stale_viewport_matches = false;

        self.clear_viewport_matches();
        while let Some(hl) = self.viewport.next() {
            if let Err(err) = self.alloc.check() {
                self.clear_viewport_matches();

                // Reset the viewport so we force an update (and therefore a
                // re-collection) on the next feed.
                self.viewport.reset();
                return Err(err);
            }
            self.viewport_matches.push(hl);
        }

        Ok(&self.viewport_matches)
    }

    fn clear_viewport_matches(&mut self) {
        self.viewport_matches.clear();
    }

    /// Select the next or previous search result on the active screen,
    /// wrapping around at the ends, and optionally scrolling the
    /// viewport so the newly selected match is visible.
    ///
    /// This feeds first so the selection always works against current
    /// terminal state, making it safe to call at any time relative to
    /// feeds. Like `feed`, this reads (and possibly scrolls) the
    /// terminal, so the caller must ensure the terminal isn't
    /// otherwise being used for the duration of this call.
    ///
    /// Returns true if a match is selected after the operation, or
    /// false if there are no matches to select.
    pub fn select(&mut self, t: &mut Terminal, to: Select, scroll: SelectScroll) -> Result<bool, AllocError> {
        // A screen can be removed or replaced between feeds. Reconcile
        // while holding the terminal lock before touching any
        // ScreenSearch pins.
        self.feed(t, false);
        let Some(screen_search) = &mut self.screens[self.active_key as usize] else {
            return Ok(false);
        };

        // Make the selection. Ignore the result because we don't
        // care if the selection didn't change.
        let _ = screen_search.select(to)?;

        // Grab our match if we have one. If we don't have a selection
        // then there was nothing to select.
        let Some(flattened) = screen_search.selected_match() else {
            return Ok(false);
        };

        match scroll {
            SelectScroll::None => return Ok(true),
            SelectScroll::IfNeeded => {}
        }

        // Grab the current screen and see if this match is visible within
        // the viewport already. If it is, we do nothing.
        let Some(mut screen) = t.screens.get(self.active_key) else {
            return Ok(true);
        };

        // Grab the viewport. Viewports and selections are usually small
        // so this check isn't very expensive, despite appearing O(N^2),
        // both Ns are usually equal to 1.
        let it = screen
            .pages
            .page_iterator(PageDirection::RightDown, Point::Viewport(Coordinate::default()), None);
        for chunk in it {
            for hl_chunk in &flattened.chunks {
                if chunk.overlaps(Chunk {
                    node: hl_chunk.node,
                    start: hl_chunk.start,
                    end: hl_chunk.end,
                }) {
                    return Ok(true);
                }
            }
        }

        screen.scroll(Scroll::Pin(flattened.start_pin()));
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::snapshot;
    use crate::terminal::snapshot::io::FixedReader;
    use crate::terminal::point::Tag;
    use crate::terminal::search::testing::{active, next_slice, terminal, terminal_default};

    /// Pump the search until complete.
    fn pump(search: &mut TerminalSearch, t: &mut Terminal) {
        while search.status() != Status::Complete {
            match search.status() {
                Status::FeedRequired => search.feed(t, true),
                Status::Running => {
                    let _ = search.tick();
                }
                Status::Complete => unreachable!(),
            }
        }
    }

    #[test]
    fn starts_feed_required_and_runs_to_complete() {
        let mut t = terminal_default(10, 2);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = TerminalSearch::init(Allocator::GLOBAL, b"Fizz").unwrap();

        // A fresh search has never seen the terminal, so it must report
        // feed_required even though isComplete() is trivially true.
        assert!(search.is_complete());
        assert_eq!(Status::FeedRequired, search.status());

        // Pump until complete.
        pump(&mut search, &mut t);

        let screen_search = search.active_screen_search().unwrap();
        assert_eq!(2, screen_search.matches_len());
        search.deinit(Some(&t));
    }

    #[test]
    fn viewport_matches_are_cached_until_the_next_feed() {
        let mut t = terminal_default(10, 10);
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz\r\nBang");

        let mut search = TerminalSearch::init(Allocator::GLOBAL, b"Fizz").unwrap();
        search.feed(&mut t, true);

        // The sliding window drains on read, so a second read must return
        // the cached results rather than an empty list.
        assert_eq!(2, search.viewport_matches().unwrap().len());
        assert_eq!(2, search.viewport_matches().unwrap().len());

        // A feed that observes a change refreshes the cache.
        next_slice(&mut t, b"\r\nFizz");
        search.feed(&mut t, true);
        assert_eq!(3, search.viewport_matches().unwrap().len());
        search.deinit(Some(&t));
    }

    #[test]
    fn select_after_active_screen_removal() {
        let mut t = terminal_default(20, 2);

        let _ = t.switch_screen(ScreenKey::Alternate).unwrap();

        let mut search = TerminalSearch::init(Allocator::GLOBAL, b"needle").unwrap();
        search.feed(&mut t, false);
        assert_eq!(ScreenKey::Alternate, search.active_key);
        assert!(search.screens[ScreenKey::Alternate as usize].is_some());

        let _ = t.switch_screen(ScreenKey::Primary).unwrap();
        t.screens.remove(ScreenKey::Alternate);

        // The select must reconcile against the live ScreenSet before
        // touching any pins from the removed screen.
        let _ = search.select(&mut t, Select::Next, SelectScroll::IfNeeded).unwrap();
        assert_eq!(ScreenKey::Primary, search.active_key);
        assert!(search.screens[ScreenKey::Alternate as usize].is_none());
        search.deinit(Some(&t));
    }

    #[test]
    fn select_scrolls_the_viewport_only_when_needed() {
        let mut t = terminal(10, 2, Some(usize::MAX));
        next_slice(&mut t, b"Fizz\r\n");
        for _ in 0..30 {
            next_slice(&mut t, b"\r\n");
        }
        next_slice(&mut t, b"Fizz");

        let mut search = TerminalSearch::init(Allocator::GLOBAL, b"Fizz").unwrap();
        pump(&mut search, &mut t);

        // Whether the selected match is within the visible viewport rows.
        // pointFromPin only bounds-checks the top of the region, so rows
        // below the viewport must be rejected by the row count.
        fn visible(term: &Terminal, s: &mut TerminalSearch) -> bool {
            let pin = s.active_screen_search().unwrap().selected_match().unwrap().start_pin();
            let pages = &term.screens.active.pages;
            let Some(pt) = pages.point_from_pin(Tag::Viewport, pin) else { return false };
            pt.coord().y < pages.rows as u32
        }

        // First match is at the bottom which is already visible: no scroll.
        assert!(search.select(&mut t, Select::Next, SelectScroll::IfNeeded).unwrap());
        {
            let pin = search.active_screen_search().unwrap().selected_match().unwrap().start_pin();
            assert_eq!(active(0, 1), t.screens.active.pages.point_from_pin(Tag::Active, pin));
        }
        assert!(visible(&t, &mut search));

        // Second match is in scrollback: selecting it must scroll the
        // viewport so it becomes visible.
        assert!(search.select(&mut t, Select::Next, SelectScroll::IfNeeded).unwrap());
        assert!(visible(&t, &mut search));

        // With scrolling disabled the viewport must stay where it is even
        // though the next selection (wrap back to the bottom) is not
        // visible.
        assert!(search.select(&mut t, Select::Next, SelectScroll::None).unwrap());
        assert!(!visible(&t, &mut search));
        search.deinit(Some(&t));
    }

    #[test]
    fn deinit_after_the_terminal_is_gone() {
        let mut t = terminal(10, 2, Some(usize::MAX));
        next_slice(&mut t, b"Fizz\r\nBuzz\r\nFizz");

        // Run to complete and select a match so the search holds tracked
        // pins within the terminal's page storage.
        let mut search = TerminalSearch::init(Allocator::GLOBAL, b"Fizz").unwrap();
        pump(&mut search, &mut t);
        assert!(search.select(&mut t, Select::Next, SelectScroll::None).unwrap());

        // Deinitialize the terminal first. The pins died with the page
        // storage, so deinit with a null terminal must free only
        // search-owned memory without touching the terminal.
        drop(t);
        search.deinit(None);
    }

    #[test]
    fn feed_after_complete_discovers_prepended_snapshot_history() {
        // A source terminal with several pages of scrollback where every line
        // is a match, so the expected total is simply the number of lines.
        let mut source = terminal(10, 2, Some(usize::MAX));
        let mut needle_count: usize = 0;
        while source.screens.active.pages.total_pages() < 4 {
            next_slice(&mut source, b"needle\r\n");
            needle_count += 1;
        }

        let mut encoded: Vec<u8> = Vec::new();
        snapshot::encode(
            &mut encoded,
            &source,
            snapshot::EncodeOptions {
                continuation: snapshot::Continuation::Ground,
            },
        )
        .unwrap();
        drop(source);

        // Restore only through READY. The terminal is usable while its history
        // pages are still in flight.
        let mut reader = FixedReader::new(&encoded);
        let mut decoder = snapshot::Decoder::init(&mut reader);
        let mut decoded = decoder
            .ready(&snapshot::DecodeOptions {
                max_continuation_bytes: 0,
                ..Default::default()
            })
            .unwrap();
        let mut t = decoded.to_owned();
        let pages_at_ready = t.screens.active.pages.total_pages();

        // Search the READY state to completion and select the oldest match.
        let mut search = TerminalSearch::init(Allocator::GLOBAL, b"needle").unwrap();
        pump(&mut search, &mut t);
        let partial = search.active_screen_search().unwrap().matches_len();
        assert!(partial > 0);
        assert!(partial < needle_count);
        assert!(search.select(&mut t, Select::Prev, SelectScroll::None).unwrap());
        let selected_idx = search.active_screen_search().unwrap().selected.unwrap().idx;
        assert_eq!(partial - 1, selected_idx);
        let selected_before = search
            .active_screen_search()
            .unwrap()
            .selected_match()
            .unwrap()
            .untracked();

        // Restore every history page below the live terminal.
        let mut restored_pages: usize = 0;
        while let Some(progress) = decoder.next(&mut t).unwrap() {
            assert!(progress.page().rows > 0);
            restored_pages += 1;
        }
        assert!(restored_pages > 0);
        assert_eq!(
            pages_at_ready + restored_pages,
            t.screens.active.pages.total_pages()
        );

        // Refresh the existing search the way ghostty_search_run does: feed,
        // then tick to completion. It must now cover the restored history.
        search.feed(&mut t, true);
        pump(&mut search, &mut t);
        let screen_search = search.active_screen_search().unwrap();
        assert_eq!(needle_count, screen_search.matches_len());

        // Restored results are older than everything already cached, so the
        // selection keeps both its index and its target.
        assert_eq!(selected_idx, screen_search.selected.unwrap().idx);
        let selected_after = screen_search.selected_match().unwrap().untracked();
        assert!(selected_before.start.eql(selected_after.start));
        assert!(selected_before.end.eql(selected_after.end));

        // Results stay ordered newest to oldest, ending at the very first line.
        let matches = screen_search.matches(Allocator::GLOBAL).unwrap();
        let pages = &t.screens.active.pages;
        let mut prev_y: Option<u32> = None;
        for hl in &matches {
            let pt = pages.point_from_pin(Tag::Screen, hl.start_pin()).unwrap();
            if let Some(y) = prev_y {
                assert!(pt.coord().y < y);
            }
            prev_y = Some(pt.coord().y);
        }
        assert_eq!(Some(0), prev_y);

        // A search created after the restore agrees with the refreshed one.
        let mut fresh = TerminalSearch::init(Allocator::GLOBAL, b"needle").unwrap();
        pump(&mut fresh, &mut t);
        assert_eq!(needle_count, fresh.active_screen_search().unwrap().matches_len());
        fresh.deinit(Some(&t));
        search.deinit(Some(&t));
    }

    #[test]
    fn no_matches_selects_nothing() {
        let mut t = terminal_default(10, 2);

        let mut search = TerminalSearch::init(Allocator::GLOBAL, b"Fizz").unwrap();
        search.feed(&mut t, true);
        assert!(!search.select(&mut t, Select::Next, SelectScroll::IfNeeded).unwrap());
        assert!(!search.select(&mut t, Select::Prev, SelectScroll::IfNeeded).unwrap());
        search.deinit(Some(&t));
    }
}
