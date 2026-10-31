//! Port of `src/synthetic/Osc.zig`.
//!
//! STATUS: ported
//!
//! Generates random terminal OSC requests.

use std::io::Write as _;

use super::bytes::{alphabet, Bytes};
use super::generator::{Error, FixedWriter, Generator, Writer};
use super::Random;
use crate::os::string_encoding::url_percent_encode;

/// Valid OSC request kinds that can be generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidKind {
    ChangeWindowTitle,
    PromptStart,
    PromptEnd,
    EndOfInput,
    EndOfCommand,
    RxvtNotify,
    MouseShape,
    ClipboardOperation,
    ReportPwd,
    HyperlinkStart,
    HyperlinkEnd,
    ConemuProgress,
    Iterm2Notification,
}

impl ValidKind {
    /// All kinds in declaration order (Zig `EnumArray` indexer order).
    pub const ALL: [ValidKind; 13] = [
        ValidKind::ChangeWindowTitle,
        ValidKind::PromptStart,
        ValidKind::PromptEnd,
        ValidKind::EndOfInput,
        ValidKind::EndOfCommand,
        ValidKind::RxvtNotify,
        ValidKind::MouseShape,
        ValidKind::ClipboardOperation,
        ValidKind::ReportPwd,
        ValidKind::HyperlinkStart,
        ValidKind::HyperlinkEnd,
        ValidKind::ConemuProgress,
        ValidKind::Iterm2Notification,
    ];
}

/// Invalid OSC request kinds that can be generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidKind {
    /// Literally random bytes. Might even be valid, but probably not.
    Random,

    /// A good prefix, but ultimately invalid format.
    GoodPrefix,
}

impl InvalidKind {
    /// All kinds in declaration order (Zig `EnumArray` indexer order).
    pub const ALL: [InvalidKind; 2] = [InvalidKind::Random, InvalidKind::GoodPrefix];
}

/// Generates random terminal OSC requests.
#[derive(Debug, Clone)]
pub struct Osc {
    /// Random number generator.
    pub rand: Random,

    /// Probability of a valid OSC sequence being generated.
    pub p_valid: f64,

    /// Probabilities of specific valid or invalid OSC request kinds.
    /// The probabilities are weighted relative to each other, so they
    /// can sum greater than 1.0. A kind of weight 1.0 and a kind of
    /// weight 2.0 will have a 2:1 chance of the latter being selected.
    ///
    /// Indexed by the kind's declaration order (Zig `std.enums.EnumArray`).
    pub p_valid_kind: [f64; 13],
    pub p_invalid_kind: [f64; 2],
}

const ESC: u8 = 0x1B;
const BEL: u8 = 0x07;

/// Zig `std.ascii.isPrint`.
fn is_print(c: u8) -> bool {
    c.is_ascii() && !c.is_ascii_control()
}

fn check_kv_alphabet(c: u8) -> bool {
    match c {
        ESC | BEL | b';' | b'=' => false,
        _ => is_print(c),
    }
}

alphabet! {
    /// The alphabet for random bytes in OSC key/value pairs (omitting 0x1B,
    /// 0x07, ';', '=').
    pub static KV_ALPHABET = check_kv_alphabet;
}

fn check_osc_alphabet(c: u8) -> bool {
    !matches!(c, ESC | BEL)
}

alphabet! {
    /// The alphabet for random bytes in OSCs (omitting 0x1B and 0x07).
    pub static OSC_ALPHABET = check_osc_alphabet;
}
alphabet! {
    pub static ASCII_ALPHABET = is_print;
}
alphabet! {
    pub static ALPHABETIC_ALPHABET = |c: u8| c.is_ascii_alphabetic();
}
alphabet! {
    pub static ALPHANUMERIC_ALPHABET = |c: u8| c.is_ascii_alphanumeric();
}

const OSC: &[u8] = &[ESC, b']'];
const ST: &[u8] = &[BEL];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Validity {
    Valid,
    Invalid,
}

