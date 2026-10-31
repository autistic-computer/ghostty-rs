//! Port of `src/benchmark/ScreenClone.zig`.
//!
//! STATUS: ported
//!
//! This benchmark tests the performance of the Screen.clone
//! function. This is useful because it is one of the primary lock
//! holders that impact IO performance when the renderer is active.
//! We do this very frequently.

use std::any::Any;
use std::fmt;
use std::hint::black_box;
use std::io::BufReader;
use std::mem::ManuallyDrop;

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use super::options;
use crate::cli::args::{cli_enum, cli_struct};
use crate::terminal::kitty::graphics::Image;
use crate::terminal::kitty::graphics_command::Format;
use crate::terminal::kitty::graphics_image::Data as ImageData;
use crate::terminal::kitty::graphics_storage::{Location, Placement};
use crate::terminal::point::{self, Coordinate, Point};
use crate::terminal::render::Overscan;
use crate::terminal::screen::Screen;
use crate::terminal::screen_set::Key as ScreenKey;
use crate::terminal::size::AllocError;
use crate::terminal::terminal::{InitError, Options as TerminalOptions, ScrollViewport, Terminal};
use crate::terminal::RenderState;

const LOG: &str = "terminal-stream-bench";

pub struct ScreenClone {
    pub opts: Options,
    pub terminal: Terminal,
}

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// The type of codepoint width calculation to use.
        pub mode: Mode = Mode::Clone,

        /// Multiplier on the number of iterations each step runs. This is
        /// useful to make a benchmark run long enough for profiling.
        pub loops: u32 = 1,

        /// The size of the terminal. This affects benchmarking when
        /// dealing with soft line wrapping and the memory impact
        /// of page sizes.
        pub terminal_rows as "terminal-rows": u16 = 80,
        pub terminal_cols as "terminal-cols": u16 = 120,

        /// The cell size in pixels. Kitty graphics images need a cell size
        /// to be sized, so without one the Kitty modes have nothing to do.
        pub cell_width as "cell-width": u32 = 10,
        pub cell_height as "cell-height": u32 = 20,

        /// The data to read as a filepath. If this is "-" then
        /// we will read stdin. If this is unset, then we will
        /// do nothing (benchmark is a noop). It'd be more unixy to
        /// use stdin by default but I find that a hanging CLI command
        /// with no interaction is a bit annoying.
        ///
        /// This will be used to initialize the terminal screen state before
        /// cloning. This data can switch to alt screen if it wants. The time
        /// to read this is not part of the benchmark.
        pub data: Option<String> = None,
    }
}

cli_enum! {
    pub enum Mode {
        /// The baseline mode copies the screen by value.
        Noop => "noop",

        /// Full clone
        Clone => "clone",

        /// RenderState rather than a screen clone.
        Render => "render",

        /// Like render, but only the portion of the render state update
        /// that requires holding a terminal lock (beginUpdate). The
        /// deferred work (endUpdate) is excluded since it happens outside
        /// of any locks.
        RenderLocked => "render-locked",

        /// RenderState update with no changes to the terminal. This is
        /// the common case for a renderer that is redrawing frames (e.g.
        /// cursor blink, mouse movement) without terminal changes.
        RenderClean => "render-clean",

        /// RenderState update where a single row is dirty. This models the
        /// common case of a shell prompt or TUI updating a small portion
        /// of the screen between frames.
        RenderPartial => "render-partial",

        /// Like render-partial, and then reads every Kitty graphics image
        /// placement found through placeholder cells, as a renderer does
        /// each frame (see `RenderState.imagePlacementIterator`). Use it with
        /// data that shows images with placeholder cells.
        RenderPartialKitty => "render-partial-kitty",

        /// Like render-partial-kitty, but the images are Kitty graphics
        /// images placed at cells on the screen instead of placeholder
        /// cells. `bench_fixed_count` of them are spread over the active
        /// area. This models a program updating a small part of the screen
        /// while images it placed earlier stay where they are.
        RenderPartialKittyFixed => "render-partial-kitty-fixed",

        /// RenderState update after scrolling the viewport by one row.
        /// This is the per-frame cost of user-driven scrolling through
        /// scrollback (e.g. a trackpad fling), which today changes the
        /// viewport pin and forces a full rebuild.
        RenderScroll => "render-scroll",

        /// Like render-scroll, and then reads every image placement, as
        /// render-partial-kitty does. The screen holds as many images placed
        /// at cells as render-partial-kitty-fixed, spread over the
        /// scrollback that the viewport scrolls through. Some are usually
        /// visible, and every one moves each frame. This models scrolling
        /// back through output that contains images.
        RenderScrollKittyFixed => "render-scroll-kitty-fixed",

        /// RenderState update after scrolling the viewport by half a
        /// screen. This models paging (page up/down, wheel ticks with
        /// large multipliers) rather than smooth scrolling.
        RenderScrollPage => "render-scroll-page",

        /// RenderState update after one new line of output is written
        /// while the viewport follows the active area. This models a
        /// program streaming output. Each frame the viewport pin moves
        /// down one row.
        RenderOutput => "render-output",

        /// The render-scroll, render-output, and render-clean modes with
        /// overscan (`RenderState.overscan_request`) of 4 rows above and 1
        /// below. This models a renderer that captures extra rows so it can
        /// draw partially visible rows while smooth scrolling.
        RenderScrollOverscan => "render-scroll-overscan",
        RenderOutputOverscan => "render-output-overscan",
        RenderCleanOverscan => "render-clean-overscan",
    }
}

