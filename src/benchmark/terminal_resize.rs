//! Port of `src/benchmark/TerminalResize.zig`.
//!
//! STATUS: ported
//!
//! This benchmark tests the performance of Terminal.resize, with a
//! primary focus on column resizes that reflow soft-wrapped text.
//! Resize happens on the IO thread while holding the terminal lock,
//! so a slow resize directly translates into dropped input and a
//! frozen-feeling UI while the user drags the window edge (which
//! produces a rapid stream of resizes).
//!
//! The terminal is populated once during setup (synthetic fill and/or
//! a data file replayed through the VT stream) and then each step
//! ping-pongs the terminal between two sizes. A full cycle returns the
//! terminal to its original dimensions so the state reaches a steady
//! state after the first cycle and every iteration performs
//! equivalent work.

use std::any::Any;
use std::hint::black_box;
use std::io::BufReader;

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use super::options;
use crate::cli::args::{cli_enum, cli_struct};
use crate::synthetic::DefaultPrng;
use crate::terminal::modes::Mode as TerminalMode;
use crate::terminal::terminal::{CellSizePx, InitError, Options as TerminalOptions, Resize, Terminal};

const LOG: &str = "terminal-resize-bench";

pub struct TerminalResize {
    pub opts: Options,
    pub terminal: Terminal,
}

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// The resize pattern to benchmark. See Mode.
        pub mode: Mode = Mode::Cols,

        /// Multiplier on the number of resize cycles each step runs. This
        /// is useful to make a benchmark run long enough for profiling.
        pub loops: u32 = 1,

        /// The initial size of the terminal. This is also the size that
        /// every resize cycle returns to.
        pub terminal_rows as "terminal-rows": u16 = 80,
        pub terminal_cols as "terminal-cols": u16 = 120,

        /// The dimensions to resize to for the cols/rows/both modes. If
        /// unset, they default to half of the respective terminal
        /// dimension, which forces every soft-wrapped line to rewrap.
        pub resize_cols as "resize-cols": Option<u16> = None,
        pub resize_rows as "resize-rows": Option<u16> = None,

        /// The number of synthetic lines written to the terminal during
        /// setup. The content is deterministic: a mix of short lines,
        /// soft-wrapped long lines, blank lines, styled cells, and wide
        /// characters, since all of these hit different reflow paths.
        /// Set to 0 to only use `data`.
        pub fill_lines as "fill-lines": u32 = 10_000,

        /// The maximum scrollback size in bytes. Defaults to the Ghostty
        /// application default. Reflow cost scales with the amount of
        /// scrollback, not just the visible screen.
        pub scrollback_bytes as "scrollback-bytes": usize = 50_000_000,

        /// The data to read as a filepath. If this is "-" then
        /// we will read stdin. If this is unset, only the synthetic fill
        /// is used. The data is streamed into the terminal during setup
        /// (not part of the benchmark) to build the screen contents that
        /// get resized.
        pub data: Option<String> = None,
    }
}

cli_enum! {
    pub enum Mode {
        /// Resize to the current dimensions. This measures the early-exit
        /// path (mode updates, pixel geometry) and acts as a baseline.
        Noop => "noop",

        /// Alternate the column count between `terminal-cols` and
        /// `resize-cols`. With wraparound enabled (the default), this
        /// reflows text in both directions: shrinking wraps long lines
        /// and growing unwraps them. This is the primary reflow benchmark.
        Cols => "cols",

        /// Like `cols`, but with wraparound mode disabled so the resize
        /// does not reflow. Useful as a baseline to isolate the cost of
        /// reflow itself from the rest of the resize.
        ColsNoReflow => "cols-no-reflow",

        /// Alternate the row count between `terminal-rows` and
        /// `resize-rows`. Column count is unchanged so no text reflows;
        /// this measures growing/trimming rows against scrollback.
        Rows => "rows",

        /// Alternate both dimensions at once, like a diagonal window drag.
        Both => "both",
    }
}

impl BenchmarkImpl for TerminalResize {
    type Options = Options;
    type CreateError = InitError;