impl Osc {
    /// A new generator with the Zig default field values.
    pub fn init(rand: Random) -> Osc {
        Osc {
            rand,
            p_valid: 1.0,
            p_valid_kind: [1.0; 13],
            p_invalid_kind: [1.0; 2],
        }
    }

    pub fn generator(&mut self) -> &mut dyn Generator {
        self
    }

    /// Get the next OSC request in bytes. The generated OSC request will
    /// have the prefix `ESC ]` and the terminator `BEL` (0x07).
    ///
    /// This will generate both valid and invalid OSC requests (based on
    /// the `p_valid` probability value). Invalid requests still have the
    /// prefix and terminator, but the content in between is not a valid
    /// OSC request.
    ///
    /// The buffer must be at least 3 bytes long to accommodate the
    /// prefix and terminator.
    pub fn next(&mut self, writer: &mut Writer<'_>, max_len: usize) -> Result<(), Error> {
        assert!(max_len >= 3);
        writer.write_all(OSC)?;
        self.next_unwrapped(writer, max_len - (OSC.len() + ST.len()))?;
        writer.write_all(ST)?;
        Ok(())
    }

    fn next_unwrapped(&mut self, writer: &mut Writer<'_>, max_len: usize) -> Result<(), Error> {
        match self.choose_validity() {
            Validity::Valid => {
                let idx = self.rand.weighted_index(&self.p_valid_kind);
                self.next_unwrapped_valid_exact(writer, ValidKind::ALL[idx], max_len)
            }

            Validity::Invalid => {
                let idx = self.rand.weighted_index(&self.p_invalid_kind);
                self.next_unwrapped_invalid_exact(writer, InvalidKind::ALL[idx], max_len)
            }
        }
    }

    fn next_unwrapped_valid_exact(
        &self,
        writer: &mut Writer<'_>,
        k: ValidKind,
        max_len: usize,
    ) -> Result<(), Error> {
        match k {
            ValidKind::ChangeWindowTitle => 'change_window_title: {
                if max_len < 3 {
                    break 'change_window_title;
                }
                writer.write_all(b"0;")?; // Set window title
                self.bytes().at_most(max_len - 3).format(writer)?;
            }

            ValidKind::PromptStart => 'prompt_start: {
                if max_len < 4 {
                    break 'prompt_start;
                }
                let mut remaining = max_len;

                writer.write_all(b"133;A")?; // Start prompt
                remaining -= 4;

                // aid
                if self.rand.boolean() {
                    'aid: {
                        if remaining < 6 {
                            break 'aid;
                        }
                        writer.write_all(b";aid=")?;
                        remaining -= 5;
                        remaining -= self
                            .bytes()
                            .new_alphabet(Some(*KV_ALPHABET))
                            .at_most(16.min(remaining))
                            .write(writer)?;
                    }
                }