/// The overscan request used by the overscan modes.
const BENCH_OVERSCAN: Overscan = Overscan { above: 4, below: 1 };

/// The overscan request for the given mode.
fn overscan_request(mode: Mode) -> Overscan {
    match mode {
        Mode::RenderScrollOverscan | Mode::RenderOutputOverscan | Mode::RenderCleanOverscan => BENCH_OVERSCAN,
        _ => Overscan::default(),
    }
}

/// The number of Kitty graphics images placed at cells in the
/// `*-kitty-fixed` modes, and the size of each one in cells.
const BENCH_FIXED_COUNT: usize = 8;
const BENCH_FIXED_COLS: u32 = 6;
const BENCH_FIXED_ROWS: u32 = 4;

/// Number of scrollback lines written during setup so that the scroll
/// modes have room to move in both directions.
const SCROLL_SETUP_LINES: usize = 4000;

/// Direction changes for the scroll modes, in iterations.
const SCROLL_REVERSE_INTERVAL: usize = 2000;
const SCROLL_PAGE_REVERSE_INTERVAL: usize = 40;

/// Errors from [`ScreenClone::add_fixed_placements`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlacementError {
    OutOfMemory,
    OutOfRows,
}

impl fmt::Display for PlacementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl From<AllocError> for PlacementError {
    fn from(_: AllocError) -> Self {
        PlacementError::OutOfMemory
    }
}

impl BenchmarkImpl for ScreenClone {
    type Options = Options;
    type CreateError = InitError;

    fn create(opts: Options) -> Result<Box<ScreenClone>, InitError> {
        let mut terminal_opts = TerminalOptions {
            rows: opts.terminal_rows,
            cols: opts.terminal_cols,
            ..TerminalOptions::default()
        };

        // The scroll modes need real scrollback to move through. The
        // default limit is small enough that the setup lines would be
        // pruned immediately.
        match opts.mode {
            Mode::RenderScroll | Mode::RenderScrollKittyFixed | Mode::RenderScrollPage | Mode::RenderScrollOverscan => {
                terminal_opts.max_scrollback_bytes = Some(256 * 1024 * 1024)
            }
            _ => {}
        }

        let mut ptr = Box::new(ScreenClone {
            terminal: Terminal::init(terminal_opts)?,
            opts,
        });
        ptr.terminal.width_px = ptr.opts.terminal_cols as u32 * ptr.opts.cell_width;
        ptr.terminal.height_px = ptr.opts.terminal_rows as u32 * ptr.opts.cell_height;

        Ok(ptr)
    }

