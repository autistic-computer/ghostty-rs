//! Port of `src/benchmark/GraphemeBreak.zig`.
//!
//! STATUS: ported
//!
//! This benchmark tests the throughput of grapheme break calculation.
//! This is a common operation in terminal character printing for terminals
//! that support grapheme clustering.

use std::any::Any;
use std::convert::Infallible;
use std::hint::black_box;
use std::io::BufReader;

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use super::options::{self, DataFile};
use crate::cli::args::{cli_enum, cli_struct};
use crate::terminal::utf8_decoder::Utf8Decoder;
use crate::unicode;
use crate::unicode::uucode::grapheme::BreakState;

const LOG: &str = "terminal-stream-bench";

pub struct GraphemeBreak {
    pub opts: Options,

    /// The file, opened in the setup function.
    pub data_f: Option<DataFile>,
}

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// The type of codepoint width calculation to use.
        pub mode: Mode = Mode::Table,

        /// The data to read as a filepath. If this is "-" then
        /// we will read stdin. If this is unset, then we will
        /// do nothing (benchmark is a noop). It'd be more unixy to
        /// use stdin by default but I find that a hanging CLI command
        /// with no interaction is a bit annoying.
        pub data: Option<String> = None,
    }
}

cli_enum! {
    pub enum Mode {
        /// The baseline mode copies the data from the fd into a buffer. This
        /// is used to show the minimal overhead of reading the fd into memory
        /// and establishes a baseline for the other modes.
        Noop => "noop",

        /// Ghostty's table-based approach.
        Table => "table",
    }
}

impl BenchmarkImpl for GraphemeBreak {
    type Options = Options;
    type CreateError = Infallible;

    /// Create a new terminal stream handler for the given arguments.
    fn create(opts: Options) -> Result<Box<GraphemeBreak>, Infallible> {
        Ok(Box::new(GraphemeBreak { opts, data_f: None }))
    }

    fn benchmark(&mut self) -> Benchmark<'_> {
        let step_fn = match self.opts.mode {
            Mode::Noop => step_noop,
            Mode::Table => step_table,
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

fn setup(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut GraphemeBreak = cast(ptr);

    // Open our data file to prepare for reading. We can do more
    // validation here eventually.
    debug_assert!(self_.data_f.is_none());
    self_.data_f = options::data_file(self_.opts.data.as_deref()).map_err(|err| {
        log::warn!(target: LOG, "error opening data file err={}", err);
        benchmark::Error::BenchmarkFailed
    })?;
    Ok(())
}

fn teardown(ptr: &mut dyn Any) {
    let self_: &mut GraphemeBreak = cast(ptr);
    self_.data_f = None;
}

/// Read the data file in 4 KiB chunks and pass each one to `f`.
#[inline(always)]
fn for_each_chunk(self_: &mut GraphemeBreak, mut f: impl FnMut(&[u8])) -> Result<(), benchmark::Error> {
    let Some(file) = self_.data_f.as_mut() else {
        return Ok(());
    };
    let mut r = BufReader::with_capacity(4096, file);

    let mut buf = [0u8; 4096];
    loop {
        let n = options::read_slice_short(&mut r, &mut buf).map_err(|err| {
            log::warn!(target: LOG, "error reading data file err={}", err);
            benchmark::Error::BenchmarkFailed
        })?;
        if n == 0 {
            break; // EOF reached
        }
        f(&buf[..n]);
    }
    Ok(())
}

fn step_noop(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut GraphemeBreak = cast(ptr);
    let mut d = Utf8Decoder::default();
    for_each_chunk(self_, |chunk| {
        for &c in chunk {
            black_box(d.next(c));
        }
    })
}

fn step_table(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut GraphemeBreak = cast(ptr);
    let mut d = Utf8Decoder::default();
    let mut state = BreakState::default();
    let mut cp1: u32 = 0;
    for_each_chunk(self_, |chunk| {
        for &c in chunk {
            let (cp_, consumed) = d.next(c);
            debug_assert!(consumed);
            if let Some(cp2) = cp_ {
                black_box(unicode::grapheme_break(cp1, cp2, &mut state));
                cp1 = cp2;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    #[test]
    fn grapheme_break() {
        let mut impl_ = GraphemeBreak::create(Options::default()).unwrap();

        let mut bench = impl_.benchmark();
        let _ = bench.run(RunMode::Once).unwrap();
    }
}