                // redraw
                if self.rand.boolean() {
                    'redraw: {
                        if remaining < 9 {
                            break 'redraw;
                        }
                        writer.write_all(b";redraw=")?;
                        if self.rand.boolean() {
                            writer.write_all(b"1")?;
                        } else {
                            writer.write_all(b"0")?;
                        }
                        remaining -= 9;
                    }
                }
                let _ = remaining;
            }

            ValidKind::PromptEnd => 'prompt_end: {
                if max_len < 4 {
                    break 'prompt_end;
                }
                writer.write_all(b"133;B")?; // End prompt
            }

            ValidKind::EndOfInput => 'end_of_input: {
                if max_len < 5 {
                    break 'end_of_input;
                }
                let mut remaining = max_len;
                writer.write_all(b"133;C")?; // End prompt
                remaining -= 5;
                if self.rand.boolean() {
                    'cmdline: {
                        const PREFIX: &[u8] = b";cmdline_url=";
                        if remaining < PREFIX.len() + 1 {
                            break 'cmdline;
                        }
                        writer.write_all(PREFIX)?;
                        remaining -= PREFIX.len();
                        let mut buf = [0u8; 128];
                        let buf_len = buf.len();
                        let mut w = FixedWriter::fixed(&mut buf);
                        self.bytes()
                            .new_alphabet(Some(*ASCII_ALPHABET))
                            .at_most(remaining.min(buf_len))
                            .format(&mut w)?;
                        url_percent_encode(writer, w.buffered())?;
                        remaining -= w.buffered().len();
                    }
                }
                let _ = remaining;
            }

            ValidKind::EndOfCommand => 'end_of_command: {
                if max_len < 4 {
                    break 'end_of_command;
                }
                writer.write_all(b"133;D")?; // End prompt
                if self.rand.boolean() {
                    'exit_code: {
                        if max_len < 7 {
                            break 'exit_code;
                        }
                        write!(writer, ";{}", self.rand.int::<8>())?;
                    }
                }
            }

            ValidKind::MouseShape => 'mouse_shape: {
                if max_len < 4 {
                    break 'mouse_shape;
                }
                writer.write_all(b"22;")?; // Start prompt
                self.bytes()
                    .new_alphabet(Some(*ALPHABETIC_ALPHABET))
                    .at_most(32.min(max_len - 3))
                    .format(writer)?;
            }

            ValidKind::RxvtNotify => 'rxvt_notify: {
                const PREFIX: &[u8] = b"777;notify;";
                if max_len < PREFIX.len() {
                    break 'rxvt_notify;
                }
                let mut remaining = max_len;
                writer.write_all(PREFIX)?;
                remaining -= PREFIX.len();
                remaining -= self
                    .bytes()
                    .new_alphabet(Some(*KV_ALPHABET))
                    .at_most((remaining - 2).min(32))
                    .write(writer)?;
                writer.write_all(b";")?;
                remaining -= 1;
                remaining -= self
                    .bytes()
                    .new_alphabet(Some(*OSC_ALPHABET))
                    .at_most(remaining)
                    .write(writer)?;
                let _ = remaining;
            }

            ValidKind::ClipboardOperation => {
                writer.write_all(b"52;")?;
                let mut remaining = max_len - 3;
                if self.rand.boolean() {
                    remaining -= self
                        .bytes()
                        .new_alphabet(Some(*ALPHABETIC_ALPHABET))
                        .at_most(1)
                        .write(writer)?;
                }
                writer.write_all(b";")?;
                remaining -= 1;
                if self.rand.boolean() {
                    remaining -= self
                        .bytes()
                        .new_alphabet(Some(*OSC_ALPHABET))
                        .at_most(remaining)
                        .write(writer)?;
                }
                let _ = remaining;
            }

            ValidKind::ReportPwd => 'report_pwd: {
                const PREFIX: &[u8] = b"7;file://localhost";
                if max_len < PREFIX.len() {
                    break 'report_pwd;
                }
                let mut remaining = max_len;
                writer.write_all(PREFIX)?;
                remaining -= PREFIX.len();
                for _ in 0..self.rand.int_range_at_most::<64>(2, 5) {
                    writer.write_all(b"/")?;
                    remaining -= 1;
                    remaining -= self
                        .bytes()
                        .new_alphabet(Some(*ALPHANUMERIC_ALPHABET))
                        .at_most(16.min(remaining))
                        .write(writer)?;
                }
            }

            ValidKind::HyperlinkStart => {
                writer.write_all(b"8;")?;
                if self.rand.boolean() {
                    writer.write_all(b"id=")?;
                    self.bytes()
                        .new_alphabet(Some(*ALPHANUMERIC_ALPHABET))
                        .at_most(16)
                        .format(writer)?;
                }
                writer.write_all(b";https://localhost")?;
                for _ in 0..self.rand.int_range_at_most::<64>(2, 5) {
                    writer.write_all(b"/")?;
                    self.bytes()
                        .new_alphabet(Some(*ALPHANUMERIC_ALPHABET))
                        .at_most(16)
                        .format(writer)?;
                }
            }

            ValidKind::HyperlinkEnd => 'hyperlink_end: {
                if max_len < 3 {
                    break 'hyperlink_end;
                }
                writer.write_all(b"8;;")?;
            }

            ValidKind::ConemuProgress => {
                writer.write_all(b"9;")?;
                match self.rand.int_range_at_most::<3>(0, 4) {
                    c @ (0 | 3) => {
                        write!(writer, ";{c}")?;
                    }
                    c @ (1 | 2 | 4) => {
                        if self.rand.boolean() {
                            write!(writer, ";{c}")?;
                        } else {
                            write!(writer, ";{};{}", c, self.rand.int_range_at_most::<8>(0, 100))?;
                        }
                    }
                    _ => unreachable!(),
                }
            }

            ValidKind::Iterm2Notification => 'iterm2_notification: {
                if max_len < 3 {
                    break 'iterm2_notification;
                }
                // add a prefix to ensure that this is not interpreted as a ConEmu OSC
                writer.write_all(b"9;_")?;
                self.bytes()
                    .new_alphabet(Some(*ASCII_ALPHABET))
                    .at_most(max_len - 3)
                    .format(writer)?;
            }
        }
        Ok(())
    }

    fn next_unwrapped_invalid_exact(
        &self,
        writer: &mut Writer<'_>,
        k: InvalidKind,
        max_len: usize,
    ) -> Result<(), Error> {
        match k {
            InvalidKind::Random => {
                self.bytes().at_most(max_len).format(writer)?;
            }

            InvalidKind::GoodPrefix => {
                writer.write_all(b"133;")?;
                self.bytes().at_most(max_len - 4).format(writer)?;
            }
        }
        Ok(())
    }

    fn bytes(&self) -> Bytes {
        Bytes {
            alphabet: Some(*OSC_ALPHABET),
            ..Bytes::init(self.rand.clone())
        }
    }

    /// Choose whether to generate a valid or invalid OSC request based
    /// on the validity probability.
    fn choose_validity(&self) -> Validity {
        if self.rand.float() > self.p_valid {
            Validity::Invalid
        } else {
            Validity::Valid
        }
    }
}

