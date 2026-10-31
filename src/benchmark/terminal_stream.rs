//! Port of `src/benchmark/TerminalStream.zig`.
//!
//! STATUS: ported
//!
//! This benchmark tests the performance of the terminal stream
//! handler from input to terminal state update. This is useful to
//! test general throughput of VT parsing and handling.
//!
//! This uses the full readonly terminal stream handler
//! (terminal.TerminalStream) so every escape sequence updates real
//! terminal state (styles, cursor movement, erases, modes, etc.).
//! This closely mirrors the work done by the real IO thread.
//!
//! For more isolated measurements see the terminal-parser and
//! osc-parser benchmarks.
//!
//! Rust note: the Zig struct holds the terminal and a stream whose
//! handler points at it. Here the terminal is boxed separately (so its
//! address is stable) and the stream borrows it for `'static`; every
//! access to the terminal goes through the stream's handler while the
//! stream exists, and `Drop` tears the stream down before freeing the
//! terminal.

use std::any::Any;
use std::hint::black_box;
use std::ptr::NonNull;

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use super::options::{self, DataFile};
use crate::cli::args::cli_struct;
use crate::terminal::stream_terminal::{Handler, Stream, UnknownSequence};
use crate::terminal::terminal::{InitError, Options as TerminalOptions, Terminal};
use crate::terminal::{osc, stream};

const LOG: &str = "terminal-stream-bench";

pub struct TerminalStream {
    pub opts: Options,

    /// Owned terminal (Zig: `terminal: Terminal`). Only accessed through
    /// `stream.handler.terminal` while the stream is alive.
    terminal: NonNull<Terminal>,

    /// Wrapped in `Option` only so `Drop` can tear it down before the
    /// terminal; it is always `Some` otherwise.
    stream: Option<Stream<'static>>,

    /// The file, opened in the setup function.
    pub data_f: Option<DataFile>,
}

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// The size of the terminal. This affects benchmarking when
        /// dealing with soft line wrapping and the memory impact
        /// of page sizes.
        pub terminal_rows as "terminal-rows": u16 = 80,
        pub terminal_cols as "terminal-cols": u16 = 120,

        /// Enable opt-in continuation tracking on the stream.
        pub continuation_enabled as "continuation-enabled": bool = false,

        /// Maximum continuation suffix retained when tracking is enabled.
        pub continuation_max_bytes as "continuation-max-bytes": usize = 1024 * 1024,

        /// The most bytes to keep from each OSC sequence whose number the
        /// parser does not implement. Zero, the default, discards them. Any
        /// other value also installs an `unknown_sequence` callback that does
        /// nothing, so the benchmark measures the whole path from input to
        /// callback.
        pub osc_unknown_max_bytes as "osc-unknown-max-bytes": usize = 0,

        /// Pre-generated data from ghostty-gen. If this is "-" then
        /// we will read stdin. If this is unset, then we will
        /// do nothing (benchmark is a noop). It'd be more unixy to
        /// use stdin by default but I find that a hanging CLI command
        /// with no interaction is a bit annoying.
        pub data: Option<String> = None,
    }
}

impl TerminalStream {
    /// The stream (always present outside of `Drop`).
    pub fn stream(&mut self) -> &mut Stream<'static> {
        self.stream.as_mut().unwrap()
    }

    /// The terminal, borrowed through the stream handler.
    pub fn terminal(&mut self) -> &mut Terminal {
        self.stream().handler.terminal
    }
}

impl BenchmarkImpl for TerminalStream {
    type Options = Options;
    type CreateError = InitError;

