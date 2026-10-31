//! Port of `src/benchmark/ScrollbackCompression.zig`.
//!
//! STATUS: ported
//!
//! Benchmarks cold-history compression and restoration on pages owned by a
//! live terminal.
//!
//! Unlike `page-compression`, which measures the standalone codec against raw
//! byte chunks, this benchmark first parses a VT corpus into a real `Terminal`.
//! Its timed operations therefore include the PageList state transition,
//! exact-sized encoded allocation, retained-mapping reclamation, and transparent
//! restoration through `PageList.Node.page`.
//!
//! ## Input
//!
//! `--data` names a pre-generated VT byte stream. Parsing happens during setup
//! and is not part of the benchmark's timed region. Use the same saved corpus,
//! terminal dimensions, and scrollback limit when comparing revisions. In
//! particular, do not pipe a generator into this benchmark: generation and
//! pipe scheduling add noise which can overwhelm the operation being measured.
//!
//! The benchmark always operates on the primary screen's PageList. This keeps
//! the result focused on scrollback even if malformed or incomplete input
//! leaves the terminal in its alternate screen at end of file. `--max-scrollback`
//! is expressed in bytes and defaults to 10 MB.
//!
//! ## Modes
//!
//! * `noop` parses the corpus but performs no timed PageList operation. This is
//!   the common process and setup baseline for the other modes.
//! * `compress` times one complete `compress` invocation.
//! * `incremental` reaches the same final representation through
//!   `compress(.drain)`. It drains the candidate-bounded steps and final
//!   no-work verification pass in the timed region, making cursor and repeated
//!   traversal overhead directly comparable with `compress`.
//! * `restore` compresses cold history during setup, outside the timed region,
//!   then visits every fully historical node through `Node.page`. That public
//!   content-access boundary transparently restores compressed nodes.
//! * `report` performs one compression pass and prints compressed page count,
//!   encoded ratio, and estimated resident-byte savings. It is intended for
//!   inspecting a corpus rather than timing comparisons.
//!
//! A fully historical page is a node strictly before the node containing the
//! top of the active area. The boundary node is deliberately excluded because
//! it can contain both history and active rows. Pages intersecting the current
//! viewport are also excluded so visible contents remain resident. Normal
//! terminal execution compresses eligible pages incrementally after activity
//! becomes idle. The benchmark invokes PageList operations directly so its
//! timed regions exclude the production scheduler's idle delay and
//! renderer-thread coordination.
//!
//! ## Examples
//!
//! Build the benchmark in release mode:
//!
//!     cargo build --release --bin ghostty-bench
//!
//! Inspect the memory reduction for a saved VT corpus:
//!
//!     ghostty-bench +scrollback-compression --mode=report \
//!       --data=/tmp/scrollback.vt --terminal-cols=120 --terminal-rows=80
//!
//! Compare PageList compression and restoration cost. Setup still contributes
//! to full process time, so use a sufficiently large corpus and compare against
//! `noop` with identical arguments:
//!
//!     hyperfine --warmup 3 \
//!       'ghostty-bench +scrollback-compression --mode=noop --data=/tmp/scrollback.vt' \
//!       'ghostty-bench +scrollback-compression --mode=compress --data=/tmp/scrollback.vt' \
//!       'ghostty-bench +scrollback-compression --mode=incremental --data=/tmp/scrollback.vt' \
//!       'ghostty-bench +scrollback-compression --mode=restore --data=/tmp/scrollback.vt'

use std::any::Any;
use std::hint::black_box;
use std::io::{self, BufReader};

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use super::options;
use crate::cli::args::{cli_enum, cli_struct};
use crate::terminal::page_list::{CompressMode, PageList};
use crate::terminal::point;
use crate::terminal::screen_set::Key as ScreenKey;
use crate::terminal::terminal::{InitError, Options as TerminalOptions, Terminal};

const LOG: &str = "scrollback-compression-bench";

pub struct ScrollbackCompression {
    pub opts: Options,
    pub terminal: Terminal,
}