impl Generator for Osc {
    fn next(&mut self, writer: &mut Writer<'_>, max_len: usize) -> Result<(), Error> {
        Osc::next(self, writer, max_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::DefaultPrng;
    use crate::terminal::osc::Parser;

    /// A fixed seed we can use for our tests to avoid flakes.
    const TEST_SEED: u64 = 0xC0FFEEEEEEEEEEEE;

    #[test]
    fn osc_generator() {
        let prng = DefaultPrng::init(TEST_SEED);
        let mut buf = [0u8; 256];
        let len = buf.len();
        {
            let mut v = Osc::init(prng.random());
            let gen = v.generator();
            for _ in 0..50 {
                let mut writer = FixedWriter::fixed(&mut buf);
                gen.next(&mut writer, len).unwrap();
                let result = writer.buffered();
                assert!(!result.is_empty());
            }
        }
    }

    #[test]
    fn osc_generator_valid() {
        let prng = DefaultPrng::init(TEST_SEED);
        let mut buf = [0u8; 256];
        let len = buf.len();
        let mut gen = Osc {
            p_valid: 1.0,
            ..Osc::init(prng.random())
        };
        for _ in 0..50 {
            let mut writer = FixedWriter::fixed(&mut buf);
            gen.next(&mut writer, len).unwrap();
            let seq = writer.buffered();
            let mut parser = Parser::init(None);
            for &c in &seq[2..seq.len() - 1] {
                parser.next(c);
            }
            assert!(parser.end(None).is_some());
        }
    }

    #[test]
    fn osc_generator_invalid() {
        let prng = DefaultPrng::init(TEST_SEED);
        let mut buf = [0u8; 256];
        let len = buf.len();
        let mut gen = Osc {
            p_valid: 0.0,
            ..Osc::init(prng.random())
        };
        for _ in 0..50 {
            let mut writer = FixedWriter::fixed(&mut buf);
            gen.next(&mut writer, len).unwrap();
            let seq = writer.buffered();
            let mut parser = Parser::init(None);
            for &c in &seq[2..seq.len() - 1] {
                parser.next(c);
            }
            assert!(parser.end(None).is_none());
        }
    }
}
