//! Port of `src/benchmark/TerminalFormatter.zig`.
//!
//! STATUS: ported
//!
//! Benchmarks the terminal formatter (`terminal/formatter.zig`).
//!
//! The formatter is the hot path for clipboard copy (plain/VT/HTML),
//! `write_screen_file`, `selectionString`, terminal search window
//! encoding, and the libghostty-vt formatter C API. This benchmark
//! measures formatting terminal contents that were built during setup
//! (outside the timed region) from a pre-generated VT stream.
//!
//! ## Input
//!
//! `--data` names a pre-generated VT byte stream (for example from
//! `ghostty-gen styled`). The stream is fed to a terminal of the
//! requested dimensions with unlimited scrollback during setup. The
//! resulting screen contents (scrollback included) are what each step
//! formats.
//!
//! ## Formatters
//!
//! * `screen` (the default) formats the active screen with a
//!   ScreenFormatter and no extras, as clipboard copy, `write_screen_file`
//!   and search do.
//! * `terminal` formats the whole terminal with a TerminalFormatter and
//!   every extra (palette, modes, scrolling region, tabstops, cursor,
//!   style and so on), as `ghostty_formatter_terminal_new` in the
//!   libghostty-vt C API does for a snapshot of a terminal. Extras only
//!   apply to `--emit=vt`.
//!
//! ## Modes
//!
//! * `noop` performs no formatting and establishes loop/setup overhead.
//!   Subtract this from `format` timings when using hyperfine.
//! * `format` formats the configured region once per loop into a
//!   reusable buffer. Buffer growth happens on the first iteration only.
//! * `report` formats once and prints content/output sizes. It is for
//!   computing throughput (cells/s, bytes/s), not timing comparisons.
//!
//! ## Examples
//!
//! Build benchmarks in release mode:
//!
//!     cargo build --release --bin ghostty-bench --bin ghostty-gen
//!
//! Generate a deterministic corpus, then measure:
//!
//!     ghostty-gen styled --seed=42 | head -c 640000 > /tmp/plain.vt
//!     hyperfine --warmup 3 \
//!       'ghostty-bench +terminal-formatter --mode=noop --data=/tmp/plain.vt' \
//!       'ghostty-bench +terminal-formatter --emit=plain --loops=50 --data=/tmp/plain.vt' \
//!       'ghostty-bench +terminal-formatter --emit=vt --loops=50 --data=/tmp/plain.vt'
//!
//! Measure the terminal formatter with every extra, and check that its
//! output reconstructs the terminal state as well as the contents:
//!
//!     ghostty-bench +terminal-formatter --formatter=terminal --loops=50 --data=/tmp/plain.vt
//!     ghostty-bench +terminal-formatter --formatter=terminal --mode=roundtrip --data=/tmp/plain.vt

use std::any::Any;
use std::convert::Infallible;
use std::fmt;
use std::hint::black_box;
use std::io::{self, BufReader, Write};

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use super::options;
use crate::cli::args::{cli_enum, cli_struct, CliEnum};
use crate::terminal::formatter as formatterpkg;
use crate::terminal::hash_map::Wyhash;
use crate::terminal::point;
use crate::terminal::selection::Selection;
use crate::terminal::terminal::{InitError, Options as TerminalOptions, Terminal};

const LOG: &str = "terminal-formatter-bench";

pub struct TerminalFormatter {
    pub opts: Options,
    pub terminal: Option<Terminal>,

    /// Reused across steps so buffer growth is a one-time setup cost.
    pub output: Vec<u8>,

    /// Reused pin map storage for `--pin-map=true`.
    pub pins: formatterpkg::PinMapMap,
}