    fn create(opts: Options) -> Result<Box<TerminalResize>, InitError> {
        let terminal = Terminal::init(TerminalOptions {
            rows: opts.terminal_rows,
            cols: opts.terminal_cols,
            max_scrollback_bytes: Some(opts.scrollback_bytes),
            ..TerminalOptions::default()
        })?;
        Ok(Box::new(TerminalResize { opts, terminal }))
    }

    fn benchmark(&mut self) -> Benchmark<'_> {
        let step_fn = match self.opts.mode {
            Mode::Noop => step_noop,
            Mode::Cols | Mode::ColsNoReflow => step_cols,
            Mode::Rows => step_rows,
            Mode::Both => step_both,
        };
        Benchmark::init(
            self,
            VTable {
                step_fn,
                setup_fn: Some(setup),
                teardown_fn: None,
            },
        )
    }
}

impl TerminalResize {
    /// The column count used by the cols/both modes.
    fn target_cols(&self) -> u16 {
        self.opts.resize_cols.unwrap_or(1.max(self.opts.terminal_cols / 2))
    }

    /// The row count used by the rows/both modes.
    fn target_rows(&self) -> u16 {
        self.opts.resize_rows.unwrap_or(1.max(self.opts.terminal_rows / 2))
    }

    /// Write deterministic synthetic content to the terminal. The goal is
    /// content that is representative of a real session so that reflow
    /// touches its interesting paths: soft-wrapped lines (must rewrap),
    /// short lines (copied as-is), blank lines, styled cells (styles must
    /// be moved across pages), and wide characters (can't be split at the
    /// wrap column).
    fn fill(&mut self) {
        if self.opts.fill_lines == 0 {
            return;
        }

        let cols: usize = self.terminal.cols as usize;
        let mut s = self.terminal.vt_stream();

        let prng = DefaultPrng::init(0xB3);
        let rand = prng.random();

        let mut line_buf = [0u8; 8192];

        for i in 0..self.opts.fill_lines {
            // Periodically toggle a background style so reflow has to
            // carry styled cells into new pages.
            if i % 64 == 0 {
                s.next_slice(b"\x1b[48;2;20;40;60m");
            }
            if i % 64 == 32 {
                s.next_slice(b"\x1b[m");
            }

            // A small portion of lines are blank.
            if i % 16 == 15 {
                s.next_slice(b"\r\n");
                continue;
            }

            // Line lengths between ~25% and ~250% of the terminal width
            // so we get a mix of short lines and soft-wrapped lines.
            let min = 1.max(cols / 4);
            let max = line_buf.len().min(cols * 5 / 2);
            let len = min + rand.uint_less_than::<64>((max - min) as u64) as usize;

            let mut j: usize = 0;
            while j < len {
                // Sprinkle wide characters into every 8th line.
                if i % 8 == 7 && j % 16 == 8 && j + 3 <= len {
                    line_buf[j..j + 3].copy_from_slice("漢".as_bytes());
                    j += 3;
                    continue;
                }

                // Words of ASCII separated by spaces.
                line_buf[j] = if j % 8 == 7 {
                    b' '
                } else {
                    rand.int_range_at_most::<8>(b'a' as u64, b'z' as u64) as u8
                };
                j += 1;
            }

            s.next_slice(&line_buf[..len]);
            s.next_slice(b"\r\n");
        }

        s.deinit();
    }

    /// Stream the data file (if any) into the terminal.
    fn replay_data(&mut self) -> Result<(), benchmark::Error> {
        let data_f = options::data_file(self.opts.data.as_deref()).map_err(|err| {
            log::warn!(target: LOG, "error opening data file err={}", err);
            benchmark::Error::BenchmarkFailed
        })?;
        let Some(data_f) = data_f else {
            return Ok(());
        };

        let mut stream = self.terminal.vt_stream();

        let mut r = BufReader::with_capacity(4096, data_f);

        let mut buf = [0u8; 4096];
        let result = loop {
            let n = match options::read_slice_short(&mut r, &mut buf) {
                Ok(n) => n,
                Err(err) => {
                    log::warn!(target: LOG, "error reading data file err={}", err);
                    break Err(benchmark::Error::BenchmarkFailed);
                }
            };
            if n == 0 {
                break Ok(()); // EOF reached
            }
            stream.next_slice(&buf[..n]);
        };

        stream.deinit();
        result
    }