cli_struct! {
    /// Zig also keeps an `_arena` for string option ownership; Rust
    /// options own their strings.
    #[derive(Debug, Clone)]
    pub struct Options {
        /// Select the PageList operation performed inside the timed benchmark.
        pub mode: Mode = Mode::Compress,

        /// Dimensions used to construct the terminal which consumes the corpus.
        /// These affect wrapping and therefore the number and contents of pages.
        pub terminal_rows as "terminal-rows": u16 = 80,
        pub terminal_cols as "terminal-cols": u16 = 120,

        /// Maximum primary-screen scrollback allocation in bytes. PageList rounds
        /// this as required by its page allocation policy.
        pub max_scrollback as "max-scrollback": usize = 10_000_000,

        /// Pre-generated VT corpus. `-` reads stdin, although a regular file is
        /// strongly recommended so comparisons can reuse identical input bytes.
        /// When unset, every mode operates on the initial empty terminal.
        pub data: Option<String> = None,
    }
}

cli_enum! {
    pub enum Mode {
        /// Establish process and setup overhead without a PageList operation.
        Noop => "noop",

        /// Compress every eligible fully historical page once.
        Compress => "compress",

        /// Compress every eligible page through bounded resumable steps.
        Incremental => "incremental",

        /// Restore pages compressed outside the timed region.
        Restore => "restore",

        /// Compress once and print aggregate memory statistics.
        Report => "report",
    }
}

impl BenchmarkImpl for ScrollbackCompression {
    type Options = Options;
    type CreateError = InitError;

    fn create(opts: Options) -> Result<Box<ScrollbackCompression>, InitError> {
        let terminal = Terminal::init(TerminalOptions {
            rows: opts.terminal_rows,
            cols: opts.terminal_cols,
            max_scrollback_bytes: Some(opts.max_scrollback),
            ..TerminalOptions::default()
        })?;
        Ok(Box::new(ScrollbackCompression { opts, terminal }))
    }

