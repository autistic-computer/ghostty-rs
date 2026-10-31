//! Port of `src/synthetic/cli/Styled.zig`.
//!
//! STATUS: ported
//!
//! Generates styled terminal content: lines of printable text
//! interspersed with SGR sequences, optional multi-byte UTF-8
//! codepoints, combining marks (multi-codepoint graphemes), and
//! OSC 8 hyperlinks.
//!
//! This exists primarily to build corpora for benchmarking code that
//! consumes terminal *contents* (e.g. the terminal formatter used for
//! clipboard copy and dumps), where we want workloads with controlled
//! amounts of styling and Unicode rather than raw random bytes.
//!
//! Examples:
//!
//! ```text
//!   # Plain ASCII lines (equivalent to `ascii` with lines)
//!   ghostty-gen styled --seed=42
//!
//!   # Heavily styled ASCII
//!   ghostty-gen styled --seed=42 --style-rate=0.8
//!
//!   # Unicode-heavy, no styling
//!   ghostty-gen styled --seed=42 --weight-two=1 --weight-three=1 \
//!     --weight-four=0.5 --grapheme-rate=0.1
//!
//!   # Mixed: styles, Unicode, and hyperlinks
//!   ghostty-gen styled --seed=42 --style-rate=0.3 --weight-two=0.5 \
//!     --weight-three=0.25 --weight-four=0.1 --grapheme-rate=0.05 \
//!     --osc8-rate=0.1
//! ```

use std::io::{self, Write as _};

use super::{ActionImpl, Error};
use crate::cli::args::cli_struct;
use crate::synthetic::{DefaultPrng, Random};

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// Seed to use for deterministic generation. If unset, a time-based
        /// seed is used by the generic synthetic CLI.
        pub seed: Option<u64> = None,

        /// Emit lines whose printable length (in characters, not columns)
        /// is uniformly distributed in `[line-min, line-max]`. Each line is
        /// terminated by CR LF.
        pub line_min as "line-min": usize = 40,
        pub line_max as "line-max": usize = 80,

        /// Probability that a style change (SGR sequence) is emitted at
        /// each run boundary. Zero (default) produces unstyled output.
        pub style_rate as "style-rate": f64 = 0.0,

        /// The length in characters of a "run": a sequence of characters
        /// that share styling. Style/hyperlink changes only happen on run
        /// boundaries.
        pub run_min as "run-min": usize = 4,
        pub run_max as "run-max": usize = 16,

        /// Relative weights for choosing the UTF-8 encoding length of each
        /// generated character. Unlike the raw `utf8` generator, characters
        /// are drawn from curated printable ranges so the output never
        /// contains control characters:
        ///
        ///   one:   printable ASCII (with occasional spaces)
        ///   two:   Latin-1 Supplement / Latin Extended (narrow)
        ///   three: Hiragana/Katakana and CJK ideographs (wide)
        ///   four:  emoji (wide)
        pub weight_one as "weight-one": f64 = 1.0,
        pub weight_two as "weight-two": f64 = 0.0,
        pub weight_three as "weight-three": f64 = 0.0,
        pub weight_four as "weight-four": f64 = 0.0,

        /// Probability that a generated character is followed by a
        /// combining diacritical mark, producing a multi-codepoint
        /// grapheme cluster.
        pub grapheme_rate as "grapheme-rate": f64 = 0.0,

        /// Probability that an OSC 8 hyperlink is toggled (opened or
        /// closed) at each run boundary.
        pub osc8_rate as "osc8-rate": f64 = 0.0,
    }
}

#[derive(Debug)]
pub struct Styled {
    pub opts: Options,
}

#[derive(Debug, Default, Clone, Copy)]
struct State {
    /// True when a non-default SGR style is currently active.
    styled: bool,

    /// True when an OSC 8 hyperlink is currently open.
    link: bool,
}

impl ActionImpl for Styled {
    type Options = Options;

