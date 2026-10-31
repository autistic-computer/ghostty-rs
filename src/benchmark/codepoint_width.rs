//! Port of `src/benchmark/CodepointWidth.zig`.
//!
//! STATUS: ported
//!
//! This benchmark tests the throughput of codepoint width calculation.
//! This is a common operation in terminal character printing and the
//! motivating factor to write this benchmark was discovering that our
//! codepoint width function was 30% of the runtime of every character
//! print.

use std::any::Any;
use std::convert::Infallible;
use std::ffi::c_int;
use std::hint::black_box;
use std::io::BufReader;

use super::benchmark::{self, cast, Benchmark, VTable};
use super::cli::BenchmarkImpl;
use super::options::{self, DataFile};
use crate::cli::args::{cli_enum, cli_struct};
use crate::simd;
use crate::terminal::utf8_decoder::Utf8Decoder;
use crate::unicode::TABLE as table;

const LOG: &str = "terminal-stream-bench";

pub struct CodepointWidth {
    pub opts: Options,

    /// The file, opened in the setup function.
    pub data_f: Option<DataFile>,
}

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// The type of codepoint width calculation to use.
        pub mode: Mode = Mode::Noop,

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

        /// libc wcwidth
        Wcwidth => "wcwidth",

        /// Our SIMD implementation.
        Simd => "simd",

        /// Test our lookup table implementation.
        Table => "table",
    }
}

impl BenchmarkImpl for CodepointWidth {
    type Options = Options;
    type CreateError = Infallible;

    /// Create a new terminal stream handler for the given arguments.
    fn create(opts: Options) -> Result<Box<CodepointWidth>, Infallible> {
        Ok(Box::new(CodepointWidth { opts, data_f: None }))
    }

    fn benchmark(&mut self) -> Benchmark<'_> {
        let step_fn = match self.opts.mode {
            Mode::Noop => step_noop,
            Mode::Wcwidth => step_wcwidth,
            Mode::Table => step_table,
            Mode::Simd => step_simd,
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
    let self_: &mut CodepointWidth = cast(ptr);

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
    let self_: &mut CodepointWidth = cast(ptr);
    self_.data_f = None;
}

fn step_noop(_ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    Ok(())
}

extern "C" {
    fn wcwidth(c: u32) -> c_int;
}

/// Decode every codepoint of the data file and pass it to `f`. This is
/// the read loop shared by every non-noop step (Zig repeats it inline
/// in each step function).
#[inline(always)]
fn for_each_codepoint(
    self_: &mut CodepointWidth,
    mut f: impl FnMut(u32),
) -> Result<(), benchmark::Error> {
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

fn step_wcwidth(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut CodepointWidth = cast(ptr);
    for_each_codepoint(self_, |cp| {
        // SAFETY: wcwidth is a pure libc function.
        black_box(unsafe { wcwidth(cp) });
    })
}

fn step_table(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut CodepointWidth = cast(ptr);
    for_each_codepoint(self_, |cp| {
        // This is the same trick we do in terminal.zig so we
        // keep it here.
        black_box(if cp <= 0xFF { 1 } else { table.get(cp).width });
    })
}

fn step_simd(ptr: &mut dyn Any) -> Result<(), benchmark::Error> {
    let self_: &mut CodepointWidth = cast(ptr);
    for_each_codepoint(self_, |cp| {
        black_box(simd::codepoint_width(cp));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::benchmark::RunMode;

    #[test]
    fn codepoint_width() {
        let mut impl_ = CodepointWidth::create(Options::default()).unwrap();

        let mut bench = impl_.benchmark();
        let _ = bench.run(RunMode::Once).unwrap();
    }
}
