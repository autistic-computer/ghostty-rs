//! Port of `src/benchmark/IsSymbol.zig`.
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
use crate::unicode::symbols_table::TABLE as symbols_table;
use crate::unicode::uucode;

const LOG: &str = "is-symbol-bench";

pub struct IsSymbol {
    pub opts: Options,

    /// The file, opened in the setup function.
    pub data_f: Option<DataFile>,
}

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// Which test to run.
        pub mode: Mode = Mode::Uucode,

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
        /// uucode implementation
        Uucode => "uucode",

        /// Ghostty's table-based approach.
        Table => "table",
    }
}

impl BenchmarkImpl for IsSymbol {
    type Options = Options;
    type CreateError = Infallible;

    /// Create a new terminal stream handler for the given arguments.
    fn create(opts: Options) -> Result<Box<IsSymbol>, Infallible> {
        Ok(Box::new(IsSymbol { opts, data_f: None }))
    }

    fn benchmark(&mut self) -> Benchmark<'_> {
        let step_fn = match self.opts.mode {
            Mode::Uucode => step_uucode,
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
    let self_: &mut IsSymbol = cast(ptr);

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
    let self_: &mut IsSymbol = cast(ptr);
    self_.data_f = None;
}

/// Decode every codepoint of the data file and pass it to `f` (the
/// read loop Zig repeats inline in each step function).
#[inline(always)]
fn for_each_codepoint(self_: &mut IsSymbol, mut f: impl FnMut(u32)) -> Result<(), benchmark::Error> {
    let Some(file) = self_.data_f.as_mut() else {
        return Ok(());
    };
    let mut r = BufReader::with_capacity(4096, file);

    let mut d = Utf8Decoder::default();
    let mut buf = [0u8; 4096];
    loop {
        let n = options::read_slice_short(&mut r, &mut buf).map_err(|err| {
            log::warn!(target: LOG, "error reading data file err={}", err);
            benchmark::Error::BenchmarkFailed
        })?;
        if n == 0 {
            break; // EOF reached
        }

        for &c in &buf[..n] {
            let (cp_, consumed) = d.next(c);
            debug_assert!(consumed);
            if let Some(cp) = cp_ {
                f(cp);
            }
        }
    }
    Ok(())
}

fn step_uucode(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut IsSymbol = cast(ptr);
    for_each_codepoint(self_, |cp| {
        black_box(uucode::is_symbol(cp));
    })
}

fn step_table(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut IsSymbol = cast(ptr);
    for_each_codepoint(self_, |cp| {
        black_box(symbols_table.get(cp));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    #[test]
    fn is_symbol() {
        let mut impl_ = IsSymbol::create(Options::default()).unwrap();

        let mut bench = impl_.benchmark();
        let _ = bench.run(RunMode::Once).unwrap();
    }
}
