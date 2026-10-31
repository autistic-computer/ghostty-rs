//! Port of `src/benchmark/TerminalParser.zig`.
//!
//! STATUS: ported
//!
//! This benchmark tests the throughput of the terminal escape code parser.

use std::any::Any;
use std::convert::Infallible;
use std::io::BufReader;

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use super::options::{self, DataFile};
use crate::cli::args::cli_struct;
use crate::terminal::Parser;

const LOG: &str = "terminal-stream-bench";

pub struct TerminalParser {
    pub opts: Options,

    /// The file, opened in the setup function.
    pub data_f: Option<DataFile>,
}

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// The data to read as a filepath. If this is "-" then
        /// we will read stdin. If this is unset, then we will
        /// do nothing (benchmark is a noop). It'd be more unixy to
        /// use stdin by default but I find that a hanging CLI command
        /// with no interaction is a bit annoying.
        pub data: Option<String> = None,
    }
}

impl BenchmarkImpl for TerminalParser {
    type Options = Options;
    type CreateError = Infallible;

    fn create(opts: Options) -> Result<Box<TerminalParser>, Infallible> {
        Ok(Box::new(TerminalParser { opts, data_f: None }))
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

fn setup(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalParser = cast(ptr);

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
    let self_: &mut TerminalParser = cast(ptr);
    self_.data_f = None;
}

fn step(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut TerminalParser = cast(ptr);

    // Get our buffered reader so we're not predominantly
    // waiting on file IO. It'd be better to move this fully into
    // memory. If we're IO bound though that should show up on
    // the benchmark results and... I know writing this that we
    // aren't currently IO bound.
    let Some(f) = self_.data_f.as_mut() else {
        return Ok(());
    };
    let mut r = BufReader::with_capacity(4096, f);

    let mut p = Parser::init();

    let mut buf = [0u8; 4096];
    loop {
        let n = options::read_slice_short(&mut r, &mut buf).map_err(|err| {
            log::warn!(target: LOG, "error reading data file err={}", err);
            benchmark::Error::BenchmarkFailed
        })?;
        if n == 0 {
            break; // EOF reached
        }
        parse_all(&mut p, &buf[..n]);
    }
    Ok(())
}

/// Separated from `step` so that the tight per-byte loop gets its own
/// function alignment, insulating it from code-layout changes elsewhere
/// in the binary that would otherwise shift its cache-line placement.
#[inline(never)]
fn parse_all(p: &mut Parser, data: &[u8]) {
    for &c in data {
        let actions = p.next(c);
        let _ = actions;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    #[test]
    fn terminal_parser() {
        let mut impl_ = TerminalParser::create(Options::default()).unwrap();

        let mut bench = impl_.benchmark();
        let _ = bench.run(RunMode::Once).unwrap();
    }
}
