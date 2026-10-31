//! Port of `src/benchmark/OscParser.zig`.
//!
//! STATUS: ported
//!
//! This benchmark tests the throughput of the OSC parser.

use std::any::Any;
use std::convert::Infallible;
use std::hint::black_box;
use std::io::{self, BufReader, Read};

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use super::options::{self, DataFile};
use crate::cli::args::cli_struct;
use crate::terminal::osc::{self, Parser};

const LOG: &str = "osc-parser-bench";

pub struct OscParser {
    pub opts: Options,

    /// The file, opened in the setup function.
    pub data_f: Option<DataFile>,

    pub parser: Parser,
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

        /// The most bytes to keep from each OSC sequence whose number the
        /// parser does not implement. Zero, the default, discards them like
        /// the parser does by default. See `osc.Parser.unknown_max_bytes`.
        pub unknown_max_bytes as "unknown-max-bytes": usize = 0,
    }
}

impl BenchmarkImpl for OscParser {
    type Options = Options;
    type CreateError = Infallible;

    /// Create a new terminal stream handler for the given arguments.
    fn create(opts: Options) -> Result<Box<OscParser>, Infallible> {
        let mut ptr = Box::new(OscParser {
            opts,
            data_f: None,
            parser: Parser::init(Some(osc::Allocator::GLOBAL)),
        });
        ptr.parser.unknown_max_bytes = ptr.opts.unknown_max_bytes;
        Ok(ptr)
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

impl Drop for OscParser {
    fn drop(&mut self) {
        self.parser.deinit();
    }
}

fn setup(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut OscParser = cast(ptr);

    // Open our data file to prepare for reading. We can do more
    // validation here eventually.
    debug_assert!(self_.data_f.is_none());
    self_.data_f = options::data_file(self_.opts.data.as_deref()).map_err(|err| {
        log::warn!(target: LOG, "error opening data file err={}", err);
        benchmark::Error::BenchmarkFailed
    })?;
    self_.parser.reset();
    Ok(())
}

fn teardown(ptr: &mut dyn Any) {
    let self_: &mut OscParser = cast(ptr);
    self_.data_f = None;
}

/// Map a read result: end of stream ends the step, other errors fail.
fn read_exact_or_end<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<bool, benchmark::Error> {
    match r.read_exact(buf) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(_) => Err(benchmark::Error::BenchmarkFailed),
    }
}

fn step(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut OscParser = cast(ptr);

    let Some(f) = self_.data_f.as_mut() else {
        return Ok(());
    };
    let mut r = BufReader::with_capacity(4096, f);

    let mut osc_buf = [0u8; 4096];
    loop {
        let mut len_buf = [0u8; std::mem::size_of::<usize>()];
        if !read_exact_or_end(&mut r, &mut len_buf)? {
            return Ok(());
        }
        let len = usize::from_le_bytes(len_buf);

        if len > osc_buf.len() {
            return Err(benchmark::Error::BenchmarkFailed);
        }

        if !read_exact_or_end(&mut r, &mut osc_buf[..len])? {
            return Ok(());
        }

        for &c in &osc_buf[..len] {
            self_.parser.next(c);
        }
        black_box(self_.parser.end(Some(0x07)));
        self_.parser.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    #[test]
    fn osc_parser() {
        let mut impl_ = OscParser::create(Options::default()).unwrap();

        let mut bench = impl_.benchmark();
        let _ = bench.run(RunMode::Once).unwrap();
    }
}
