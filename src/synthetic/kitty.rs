//! Port of `src/synthetic/Kitty.zig`.
//!
//! STATUS: ported
//!
//! Generates random Kitty graphics protocol APC sequences.
//!
//! The payload is random base64, NOT a valid image: decoding it as
//! PNG will fail. This corpus is meant for benchmarking the stream,
//! APC, and Kitty command parsing paths, which never decode the
//! image data. It does not exercise successful image loading.

use super::bytes::{alphabet, Bytes};
use super::generator::{Error, Generator, Writer};
use super::Random;

/// Generates random Kitty graphics protocol APC sequences.
#[derive(Debug, Clone)]
pub struct Kitty {
    /// Random number generator.
    pub rand: Random,

    /// The base64 payload length for each generated command. This is
    /// rounded down to a multiple of four so the payload is always valid
    /// base64 without requiring padding. Kitty clients typically chunk
    /// payloads at 4096 bytes.
    pub data_len: usize,
}

fn check_base64_alphabet(c: u8) -> bool {
    matches!(c, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/')
}

alphabet! {
    /// The base64 alphabet, without the padding character.
    pub static BASE64_ALPHABET = check_base64_alphabet;
}

const PREFIX: &[u8] = b"\x1b_G";
const ST: &[u8] = b"\x1b\\";

impl Kitty {
    /// A new generator with the Zig default field values.
    pub fn init(rand: Random) -> Kitty {
        Kitty {
            rand,
            data_len: 4096,
        }
    }

    pub fn generator(&mut self) -> &mut dyn Generator {
        self
    }

    /// Get the next Kitty graphics APC sequence: a well-formed transmit
    /// command with a random base64 payload (not a valid image; see the
    /// module comment), including the APC prefix and the ST terminator.
    pub fn next(&mut self, writer: &mut Writer<'_>, max_len: usize) -> Result<(), Error> {
        let control = format!(
            "a=t,f=100,i={};",
            self.rand.int_range_at_most::<32>(1, 1_000_000)
        );

        let overhead = PREFIX.len() + control.len() + ST.len();
        assert!(max_len > overhead);

        let avail = self.data_len.min(max_len - overhead);
        let payload_len = avail - (avail % 4);

        writer.write_all(PREFIX)?;
        writer.write_all(control.as_bytes())?;
        if payload_len > 0 {
            let bytes = Bytes {
                rand: self.rand.clone(),
                alphabet: Some(*BASE64_ALPHABET),
                min_len: payload_len,
                max_len: payload_len,
            };
            let _ = bytes.write(writer)?;
        }
        writer.write_all(ST)?;
        Ok(())
    }
}

impl Generator for Kitty {
    fn next(&mut self, writer: &mut Writer<'_>, max_len: usize) -> Result<(), Error> {
        Kitty::next(self, writer, max_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::{DefaultPrng, FixedWriter};

    #[test]
    fn kitty() {
        let prng = DefaultPrng::init(0);

        let mut buf = [0u8; 8192];
        let len = buf.len();
        let mut writer = FixedWriter::fixed(&mut buf);
        let mut v = Kitty::init(prng.random());
        let gen = v.generator();
        gen.next(&mut writer, len).unwrap();

        let data = writer.buffered();
        assert!(data.starts_with(b"\x1b_Ga=t,f=100,i="));
        assert!(data.ends_with(b"\x1b\\"));
    }
}