    fn create(opts: Options) -> Result<Box<Styled>, Error> {
        for rate in [opts.style_rate, opts.grapheme_rate, opts.osc8_rate] {
            if !(0.0..=1.0).contains(&rate) {
                return Err(Error::InvalidValue);
            }
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

        if opts.line_min == 0 {
            return Err(Error::InvalidValue);
        }
        if opts.run_min == 0 {
            return Err(Error::InvalidValue);
        }

        Ok(Box::new(Styled { opts }))
    }

    fn run(&mut self, writer: &mut dyn io::Write, rand: Random) -> Result<(), Error> {
        let mut gen_rand = rand;
        if let Some(seed) = self.opts.seed {
            gen_rand = DefaultPrng::init(seed).random();
        }

        let mut state = State::default();
        loop {
            if self.write_line(writer, &gen_rand, &mut state).is_err() {
                // stdout closed (BrokenPipe) or fixed buffer full
                // (WriteFailed).
                return Ok(());
            }
        }
    }
}

impl Styled {
    fn write_line(
        &self,
        writer: &mut dyn io::Write,
        rand: &Random,
        state: &mut State,
    ) -> io::Result<()> {
        let line_min = self.opts.line_min;
        let line_max = line_min.max(self.opts.line_max);
        let run_min = self.opts.run_min;
        let run_max = run_min.max(self.opts.run_max);

        let line_len = rand.int_range_at_most_biased::<64>(line_min as u64, line_max as u64) as usize;
        let mut remaining = line_len;
        while remaining > 0 {
            if self.opts.style_rate > 0.0 && rand.float() < self.opts.style_rate {
                self.write_sgr(writer, rand, state)?;
            }

            if self.opts.osc8_rate > 0.0 && rand.float() < self.opts.osc8_rate {
                self.toggle_link(writer, rand, state)?;
            }

            let run_len = remaining
                .min(rand.int_range_at_most_biased::<64>(run_min as u64, run_max as u64) as usize);
            for _ in 0..run_len {
                self.write_char(writer, rand)?;
            }
            remaining -= run_len;
        }

        // Close any open styling/hyperlink so state never bleeds across
        // lines. This mirrors what well-behaved programs do.
        if state.styled {
            writer.write_all(b"\x1b[0m")?;
            state.styled = false;
        }
        if state.link {
            writer.write_all(b"\x1b]8;;\x1b\\")?;
            state.link = false;
        }

        writer.write_all(b"\r\n")
    }

    fn write_char(&self, writer: &mut dyn io::Write, rand: &Random) -> io::Result<()> {
        let weights = [
            self.opts.weight_one,
            self.opts.weight_two,
            self.opts.weight_three,
            self.opts.weight_four,
        ];

        let cp: u32 = match rand.weighted_index(&weights) {
            // Printable ASCII with occasional word-ish spacing.
            0 => {
                if rand.float() < 0.15 {
                    b' ' as u32
                } else {
                    rand.int_range_at_most_biased::<21>(0x21, 0x7E) as u32
                }
            }

            // Latin-1 Supplement and Latin Extended-A/B (narrow).
            1 => rand.int_range_at_most_biased::<21>(0xC0, 0x24F) as u32,

            // Kana or CJK ideographs (wide).
            2 => {
                if rand.boolean() {
                    rand.int_range_at_most_biased::<21>(0x3041, 0x30FE) as u32
                } else {
                    rand.int_range_at_most_biased::<21>(0x4E00, 0x9FFF) as u32
                }
            }

            // Emoji (wide).
            3 => rand.int_range_at_most_biased::<21>(0x1F600, 0x1F64F) as u32,

            _ => unreachable!(),
        };

        write_codepoint(writer, cp)?;

        // Optionally follow with a combining diacritical mark to form a
        // multi-codepoint grapheme cluster.
        if self.opts.grapheme_rate > 0.0 && rand.float() < self.opts.grapheme_rate {
            let mark = rand.int_range_at_most_biased::<21>(0x300, 0x36F) as u32;
            write_codepoint(writer, mark)?;
        }
        Ok(())
    }

    fn write_sgr(&self, writer: &mut dyn io::Write, rand: &Random, state: &mut State) -> io::Result<()> {
        match rand.uint_less_than::<8>(8) {
            // Reset.
            0 => {
                writer.write_all(b"\x1b[0m")?;
                state.styled = false;
                return Ok(());
            }

            // 16-color foreground.
            1 => {
                let base: u64 = if rand.boolean() { 30 } else { 90 };
                write!(writer, "\x1b[{}m", base + rand.uint_less_than::<8>(8))?;
            }

            // 16-color background.
            2 => {
                let base: u64 = if rand.boolean() { 40 } else { 100 };
                write!(writer, "\x1b[{}m", base + rand.uint_less_than::<8>(8))?;
            }

            // 256-color foreground/background.
            3 => write!(writer, "\x1b[38;5;{}m", rand.int::<8>())?,
            4 => write!(writer, "\x1b[48;5;{}m", rand.int::<8>())?,

            // RGB foreground/background. Components are quantized so the
            // number of unique styles stays bounded (6^3 per channel pair)
            // rather than pathologically unique per cell.
            kind @ (5 | 6) => {
                let layer: u8 = if kind == 5 { 38 } else { 48 };
                let r = rand.uint_less_than::<8>(6) * 51;
                let g = rand.uint_less_than::<8>(6) * 51;
                let b = rand.uint_less_than::<8>(6) * 51;
                write!(writer, "\x1b[{layer};2;{r};{g};{b}m")?;
            }

            // Attributes: bold, italic, underline, inverse, strikethrough.
            7 => {
                let attrs: [u8; 5] = [1, 3, 4, 7, 9];
                write!(
                    writer,
                    "\x1b[{}m",
                    attrs[rand.uint_less_than::<64>(attrs.len() as u64) as usize]
                )?;
            }

            _ => unreachable!(),
        }

        state.styled = true;
        Ok(())
    }

