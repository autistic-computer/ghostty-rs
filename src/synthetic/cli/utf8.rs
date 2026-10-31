//! Port of `src/synthetic/cli/Utf8.zig`.
//!
//! STATUS: ported

use std::io;

use super::{ActionImpl, Error};
use crate::cli::args::cli_struct;
use crate::synthetic::utf8::Utf8Len;
use crate::synthetic::{self, DefaultPrng, Random};

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// Seed to use for deterministic generation. If unset, a time-based
        /// seed is used by the generic synthetic CLI.
        pub seed: Option<u64> = None,

        /// Relative weight for choosing 1-byte UTF-8 sequences.
        pub weight_one as "weight-one": f64 = 1.0,

        /// Relative weight for choosing 2-byte UTF-8 sequences.
        pub weight_two as "weight-two": f64 = 1.0,

        /// Relative weight for choosing 3-byte UTF-8 sequences.
        pub weight_three as "weight-three": f64 = 1.0,

        /// Relative weight for choosing 4-byte UTF-8 sequences.
        pub weight_four as "weight-four": f64 = 1.0,

        /// Restrict ASCII codepoints to printable characters.
        pub ascii_printable_only as "ascii-printable-only": bool = false,

        /// Probability that an emitted sequence is malformed UTF-8.
        pub invalid_rate as "invalid-rate": f64 = 0.0,
    }
}

#[derive(Debug)]
pub struct Utf8 {
    pub opts: Options,
}

impl ActionImpl for Utf8 {
    type Options = Options;

    /// Create a new terminal stream handler for the given arguments.
    fn create(opts: Options) -> Result<Box<Utf8>, Error> {
        if opts.invalid_rate < 0.0 || opts.invalid_rate > 1.0 {
            return Err(Error::InvalidValue);
        }

        let weights = [
            opts.weight_one,
            opts.weight_two,
            opts.weight_three,
            opts.weight_four,
        ];
        let mut weight_sum: f64 = 0.0;
        for weight in weights {
            if weight < 0.0 {
                return Err(Error::InvalidValue);
            }
            weight_sum += weight;
        }
        if weight_sum <= 0.0 {
            return Err(Error::InvalidValue);
        }

        Ok(Box::new(Utf8 { opts }))
    }

    fn run(&mut self, writer: &mut dyn io::Write, rand: Random) -> Result<(), Error> {
        let mut gen_rand = rand;
        if let Some(seed) = self.opts.seed {
            gen_rand = DefaultPrng::init(seed).random();
        }

        let mut gen = synthetic::Utf8 {
            ascii_printable_only: self.opts.ascii_printable_only,
            invalid_rate: self.opts.invalid_rate,
            ..synthetic::Utf8::init(gen_rand)
        };
        gen.set_p_length(Utf8Len::One, self.opts.weight_one);
        gen.set_p_length(Utf8Len::Two, self.opts.weight_two);
        gen.set_p_length(Utf8Len::Three, self.opts.weight_three);
        gen.set_p_length(Utf8Len::Four, self.opts.weight_four);

        loop {
            if gen.next(writer, 1024).is_err() {
                // stdout closed (BrokenPipe) or fixed buffer full
                // (WriteFailed).
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::FixedWriter;

    #[test]
    fn utf8() {
        let mut impl_ = Utf8::create(Options {
            seed: Some(1),
            ..Options::default()
        })
        .unwrap();

        let prng = DefaultPrng::init(1);
        let rand = prng.random();

        let mut buf = [0u8; 1024];
        let mut writer = FixedWriter::fixed(&mut buf);
        impl_.run(&mut writer, rand).unwrap();
        assert_eq!(1024usize, writer.buffered().len());
    }
}