    fn benchmark(&mut self) -> Benchmark<'_> {
        let step_fn = match self.opts.mode {
            Mode::Noop => step_noop,
            Mode::Compress => step_compress,
            Mode::Incremental => step_incremental,
            Mode::Restore => step_restore,
            Mode::Report => step_report,
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

impl ScrollbackCompression {
    /// Feed the corpus in the same 64 KiB chunks used by the real IO thread and
    /// terminal-stream benchmark. Parser and file IO costs remain in setup.
    fn load_corpus(&mut self) -> io::Result<()> {
        let Some(data_file) = options::data_file(self.opts.data.as_deref())? else {
            return Ok(());
        };

        let mut stream = self.terminal.vt_stream();

        let mut reader = BufReader::with_capacity(64 * 1024, data_file);

        let mut buf = vec![0u8; 64 * 1024];
        let result = loop {
            let n = match options::read_slice_short(&mut reader, &mut buf) {
                Ok(n) => n,
                Err(err) => break Err(err),
            };
            if n == 0 {
                break Ok(());
            }
            stream.next_slice(&buf[..n]);
        };

        stream.deinit();
        result
    }

    /// Return the primary screen because it is the terminal screen which owns
    /// scrollback. A corpus ending in the alternate screen must not turn this into
    /// an alternate-screen allocation benchmark.
    fn pages(&mut self) -> &mut PageList {
        let screen = self.terminal.screens.get(ScreenKey::Primary).unwrap();
        // SAFETY: the primary screen is owned by our terminal's screen set
        // and lives as long as the terminal borrow.
        &mut unsafe { screen.as_mut() }.pages
    }

    /// Visit all nodes which were eligible for the setup compression pass.
    ///
    /// `page` is intentionally used instead of inspecting the union payload so the
    /// benchmark follows the same transparent restoration boundary as consumers.
    /// Compressible nodes restore here; resident candidates which failed the
    /// opportunistic pass are harmlessly visited through the same boundary.
    fn visit_cold_pages(&mut self) -> usize {
        let page_list = self.pages();
        let active_node = page_list.get_top_left(point::Tag::Active).node;
        let mut visited: usize = 0;
        let mut node_ = page_list.pages.first;
        while let Some(node) = node_ {
            if node == active_node {
                break;
            }
            // SAFETY: nodes of a live page list.
            let n = unsafe { &mut *node.as_ptr() };
            black_box(n.page());
            visited += 1;
            node_ = n.next;
        }
        visited
    }
}

/// Reset the terminal and consume the complete VT corpus before timing starts.
/// Restore mode also prepares its compressed representation here so its timed
/// step contains decoding and mapping writes, but not encoding.
fn setup(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScrollbackCompression = cast(ptr);
    self_.terminal.full_reset();

    self_.load_corpus().map_err(|err| {
        log::warn!(target: LOG, "failed to prepare scrollback compression benchmark err={}", err);
        benchmark::Error::BenchmarkFailed
    })?;

    if self_.opts.mode == Mode::Restore {
        let _ = self_.pages().compress(CompressMode::Full);
    }
    Ok(())
}

fn step_noop(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScrollbackCompression = cast(ptr);
    black_box(&self_.terminal);
    Ok(())
}

fn step_compress(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScrollbackCompression = cast(ptr);
    let _ = self_.pages().compress(CompressMode::Full);
    black_box(&self_.terminal);
    Ok(())
}

fn step_incremental(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScrollbackCompression = cast(ptr);
    let _ = self_.pages().compress(CompressMode::Drain);
    black_box(&self_.terminal);
    Ok(())
}

fn step_restore(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScrollbackCompression = cast(ptr);
    black_box(self_.visit_cold_pages());
    Ok(())
}

fn step_report(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ScrollbackCompression = cast(ptr);
    let _ = self_.pages().compress(CompressMode::Full);
    let memory = self_.pages().memory_stats();

    eprintln!(
        "scrollback-compression compressed={} raw={} encoded={} ratio={:.2}% savings={}",
        memory.compressed_pages,
        memory.decommitted_raw_bytes,
        memory.encoded_bytes,
        percentage(memory.encoded_bytes, memory.decommitted_raw_bytes),
        memory.estimated_savings(),
    );
    Ok(())
}

fn percentage(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    part as f64 * 100.0 / whole as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    #[test]
    fn scrollback_compression() {
        let mut impl_ = ScrollbackCompression::create(Options::default()).unwrap();

        let mut bench = impl_.benchmark();
        let _ = bench.run(RunMode::Once).unwrap();
    }

    fn feed(impl_: &mut ScrollbackCompression) {
        let mut stream = impl_.terminal.vt_stream();
        for _ in 0..256 {
            stream.next_slice(b"aaaa\r\n");
        }
        stream.deinit();
    }

    #[test]
    fn scrollback_compression_restores_cold_terminal_pages() {
        let mut impl_ = ScrollbackCompression::create(Options {
            mode: Mode::Restore,
            terminal_rows: 4,
            // Standard-width pages hold 215 rows. Keeping that row capacity here
            // makes this test corpus small while still producing cold history.
            terminal_cols: 215,
            max_scrollback: 1_000_000,
            ..Options::default()
        })
        .unwrap();

        feed(&mut impl_);

        let _ = impl_.pages().compress(CompressMode::Full);
        let compressed = impl_.pages().memory_stats();
        assert!(compressed.compressed_pages > 0);
        assert!(impl_.visit_cold_pages() >= compressed.compressed_pages);

        // Restored historical pages are resident and therefore eligible for a
        // later explicit pass. This also verifies that the benchmark traversal
        // went through Node.page rather than merely inspecting page metadata.
        let _ = impl_.pages().compress(CompressMode::Full);
        let recompressed = impl_.pages().memory_stats();
        assert_eq!(compressed.compressed_pages, recompressed.compressed_pages);
    }

    #[test]
    fn scrollback_compression_drains_incremental_compression_steps() {
        let mut impl_ = ScrollbackCompression::create(Options {
            mode: Mode::Incremental,
            terminal_rows: 4,
            terminal_cols: 215,
            max_scrollback: 1_000_000,
            ..Options::default()
        })
        .unwrap();

        feed(&mut impl_);

        let _ = impl_.pages().compress(CompressMode::Drain);
        let incremental = impl_.pages().memory_stats();
        assert!(incremental.compressed_pages > 0);

        // Restore the same pages and compare against the monolithic operation.
        // Both paths should produce the same final storage representation.
        let _ = impl_.visit_cold_pages();
        let _ = impl_.pages().compress(CompressMode::Full);
        let monolithic = impl_.pages().memory_stats();
        assert_eq!(monolithic, incremental);
    }
}
