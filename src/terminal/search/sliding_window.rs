//! Port of `src/terminal/search/sliding_window.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - The allocator is kept (as the copyable [`Allocator`] handle from
//!   `page_list`): it decodes compressed pages and, via
//!   [`Allocator::check`], models the allocations Zig makes through it so
//!   tests can inject failures.
//! - `MetaBuf` is a `CircBuf<Meta>` whose (Zig: `undefined`) default value
//!   holds a dangling node pointer; it is never dereferenced.
//! - Zig's `next()` returns a flattened highlight whose chunks are a view
//!   into the window-owned `chunk_buf`. Here `next()` returns an owned
//!   [`FlattenedHighlight`] (a copy of `chunk_buf`), so callers don't need
//!   to `clone` it to retain it.
//! - `std.ascii.findIgnoreCase` is [`find_ignore_case`] (ASCII-only case
//!   folding, Boyer-Moore-Horspool like the Zig standard library).
//! - `deinit` is `Drop`.

use core::ptr::NonNull;

use crate::datastruct::circ_buf::{self, CircBuf};
use crate::terminal::formatter::{Options as FormatterOptions, PageFormatter, PointMap};
use crate::terminal::highlight::{Flattened as FlattenedHighlight, FlattenedChunk};
use crate::terminal::page::Page;
use crate::terminal::page_list::{Allocator, NodePtr};
use crate::terminal::point;
use crate::terminal::size::{AllocError, CellCountInt};

/// Searches page nodes via a sliding window. The sliding window maintains
/// the invariant that data isn't pruned until (1) we've searched it and
/// (2) we've accounted for overlaps across pages to fit the needle.
///
/// The sliding window is first initialized empty. Pages are then appended
/// in the order to search them. The sliding window supports both a forward
/// and reverse order specified via `init`. The pages should be appended
/// in the correct order matching the search direction.
///
/// All appends grow the window. The window is only pruned when a search
/// is done (positive or negative match) via `next()`.
///
/// To avoid unnecessary memory growth, the recommended usage is to
/// call `next()` until it returns null and then `append` the next page
/// and repeat the process. This will always maintain the minimum
/// required memory to search for the needle.
///
/// The caller is responsible for providing the pages and ensuring they're
/// in the proper order. The SlidingWindow itself doesn't own the pages, but
/// it will contain pointers to them in order to return selections. If any
/// pages become invalid, the caller should clear the sliding window and
/// start over.
#[derive(Debug)]
pub struct SlidingWindow {
    /// The allocator to use for all the data within this window. We
    /// store this rather than passing it around because its already
    /// part of multiple elements (eg. Meta's CellMap) and we want to
    /// ensure we always use a consistent allocator. Additionally, only
    /// a small amount of sliding windows are expected to be in use
    /// at any one time so the memory overhead isn't that large.
    pub alloc: Allocator,

    /// The data buffer is a circular buffer of u8 that contains the
    /// encoded page text that we can use to search for the needle.
    pub data: DataBuf,

    /// The meta buffer is a circular buffer that contains the metadata
    /// about the pages we're searching. This usually isn't that large
    /// so callers must iterate through it to find the offset to map
    /// data to meta.
    pub meta: MetaBuf,

    /// Buffer that can fit any amount of chunks necessary for next
    /// to never fail allocation.
    chunk_buf: Vec<FlattenedChunk>,

    /// Offset into data for our current state. This handles the
    /// situation where our search moved through meta[0] but didn't
    /// do enough to prune it.
    pub data_offset: usize,

    /// The needle we're searching for. Does own the memory.
    pub needle: Vec<u8>,

    /// The search direction. If the direction is forward then pages should
    /// be appended in forward linked list order from the PageList. If the
    /// direction is reverse then pages should be appended in reverse order.
    ///
    /// This is important because in most cases, a reverse search is going
    /// to be more desirable to search from the end of the active area
    /// backwards so more recent data is found first. Supporting both is
    /// trivial though and will let us do more complex optimizations in the
    /// future (e.g. starting from the viewport and doing a forward/reverse
    /// concurrently from that point).
    pub direction: Direction,

    /// A buffer to store the overlap search data. This is used to search
    /// overlaps between pages where the match starts on one page and
    /// ends on another. The length is always `needle.len * 2`.
    overlap_buf: Vec<u8>,
}

/// (Zig: `SlidingWindow.Direction`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Reverse,
}

/// (Zig: `SlidingWindow.DataBuf`.)
pub type DataBuf = CircBuf<u8>;

/// (Zig: `SlidingWindow.MetaBuf`.)
pub type MetaBuf = CircBuf<Meta>;

/// (Zig: `SlidingWindow.Meta`.)
#[derive(Debug, Clone)]
pub struct Meta {
    pub node: NodePtr,
    pub serial: u64,
    pub rows: CellCountInt,
    pub cell_map: Vec<point::Coordinate>,
}

impl Meta {
    /// The default value for unused circular buffer slots (Zig:
    /// `undefined`). The node pointer is dangling and never dereferenced.
    fn undefined() -> Meta {
        Meta {
            node: NonNull::dangling(),
            serial: 0,
            rows: 0,
            cell_map: Vec::new(),
        }
    }
}

/// Information copied from a page while appending it to the window.
/// (Zig: `SlidingWindow.AppendResult`.)
///
/// Both values remain safe after the node's preserved page is released.
/// In particular, callers use `last_row_wrapped` to decide whether an
/// adjacent page can contribute to a cross-page match without reading the
/// node again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendResult {
    pub content_len: usize,
    pub last_row_wrapped: bool,
}

