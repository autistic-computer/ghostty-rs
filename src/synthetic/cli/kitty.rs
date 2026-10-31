//! Port of `src/synthetic/cli/Kitty.zig`.
//!
//! STATUS: ported

use std::io;

use super::{ActionImpl, Error};
use crate::cli::args::cli_struct;
use crate::synthetic::{self, FixedWriter, Random};

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// The base64 payload length of each command in bytes, rounded
        /// down to a multiple of four. Kitty clients typically chunk
        /// payloads at 4096 bytes.
        pub data_len as "data-len": usize = 4096,
    }
}

#[derive(Debug)]
pub struct Kitty {
    pub opts: Options,

    /// The buffer a single generated sequence is written into. Sized to
    /// the payload length plus room for the control data and terminator.
    pub buf: Vec<u8>,
}

impl ActionImpl for Kitty {
    type Options = Options;

    /// Create a new Kitty graphics sequence generator for the given arguments.
    fn create(opts: Options) -> Result<Box<Kitty>, Error> {
        let buf = vec![0u8; opts.data_len + 128];
        Ok(Box::new(Kitty { opts, buf }))
    }

    fn run(&mut self, writer: &mut dyn io::Write, rand: Random) -> Result<(), Error> {
        let mut gen = synthetic::Kitty {
            rand,
            data_len: self.opts.data_len,
        };

        loop {
            let len = self.buf.len();
            let mut fixed = FixedWriter::fixed(&mut self.buf);
            gen.next(&mut fixed, len)?;
            if writer.write_all(fixed.buffered()).is_err() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::DefaultPrng;

    #[test]
    fn kitty() {
        let mut impl_ = Kitty::create(Options::default()).unwrap();

        let prng = DefaultPrng::init(1);
        let rand = prng.random();

        let mut buf = [0u8; 8192];
        let mut writer = FixedWriter::fixed(&mut buf);
        impl_.run(&mut writer, rand).unwrap();
    }
}