    fn toggle_link(&self, writer: &mut dyn io::Write, rand: &Random, state: &mut State) -> io::Result<()> {
        if state.link {
            writer.write_all(b"\x1b]8;;\x1b\\")?;
            state.link = false;
            return Ok(());
        }

        write!(
            writer,
            "\x1b]8;;http://example.com/{}\x1b\\",
            rand.uint_less_than::<16>(1024)
        )?;
        state.link = true;
        Ok(())
    }
}

/// Zig `writer.print("{u}", .{cp})`: write the UTF-8 encoding of `cp`.
fn write_codepoint(writer: &mut dyn io::Write, cp: u32) -> io::Result<()> {
    // All generated ranges exclude surrogates; Zig would print U+FFFD.
    let c = char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER);
    let mut buf = [0u8; 4];
    writer.write_all(c.encode_utf8(&mut buf).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::FixedWriter;

    #[test]
    fn styled() {
        let mut impl_ = Styled::create(Options {
            seed: Some(1),
            ..Options::default()
        })
        .unwrap();

        let prng = DefaultPrng::init(1);
        let rand = prng.random();

        let mut buf = [0u8; 4096];
        let mut writer = FixedWriter::fixed(&mut buf);
        impl_.run(&mut writer, rand).unwrap();
        let output = writer.buffered();
        assert!(!output.is_empty());

        // Default options: plain printable ASCII lines only.
        for &byte in output {
            assert!(byte == b'\r' || byte == b'\n' || (0x20..0x7F).contains(&byte));
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn styled_styled_output() {
        let mut impl_ = Styled::create(Options {
            seed: Some(1),
            style_rate: 0.5,
            osc8_rate: 0.2,
            ..Options::default()
        })
        .unwrap();

        let prng = DefaultPrng::init(1);
        let rand = prng.random();

        let mut buf = vec![0u8; 16384];
        let mut writer = FixedWriter::fixed(&mut buf);
        impl_.run(&mut writer, rand).unwrap();
        let output = writer.buffered();

        // Must contain SGR and OSC 8 sequences.
        assert!(contains(output, b"\x1b["));
        assert!(contains(output, b"\x1b]8;;"));
    }

    #[test]
    fn styled_unicode_output() {
        let mut impl_ = Styled::create(Options {
            seed: Some(1),
            weight_two: 1.0,
            weight_three: 1.0,
            weight_four: 1.0,
            grapheme_rate: 0.25,
            ..Options::default()
        })
        .unwrap();

        let prng = DefaultPrng::init(1);
        let rand = prng.random();

        let mut buf = vec![0u8; 16384];
        let mut writer = FixedWriter::fixed(&mut buf);
        impl_.run(&mut writer, rand).unwrap();
        let output = writer.buffered();

        // No escapes, valid UTF-8 (modulo a possibly truncated tail from
        // the fixed buffer filling up mid-sequence).
        assert!(!output.contains(&0x1B));
        let mut end = output.len();
        while end > 0 && output[end - 1] & 0xC0 == 0x80 {
            end -= 1;
        }
        if end > 0 && output[end - 1] >= 0xC0 {
            end -= 1;
        }
        assert!(std::str::from_utf8(&output[..end]).is_ok());
    }

    #[test]
    fn styled_invalid_options() {
        assert_eq!(
            Error::InvalidValue,
            Styled::create(Options {
                style_rate: 1.5,
                ..Options::default()
            })
            .unwrap_err()
        );
        assert_eq!(
            Error::InvalidValue,
            Styled::create(Options {
                weight_one: 0.0,
                ..Options::default()
            })
            .unwrap_err()
        );
        assert_eq!(
            Error::InvalidValue,
            Styled::create(Options {
                line_min: 0,
                ..Options::default()
            })
            .unwrap_err()
        );
    }
}