    /// Create a new terminal stream handler for the given arguments.
    fn create(opts: Options) -> Result<Box<TerminalStream>, InitError> {
        let terminal = Box::new(Terminal::init(TerminalOptions {
            rows: opts.terminal_rows,
            cols: opts.terminal_cols,
            ..TerminalOptions::default()
        })?);
        let terminal = NonNull::from(Box::leak(terminal));

        // SAFETY: the terminal is heap allocated, never moves, and is
        // only freed in `Drop` after the stream is gone. The stream's
        // handler is the only path used to access it.
        let mut handler = Handler::init(unsafe { &mut *terminal.as_ptr() });
        if opts.osc_unknown_max_bytes > 0 {
            handler.unknown_sequence = Some(unknown_sequence);
        }
        let stream = Stream::init(stream::Options {
            allocator: Some(osc::Allocator::GLOBAL),
            handler,
            continuation_max_bytes: if opts.continuation_enabled {
                Some(opts.continuation_max_bytes)
            } else {
                None
            },
            osc_unknown_max_bytes: opts.osc_unknown_max_bytes,
        });

        Ok(Box::new(TerminalStream {
            opts,
            terminal,
            stream: Some(stream),
            data_f: None,
        }))
    }

    fn benchmark(&mut self) -> Benchmark<'_> {
        Benchmark::init(
            self,
            VTable {
                step_fn: step,
                setup_fn: Some(setup),
                teardown_fn: Some(teardown),
            },
        )
    }
}

/// Stands in for an application that receives every unknown sequence
/// and does nothing with it.
fn unknown_sequence(_: &mut Handler<'_>, value: UnknownSequence<'_>) {
    match value {
        UnknownSequence::Apc(v) => {
            black_box(v.content.len());
        }
        UnknownSequence::Osc(v) => {
            black_box(v.content.len());
        }
    }
}

impl Drop for TerminalStream {
    fn drop(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            stream.deinit();
        }
        // SAFETY: allocated with `Box` in `create`; the stream that
        // borrowed it has been dropped above.
        drop(unsafe { Box::from_raw(self.terminal.as_ptr()) });
    }
}

fn setup(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalStream = cast(ptr);

    // Always reset our terminal state
    self_.terminal().full_reset();

    // Open our data file to prepare for reading. We can do more validation
    // here eventually.
    debug_assert!(self_.data_f.is_none());
    self_.data_f = options::data_file(self_.opts.data.as_deref()).map_err(|err| {
        log::warn!(target: LOG, "error opening data file err={}", err);
        benchmark::Error::BenchmarkFailed
    })?;
    Ok(())
}

fn teardown(ptr: &mut dyn Any) {
    let self_: &mut TerminalStream = cast(ptr);
    self_.data_f = None;
}

fn step(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalStream = cast(ptr);

    // Get our buffered reader so we're not predominantly
    // waiting on file IO. It'd be better to move this fully into
    // memory. If we're IO bound though that should show up on
    // the benchmark results and... I know writing this that we
    // aren't currently IO bound.
    //
    // Unbuffered: read_slice_short below reads directly into `buf`,
    // avoiding a per-chunk memcpy through an intermediate reader
    // buffer that would pollute the measurement.
    let Some(r) = self_.data_f.as_mut() else {
        return Ok(());
    };
    let stream = self_.stream.as_mut().unwrap();

    // This buffer size matches the read buffer size used by the
    // real IO thread (see termio Exec.zig buffer_capacity) so that
    // the benchmark exercises the stream with realistic chunk sizes.
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = options::read_slice_short(r, &mut buf).map_err(|err| {
            log::warn!(target: LOG, "error reading data file err={}", err);
            benchmark::Error::BenchmarkFailed
        })?;
        if n == 0 {
            break; // EOF reached
        }
        stream.next_slice(&buf[..n]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    #[test]
    fn terminal_stream() {
        let mut impl_ = TerminalStream::create(Options::default()).unwrap();

        let mut bench = impl_.benchmark();
        let _ = bench.run(RunMode::Once).unwrap();
        drop(bench);

        let mut tracked = TerminalStream::create(Options {
            continuation_enabled: true,
            ..Options::default()
        })
        .unwrap();

        let mut tracked_bench = tracked.benchmark();
        let _ = tracked_bench.run(RunMode::Once).unwrap();
    }
}
