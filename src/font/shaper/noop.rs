//! Port of `src/font/shaper/noop.zig`.
//!
//! STATUS: ported

use crate::font::collection::GetFaceError;
use crate::font::shape::{self, Cell, RunOptions, TextRun};
use crate::font::shaper::run::{self, RunIterator};

#[allow(dead_code)]
const LOG: &str = "font_shaper";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Codepoint {
    codepoint: u32,
    cluster: u32,
}

#[derive(Debug, Default)]
struct RunState {
    codepoints: Vec<Codepoint>,
}

impl RunState {
    fn reset(&mut self) {
        self.codepoints.clear();
    }
}

/// Shaper that doesn't do any shaping. Each individual codepoint is mapped
/// directly to the detected text run font's glyph index.
#[derive(Debug, Default)]
pub struct Shaper {
    /// The string used for shaping the current run.
    run_state: RunState,

    /// The shared memory used for shaping results.
    cell_buf: Vec<Cell>,
}

impl Shaper {
    /// The cell_buf argument is the buffer to use for storing shaped results.
    /// This should be at least the number of columns in the terminal.
    pub fn init(opts: shape::Options<'_>) -> Result<Shaper, std::convert::Infallible> {
        let _ = opts;

        Ok(Shaper {
            cell_buf: Vec::new(),
            run_state: RunState::default(),
        })
    }

    pub fn end_frame(&self) {}

    pub fn run_iterator<'a>(&'a mut self, opts: RunOptions<'a>) -> RunIterator<'a, RunIteratorHook<'a>> {
        RunIterator::new(RunIteratorHook { shaper: self }, opts)
    }

    pub fn shape(&mut self, run: TextRun<'_>) -> Result<&[Cell], GetFaceError> {
        let state = &self.run_state;

        // Special fonts aren't shaped and their codepoint == glyph so we
        // can just return the codepoints as-is.
        if run.font_index.special().is_some() {
            self.cell_buf.clear();
            self.cell_buf.reserve(state.codepoints.len());
            for entry in &state.codepoints {
                self.cell_buf.push(Cell {
                    x: entry.cluster as u16,
                    glyph_index: entry.codepoint,
                    ..Default::default()
                });
            }

            return Ok(&self.cell_buf);
        }

        // Go through the run and map each codepoint to a glyph index.
        self.cell_buf.clear();

        // Note: this is digging into some internal details, we should maybe
        // expose a public API for this.
        let mut grid_state = run.grid.lock.write().unwrap_or_else(|e| e.into_inner());
        let face = grid_state.resolver.collection.get_face(run.font_index)?;
        for entry in &state.codepoints {
            let Some(glyph_index) = face.glyph_index(entry.codepoint) else {
                // The run iterator shared logic should guarantee that
                // there is a glyph index for all codepoints in the run.
                // This is not well tested because we don't use the noop
                // shaper in any release builds.
                unreachable!();
            };
            self.cell_buf.push(Cell {
                x: entry.cluster as u16,
                glyph_index,
                ..Default::default()
            });
        }

        Ok(&self.cell_buf)
    }
}

/// The hooks for RunIterator.
pub struct RunIteratorHook<'a> {
    pub shaper: &'a mut Shaper,
}

impl run::RunIteratorHook for RunIteratorHook<'_> {
    fn prepare(&mut self) {
        self.shaper.run_state.reset();
    }

    fn add_codepoint(&mut self, cp: u32, cluster: u32) {
        self.shaper.run_state.codepoints.push(Codepoint {
            codepoint: cp,
            cluster,
        });
    }

    fn finalize(&mut self) {}
}