    fn resize_terminal(&mut self, cols: u16, rows: u16) -> Result<(), benchmark::Error> {
        self.terminal
            .resize(Resize {
                cols,
                rows,
                // Realistic cell pixel geometry: real apprt resizes always
                // carry it, and it exercises the pixel dimension updates.
                cell_size_px: Some(CellSizePx {
                    width: 10,
                    height: 20,
                }),
            })
            .map_err(|err| {
                log::warn!(target: LOG, "error resizing terminal err={:?}", err);
                benchmark::Error::BenchmarkFailed
            })?;
        black_box(&self.terminal);
        Ok(())
    }
}

fn setup(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalResize = cast(ptr);

    // Always reset our terminal state. Note this doesn't resize, but
    // create initializes (and steps return) the terminal to the
    // requested dimensions so we're always at terminal-rows/cols here.
    self_.terminal.full_reset();
    debug_assert_eq!(self_.terminal.cols, self_.opts.terminal_cols);
    debug_assert_eq!(self_.terminal.rows, self_.opts.terminal_rows);

    // Fill with synthetic content first, then replay the data file on
    // top if given. Both go through the VT stream so soft wraps,
    // styles, etc. are all set exactly as they would be in a real
    // session.
    self_.fill();
    self_.replay_data()?;

    // Reflow only happens when wraparound mode is set (it is by
    // default). We only disable it after filling so the fill itself
    // still soft-wraps identically in every mode.
    if self_.opts.mode == Mode::ColsNoReflow {
        self_.terminal.modes.set(TerminalMode::Wraparound, false);
    }
    Ok(())
}

fn step_noop(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalResize = cast(ptr);

    let cols = self_.opts.terminal_cols;
    let rows = self_.opts.terminal_rows;

    // We loop because it's so fast (a few ns) that a single resize
    // doesn't properly capture our speeds.
    for _ in 0..50_000_000 * self_.opts.loops as u64 {
        self_.resize_terminal(cols, rows)?;
    }
    Ok(())
}

fn step_cols(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalResize = cast(ptr);

    let cols = self_.opts.terminal_cols;
    let rows = self_.opts.terminal_rows;
    let target = self_.target_cols();

    // Per-cycle cost differs by orders of magnitude between the two
    // modes (a reflow resize walks the entire scrollback; a non-reflow
    // resize doesn't), so pick a cycle count that makes each run long
    // enough to measure well above process startup and setup cost.
    let cycles: u64 = match self_.opts.mode {
        Mode::Cols => 25,
        Mode::ColsNoReflow => 1_500,
        _ => unreachable!(),
    };

    // Each cycle shrinks (rewrapping long lines) and grows back
    // (unwrapping them), ending at the original size.
    for _ in 0..cycles * self_.opts.loops as u64 {
        self_.resize_terminal(target, rows)?;
        self_.resize_terminal(cols, rows)?;
    }
    Ok(())
}

fn step_rows(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalResize = cast(ptr);

    let cols = self_.opts.terminal_cols;
    let rows = self_.opts.terminal_rows;
    let target = self_.target_rows();

    // Row-only resizes don't reflow so they're much cheaper (tens of
    // ns); loop a lot more so the measurement isn't dominated by
    // process overhead.
    for _ in 0..2_500_000 * self_.opts.loops as u64 {
        self_.resize_terminal(cols, target)?;
        self_.resize_terminal(cols, rows)?;
    }
    Ok(())
}

fn step_both(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalResize = cast(ptr);

    let cols = self_.opts.terminal_cols;
    let rows = self_.opts.terminal_rows;
    let target_cols = self_.target_cols();
    let target_rows = self_.target_rows();

    for _ in 0..25 * self_.opts.loops as u64 {
        self_.resize_terminal(target_cols, target_rows)?;
        self_.resize_terminal(cols, rows)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    #[test]
    fn terminal_resize() {
        // Small dimensions and fill so this is fast in debug builds while
        // still exercising real reflow in both directions.
        let mut impl_ = TerminalResize::create(Options {
            mode: Mode::Cols,
            terminal_rows: 10,
            terminal_cols: 20,
            fill_lines: 50,
            ..Options::default()
        })
        .unwrap();

        let mut bench = impl_.benchmark();
        let _ = bench.run(RunMode::Once).unwrap();
    }
}