cli_struct! {
    /// Zig also keeps an `_arena` for string option ownership; Rust
    /// options own their strings.
    #[derive(Debug, Clone)]
    pub struct Options {
        /// Select the operation performed inside the timed benchmark step.
        pub mode: Mode = Mode::Format,

        /// The output format to emit.
        pub emit: Emit = Emit::Vt,

        /// The formatter to measure.
        pub formatter: Formatter = Formatter::Screen,

        /// The region of the screen to format.
        pub region: Region = Region::Screen,

        /// Unwrap soft-wrapped lines.
        pub unwrap: bool = false,

        /// Track the source pin of every emitted byte. This exercises the
        /// (documented as expensive) pin_map path used by selectionString
        /// and search.
        pub pin_map as "pin-map": bool = false,

        /// Number of format operations per benchmark step. Increase this
        /// when the content is too small for stable hyperfine measurements.
        pub loops: u32 = 25,

        /// The size of the terminal. This affects wrapping and page sizes.
        pub terminal_rows as "terminal-rows": u16 = 24,
        pub terminal_cols as "terminal-cols": u16 = 80,

        /// Pre-generated VT stream fed to the terminal during setup. `-`
        /// reads stdin, although a regular file is recommended so identical
        /// state can be reused across runs. When unset, the terminal is
        /// empty.
        pub data: Option<String> = None,
    }
}

cli_enum! {
    pub enum Mode {
        /// Establish the benchmark loop and setup overhead.
        Noop => "noop",

        /// Format the configured region once per loop.
        Format => "format",

        /// Print content and output sizes. Not a timing benchmark.
        Report => "report",

        /// Semantic verification, not a timing benchmark: format the
        /// terminal (must be `--emit=vt`), feed the output into a fresh
        /// terminal of the same dimensions, format that, and verify the
        /// two outputs converge to identical bytes. This proves the VT
        /// output faithfully reconstructs the terminal content even when
        /// the exact byte encoding changes.
        Roundtrip => "roundtrip",
    }
}

cli_enum! {
    pub enum Emit {
        Plain => "plain",
        Vt => "vt",
        Html => "html",
    }
}

impl Emit {
    fn format(self) -> formatterpkg::Format {
        match self {
            Emit::Plain => formatterpkg::Format::Plain,
            Emit::Vt => formatterpkg::Format::Vt,
            Emit::Html => formatterpkg::Format::Html,
        }
    }
}

cli_enum! {
    pub enum Formatter {
        /// The active screen with a ScreenFormatter and no extras.
        Screen => "screen",

        /// The whole terminal with a TerminalFormatter and every extra.
        Terminal => "terminal",
    }
}

cli_enum! {
    pub enum Region {
        /// Everything: scrollback and active screen.
        Screen => "screen",

        /// Only the active screen (bottom rows).
        Active => "active",

        /// Only the scrollback.
        History => "history",
    }
}

/// Errors from setup and roundtrip (Zig's inferred error sets).
#[derive(Debug)]
enum StepError {
    Init(InitError),
    Io(io::Error),
    RoundtripMismatch,
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StepError::Init(e) => write!(f, "{e}"),
            StepError::Io(e) => write!(f, "{e}"),
            StepError::RoundtripMismatch => f.write_str("RoundtripMismatch"),
        }
    }
}

impl From<InitError> for StepError {
    fn from(e: InitError) -> Self {
        StepError::Init(e)
    }
}

impl From<io::Error> for StepError {
    fn from(e: io::Error) -> Self {
        StepError::Io(e)
    }
}

impl BenchmarkImpl for TerminalFormatter {
    type Options = Options;
    type CreateError = Infallible;

    fn create(opts: Options) -> Result<Box<TerminalFormatter>, Infallible> {
        Ok(Box::new(TerminalFormatter {
            opts,
            terminal: None,
            output: Vec::new(),
            pins: formatterpkg::PinMapMap::EMPTY,
        }))
    }

