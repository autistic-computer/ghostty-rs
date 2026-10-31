//! Port of `src/synthetic/cli/Osc.zig`.
//!
//! STATUS: ported

use std::io;

use super::{ActionImpl, Error};
use crate::cli::args::{cli_enum, cli_struct};
use crate::synthetic::{self, FixedWriter, Random};

cli_enum! {
    pub enum Style {
        /// Write all OSC data, including ESC ] and ST for end-to-end tests
        Streaming => "streaming",
        /// Only write data, prefixed with a length, used for testing just the
        /// OSC parser.
        Parser => "parser",
    }
}

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// Probability of generating a valid value.
        pub p_valid as "p-valid": f64 = 0.5,

        pub style: Style = Style::Streaming,
    }
}

#[derive(Debug)]
pub struct Osc {
    pub opts: Options,
}

impl ActionImpl for Osc {
    type Options = Options;

    /// Create a new terminal stream handler for the given arguments.
    fn create(opts: Options) -> Result<Box<Osc>, Error> {
        Ok(Box::new(Osc { opts }))
    }

    fn run(&mut self, writer: &mut dyn io::Write, rand: Random) -> Result<(), Error> {
        let mut gen = synthetic::Osc {
            p_valid: self.opts.p_valid,
            ..synthetic::Osc::init(rand)
        };

        let mut buf = [0u8; 1024];
        loop {
            let len = buf.len();
            let mut fixed = FixedWriter::fixed(&mut buf);
            gen.next(&mut fixed, len)?;
            let data = fixed.buffered();
            match self.opts.style {
                Style::Streaming => {
                    if writer.write_all(data).is_err() {
                        return Ok(());
                    }
                }
                Style::Parser => {
                    if writer.write_all(&(data.len() - 3).to_le_bytes()).is_err() {
                        return Ok(());
                    }
                    if writer.write_all(&data[2..data.len() - 1]).is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::DefaultPrng;

    #[test]
    fn osc() {
        let mut impl_ = Osc::create(Options::default()).unwrap();

        let prng = DefaultPrng::init(1);
        let rand = prng.random();

        let mut buf = [0u8; 1024];
        let mut writer = FixedWriter::fixed(&mut buf);
        impl_.run(&mut writer, rand).unwrap();
    }
}