impl SlidingWindow {
    pub fn init(alloc: Allocator, direction: Direction, needle_unowned: &[u8]) -> Result<SlidingWindow, AllocError> {
        let data = DataBuf::init(0, 0);
        let meta = MetaBuf::init(0, Meta::undefined());

        // Zig allocates the needle copy and overlap buffer here.
        alloc.check()?;
        let mut needle = needle_unowned.to_vec();
        match direction {
            Direction::Forward => {}
            Direction::Reverse => needle.reverse(),
        }

        let overlap_buf = vec![0u8; needle.len() * 2];

        Ok(SlidingWindow {
            alloc,
            data,
            meta,
            chunk_buf: Vec::new(),
            data_offset: 0,
            needle,
            direction,
            overlap_buf,
        })
    }

    /// Clear all data but retain allocated capacity.
    pub fn clear_and_retain_capacity(&mut self) {
        self.meta.clear();
        self.data.clear();
        self.data_offset = 0;
    }

    /// Search the window for the next occurrence of the needle. As
    /// the window moves, the window will prune itself while maintaining
    /// the invariant that the window is always big enough to contain
    /// the needle.
    ///
    /// This returns a flattened highlight on a match. The
    /// flattened highlight requires allocation and is therefore more expensive
    /// than a normal selection, but it is more efficient to render since it
    /// has all the information without having to dereference pointers into
    /// the terminal state.
    ///
    /// In Zig the flattened highlight chunks reference internal memory for
    /// this sliding window; here the returned highlight is owned.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<FlattenedHighlight> {
        // An empty needle represents an inactive search. Searching for it
        // would otherwise produce a zero-length match, which highlight()
        // cannot represent because its end offset is inclusive.
        if self.needle.is_empty() {
            return None;
        }

        // If we have less data then the needle then we can't possibly match
        let data_len = self.data.len();
        if data_len < self.needle.len() {
            return None;
        }

        let found: Option<usize> = 'found: {
            let [s0, s1] = self.data.get_ptr_slice(self.data_offset, data_len - self.data_offset);
            let (s0, s1): (&[u8], &[u8]) = (s0, s1);
            let needle = &self.needle[..];

            // Search the first slice for the needle.
            if let Some(idx) = find_ignore_case(s0, needle) {
                break 'found Some(idx);
            }

            // Search the overlap buffer for the needle.
            if !s0.is_empty() && !s1.is_empty() {
                // Get up to needle.len - 1 bytes from each side (as much as
                // we can) and store it in the overlap buffer.
                let prefix: &[u8] = {
                    let len = s0.len().min(needle.len() - 1);
                    let idx = s0.len() - len;
                    &s0[idx..]
                };
                let suffix: &[u8] = {
                    let len = s1.len().min(needle.len() - 1);
                    &s1[..len]
                };
                let overlap_len = prefix.len() + suffix.len();
                debug_assert!(overlap_len <= self.overlap_buf.len());
                self.overlap_buf[..prefix.len()].copy_from_slice(prefix);
                self.overlap_buf[prefix.len()..overlap_len].copy_from_slice(suffix);

                // Search the overlap
                if let Some(idx) = find_ignore_case(&self.overlap_buf[..overlap_len], needle) {
                    // We found a match in the overlap buffer. We need to map the
                    // index back to the data buffer in order to get our selection.
                    break 'found Some(s0.len() - prefix.len() + idx);
                }
            }

            // Search the last slice for the needle.
            if let Some(idx) = find_ignore_case(s1, needle) {
                break 'found Some(s0.len() + idx);
            }