    fn benchmark(&mut self) -> Benchmark<'_> {
        let step_fn = match self.opts.mode {
            Mode::Noop => step_noop,
            Mode::Format => step_format,
            Mode::Report => step_report,
            Mode::Roundtrip => step_roundtrip,
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

fn new_terminal(opts: &Options) -> Result<Terminal, InitError> {
    Terminal::init(TerminalOptions {
        cols: opts.terminal_cols,
        rows: opts.terminal_rows,
        max_scrollback_bytes: None,
        max_scrollback_lines: None,
        ..TerminalOptions::default()
    })
}

impl TerminalFormatter {
    fn setup_impl(&mut self) -> Result<(), StepError> {
        self.terminal = None;
        self.terminal = Some(new_terminal(&self.opts)?);
        let terminal = self.terminal.as_mut().unwrap();

        // Feed the input corpus through the standard VT stream.
        if let Some(data_f) = options::data_file(self.opts.data.as_deref())? {
            let mut stream = terminal.vt_stream();

            let mut r = BufReader::with_capacity(4096, data_f);

            let mut buf = [0u8; 4096];
            let result = loop {
                let n = match options::read_slice_short(&mut r, &mut buf) {
                    Ok(n) => n,
                    Err(e) => break Err(e),
                };
                if n == 0 {
                    break Ok(()); // EOF reached
                }
                stream.next_slice(&buf[..n]);
            };
            stream.deinit();
            result?;
        }
        Ok(())
    }

    /// Format the terminal, replay the output into a fresh terminal of the
    /// same dimensions, format that, and require both outputs to be
    /// identical. This verifies that the emitted VT sequences faithfully
    /// reconstruct the terminal contents (text, styles, wrapping) without
    /// requiring any specific byte encoding of the first output. With
    /// `--formatter=terminal` both outputs include every extra, so the
    /// terminal state they describe (modes, scrolling region, cursor and so
    /// on) must be reconstructed too.
    fn step_roundtrip_impl(&mut self) -> Result<(), StepError> {
        // Format the original terminal.
        self.output.clear();
        if let Some(mut f) = formatter(&self.opts, self.terminal.as_ref().unwrap(), self.opts.region) {
            f.format(&mut self.output)?;
        }
        let first = &self.output[..];

        // Replay into a fresh terminal.
        let mut t2 = new_terminal(&self.opts)?;
        {
            let mut stream = t2.vt_stream();
            stream.next_slice(first);
            stream.deinit();
        }

        // Format all of the replayed terminal with the same formatter.
        let mut out2: Vec<u8> = Vec::new();
        if let Some(mut f2) = formatter(&self.opts, &t2, Region::Screen) {
            f2.format(&mut out2)?;
        }
        let second = &out2[..];

        let equal = first == second;
        log::debug!(
            target: LOG,
            "terminal-formatter roundtrip emit={} formatter={} bytes={} replay_bytes={} equal={}",
            self.opts.emit.tag_name(),
            self.opts.formatter.tag_name(),
            first.len(),
            second.len(),
            equal,
        );

        if !equal {
            // Find the first differing offset to ease debugging.
            let n = first.len().min(second.len());
            let mut i: usize = 0;
            while i < n && first[i] == second[i] {
                i += 1;
            }
            log::debug!(
                target: LOG,
                "terminal-formatter roundtrip mismatch offset={} first=\"{}\" second=\"{}\"",
                i,
                first[i.saturating_sub(32)..first.len().min(i + 32)].escape_ascii(),
                second[i.saturating_sub(32)..second.len().min(i + 32)].escape_ascii(),
            );
            return Err(StepError::RoundtripMismatch);
        }
        Ok(())
    }
}

/// Build the terminal state every mode shares. All of this is outside
/// the timed region for `Benchmark`, but is included in whole-process
/// timings, hence the `noop` mode.
fn setup(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalFormatter = cast(ptr);
    self_.setup_impl().map_err(|err| {
        log::warn!(target: LOG, "failed to prepare formatter benchmark err={}", err);
        benchmark::Error::BenchmarkFailed
    })
}

fn teardown(ptr: &mut dyn Any) {
    let self_: &mut TerminalFormatter = cast(ptr);
    self_.terminal = None;
    self_.output.clear();
    self_.pins.clear_retaining_capacity();
}

/// The formatter selected by `--formatter`.
enum AnyFormatter<'a> {
    Screen(formatterpkg::ScreenFormatter<'a>),
    Terminal(formatterpkg::TerminalFormatter<'a>),
}

impl<'a> AnyFormatter<'a> {
    fn set_pin_map(&mut self, pin_map: formatterpkg::PinMap<'a>) {
        match self {
            AnyFormatter::Screen(f) => f.pin_map = Some(pin_map),
            AnyFormatter::Terminal(f) => f.pin_map = Some(pin_map),
        }
    }

    fn format(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        match self {
            AnyFormatter::Screen(f) => f.format(writer),
            AnyFormatter::Terminal(f) => f.format(writer),
        }
    }
}

/// Build the formatter matching our options for the terminal t. The
/// screen formatter mirrors how Surface clipboard copy and
/// write_screen_file construct formatters; the terminal formatter mirrors
/// the libghostty-vt C API's ghostty_formatter_terminal_new.
fn formatter<'a>(opts_: &Options, t: &'a Terminal, region: Region) -> Option<AnyFormatter<'a>> {
    let screen = &*t.screens.active;
    let opts = formatterpkg::Options {
        unwrap: opts_.unwrap,
        ..formatterpkg::Options::new(opts_.emit.format())
    };

    let content = match region {
        Region::Screen => formatterpkg::ScreenFormatterContent::Selection(None),

        Region::Active | Region::History => {
            let tag = match region {
                Region::Active => point::Tag::Active,
                Region::History => point::Tag::History,
                Region::Screen => unreachable!(),
            };
            let tl = screen.pages.get_top_left(tag);
            let br = screen.pages.get_bottom_right(tag)?;
            formatterpkg::ScreenFormatterContent::Selection(Some(Selection::init(tl, br, false)))
        }
    };

    match opts_.formatter {
        Formatter::Screen => {
            let mut f = formatterpkg::ScreenFormatter::init(screen, opts);
            f.content = content;
            Some(AnyFormatter::Screen(f))
        }

        Formatter::Terminal => {
            let mut f = formatterpkg::TerminalFormatter::init(t, opts);
            f.content = content;
            f.extra = formatterpkg::TerminalFormatterExtra::ALL;
            Some(AnyFormatter::Terminal(f))
        }
    }
}

fn step_noop(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalFormatter = cast(ptr);
    for _ in 0..self_.opts.loops {
        black_box(&self_.output[..]);
    }
    Ok(())
}

fn step_format(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalFormatter = cast(ptr);
    for _ in 0..self_.opts.loops {
        self_.output.clear();

        let Some(mut f) = formatter(&self_.opts, self_.terminal.as_ref().unwrap(), self_.opts.region) else {
            continue;
        };
        if self_.opts.pin_map {
            self_.pins.clear_retaining_capacity();
            f.set_pin_map(formatterpkg::PinMap { map: &mut self_.pins });
        }

        f.format(&mut self_.output).map_err(|err| {
            log::warn!(target: LOG, "formatting failed err={}", err);
            benchmark::Error::BenchmarkFailed
        })?;
        drop(f);
        black_box(&self_.output[..]);
    }
    Ok(())
}

/// Print the content dimensions and emitted output size. This shares
/// the formatting code with format mode but deliberately makes no
/// timing claims. Use it to compute cells/s and bytes/s from timings.
fn step_report(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalFormatter = cast(ptr);

    self_.output.clear();
    if let Some(mut f) = formatter(&self_.opts, self_.terminal.as_ref().unwrap(), self_.opts.region) {
        if self_.opts.pin_map {
            self_.pins.clear_retaining_capacity();
            f.set_pin_map(formatterpkg::PinMap { map: &mut self_.pins });
        }
        f.format(&mut self_.output).map_err(|err| {
            log::warn!(target: LOG, "formatting failed err={}", err);
            benchmark::Error::BenchmarkFailed
        })?;
    }

    // Count the total pages and rows in the pagelist.
    let screen = &self_.terminal.as_ref().unwrap().screens.active;
    let mut pages: usize = 0;
    let mut rows: usize = 0;
    {
        let mut node = screen.pages.pages.first;
        while let Some(n) = node {
            // SAFETY: nodes of the live page list; `page` may restore a
            // compressed page, which needs exclusive access like in Zig.
            let n = unsafe { &mut *n.as_ptr() };
            pages += 1;
            rows += n.page().size.rows as usize;
            node = n.next;
        }
    }

    // Hash the output (and the pin coordinates, which are stable across
    // runs unlike the node pointers) so different implementations can be
    // checked for identical output.
    let out_hash = Wyhash::hash(0, &self_.output);
    let mut pin_hasher = Wyhash::init(0);
    for coord in &self_.pins.points {
        let x: u16 = coord.x;
        let y: u16 = coord.y as u16;
        pin_hasher.update(&x.to_ne_bytes());
        pin_hasher.update(&y.to_ne_bytes());
    }

    log::debug!(
        target: LOG,
        "terminal-formatter emit={} formatter={} region={} pages={} rows={} cols={} cells={} out_bytes={} pin_bytes={} out_hash={:x} pin_hash={:x}",
        self_.opts.emit.tag_name(),
        self_.opts.formatter.tag_name(),
        self_.opts.region.tag_name(),
        pages,
        rows,
        self_.opts.terminal_cols,
        rows * self_.opts.terminal_cols as usize,
        self_.output.len(),
        self_.pins.count(),
        out_hash,
        pin_hasher.final_(),
    );

    // Pin map storage details on a separate line so the main report
    // line remains comparable across implementations.
    if self_.opts.pin_map {
        log::debug!(
            target: LOG,
            "terminal-formatter-pins points={} nodes={}",
            self_.pins.points.len(),
            self_.pins.nodes.len(),
        );
    }
    Ok(())
}

fn step_roundtrip(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalFormatter = cast(ptr);
    self_.step_roundtrip_impl().map_err(|err| {
        log::warn!(target: LOG, "roundtrip failed err={}", err);
        benchmark::Error::BenchmarkFailed
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    fn run(opts: Options) {
        let mut impl_ = TerminalFormatter::create(opts).unwrap();

        let mut bench = impl_.benchmark();
        let _ = bench.run(RunMode::Once).unwrap();
    }

    #[test]
    fn terminal_formatter() {
        run(Options::default());
    }

    #[test]
    fn terminal_formatter_roundtrip() {
        run(Options {
            mode: Mode::Roundtrip,
            terminal_rows: 4,
            terminal_cols: 8,
            ..Options::default()
        });
    }

    #[test]
    fn terminal_formatter_terminal_roundtrip() {
        run(Options {
            mode: Mode::Roundtrip,
            formatter: Formatter::Terminal,
            terminal_rows: 4,
            terminal_cols: 8,
            ..Options::default()
        });
    }

    #[test]
    fn terminal_formatter_formats_all_emit_formats_and_regions() {
        for formatter_ in [Formatter::Screen, Formatter::Terminal] {
            for emit in [Emit::Plain, Emit::Vt, Emit::Html] {
                for region in [Region::Screen, Region::Active, Region::History] {
                    run(Options {
                        emit,
                        formatter: formatter_,
                        region,
                        loops: 1,
                        terminal_rows: 4,
                        terminal_cols: 8,
                        ..Options::default()
                    });
                }
            }
        }
    }

    #[test]
    fn terminal_formatter_pin_map() {
        for formatter_ in [Formatter::Screen, Formatter::Terminal] {
            run(Options {
                pin_map: true,
                formatter: formatter_,
                loops: 1,
                terminal_rows: 4,
                terminal_cols: 8,
                ..Options::default()
            });
        }
    }

    /// Not in Zig: the report mode (which prints) runs too.
    #[test]
    fn terminal_formatter_report() {
        let _ = Mode::Report.tag_name();
        run(Options {
            mode: Mode::Report,
            pin_map: true,
            terminal_rows: 4,
            terminal_cols: 8,
            ..Options::default()
        });
    }
}
