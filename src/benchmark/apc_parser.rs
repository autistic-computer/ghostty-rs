//! Port of `src/benchmark/ApcParser.zig`.
//!
//! STATUS: ported
//!
//! This benchmark tests the throughput of APC sequence parsing
//! through the terminal stream: VT state machine dispatch, APC
//! protocol identification, and the protocol command parsers
//! (e.g. Kitty graphics). Completed commands are parsed and then
//! discarded; command execution (image decoding, storage) is not
//! included so this isolates the parsing path.

use std::any::Any;
use std::convert::Infallible;
use std::hint::black_box;
use std::io::BufReader;

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use super::options::{self, DataFile};
use crate::cli::args::cli_struct;
use crate::terminal::{apc, osc, stream};

const LOG: &str = "apc-parser-bench";

pub struct ApcParser {
    pub opts: Options,
    pub stream: Stream,

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

pub type Stream = stream::Stream<Handler>;

/// A stream handler that only processes APC actions, parsing and
/// immediately discarding completed commands.
#[derive(Debug, Default)]
pub struct Handler {
    pub apc: apc::Handler,
}

impl stream::Handler for Handler {
    fn vt(&mut self, action: stream::Action<'_>) {
        match action {
            stream::Action::ApcStart => self.apc.start(),
            stream::Action::ApcPut(value) => self.apc.feed(value),
            stream::Action::ApcPutSlice(value) => self.apc.feed_slice(value.bytes),
            stream::Action::ApcEnd(_) => {
                if let Some(result) = self.apc.end() {
                    let r = result;
                    black_box(&r);
                    drop(r);
                }
            }
            _ => {}
        }
    }

    fn deinit(&mut self) {
        self.apc.deinit();
    }
}

impl BenchmarkImpl for ApcParser {
    type Options = Options;
    type CreateError = Infallible;

    /// Create a new APC parser benchmark for the given arguments.
    fn create(opts: Options) -> Result<Box<ApcParser>, Infallible> {
        Ok(Box::new(ApcParser {
            opts,
            stream: Stream::init(stream::Options {
                allocator: Some(osc::Allocator::GLOBAL),
                ..stream::Options::new(Handler::default())
            }),
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

impl Drop for ApcParser {
    fn drop(&mut self) {
        self.stream.deinit();
    }
}

fn setup(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ApcParser = cast(ptr);

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
    let self_: &mut ApcParser = cast(ptr);
    self_.data_f = None;
}

fn step(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut ApcParser = cast(ptr);

    let Some(f) = self_.data_f.as_mut() else {
        return Ok(());
    };
    let mut r = BufReader::with_capacity(64 * 1024, f);

    // This buffer size matches the read buffer size used by the
    // real IO thread (see termio Exec.zig buffer_capacity) so that
    // the benchmark exercises the stream with realistic chunk sizes.
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = options::read_slice_short(&mut r, &mut buf).map_err(|err| {
            log::warn!(target: LOG, "error reading data file err={}", err);
            benchmark::Error::BenchmarkFailed
        })?;
        if n == 0 {
            break; // EOF reached
        }
        self_.stream.next_slice(&buf[..n]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    #[test]
    fn apc_parser() {
        let mut impl_ = ApcParser::create(Options::default()).unwrap();

        let mut bench = impl_.benchmark();
        let _ = bench.run(RunMode::Once).unwrap();
    }
}