    fn benchmark(&mut self) -> Benchmark<'_> {
        let step_fn = match self.opts.mode {
            Mode::Noop => step_noop,
            Mode::Clone => step_clone,
            Mode::Render => step_render,
            Mode::RenderLocked => step_render_locked,
            Mode::RenderClean | Mode::RenderCleanOverscan => step_render_clean,
            Mode::RenderPartial => step_render_partial,
            Mode::RenderPartialKitty | Mode::RenderPartialKittyFixed => step_render_partial_kitty,
            Mode::RenderScroll | Mode::RenderScrollOverscan => step_render_scroll,
            Mode::RenderScrollKittyFixed => step_render_scroll_kitty,
            Mode::RenderScrollPage => step_render_scroll_page,
            Mode::RenderOutput | Mode::RenderOutputOverscan => step_render_output,
        };
        Benchmark::init(
            self,
            VTable {
                step_fn,
                setup_fn: Some(setup),
                teardown_fn: Some(teardown),
            },
        )
    }
}

impl ScreenClone {
    /// Add one Kitty graphics image and place it at `bench_fixed_count`
    /// cells, spread evenly over the last `span` rows of the screen. The
    /// columns vary so that the images don't all line up.
    fn add_fixed_placements(&mut self, span: usize) -> Result<(), PlacementError> {
        let cols = self.terminal.cols as u32;
        let screen: &mut Screen = &mut self.terminal.screens.active;
        let storage = &mut screen.kitty_images;

        let width = 16;
        let height = 16;
        let pixels = vec![0x80u8; width * height * 4];
        storage.add_image(
            &mut screen.pages,
            Image {
                id: 1,
                width: width as u32,
                height: height as u32,
                format: Format::Rgba,
                data: ImageData::Complete(pixels),
                ..Image::default()
            },
        )?;

        let bottom = screen.pages.get_bottom_right(point::Tag::Screen).unwrap();
        let step = span / BENCH_FIXED_COUNT;
        for i in 0..BENCH_FIXED_COUNT {
            let mut p = bottom.up(span - 1 - i * step).ok_or(PlacementError::OutOfRows)?;
            p.x = ((i as u32 * 13) % (cols - BENCH_FIXED_COLS)) as _;
            let pin = screen.pages.track_pin(p)?;
            storage.add_placement(
                &mut screen.pages,
                1,
                (i + 1) as u32,
                Placement {
                    columns: BENCH_FIXED_COLS,
                    rows: BENCH_FIXED_ROWS,
                    ..Placement::new(Location::Pin(pin))
                },
            )?;
        }
        Ok(())
    }
}

