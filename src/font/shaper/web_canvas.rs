//! Port of `src/font/shaper/web_canvas.zig`.
//!
//! STATUS: ported (wasm32-only; not compiled on Linux, so this has never
//! been compiled.)
//!
//! Port notes: the Zig file predates several refactors (it reads
//! `opts.cell_buf`, which `shape.Options` no longer has, uses the removed
//! `run.group.group.faceFromIndex` and the old `Terminal`/`GroupCache`
//! APIs in `Wasm.shaper_test`). This port keeps the logic: the cell buffer
//! is passed to `init` explicitly, the face is fetched through the run's
//! shared grid, and `shaper_test` is written against the current APIs.

#![cfg(target_arch = "wasm32")]

use crate::font::collection::GetFaceError;
use crate::font::face::web_canvas::Error as FaceError;
use crate::font::shape::{self, Cell, RunOptions, TextRun};
use crate::font::shaper::run::{self, RunIterator};
use crate::quirks::inline_assert as assert;
use crate::unicode;

const LOG: &str = "font_shaper";

/// One entry of the run buffer (Zig `std.MultiArrayList(struct { ... })`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunBufEntry {
    /// The codepoint for this cell. This must be used in conjunction
    /// with cluster to find the total set of codepoints for a given
    /// cell. See cluster for more information.
    codepoint: u32,

    /// Cluster is set to the X value of the cell that this codepoint
    /// is part of. Note that a cell can have multiple codepoints
    /// with zero-width joiners (ZWJ) and such. Note that terminals
    /// do NOT handle full extended grapheme clustering well so it
    /// is possible a single grapheme extends multiple clusters.
    /// For example, skin tone emoji thumbs up may show up as two
    /// clusters: one with thumbs up and the ZWJ, and a second
    /// cluster with the tone block. It is up to the shaper to handle
    /// shaping these together into a single glyph, if it wishes.
    cluster: u32,
}

/// Errors from [`Shaper::shape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeError {
    GetFace(GetFaceError),
    Face(FaceError),
    InvalidCodepoint,
}

impl std::fmt::Display for ShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for ShapeError {}

pub struct Shaper {
    /// The shared memory used for shaping results.
    pub cell_buf: Vec<Cell>,

    /// The shared memory used for storing information about a run.
    run_buf: Vec<RunBufEntry>,
}

impl Shaper {
    /// The cell_buf argument is the buffer to use for storing shaped results.
    /// This should be at least the number of columns in the terminal.
    pub fn init(cell_buf: Vec<Cell>, opts: shape::Options<'_>) -> Result<Shaper, std::convert::Infallible> {
        // Note: we do not support opts.font_features
        let _ = opts;

        Ok(Shaper {
            cell_buf,
            run_buf: Vec::new(),
        })
    }

    pub fn end_frame(&self) {}

    /// Returns an iterator that returns one text run at a time for the
    /// given terminal row. Note that text runs are only valid one at a time
    /// for a Shaper struct since they share state.
    pub fn run_iterator<'a>(&'a mut self, opts: RunOptions<'a>) -> RunIterator<'a, RunIteratorHook<'a>> {
        RunIterator::new(RunIteratorHook { shaper: self }, opts)
    }

    /// Shape the given text run. The text run must be the immediately
    /// previous text run that was iterated since the text run does share
    /// state with the Shaper struct.
    ///
    /// The return value is only valid until the next shape call is called.
    ///
    /// If there is not enough space in the cell buffer, an error is
    /// returned.
    pub fn shape(&mut self, run: TextRun<'_>) -> Result<&[Cell], ShapeError> {
        // TODO: memory check that cell_buf can fit results

        let codepoints: Vec<u32> = self.run_buf.iter().map(|e| e.codepoint).collect();
        let clusters: Vec<u32> = self.run_buf.iter().map(|e| e.cluster).collect();
        assert(codepoints.len() == clusters.len());

        match codepoints.len() {
            // Special cases: if we have no codepoints (is this possible?)
            // then our result is also an empty cell run.
            0 => return Ok(&self.cell_buf[0..0]),

            // If we have only 1 codepoint, then we assume that it is
            // a single grapheme and just let it through. At this point,
            // we can't have any more information to do anything else.
            1 => {
                self.cell_buf[0] = Cell {
                    x: clusters[0] as u16,
                    glyph_index: codepoints[0],
                    ..Default::default()
                };

                return Ok(&self.cell_buf[0..1]);
            }

            _ => {}
        }

        // We know we have at least two codepoints, so we now go through
        // each and perform grapheme clustering.
        //
        // Note that due to limitations of canvas, we can NOT support
        // font ligatures. However, we do support grapheme clustering.
        // This means we can render things like skin tone emoji but
        // we can't render things like single glyph "=>".
        let mut break_state = unicode::uucode::grapheme::BreakState::default();
        let mut cp1: u32 = codepoints[0];

        let mut start: usize = 0;
        let mut i: usize = 1;
        let mut cur: usize = 0;
        while i <= codepoints.len() {
            // We loop to codepoints.len so that we can handle the end
            // case. In the end case, we always assume it is a grapheme
            // break. This isn't strictly true but its how terminals
            // work today.
            let grapheme_break = i == codepoints.len() || {
                let cp2: u32 = codepoints[i];
                let v = unicode::grapheme_break(cp1, cp2, &mut break_state);
                cp1 = cp2;
                v
            };

            // If this is NOT a grapheme break, cp2 is part of a single
            // grapheme cluster and we expect there could be more. We
            // move on to the next codepoint to try again.
            if !grapheme_break {
                i += 1;
                continue;
            }

            // This IS a grapheme break, meaning that cp2 is NOT part
            // of cp1. So we need to render the prior grapheme.
            let len = i - start;
            assert(len > 0);
            match len {
                // If we have only a single codepoint then just render it
                // as-is.
                1 => {
                    self.cell_buf[cur] = Cell {
                        x: clusters[start] as u16,
                        glyph_index: codepoints[start],
                        ..Default::default()
                    }
                }

                // We must have multiple codepoints (see assert above). In
                // this case we UTF-8 encode the codepoints and send them
                // to the face to reserve a private glyph index.
                _ => {
                    // UTF-8 encode the codepoints in this cluster.
                    let cluster: String = {
                        let cluster_points = &codepoints[start..i];
                        assert(cluster_points.len() == len);

                        let mut buf = String::new();
                        for &cp in cluster_points {
                            buf.push(char::from_u32(cp).ok_or(ShapeError::InvalidCodepoint)?);
                        }

                        buf
                    };

                    let mut state = run.grid.lock.write().unwrap_or_else(|e| e.into_inner());
                    let face = state
                        .resolver
                        .collection
                        .get_face(run.font_index)
                        .map_err(ShapeError::GetFace)?;
                    let index = face
                        .grapheme_glyph_index(cluster.as_bytes())
                        .map_err(ShapeError::Face)?;

                    self.cell_buf[cur] = Cell {
                        x: clusters[start] as u16,
                        glyph_index: index,
                        ..Default::default()
                    };
                }
            }

            start = i;
            cur += 1;
            i += 1;
        }

        Ok(&self.cell_buf[0..cur])
    }
}

