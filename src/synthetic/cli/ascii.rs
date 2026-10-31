//! Port of `src/synthetic/cli/Ascii.zig`.
//!
//! STATUS: ported

use std::io;

use super::{ActionImpl, Error};
use crate::cli::args::cli_struct;
use crate::synthetic::bytes::{alphabet, Bytes};
use crate::synthetic::Random;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// When nonzero, emit lines whose printable length is uniformly
        /// distributed in `[line-min, line-max]`, each terminated by CR LF.
        /// When zero (the default), emit an unbroken stream of printable
        /// bytes that relies on terminal wrapping.
        pub line_min as "line-min": usize = 0,
        pub line_max as "line-max": usize = 0,
    }
}

fn check_ascii_alphabet(c: u8) -> bool {
    match c {
        b' ' => false,
        // std.ascii.isPrint
        _ => c.is_ascii() && !c.is_ascii_control(),
    }
}

alphabet! {
    pub static ASCII = check_ascii_alphabet;
}

#[derive(Debug)]
pub struct Ascii {
    pub opts: Options,
}

impl ActionImpl for Ascii {
    type Options = Options;

    /// Create a new terminal stream handler for the given arguments.
    fn create(opts: Options) -> Result<Box<Ascii>, Error> {
        Ok(Box::new(Ascii { opts }))
    }

    fn run(&mut self, writer: &mut dyn io::Write, rand: Random) -> Result<(), Error> {
        let line_max = self.opts.line_min.max(self.opts.line_max);
        let lines = line_max > 0;
        let gen = Bytes {
            rand,
            alphabet: Some(*ASCII),
            min_len: if lines { self.opts.line_min.max(1) } else { 1024 },
            max_len: if lines { line_max } else { 1024 },
        };

        loop {
            if write_chunk(&gen, writer, lines).is_err() {
                // stdout closed (BrokenPipe) or fixed buffer full
                // (WriteFailed).
                return Ok(());
            }
        }
    }
}

fn write_chunk(gen: &Bytes, writer: &mut dyn io::Write, lines: bool) -> io::Result<()> {
    let _ = gen.write(writer)?;
    if lines {
        writer.write_all(b"\r\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::{DefaultPrng, FixedWriter};

    #[test]
    fn ascii() {
        let mut impl_ = Ascii::create(Options::default()).unwrap();

        let prng = DefaultPrng::init(1);
        let rand = prng.random();

        let mut buf = [0u8; 1024];
        let mut writer = FixedWriter::fixed(&mut buf);
        impl_.run(&mut writer, rand).unwrap();
    }

    #[test]
    fn ascii_lines() {
        let mut impl_ = Ascii::create(Options {
            line_min: 5,
            line_max: 20,
        })
        .unwrap();

        let prng = DefaultPrng::init(1);
        let rand = prng.random();

        let mut buf = [0u8; 1024];
        let mut writer = FixedWriter::fixed(&mut buf);
        impl_.run(&mut writer, rand).unwrap();

        // Every emitted line respects the configured bounds.
        let data = writer.buffered();
        let parts: Vec<&[u8]> = split_seq(data, b"\r\n");
        for (i, line) in parts.iter().enumerate() {
            // The fixed buffer may end mid-line.
            if i == parts.len() - 1 {
                break;
            }
            assert!(line.len() >= 5);
            assert!(line.len() <= 20);
        }
    }

    fn split_seq<'a>(data: &'a [u8], sep: &[u8]) -> Vec<&'a [u8]> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i + sep.len() <= data.len() {
            if &data[i..i + sep.len()] == sep {
                out.push(&data[start..i]);
                i += sep.len();
                start = i;
            } else {
                i += 1;
            }
        }
        out.push(&data[start..]);
        out
    }
}
