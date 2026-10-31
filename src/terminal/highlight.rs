//! Port of `src/terminal/highlight.zig`.
//!
//! STATUS: ported
//!
//! Highlights are any contiguous sequences of cells that should
//! be called out in some way, most commonly for text selection but
//! also search results or any other purpose.
//!
//! Within the terminal package, a highlight is a generic concept
//! that represents a range of cells.
//!
//! Port notes:
//! - `Tracked.init`/`deinit` take the screen's `PageList` (Zig: `*Screen`,
//!   of which only `screen.pages` is used) so this module doesn't depend on
//!   the Screen port. Call them with `&mut screen.pages`.
//! - `Flattened.chunks` is a `Vec` of chunks (Zig: `std.MultiArrayList`).
//! - Zig's `Flattened.init` (never instantiated there) initializes a
//!   nonexistent `end_x` field; the port sets `bot_x = end.x`.

// NOTE: The plan is for highlights to ultimately replace Selection
// completely. Selection is deeply tied to various parts of the Ghostty
// internals so this may take some time.

use crate::terminal::page_list::{Direction, NodePtr, PageList, Pin, PinPtr};
use crate::terminal::size::{self, AllocError};

/// An untracked highlight is a highlight that stores its highlighted
/// area as a top-left and bottom-right screen pin. Since it is untracked,
/// the pins are only valid for the current terminal state and may not
/// be safe to use after any terminal modifications.
///
/// For rectangle highlights/selections, the downstream consumer of this
/// code is expected to interpret the pins in whatever shape they want.
/// For example, a rectangular selection would interpret the pins as
/// setting the x bounds for each row between start.y and end.y.
///
/// To simplify all operations, start MUST be before or equal to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Untracked {
    pub start: Pin,
    pub end: Pin,
}

impl Untracked {
    pub fn track(&self, pages: &mut PageList) -> Result<Tracked, AllocError> {
        Tracked::init(pages, self.start, self.end)
    }

    pub fn eql(&self, other: Untracked) -> bool {
        self.start.eql(other.start) && self.end.eql(other.end)
    }
}

/// A tracked highlight is a highlight that stores its highlighted
/// area as tracked pins within a screen.
///
/// A tracked highlight ensures that the pins remain valid even as
/// the terminal state changes. Because of this, tracked highlights
/// have more operations available to them.
///
/// There is more overhead to creating and maintaining tracked highlights.
/// If you're manipulating highlights that are untracked and you're sure
/// that the terminal state won't change, you can use the `initAssume`
/// function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tracked {
    pub start: PinPtr,
    pub end: PinPtr,
}

impl Tracked {
    pub fn init(pages: &mut PageList, start: Pin, end: Pin) -> Result<Tracked, AllocError> {
        let start_tracked = pages.track_pin(start)?;
        let end_tracked = match pages.track_pin(end) {
            Ok(p) => p,
            Err(e) => {
                pages.untrack_pin(start_tracked);
                return Err(e);
            }
        };
        Ok(Tracked {
            start: start_tracked,
            end: end_tracked,
        })
    }

    /// Initializes a tracked highlight by assuming that the provided
    /// pins are already tracked. This allows callers to perform tracked
    /// operations without the overhead of tracking the pins, if the
    /// caller can guarantee that the pins are already tracked or that
    /// the terminal state will not change.
    ///
    /// Do not call deinit on highlights created with this function.
    pub fn init_assume(start: PinPtr, end: PinPtr) -> Tracked {
        Tracked { start, end }
    }

    pub fn deinit(self, pages: &mut PageList) {
        pages.untrack_pin(self.start);
        pages.untrack_pin(self.end);
    }
}

/// A flattened highlight is a highlight that stores its highlighted
/// area as a list of page chunks. This representation allows for
/// traversing the entire highlighted area without needing to read any
/// terminal state or dereference any page nodes (which may have been
/// pruned).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Flattened {
    /// The page chunks that make up this highlight. This handles the
    /// y bounds since chunks[0].start is the first highlighted row
    /// and chunks[len - 1].end is the last highlighted row (exclsive).
    pub chunks: Vec<FlattenedChunk>,

    /// The x bounds of the highlight. `bot_x` may be less than `top_x`
    /// for typical left-to-right highlights: can start the selection right
    /// of the end on a higher row.
    pub top_x: size::CellCountInt,
    pub bot_x: size::CellCountInt,
}

/// A flattened chunk is almost identical to a PageList.Chunk but
/// we also flatten the serial number. This lets the flattened
/// highlight more robust for comparisons and validity checks with
/// the PageList. (Zig: `Flattened.Chunk`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlattenedChunk {
    pub node: NodePtr,
    pub serial: u64,
    pub start: size::CellCountInt,
    pub end: size::CellCountInt,
}

impl Flattened {
    pub const EMPTY: Flattened = Flattened {
        chunks: Vec::new(),
        top_x: 0,
        bot_x: 0,
    };

    pub fn init(start: Pin, end: Pin) -> Flattened {
        let mut result: Vec<FlattenedChunk> = Vec::new();
        let it = start.page_iterator(Direction::RightDown, Some(end));
        for chunk in it {
            result.push(FlattenedChunk {
                node: chunk.node,
                // SAFETY: chunk nodes are live list nodes.
                serial: unsafe { chunk.node.as_ref().serial },
                start: chunk.start,
                end: chunk.end,
            });
        }
        Flattened {
            chunks: result,
            top_x: start.x,
            bot_x: end.x,
        }
    }

    pub fn deinit(&mut self) {
        self.chunks = Vec::new();
    }

    pub fn start_pin(&self) -> Pin {
        let first = &self.chunks[0];
        Pin {
            x: self.top_x,
            y: first.start,
            ..Pin::new(first.node)
        }
    }

    pub fn end_pin(&self) -> Pin {
        let last = &self.chunks[self.chunks.len() - 1];
        Pin {
            x: self.bot_x,
            y: last.end - 1,
            ..Pin::new(last.node)
        }
    }

    /// Convert to an Untracked highlight.
    pub fn untracked(&self) -> Untracked {
        Untracked {
            start: self.start_pin(),
            end: self.end_pin(),
        }
    }
}