            None
        };

        if let Some(idx) = found {
            let len = self.needle.len();
            return Some(self.highlight(idx, len));
        }

        // Special case 1-lengthed needles to delete the entire buffer.
        if self.needle.len() == 1 {
            self.clear_and_retain_capacity();
            self.assert_integrity();
            return None;
        }

        // No match. We keep `needle.len - 1` bytes available to
        // handle the future overlap case.
        'prune: {
            let mut saved: usize = 0;
            // Zig: `meta_it.idx`, the number of metas consumed.
            let mut consumed: usize = 0;
            let mut found_meta = false;
            for meta in self.meta.iterator(circ_buf::Direction::Reverse) {
                consumed += 1;
                let needed = self.needle.len() - 1 - saved;
                if meta.cell_map.len() >= needed {
                    // We save up to this meta. We set our data offset
                    // to exactly where it needs to be to continue
                    // searching.
                    self.data_offset = meta.cell_map.len() - needed;
                    found_meta = true;
                    break;
                }

                saved += meta.cell_map.len();
            }
            if !found_meta {
                // If we exited the while loop naturally then we
                // never got the amount we needed and so there is
                // nothing to prune.
                debug_assert!(saved < self.needle.len() - 1);
                break 'prune;
            }

            let prune_count = self.meta.len() - consumed;
            if prune_count == 0 {
                // This can happen if we need to save up to the first
                // meta value to retain our window.
                break 'prune;
            }

            // We can now delete all the metas up to but NOT including
            // the meta we found through meta_it.
            let prune_data_len: usize = self
                .meta
                .iterator(circ_buf::Direction::Forward)
                .take(prune_count)
                .map(|meta| meta.cell_map.len())
                .sum();
            self.meta.delete_oldest(prune_count);
            self.data.delete_oldest(prune_data_len);
        }

        // Our data offset now moves to needle.len - 1 from the end so
        // that we can handle the overlap case.
        self.data_offset = self.data.len() - self.needle.len() + 1;

        self.assert_integrity();
        None
    }

    /// Return a flattened highlight for the given start and length.
    ///
    /// The flattened highlight can be used to render the highlight
    /// in the most efficient way because it doesn't require a terminal
    /// lock to access terminal data to compare whether some viewport
    /// matches the highlight (because it doesn't need to traverse
    /// the page nodes).
    ///
    /// The start index is assumed to be relative to the offset. i.e.
    /// index zero is actually at `self.data[self.data_offset]`. The
    /// selection will account for the offset.
    fn highlight(&mut self, start_offset: usize, len: usize) -> FlattenedHighlight {
        let start = start_offset + self.data_offset;
        let end = start + len - 1;
        debug_assert!(start < self.data.len());
        debug_assert!(start + len <= self.data.len());

        // Clear our previous chunk buffer to store this result
        self.chunk_buf.clear();
        let mut result = FlattenedHighlight::EMPTY;

        // A reverse cross-page match needs the row count of the last meta in
        // search order after the chunks themselves are reversed. Snapshot it
        // while traversing Meta rather than dereferencing a PageList node after
        // the terminal lock has been released.
        let mut cross_page_end_rows: Option<CellCountInt> = None;

        // Go through the meta nodes to find our start.
        //
        // `br`: if non-null, we need to continue searching for the
        // bottom-right from this meta index with this consumed count.
        // `prune_meta`/`prune_data`: data to prune, both are lengths.
        let (br, prune_meta, prune_data): (Option<(usize, usize)>, usize, usize) = 'tl: {
            let mut meta_consumed: usize = 0;
            for (idx, meta) in self.meta.iterator(circ_buf::Direction::Forward).enumerate() {
                // Always increment our consumed count so that our index
                // is right for the end search if we do it.
                let prior_meta_consumed = meta_consumed;
                meta_consumed += meta.cell_map.len();

                // meta_i is the index we expect to find the match in the
                // cell map within this meta if it contains it.
                let meta_i = start - prior_meta_consumed;

                // This meta doesn't contain the match. This means we
                // can also prune this set of data because we only look
                // forward.
                if meta_i >= meta.cell_map.len() {
                    continue;
                }

                // Now we look for the end. In MOST cases it is the same as
                // our starting chunk because highlights are usually small and
                // not on a boundary, so let's optimize for that.
                let end_i = end - prior_meta_consumed;
                if end_i < meta.cell_map.len() {
                    // The entire highlight is within this meta.
                    let start_map = meta.cell_map[meta_i];
                    let end_map = meta.cell_map[end_i];
                    result.top_x = start_map.x;
                    result.bot_x = end_map.x;
                    self.chunk_buf.push(FlattenedChunk {
                        node: meta.node,
                        serial: meta.serial,
                        start: start_map.y as CellCountInt,
                        end: (end_map.y + 1) as CellCountInt,
                    });

                    break 'tl (None, idx, prior_meta_consumed);
                } else {
                    // We found the meta that contains the start of the match
                    // only. Consume this entire node from our start offset.
                    let map = meta.cell_map[meta_i];
                    result.top_x = map.x;
                    self.chunk_buf.push(FlattenedChunk {
                        node: meta.node,
                        serial: meta.serial,
                        start: map.y as CellCountInt,
                        end: meta.rows,
                    });

                    break 'tl (Some((idx + 1, meta_consumed)), idx, prior_meta_consumed);
                }
            }

            // Precondition that the start index is within the data buffer.
            unreachable!()
        };

        // Search for our end.
        if let Some((next_idx, consumed)) = br {
            let mut meta_consumed: usize = consumed;
            let mut found = false;
            for meta in self.meta.iterator(circ_buf::Direction::Forward).skip(next_idx) {
                // meta_i is the index we expect to find the match in the
                // cell map within this meta if it contains it.
                let meta_i = end - meta_consumed;
                if meta_i >= meta.cell_map.len() {
                    // This meta doesn't contain the match. We still add it
                    // to our results because we want the full flattened list.
                    self.chunk_buf.push(FlattenedChunk {
                        node: meta.node,
                        serial: meta.serial,
                        start: 0,
                        end: meta.rows,
                    });

                    meta_consumed += meta.cell_map.len();
                    continue;
                }

                // We found it
                let map = meta.cell_map[meta_i];
                result.bot_x = map.x;
                cross_page_end_rows = Some(meta.rows);
                self.chunk_buf.push(FlattenedChunk {
                    node: meta.node,
                    serial: meta.serial,
                    start: 0,
                    end: (map.y + 1) as CellCountInt,
                });
                found = true;
                break;
            }

            // Precondition that the end index is within the data buffer.
            if !found {
                unreachable!();
            }
        }

        // Our offset into the current meta block is the start index
        // minus the amount of data fully consumed. We then add one
        // to move one past the match so we don't repeat it.
        self.data_offset = start - prune_data + 1;

        // If we went beyond our initial meta node we can prune.
        if prune_meta > 0 {
            // Free all our memory in the meta blocks prior to our
            // match (delete_oldest resets the slots).
            if cfg!(debug_assertions) {
                let meta = self
                    .meta
                    .iterator(circ_buf::Direction::Forward)
                    .nth(prune_meta)
                    .unwrap();
                debug_assert!(meta.node == self.chunk_buf[0].node);
            }
            self.meta.delete_oldest(prune_meta);

            // Delete all the data up to our current index.
            debug_assert!(prune_data > 0);
            self.data.delete_oldest(prune_data);
        }

        match self.direction {
            Direction::Forward => {}
            Direction::Reverse => {
                let chunks = &mut self.chunk_buf;
                if chunks.len() > 1 {
                    // Reverse all our chunks. This should be pretty obvious why.
                    chunks.reverse();

                    // Now normally with forward traversal with multiple pages,
                    // the suffix of the first page and the prefix of the last
                    // page are used.
                    //
                    // For a reverse traversal, this is inverted (since the
                    // pages are in reverse order we get the suffix of the last
                    // page and the prefix of the first page). So we need to
                    // invert this.
                    //
                    // We DON'T need to do this for any middle pages because
                    // they always use the full page.
                    //
                    // This is a fixup that makes our start/end match the
                    // same logic as the loops above if they were in forward
                    // order.
                    let n = chunks.len();
                    debug_assert!(n >= 2);
                    chunks[0].start = chunks[0].end - 1;
                    chunks[0].end = cross_page_end_rows.unwrap();
                    chunks[n - 1].end = chunks[n - 1].start + 1;
                    chunks[n - 1].start = 0;
                } else {
                    // For a single chunk, the y values are in reverse order
                    // (start is the screen-end, end is the screen-start).
                    // Swap them to get proper top-to-bottom order.
                    let start_y = chunks[0].start;
                    chunks[0].start = chunks[0].end - 1;
                    chunks[0].end = start_y + 1;
                }

                // X values also need to be reversed since the top/bottom
                // are swapped for the nodes.
                core::mem::swap(&mut result.top_x, &mut result.bot_x);
            }
        }

        // Copy over our chunk list.
        result.chunks = self.chunk_buf.clone();
        result
    }

    /// Add a new node to the sliding window. This will always grow
    /// the sliding window; data isn't pruned until it is consumed
    /// via a search (via next()).
    ///
    /// Returns the number of bytes of content added to the sliding window.
    /// The total bytes will be larger since this omits metadata, but it is
    /// an accurate measure of the text content size added.
    ///
    /// The node must be live (see the `page_list` module notes).
    pub fn append(&mut self, node: NodePtr) -> Result<AppendResult, AllocError> {
        // SAFETY: caller guarantees the node is live.
        let node_ref = unsafe { node.as_ref() };
        let mut preserved = node_ref.page_preserving_state(self.alloc)?;

        let page = preserved.page();
        // SAFETY: the row is within the page.
        let last_row_wrapped = unsafe { (*page.get_row(page.size.rows as usize - 1)).wrap() };
        let result = self.append_page(node, page, last_row_wrapped);
        preserved.deinit();
        result
    }

    /// Append a node only when its last row is soft wrapped.
    ///
    /// This acquires one preserved page for both the wrap check and formatting
    /// so a compressed node is decoded at most once. It is used when loading
    /// the overlap around an otherwise complete search region.
    pub fn append_if_wrapped(&mut self, node: NodePtr) -> Result<Option<AppendResult>, AllocError> {
        // SAFETY: caller guarantees the node is live.
        let node_ref = unsafe { node.as_ref() };
        let mut preserved = node_ref.page_preserving_state(self.alloc)?;

        let page = preserved.page();
        // SAFETY: the row is within the page.
        let last_row_wrapped = unsafe { (*page.get_row(page.size.rows as usize - 1)).wrap() };
        let result = if !last_row_wrapped {
            Ok(None)
        } else {
            self.append_page(node, page, last_row_wrapped).map(Some)
        };
        preserved.deinit();
        result
    }

    /// Copy one preserved page into the window's owned search buffers.
    ///
    /// No pointer into `page` may escape this function: compressed preserved
    /// pages own temporary decode storage, while resident values borrow node
    /// memory.
    fn append_page(&mut self, node: NodePtr, page: &Page, last_row_wrapped: bool) -> Result<AppendResult, AllocError> {
        // Initialize our metadata for the node.
        let mut meta = Meta {
            node,
            // SAFETY: caller guarantees the node is live.
            serial: unsafe { node.as_ref().serial },
            rows: page.size.rows,
            cell_map: Vec::new(),
        };

        // This is suboptimal but we need to encode the page once to
        // temporary memory, and then copy it into our circular buffer.
        // In the future, we should benchmark and see if we can encode
        // directly into the circular buffer.
        self.alloc.check()?;
        let mut encoded: Vec<u8> = Vec::new();

        // Encode the page into the buffer.
        {
            let mut formatter = PageFormatter::init(
                page,
                FormatterOptions {
                    unwrap: true,
                    ..FormatterOptions::PLAIN
                },
            );
            formatter.point_map = Some(PointMap {
                map: &mut meta.cell_map,
                base: 0,
            });
            if formatter.format(&mut encoded).is_err() {
                // writer uses anyerror but the only realistic error on
                // an ArrayList is out of memory.
                return Err(AllocError::OutOfMemory);
            }
        }
        debug_assert_eq!(meta.cell_map.len(), encoded.len());

        // If the node we're adding isn't soft-wrapped, we add the
        // trailing newline.
        if !last_row_wrapped {
            encoded.push(b'\n');
            let last = meta
                .cell_map
                .last()
                .copied()
                .unwrap_or(point::Coordinate { x: 0, y: 0 });
            meta.cell_map.push(last);
        }

        // If our written data is empty, then there is nothing to
        // add to our data set.
        if encoded.is_empty() {
            self.assert_integrity();
            return Ok(AppendResult {
                content_len: 0,
                last_row_wrapped,
            });
        }

        // Get our written data. If we're doing a reverse search then we
        // need to reverse all our encodings.
        match self.direction {
            Direction::Forward => {}
            Direction::Reverse => {
                encoded.reverse();
                meta.cell_map.reverse();
            }
        }

        // Ensure our buffers are big enough to store what we need.
        self.data.ensure_unused_capacity(encoded.len());
        self.meta.ensure_unused_capacity(1);
        self.chunk_buf.reserve(self.meta.capacity().saturating_sub(self.chunk_buf.len()));

        // Append our new node to the circular buffer.
        self.data.append_slice_assume_capacity_copy(&encoded);
        self.meta.append_assume_capacity(meta);

        self.assert_integrity();
        Ok(AppendResult {
            content_len: encoded.len(),
            last_row_wrapped,
        })
    }

    /// Only for tests!
    #[cfg(test)]
    fn test_change_needle(&mut self, new: &[u8]) {
        assert_eq!(new.len(), self.needle.len());
        self.needle = new.to_vec();
    }

    fn assert_integrity(&self) {
        if !cfg!(debug_assertions) {
            return;
        }

        // Integrity check: verify our data matches our metadata exactly.
        let data_len: usize = self
            .meta
            .iterator(circ_buf::Direction::Forward)
            .map(|m| m.cell_map.len())
            .sum();
        assert_eq!(data_len, self.data.len());

        // Integrity check: verify our data offset is within bounds.
        assert!(self.data.len() == 0 || self.data_offset < self.data.len());
    }
}