/// The hooks for RunIterator.
pub struct RunIteratorHook<'a> {
    pub shaper: &'a mut Shaper,
}

impl run::RunIteratorHook for RunIteratorHook<'_> {
    fn prepare(&mut self) {
        // Reset the buffer for our current run
        self.shaper.run_buf.clear();
    }

    fn add_codepoint(&mut self, cp: u32, cluster: u32) {
        self.shaper.run_buf.push(RunBufEntry { codepoint: cp, cluster });
    }

    fn finalize(&mut self) {}
}

/// The wasm-compatible API.
pub mod wasm {
    use super::*;
    use crate::font::shared_grid::SharedGrid;

    #[no_mangle]
    pub extern "C" fn shaper_new(cap: usize) -> *mut Shaper {
        let cell_buf = vec![Cell::default(); cap];
        match Shaper::init(cell_buf, shape::Options::default()) {
            Ok(shaper) => Box::into_raw(Box::new(shaper)),
            Err(_) => std::ptr::null_mut(),
        }
    }

    #[no_mangle]
    pub extern "C" fn shaper_free(ptr: *mut Shaper) {
        if !ptr.is_null() {
            // SAFETY: the pointer was created by `shaper_new`.
            drop(unsafe { Box::from_raw(ptr) });
        }
    }

    /// Runs a test to verify shaping works properly.
    ///
    /// The Zig version takes a `*font.GroupCache` (a type that no longer
    /// exists); this takes the shared grid instead.
    #[no_mangle]
    pub extern "C" fn shaper_test(this: *mut Shaper, grid: *const SharedGrid, str: *const u8, len: usize) {
        // SAFETY: the host passes valid pointers.
        let (this, grid, s) = unsafe { (&mut *this, &*grid, std::slice::from_raw_parts(str, len)) };
        if let Err(err) = shaper_test_(this, grid, s) {
            log::warn!(target: LOG, "error during shaper test err={}", err);
        }
    }

    fn shaper_test_(this: &mut Shaper, grid: &SharedGrid, s: &[u8]) -> Result<(), String> {
        // Create a "terminal" row and print all our characters into it.
        // TODO(port-terminal): the Zig version prints into a
        // `terminal.Terminal` and iterates its rows; until the terminal is
        // ported this builds a single row of narrow cells directly.
        let s = std::str::from_utf8(s).map_err(|e| e.to_string())?;
        let mut cells = crate::terminal::render::Cells::default();
        for c in s.chars() {
            cells.append(crate::terminal::render::Cell {
                raw: crate::terminal::page::Cell::init(c as u32),
                ..Default::default()
            });
        }

        // Iterate over the rows and print out all the runs we get.
        let y: usize = 0;
        let mut iter = this.run_iterator(RunOptions {
            cells: cells.slice(),
            ..RunOptions::new(grid)
        });
        while let Some(run) = iter.next().map_err(|e| e.to_string())? {
            let run_cells = run.cells;
            let font_index = run.font_index;
            let shaped = iter.hooks.shaper.shape(run).map_err(|e| e.to_string())?;
            log::info!(
                target: LOG,
                "y={} run={} shape={:?} idx={:?}",
                y,
                run_cells,
                shaped,
                font_index,
            );
        }

        Ok(())
    }
}