fn setup(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScreenClone = cast(ptr);

    // Always reset our terminal state
    self_.terminal.full_reset();

    // Force a style on every single row, which
    let rows = self_.terminal.rows;
    {
        let mut s = self_.terminal.vt_stream();
        s.next_slice(b"\x1b[48;2;20;40;60m");

        // The scroll modes need scrollback above the screen so the
        // viewport has somewhere to go.
        match self_.opts.mode {
            Mode::RenderScroll | Mode::RenderScrollKittyFixed | Mode::RenderScrollPage | Mode::RenderScrollOverscan => {
                for _ in 0..SCROLL_SETUP_LINES {
                    s.next_slice(b"hello\r\n");
                }
            }
            _ => {}
        }

        for _ in 0..rows - 1 {
            s.next_slice(b"hello\r\n");
        }
        s.next_slice(b"hello");
        s.deinit();
    }

    // Kitty graphics images placed at cells, spread over the rows that
    // the mode shows. That is the active area, or the scrollback that
    // the scroll mode moves through.
    let span = match self_.opts.mode {
        Mode::RenderPartialKittyFixed => Some(rows as usize),
        Mode::RenderScrollKittyFixed => Some(rows as usize + SCROLL_REVERSE_INTERVAL),
        _ => None,
    };
    if let Some(span) = span {
        self_.add_fixed_placements(span).map_err(|err| {
            log::warn!(target: LOG, "error adding kitty placements err={}", err);
            benchmark::Error::BenchmarkFailed
        })?;
    }

    // Setup our terminal state
    let data_f = options::data_file(self_.opts.data.as_deref()).map_err(|err| {
        log::warn!(target: LOG, "error opening data file err={}", err);
        benchmark::Error::BenchmarkFailed
    })?;
    let Some(data_f) = data_f else {
        return Ok(());
    };

    let mut stream = self_.terminal.vt_stream();

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

fn teardown(ptr: &mut dyn Any) {
    let _self: &mut ScreenClone = cast(ptr);
}

fn step_noop(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScreenClone = cast(ptr);

    // We loop because its so fast that a single benchmark run doesn't
    // properly capture our speeds.
    for _ in 0..1000 {
        // A bitwise copy of the screen by value, like Zig's
        // `const s: Screen = self.terminal.screens.active.*`. It must
        // never be dropped since it does not own anything.
        // SAFETY: a plain memcpy of a live value that is never dropped.
        let s: ManuallyDrop<Screen> = ManuallyDrop::new(unsafe { std::ptr::read(self_.terminal.screens.active.as_ptr()) });
        black_box(&s);
    }
    Ok(())
}

fn step_clone(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScreenClone = cast(ptr);

    // We loop because its so fast that a single benchmark run doesn't
    // properly capture our speeds.
    for _ in 0..1000 {
        let s: &Screen = &self_.terminal.screens.active;
        let copy = s.clone(Point::Viewport(Coordinate::default()), None).map_err(|err| {
            log::warn!(target: LOG, "error cloning screen err={:?}", err);
            benchmark::Error::BenchmarkFailed
        })?;
        black_box(&copy);

        // Note: we purposely do not free memory because we don't want
        // to benchmark that. We'll free when the benchmark exits.
        std::mem::forget(copy);
    }
    Ok(())
}

fn step_render(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScreenClone = cast(ptr);

    // We do this once out of the loop because a significant slowdown
    // on the first run is allocation. After that first run, even with
    // a full rebuild, it is much faster. Let's ignore that first run
    // slowdown.
    let mut state = RenderState::empty();
    state.update(&mut self_.terminal);

    // We loop because its so fast that a single benchmark run doesn't
    // properly capture our speeds.
    for _ in 0..50_000 * self_.opts.loops as u64 {
        // Forces a full rebuild because it thinks our screen changed
        state.screen = ScreenKey::Alternate;
        state.update(&mut self_.terminal);
        black_box(&state);

        // Note: we purposely do not free memory because we don't want
        // to benchmark that. We'll free when the benchmark exits.
    }
    Ok(())
}

fn step_render_locked(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScreenClone = cast(ptr);

    // We do this once out of the loop because a significant slowdown
    // on the first run is allocation. After that first run, even with
    // a full rebuild, it is much faster. Let's ignore that first run
    // slowdown.
    let mut state = RenderState::empty();
    state.update(&mut self_.terminal);

    // We loop because its so fast that a single benchmark run doesn't
    // properly capture our speeds.
    for _ in 0..50_000 * self_.opts.loops as u64 {
        // Forces a full rebuild because it thinks our screen changed
        state.screen = ScreenKey::Alternate;
        state.begin_update(&mut self_.terminal);
        black_box(&state);

        // Note: we purposely do not free memory because we don't want
        // to benchmark that. We'll free when the benchmark exits.
    }
    Ok(())
}

fn step_render_clean(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScreenClone = cast(ptr);

    // Initial update so that subsequent updates are clean (nothing
    // dirty, no rebuilds).
    let mut state = RenderState::empty();
    state.overscan_request = overscan_request(self_.opts.mode);
    state.update(&mut self_.terminal);

    // We loop because its so fast that a single benchmark run doesn't
    // properly capture our speeds.
    for _ in 0..3_000_000 * self_.opts.loops as u64 {
        state.update(&mut self_.terminal);
        black_box(&state);
    }
    Ok(())
}

/// A pin roughly in the middle of the active area that we dirty on
/// every iteration to simulate a small screen update.
fn middle_pin(t: &Terminal) -> crate::terminal::page_list::Pin {
    let pages = &t.screens.active.pages;
    pages
        .pin(Point::Active(Coordinate {
            x: 0,
            y: (t.rows / 2) as _,
        }))
        .unwrap()
}

fn step_render_partial(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScreenClone = cast(ptr);

    // Initial update so that subsequent updates are incremental.
    let mut state = RenderState::empty();
    state.update(&mut self_.terminal);

    // Grab a pin roughly in the middle of the active area that we
    // dirty on every iteration to simulate a small screen update.
    let pin = middle_pin(&self_.terminal);

    // We loop because its so fast that a single benchmark run doesn't
    // properly capture our speeds.
    for _ in 0..2_000_000 * self_.opts.loops as u64 {
        // Mark a single row dirty. `update` clears this so each
        // iteration rebuilds exactly one row.
        pin.mark_dirty();
        state.update(&mut self_.terminal);
        black_box(&state);
    }
    Ok(())
}

fn step_render_partial_kitty(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScreenClone = cast(ptr);

    let mut state = RenderState::empty();
    state.update(&mut self_.terminal);

    let pin = middle_pin(&self_.terminal);

    for _ in 0..200_000 * self_.opts.loops as u64 {
        pin.mark_dirty();
        state.update(&mut self_.terminal);

        for p in state.image_placement_iterator() {
            black_box(p);
        }
    }
    Ok(())
}

fn step_render_scroll(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScreenClone = cast(ptr);
    render_scroll_loop(self_, 1, SCROLL_REVERSE_INTERVAL)
}

fn step_render_scroll_kitty(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScreenClone = cast(ptr);

    let mut state = RenderState::empty();
    state.update(&mut self_.terminal);

    let mut dir: isize = 1;
    for i in 0..50_000 * self_.opts.loops as u64 {
        if i % SCROLL_REVERSE_INTERVAL as u64 == 0 {
            dir = -dir;
        }
        self_.terminal.scroll_viewport(ScrollViewport::Delta(dir));
        state.update(&mut self_.terminal);

        for p in state.image_placement_iterator() {
            black_box(p);
        }
    }
    Ok(())
}

fn step_render_scroll_page(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScreenClone = cast(ptr);
    let step = 1.max(self_.terminal.rows / 2) as isize;
    render_scroll_loop(self_, step, SCROLL_PAGE_REVERSE_INTERVAL)
}

/// Scroll the viewport by `step` rows per iteration, reversing
/// direction every `interval` iterations, and update the render state
/// after each scroll. The viewport starts at the bottom, so the first
/// stretch scrolls up into scrollback.
fn render_scroll_loop(self_: &mut ScreenClone, step: isize, interval: usize) -> Result<(), benchmark::Error> {
    let mut state = RenderState::empty();
    state.overscan_request = overscan_request(self_.opts.mode);
    state.update(&mut self_.terminal);

    let mut dir: isize = 1;
    for i in 0..50_000 * self_.opts.loops as u64 {
        if i % interval as u64 == 0 {
            dir = -dir;
        }
        self_.terminal.scroll_viewport(ScrollViewport::Delta(dir * step));
        state.update(&mut self_.terminal);
        black_box(&state);
    }
    Ok(())
}

fn step_render_output(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScreenClone = cast(ptr);

    let mut state = RenderState::empty();
    state.overscan_request = overscan_request(self_.opts.mode);
    state.update(&mut self_.terminal);

    // Make sure the viewport follows the active area so each line
    // written moves the viewport.
    self_.terminal.scroll_viewport(ScrollViewport::Bottom);

    // The stream borrows the terminal, so the render state reads it
    // through the stream's handler.
    let mut stream = self_.terminal.vt_stream();
    for _ in 0..50_000 * self_.opts.loops as u64 {
        stream.next_slice(b"hello\r\n");
        state.update(&mut *stream.handler.terminal);
        black_box(&state);
    }
    stream.deinit();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    /// Not in Zig (which only refAllDecls this file); exercises every
    /// mode once with tiny loop counts to keep the port honest.
    #[test]
    fn screen_clone_modes_setup() {
        for mode in [
            Mode::Noop,
            Mode::RenderPartialKittyFixed,
            Mode::RenderScrollKittyFixed,
        ] {
            let mut impl_ = ScreenClone::create(Options {
                mode,
                terminal_rows: 10,
                terminal_cols: 20,
                ..Options::default()
            })
            .unwrap();
            setup(&mut *impl_).unwrap();
            if mode == Mode::Noop {
                let mut bench = impl_.benchmark();
                let _ = bench.run(RunMode::Once).unwrap();
            }
        }
    }
}