/// Find the first index of `needle` in `haystack`, ignoring ASCII case
/// (Zig: `std.ascii.findIgnoreCase`). Uses Boyer-Moore-Horspool with a
/// case-folded skip table.
pub fn find_ignore_case(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let n = needle.len();
    if n > haystack.len() {
        return None;
    }
    if n == 0 {
        return Some(0);
    }

    let mut skip = [n; 256];
    for (i, &c) in needle[..n - 1].iter().enumerate() {
        skip[c.to_ascii_lowercase() as usize] = n - 1 - i;
    }

    let mut i = 0;
    while i <= haystack.len() - n {
        if haystack[i..i + n].eq_ignore_ascii_case(needle) {
            return Some(i);
        }
        i += skip[haystack[i + n - 1].to_ascii_lowercase() as usize];
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::page_list::Pin;
    use crate::terminal::point::{Point, Tag};
    use crate::terminal::screen::{Options as ScreenOptions, Screen};
    use crate::terminal::search::testing::{active, next_slice, terminal_default};

    fn screen(max_scrollback_bytes: usize) -> Screen {
        Screen::init(ScreenOptions {
            cols: 80,
            rows: 24,
            max_scrollback_bytes: Some(max_scrollback_bytes),
            ..Default::default()
        })
        .unwrap()
    }

    fn window(direction: Direction, needle: &[u8]) -> SlidingWindow {
        SlidingWindow::init(Allocator::GLOBAL, direction, needle).unwrap()
    }

    fn pt(s: &Screen, p: Pin) -> Option<Point> {
        s.pages.point_from_pin(Tag::Active, p)
    }

    fn first(s: &Screen) -> NodePtr {
        s.pages.pages.first.unwrap()
    }

    fn single_page(s: &Screen) -> bool {
        s.pages.pages.first == s.pages.pages.last
    }

    fn next_node(node: NodePtr) -> NodePtr {
        // SAFETY: live test node.
        unsafe { node.as_ref().next.unwrap() }
    }

    /// Fill up the first page so that its final bytes are `tail`, then
    /// write `sep` and `rest` (which must end up on a new page).
    fn fill_first_page(s: &mut Screen, tail: &str, sep: &str, rest: &str) {
        // SAFETY: live test node.
        let first_page_rows = unsafe { first(s).as_ref().capacity().rows };
        for _ in 0..first_page_rows - 1 {
            s.test_write_string("\n").unwrap();
        }
        for _ in 0..s.pages.cols as usize - tail.len() {
            s.test_write_string("x").unwrap();
        }
        s.test_write_string(tail).unwrap();
        assert!(single_page(s));
        s.test_write_string(sep).unwrap();
        if !sep.is_empty() {
            assert!(!single_page(s));
        }
        s.test_write_string(rest).unwrap();
        assert!(!single_page(s));
    }

    fn check(s: &Screen, h: FlattenedHighlight, start: (u16, u32), end: (u16, u32)) {
        let sel = h.untracked();
        assert_eq!(active(start.0, start.1), pt(s, sel.start));
        assert_eq!(active(end.0, end.1), pt(s, sel.end));
    }

    fn set_last_row_wrap(node: NodePtr) {
        // SAFETY: live test node with exclusive access.
        unsafe {
            let n = &mut *node.as_ptr();
            let rows = n.rows() as usize;
            (*n.page().get_row(rows - 1)).set_wrap(true);
        }
    }

    fn assert_wrapped(w: &mut SlidingWindow, wrapped: bool) {
        let len = w.data.len();
        let [a, b] = w.data.get_ptr_slice(0, len);
        assert!(!a.is_empty());
        assert_eq!(wrapped, !b.is_empty());
    }

    #[test]
    fn sliding_window_empty_on_init() {
        let w = window(Direction::Forward, b"boo!");
        assert_eq!(0, w.data.len());
        assert_eq!(0, w.meta.len());
    }

    #[test]
    fn sliding_window_empty_needle_has_no_matches() {
        let mut w = window(Direction::Forward, b"");
        let mut s = screen(0);
        s.test_write_string("hello").unwrap();
        w.append(first(&s)).unwrap();
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_single_append() {
        let mut w = window(Direction::Forward, b"boo!");
        let mut s = screen(0);
        s.test_write_string("hello. boo! hello. boo!").unwrap();

        // We want to test single-page cases.
        assert!(single_page(&s));
        w.append(first(&s)).unwrap();

        // We should be able to find two matches.
        check(&s, w.next().unwrap(), (7, 0), (10, 0));
        check(&s, w.next().unwrap(), (19, 0), (22, 0));
        assert!(w.next().is_none());
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_single_append_case_insensitive_ascii() {
        let mut w = window(Direction::Forward, b"Boo!");
        let mut s = screen(0);
        s.test_write_string("hello. boo! hello. boo!").unwrap();

        assert!(single_page(&s));
        w.append(first(&s)).unwrap();

        check(&s, w.next().unwrap(), (7, 0), (10, 0));
        check(&s, w.next().unwrap(), (19, 0), (22, 0));
        assert!(w.next().is_none());
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_single_append_single_char() {
        let mut w = window(Direction::Forward, b"b");
        let mut s = screen(0);
        s.test_write_string("hello. boo! hello. boo!").unwrap();

        assert!(single_page(&s));
        w.append(first(&s)).unwrap();

        check(&s, w.next().unwrap(), (7, 0), (7, 0));
        check(&s, w.next().unwrap(), (19, 0), (19, 0));
        assert!(w.next().is_none());
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_single_append_no_match() {
        let mut w = window(Direction::Forward, b"nope!");
        let mut s = screen(0);
        s.test_write_string("hello. boo! hello. boo!").unwrap();

        assert!(single_page(&s));
        w.append(first(&s)).unwrap();

        // No matches
        assert!(w.next().is_none());
        assert!(w.next().is_none());

        // Should still keep the page
        assert_eq!(1, w.meta.len());
    }

    #[test]
    fn sliding_window_two_pages() {
        let mut w = window(Direction::Forward, b"boo!");
        let mut s = screen(1000);

        // Fill up the first page. The final bytes in the first page
        // are "boo!"
        fill_first_page(&mut s, "boo!", "\n", "hello. boo!");

        // Add both pages
        let node = first(&s);
        w.append(node).unwrap();
        w.append(next_node(node)).unwrap();

        // Search should find two matches
        check(&s, w.next().unwrap(), (76, 22), (79, 22));
        check(&s, w.next().unwrap(), (7, 23), (10, 23));
        assert!(w.next().is_none());
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_two_pages_single_char() {
        let mut w = window(Direction::Forward, b"b");
        let mut s = screen(1000);
        fill_first_page(&mut s, "boo!", "\n", "hello. boo!");

        let node = first(&s);
        w.append(node).unwrap();
        w.append(next_node(node)).unwrap();

        check(&s, w.next().unwrap(), (76, 22), (76, 22));
        check(&s, w.next().unwrap(), (7, 23), (7, 23));
        assert!(w.next().is_none());
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_two_pages_match_across_boundary() {
        let mut w = window(Direction::Forward, b"hello, world");
        let mut s = screen(1000);
        fill_first_page(&mut s, "hell", "", "o, world!");

        let node = first(&s);
        w.append(node).unwrap();
        w.append(next_node(node)).unwrap();

        // Search should find a match
        check(&s, w.next().unwrap(), (76, 22), (7, 23));
        assert!(w.next().is_none());
        assert!(w.next().is_none());

        // We shouldn't prune because we don't have enough space
        assert_eq!(2, w.meta.len());
    }

    #[test]
    fn sliding_window_two_pages_no_match_across_boundary_with_newline() {
        let mut w = window(Direction::Forward, b"hello, world");
        let mut s = screen(1000);
        fill_first_page(&mut s, "hell", "", "\no, world!");

        let node = first(&s);
        w.append(node).unwrap();
        w.append(next_node(node)).unwrap();

        // Search should NOT find a match
        assert!(w.next().is_none());
        assert!(w.next().is_none());

        // We shouldn't prune because we don't have enough space
        assert_eq!(2, w.meta.len());
    }

    #[test]
    fn sliding_window_two_pages_no_match_across_boundary_with_newline_reverse() {
        let mut w = window(Direction::Reverse, b"hello, world");
        let mut s = screen(1000);
        fill_first_page(&mut s, "hell", "", "\no, world!");

        // Add both pages in reverse order
        let node = first(&s);
        w.append(next_node(node)).unwrap();
        w.append(node).unwrap();

        // Search should NOT find a match
        assert!(w.next().is_none());
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_two_pages_no_match_prunes_first_page() {
        let mut w = window(Direction::Forward, b"nope!");
        let mut s = screen(1000);
        fill_first_page(&mut s, "boo!", "\n", "hello. boo!");

        let node = first(&s);
        w.append(node).unwrap();
        w.append(next_node(node)).unwrap();

        // Search should find nothing
        assert!(w.next().is_none());
        assert!(w.next().is_none());

        // We should've pruned our page because the second page
        // has enough text to contain our needle.
        assert_eq!(1, w.meta.len());
    }

    #[test]
    fn sliding_window_two_pages_no_match_keeps_both_pages() {
        let mut s = screen(1000);
        fill_first_page(&mut s, "boo!", "\n", "hello. boo!");

        // Imaginary needle for search. Doesn't match!
        // SAFETY: live test node.
        let first_page_rows = unsafe { first(&s).as_ref().capacity().rows } as usize;
        let needle = vec![b'x'; first_page_rows * s.pages.cols as usize];
        let mut w = window(Direction::Forward, &needle);

        let node = first(&s);
        w.append(node).unwrap();
        w.append(next_node(node)).unwrap();

        // Search should find nothing
        assert!(w.next().is_none());
        assert!(w.next().is_none());

        // No pruning because both pages are needed to fit needle.
        assert_eq!(2, w.meta.len());
    }

    #[test]
    fn sliding_window_single_append_across_circular_buffer_boundary() {
        let mut w = window(Direction::Forward, b"abc");
        let mut s = screen(0);
        s.test_write_string("XXXXXXXXXXXXXXXXXXXboo!XXXXX").unwrap();

        // We are trying to break a circular buffer boundary so the way we
        // do this is to duplicate the data then do a failing search. This
        // will cause the first page to be pruned. The next time we append we'll
        // put it in the middle of the circ buffer. We assert this so that if
        // our implementation changes our test will fail.
        assert!(single_page(&s));
        let node = first(&s);
        w.append(node).unwrap();
        w.append(node).unwrap();
        // No wrap around yet
        assert_wrapped(&mut w, false);

        // Search non-match, prunes page
        assert!(w.next().is_none());
        assert_eq!(1, w.meta.len());

        // Change the needle, just needs to be the same length (not a real API)
        w.test_change_needle(b"boo");

        // Add new page, now wraps
        w.append(node).unwrap();
        assert_wrapped(&mut w, true);
        check(&s, w.next().unwrap(), (19, 0), (21, 0));
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_single_append_match_on_boundary() {
        let mut w = window(Direction::Forward, b"abcd");
        let mut s = screen(0);
        s.test_write_string("o!XXXXXXXXXXXXXXXXXXXbo").unwrap();

        // We need to surgically modify the last row to be soft-wrapped
        assert!(single_page(&s));
        let node = first(&s);
        set_last_row_wrap(node);

        w.append(node).unwrap();
        w.append(node).unwrap();
        // No wrap around yet
        assert_wrapped(&mut w, false);

        // Search non-match, prunes page
        assert!(w.next().is_none());
        assert_eq!(1, w.meta.len());

        // Change the needle, just needs to be the same length (not a real API)
        w.test_change_needle(b"boo!");

        // Add new page, now wraps
        w.append(node).unwrap();
        assert_wrapped(&mut w, true);
        check(&s, w.next().unwrap(), (21, 0), (1, 0));
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_single_append_reversed() {
        let mut w = window(Direction::Reverse, b"boo!");
        let mut s = screen(0);
        s.test_write_string("hello. boo! hello. boo!").unwrap();

        assert!(single_page(&s));
        w.append(first(&s)).unwrap();

        // We should be able to find two matches.
        check(&s, w.next().unwrap(), (19, 0), (22, 0));
        check(&s, w.next().unwrap(), (7, 0), (10, 0));
        assert!(w.next().is_none());
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_single_append_no_match_reversed() {
        let mut w = window(Direction::Reverse, b"nope!");
        let mut s = screen(0);
        s.test_write_string("hello. boo! hello. boo!").unwrap();

        assert!(single_page(&s));
        w.append(first(&s)).unwrap();

        // No matches
        assert!(w.next().is_none());
        assert!(w.next().is_none());

        // Should still keep the page
        assert_eq!(1, w.meta.len());
    }

    #[test]
    fn sliding_window_two_pages_reversed() {
        let mut w = window(Direction::Reverse, b"boo!");
        let mut s = screen(1000);
        fill_first_page(&mut s, "boo!", "\n", "hello. boo!");

        // Add both pages in reverse order
        let node = first(&s);
        w.append(next_node(node)).unwrap();
        w.append(node).unwrap();

        // Search should find two matches (in reverse order)
        check(&s, w.next().unwrap(), (7, 23), (10, 23));
        check(&s, w.next().unwrap(), (76, 22), (79, 22));
        assert!(w.next().is_none());
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_two_pages_match_across_boundary_reversed() {
        let mut w = window(Direction::Reverse, b"hello, world");
        let mut s = screen(1000);

        // Fill up the first page. The final bytes in the first page
        // are "hell"
        fill_first_page(&mut s, "hell", "", "o, world!");

        // Add both pages in reverse order
        let node = first(&s);
        w.append(next_node(node)).unwrap();
        w.append(node).unwrap();

        // Search should find a match
        {
            let h = w.next().unwrap();
            assert_eq!(2, h.chunks.len());
            assert_eq!(node, h.chunks[0].node);
            // SAFETY: live test nodes.
            unsafe {
                assert_eq!(node.as_ref().serial, h.chunks[0].serial);
                assert_eq!(next_node(node), h.chunks[1].node);
                assert_eq!(next_node(node).as_ref().serial, h.chunks[1].serial);
            }
            check(&s, h, (76, 22), (7, 23));
        }
        assert!(w.next().is_none());
        assert!(w.next().is_none());

        // In reverse mode, the last appended meta (first original page) is large
        // enough to contain needle.len - 1 bytes, so pruning occurs
        assert_eq!(1, w.meta.len());
    }

    #[test]
    fn sliding_window_two_pages_no_match_prunes_first_page_reversed() {
        let mut w = window(Direction::Reverse, b"nope!");
        let mut s = screen(1000);
        fill_first_page(&mut s, "boo!", "\n", "hello. boo!");

        // Add both pages in reverse order
        let node = first(&s);
        w.append(next_node(node)).unwrap();
        w.append(node).unwrap();

        // Search should find nothing
        assert!(w.next().is_none());
        assert!(w.next().is_none());

        // We should've pruned our page because the second page
        // has enough text to contain our needle.
        assert_eq!(1, w.meta.len());
    }

    #[test]
    fn sliding_window_two_pages_no_match_keeps_both_pages_reversed() {
        let mut s = screen(1000);
        fill_first_page(&mut s, "boo!", "\n", "hello. boo!");

        // Imaginary needle for search. Doesn't match!
        // SAFETY: live test node.
        let first_page_rows = unsafe { first(&s).as_ref().capacity().rows } as usize;
        let needle = vec![b'x'; first_page_rows * s.pages.cols as usize];
        let mut w = window(Direction::Reverse, &needle);

        // Add both pages in reverse order
        let node = first(&s);
        w.append(next_node(node)).unwrap();
        w.append(node).unwrap();

        // Search should find nothing
        assert!(w.next().is_none());
        assert!(w.next().is_none());

        // No pruning because both pages are needed to fit needle.
        assert_eq!(2, w.meta.len());
    }

    #[test]
    fn sliding_window_single_append_across_circular_buffer_boundary_reversed() {
        let mut w = window(Direction::Reverse, b"abc");
        let mut s = screen(0);
        s.test_write_string("XXXXXXXXXXXXXXXXXXXboo!XXXXX").unwrap();

        assert!(single_page(&s));
        let node = first(&s);
        w.append(node).unwrap();
        w.append(node).unwrap();
        // No wrap around yet
        assert_wrapped(&mut w, false);

        // Search non-match, prunes page
        assert!(w.next().is_none());
        assert_eq!(1, w.meta.len());

        // Change the needle, just needs to be the same length (not a real API)
        // testChangeNeedle doesn't reverse, so pass reversed needle for reverse mode
        w.test_change_needle(b"oob");

        // Add new page, now wraps
        w.append(node).unwrap();
        assert_wrapped(&mut w, true);
        check(&s, w.next().unwrap(), (19, 0), (21, 0));
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_single_append_match_on_boundary_reversed() {
        let mut w = window(Direction::Reverse, b"abcd");
        let mut s = screen(0);
        s.test_write_string("o!XXXXXXXXXXXXXXXXXXXbo").unwrap();

        // We need to surgically modify the last row to be soft-wrapped
        assert!(single_page(&s));
        let node = first(&s);
        set_last_row_wrap(node);

        w.append(node).unwrap();
        w.append(node).unwrap();
        // No wrap around yet
        assert_wrapped(&mut w, false);

        // Search non-match, prunes page
        assert!(w.next().is_none());
        assert_eq!(1, w.meta.len());

        // Change the needle, just needs to be the same length (not a real API)
        // testChangeNeedle doesn't reverse, so pass reversed needle for reverse mode
        w.test_change_needle(b"!oob");

        // Add new page, now wraps
        w.append(node).unwrap();
        assert_wrapped(&mut w, true);
        check(&s, w.next().unwrap(), (21, 0), (1, 0));
        assert!(w.next().is_none());
    }

    fn soft_wrapped(direction: Direction) {
        let mut w = window(direction, b"boo!");
        let mut t = terminal_default(4, 5);
        next_slice(&mut t, b"A\r\nxxboo!\r\nC");

        // We want to test single-page cases.
        let screen: &Screen = &t.screens.active;
        assert!(single_page(screen));
        w.append(first(screen)).unwrap();

        // We should be able to find a match.
        check(screen, w.next().unwrap(), (2, 1), (1, 2));
        assert!(w.next().is_none());
        assert!(w.next().is_none());
    }

    #[test]
    fn sliding_window_single_append_soft_wrapped() {
        soft_wrapped(Direction::Forward);
    }

    #[test]
    fn sliding_window_single_append_reversed_soft_wrapped() {
        soft_wrapped(Direction::Reverse);
    }

    // This tests a real bug that occurred where a whitespace-only page
    // that encodes to zero bytes would crash.
    #[test]
    fn sliding_window_append_whitespace_only_node() {
        let mut w = window(Direction::Forward, b"x");
        let s = screen(0);

        // By setting the empty page to wrap we get a zero-byte page.
        // This is invasive but its otherwise hard to reproduce naturally
        // without creating a slow test.
        let node = first(&s);
        set_last_row_wrap(node);

        assert!(single_page(&s));
        w.append(node).unwrap();

        // No matches expected
        assert!(w.next().is_none());
    }

    #[test]
    fn find_ignore_case_basic() {
        assert_eq!(Some(0), find_ignore_case(b"Hello", b"hello"));
        assert_eq!(Some(7), find_ignore_case(b"hello. BOO!", b"boo!"));
        assert_eq!(None, find_ignore_case(b"hello", b"world"));
        assert_eq!(None, find_ignore_case(b"he", b"hello"));
    }
}
