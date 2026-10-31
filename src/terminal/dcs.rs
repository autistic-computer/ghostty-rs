//! Port of `src/terminal/dcs.zig`.
//!
//! STATUS: ported
//!
//! Port notes / deviations:
//! - [`Decrqss::encode`] takes `&Terminal` (Zig: `*Terminal`) since it only
//!   reads terminal state.
//! - `std.Io.Writer.Allocating` buffers are `Vec<u8>`; `deinit` is `Drop`.
//! - `build_options.tmux_control_mode` is checked at runtime via the
//!   `TMUX_CONTROL_MODE` const; the tmux types always exist in Rust.

use std::fmt;
use std::io::Write;

use crate::terminal::build_options::TMUX_CONTROL_MODE;
use crate::terminal::cursor::Style as CursorStyle;
use crate::terminal::modes::Mode;
use crate::terminal::Terminal;
use crate::terminal::tmux::{ControlNotification, ControlParser};

const LOG_TARGET: &str = "terminal_dcs";

/// `Parser.Action.DCS`.
pub use crate::terminal::parser::Dcs;

/// Writer error (Zig: `error.WriteFailed` on a fixed writer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoSpaceLeft;

impl fmt::Display for NoSpaceLeft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WriteFailed")
    }
}

impl std::error::Error for NoSpaceLeft {}

impl From<std::io::Error> for NoSpaceLeft {
    fn from(_: std::io::Error) -> Self {
        NoSpaceLeft
    }
}

/// Allocation failure (Zig: `error.OutOfMemory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

/// DCS command handler. This should be hooked into a terminal.Stream handler.
/// The hook/put/unhook functions are meant to be called from the
/// terminal.stream dcsHook, dcsPut, and dcsUnhook functions, respectively.
#[derive(Debug)]
pub struct Handler {
    state: State,

    /// Maximum bytes any DCS command can take. This is to prevent
    /// malicious input from causing us to allocate too much memory.
    /// This is arbitrarily set to 1MB today, increase if needed.
    pub max_bytes: usize,
}

impl Default for Handler {
    fn default() -> Self {
        Handler {
            state: State::Inactive,
            max_bytes: 1024 * 1024,
        }
    }
}

/// `Handler.Hook`
struct Hook {
    state: State,
    command: Option<Command<'static>>,
}

impl Handler {
    pub fn hook(&mut self, dcs: Dcs<'_>) -> Option<Command<'static>> {
        debug_assert!(matches!(self.state, State::Inactive));

        // Initialize our state to ignore in case of error
        self.state = State::Ignore;

        // Try to parse the hook.
        let hk = match self.try_hook(dcs) {
            Ok(hk) => hk,
            Err(err) => {
                log::info!(
                    target: LOG_TARGET,
                    "error initializing DCS hook, will ignore hook err={:?}",
                    err
                );
                return None;
            }
        };
        let Some(hk) = hk else {
            log::info!(target: LOG_TARGET, "unknown DCS hook: {:?}", dcs);
            return None;
        };

        self.state = hk.state;
        hk.command
    }

    fn try_hook(&self, dcs: Dcs<'_>) -> Result<Option<Hook>, Error> {
        Ok(match dcs.intermediates.len() {
            0 => match dcs.r#final {
                // Tmux control mode
                b'p' => 'tmux: {
                    if !TMUX_CONTROL_MODE {
                        log::debug!(
                            target: LOG_TARGET,
                            "tmux control mode not enabled in build, ignoring"
                        );
                        break 'tmux None;
                    }

                    // Tmux control mode must start with ESC P 1000 p
                    if dcs.params.len() != 1 || dcs.params[0] != 1000 {
                        break 'tmux None;
                    }

                    // Arbitrary choice to limit initial reallocs
                    let buffer = with_capacity(128)?;
                    Some(Hook {
                        state: State::Tmux(ControlParser {
                            max_bytes: self.max_bytes,
                            buffer,
                            ..Default::default()
                        }),
                        command: Some(Command::Tmux(ControlNotification::Enter)),
                    })
                }

                _ => None,
            },

            1 => match dcs.intermediates[0] {
                b'+' => match dcs.r#final {
                    // XTGETTCAP
                    // https://github.com/mitchellh/ghostty/issues/517
                    b'q' => Some(Hook {
                        // Arbitrary choice
                        state: State::Xtgettcap(with_capacity(128)?),
                        command: None,
                    }),

                    _ => None,
                },

                b'$' => match dcs.r#final {
                    // DECRQSS
                    b'q' => Some(Hook {
                        state: State::Decrqss(DecrqssBuffer::default()),
                        command: None,
                    }),

                    _ => None,
                },

                _ => None,
            },

            _ => None,
        })
    }

    /// Put a byte into the DCS handler. This will return a command
    /// if a command needs to be executed.
    pub fn put(&mut self, byte: u8) -> Option<Command<'_>> {
        let this: *mut Handler = self;
        match self.try_put(byte) {
            Ok(cmd) => cmd,
            Err(err) => {
                // On error we just discard our state and ignore the rest
                log::info!(
                    target: LOG_TARGET,
                    "error putting byte into DCS handler err={:?}",
                    err
                );
                // SAFETY: the `Err` variant holds no borrow of `self`, so
                // the mutable borrow taken by `try_put` is dead on this
                // path. Rust's current borrow checker cannot see that
                // (NLL "problem case #3"), so we go through the raw pointer.
                unsafe {
                    (*this).discard();
                    (*this).state = State::Ignore;
                }
                None
            }
        }
    }

    fn try_put(&mut self, byte: u8) -> Result<Option<Command<'_>>, Error> {
        match &mut self.state {
            State::Inactive | State::Ignore => {}

            State::Tmux(tmux) => {
                if TMUX_CONTROL_MODE {
                    return match tmux.put(byte) {
                        Ok(Some(n)) => Ok(Some(Command::Tmux(n))),
                        Ok(None) => Ok(None),
                        Err(_) => Err(Error::OutOfMemory),
                    };
                } else {
                    unreachable!()
                }
            }

            State::Xtgettcap(list) => {
                if list.len() >= self.max_bytes {
                    return Err(Error::OutOfMemory);
                }

                list.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                list.push(byte);
            }

            State::Decrqss(buffer) => {
                if buffer.len as usize >= buffer.data.len() {
                    return Err(Error::OutOfMemory);
                }

                buffer.data[buffer.len as usize] = byte;
                buffer.len += 1;
            }
        }

        Ok(None)
    }

    pub fn unhook(&mut self) -> Option<Command<'static>> {
        // Note: we do NOT call deinit here on purpose because some commands
        // transfer memory ownership. If state needs cleanup, the switch
        // prong below should handle it.
        let state = std::mem::replace(&mut self.state, State::Inactive);

        match state {
            State::Inactive | State::Ignore => None,

            State::Tmux(mut tmux) => {
                if TMUX_CONTROL_MODE {
                    tmux.deinit();
                    Some(Command::Tmux(ControlNotification::Exit))
                } else {
                    unreachable!()
                }
            }

            State::Xtgettcap(mut list) => {
                // Note: purposely do not deinit our state here because
                // we copy it into the resulting command.
                list.make_ascii_uppercase();
                Some(Command::Xtgettcap(Xtgettcap { data: list, i: 0 }))
            }

            State::Decrqss(buffer) => Some(Command::Decrqss(match buffer.len {
                0 => Decrqss::None,
                1 => match buffer.data[0] {
                    b'm' => Decrqss::Sgr,
                    b'r' => Decrqss::Decstbm,
                    b's' => Decrqss::Decslrm,
                    _ => Decrqss::None,
                },
                2 => match buffer.data[0] {
                    b' ' => match buffer.data[1] {
                        b'q' => Decrqss::Decscusr,
                        _ => Decrqss::None,
                    },
                    _ => Decrqss::None,
                },
                _ => unreachable!(),
            })),
        }
    }

    fn discard(&mut self) {
        self.state.deinit();
        self.state = State::Inactive;
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        self.discard();
    }
}

/// `Vec::with_capacity` that reports allocation failure.
fn with_capacity(n: usize) -> Result<Vec<u8>, Error> {
    let mut v = Vec::new();
    v.try_reserve(n).map_err(|_| Error::OutOfMemory)?;
    Ok(v)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command<'a> {
    /// XTGETTCAP
    Xtgettcap(Xtgettcap),

    /// DECRQSS
    Decrqss(Decrqss),

    /// Tmux control mode
    Tmux(ControlNotification<'a>),
}

/// `Command.XTGETTCAP`
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Xtgettcap {
    pub data: Vec<u8>,
    pub i: usize,
}

impl Xtgettcap {
    /// Returns the next terminfo key being requested and null
    /// when there are no more keys. The returned value is NOT hex-decoded
    /// because we expect to use a comptime lookup table.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<&[u8]> {
        let items = &self.data[..];
        if self.i >= items.len() {
            return None;
        }
        let rem = &items[self.i..];
        let idx = memchr::memchr(b';', rem).unwrap_or(rem.len());

        // Note that if we're at the end, idx + 1 is len + 1 so we're over
        // the end but that's okay because our check above is >= so we'll
        // never read.
        self.i += idx + 1;

        Some(&rem[..idx])
    }
}

/// Supported DECRQSS settings (`Command.DECRQSS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decrqss {
    None,
    Sgr,
    Decscusr,
    Decstbm,
    Decslrm,
}

impl Decrqss {
    /// Fixed upper bound for an encoded DECRPSS response. The comptime
    /// calculated max at the time of writing this was around 63 so this
    /// leaves a ton of space for future stuff. We don't do the comptime
    /// calculation cause it complicated the implementation a bit too
    /// much.
    pub const MAX_RESPONSE_BYTES: usize = 256;

    /// Encode the response for this request.
    pub fn encode<'r>(
        self,
        t: &Terminal,
        response: &'r mut [u8],
    ) -> Result<&'r [u8], NoSpaceLeft> {
        // const prefix_fmt = "\x1bP{d}$r";
        const PREFIX_LEN: usize = b"\x1bP0$r".len();
        if response.len() < PREFIX_LEN {
            return Err(NoSpaceLeft);
        }
        let mut end = PREFIX_LEN;

        {
            let mut writer: &mut [u8] = &mut response[end..];
            let avail = writer.len();
            match self {
                Decrqss::None => {}
                Decrqss::Sgr => {
                    let n = t.print_attributes(writer)?.len();
                    writer = &mut writer[n..];
                    writer.write_all(b"m")?;
                }
                Decrqss::Decscusr => {
                    let blink = t.modes.get(Mode::CursorBlinking);
                    let style: u8 = match t.screens.active.cursor.cursor_style {
                        CursorStyle::Block | CursorStyle::BlockHollow => {
                            if blink {
                                1
                            } else {
                                2
                            }
                        }
                        CursorStyle::Underline => {
                            if blink {
                                3
                            } else {
                                4
                            }
                        }
                        CursorStyle::Bar => {
                            if blink {
                                5
                            } else {
                                6
                            }
                        }
                    };
                    write!(writer, "{} q", style)?;
                }
                Decrqss::Decstbm => {
                    let r = t.scrolling_region;
                    write!(writer, "{};{}r", r.top as u32 + 1, r.bottom as u32 + 1)?;
                }
                Decrqss::Decslrm => {
                    if t.modes.get(Mode::EnableLeftAndRightMargin) {
                        let r = t.scrolling_region;
                        write!(writer, "{};{}s", r.left as u32 + 1, r.right as u32 + 1)?;
                    }
                }
            }
            end += avail - writer.len();
        }

        let valid = end > PREFIX_LEN;
        {
            let mut writer: &mut [u8] = &mut response[end..];
            writer.write_all(b"\x1b\\")?;
        }
        end += 2;
        {
            let mut writer: &mut [u8] = &mut response[..PREFIX_LEN];
            write!(writer, "\x1bP{}$r", valid as u8)?;
        }
        Ok(&response[..end])
    }
}

/// `State.decrqss`
#[derive(Debug, Clone, Copy, Default)]
struct DecrqssBuffer {
    data: [u8; 2],
    /// Zig: `u2`.
    len: u8,
}

#[derive(Debug)]
enum State {
    /// We're not in a DCS state at the moment.
    Inactive,

    /// We're hooked, but its an unknown DCS command or one that went
    /// invalid due to some bad input, so we're ignoring the rest.
    Ignore,

    /// XTGETTCAP
    Xtgettcap(Vec<u8>),

    /// DECRQSS
    Decrqss(DecrqssBuffer),

    /// Tmux control mode: https://github.com/tmux/tmux/wiki/Control-Mode
    Tmux(ControlParser),
}

impl State {
    fn deinit(&mut self) {
        match self {
            State::Inactive | State::Ignore => {}
            State::Xtgettcap(v) => *v = Vec::new(),
            State::Decrqss(_) => {}
            State::Tmux(v) => {
                if TMUX_CONTROL_MODE {
                    v.deinit();
                } else {
                    unreachable!()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::ansi;
    use crate::terminal::color::Rgb;
    use crate::terminal::sgr::{Attribute, Underline};

    fn dcs<'a>(intermediates: &'a [u8], params: &'a [u16], r#final: u8) -> Dcs<'a> {
        Dcs {
            intermediates,
            params,
            r#final,
        }
    }

    #[test]
    fn unknown_dcs_command() {
        let mut h = Handler::default();
        assert!(h.hook(dcs(b"", &[], b'A')).is_none());
        assert!(matches!(h.state, State::Ignore));
        assert!(h.unhook().is_none());
        assert!(matches!(h.state, State::Inactive));
    }

    #[test]
    fn xtgettcap_command() {
        let mut h = Handler::default();
        assert!(h.hook(dcs(b"+", &[], b'q')).is_none());
        for &byte in b"536D756C78" {
            let _ = h.put(byte);
        }
        let cmd = h.unhook().unwrap();
        let Command::Xtgettcap(mut cmd) = cmd else {
            panic!("expected xtgettcap");
        };
        assert_eq!(b"536D756C78", cmd.next().unwrap());
        assert!(cmd.next().is_none());
    }

    #[test]
    fn xtgettcap_mixed_case() {
        let mut h = Handler::default();
        assert!(h.hook(dcs(b"+", &[], b'q')).is_none());
        for &byte in b"536d756C78" {
            let _ = h.put(byte);
        }
        let cmd = h.unhook().unwrap();
        let Command::Xtgettcap(mut cmd) = cmd else {
            panic!("expected xtgettcap");
        };
        assert_eq!(b"536D756C78", cmd.next().unwrap());
        assert!(cmd.next().is_none());
    }

    #[test]
    fn xtgettcap_command_multiple_keys() {
        let mut h = Handler::default();
        assert!(h.hook(dcs(b"+", &[], b'q')).is_none());
        for &byte in b"536D756C78;536D756C78" {
            let _ = h.put(byte);
        }
        let cmd = h.unhook().unwrap();
        let Command::Xtgettcap(mut cmd) = cmd else {
            panic!("expected xtgettcap");
        };
        assert_eq!(b"536D756C78", cmd.next().unwrap());
        assert_eq!(b"536D756C78", cmd.next().unwrap());
        assert!(cmd.next().is_none());
    }

    #[test]
    fn xtgettcap_command_invalid_data() {
        let mut h = Handler::default();
        assert!(h.hook(dcs(b"+", &[], b'q')).is_none());
        for &byte in b"who;536D756C78" {
            let _ = h.put(byte);
        }
        let cmd = h.unhook().unwrap();
        let Command::Xtgettcap(mut cmd) = cmd else {
            panic!("expected xtgettcap");
        };
        assert_eq!(b"WHO", cmd.next().unwrap());
        assert_eq!(b"536D756C78", cmd.next().unwrap());
        assert!(cmd.next().is_none());
    }

    #[test]
    fn decrqss_command() {
        let mut h = Handler::default();
        assert!(h.hook(dcs(b"$", &[], b'q')).is_none());
        let _ = h.put(b'm');
        let cmd = h.unhook().unwrap();
        assert_eq!(Command::Decrqss(Decrqss::Sgr), cmd);
    }

    #[test]
    fn decrqss_invalid_command() {
        let mut h = Handler::default();
        assert!(h.hook(dcs(b"$", &[], b'q')).is_none());
        let _ = h.put(b'z');
        let cmd = h.unhook().unwrap();
        assert_eq!(Command::Decrqss(Decrqss::None), cmd);

        h.discard();

        assert!(h.hook(dcs(b"$", &[], b'q')).is_none());
        let _ = h.put(b'"');
        let _ = h.put(b' ');
        let _ = h.put(b'q');
        assert!(h.unhook().is_none());
    }

    fn expect_response(term: &mut Terminal, request: Decrqss, expected: &[u8]) {
        let mut buf = [0u8; Decrqss::MAX_RESPONSE_BYTES];
        let encoded = request.encode(term, &mut buf).unwrap();
        assert_eq!(
            String::from_utf8_lossy(expected),
            String::from_utf8_lossy(encoded)
        );
    }

    fn term(cols: u16, rows: u16) -> Terminal {
        Terminal::init(crate::terminal::terminal::Options::new(cols, rows)).unwrap()
    }

    #[test]
    fn decrqss_response_encoding() {
        let mut t = term(80, 24);

        expect_response(&mut t, Decrqss::None, b"\x1BP0$r\x1B\\");
        expect_response(&mut t, Decrqss::Sgr, b"\x1BP1$r0m\x1B\\");

        t.set_attribute(Attribute::Bold).unwrap();
        t.set_attribute(Attribute::Underline(Underline::Curly)).unwrap();
        expect_response(&mut t, Decrqss::Sgr, b"\x1BP1$r0;1;4:3m\x1B\\");

        t.set_cursor_style(ansi::CursorStyle::SteadyUnderline);
        expect_response(&mut t, Decrqss::Decscusr, b"\x1BP1$r4 q\x1B\\");

        t.scrolling_region.top = 4;
        t.scrolling_region.bottom = 19;
        expect_response(&mut t, Decrqss::Decstbm, b"\x1BP1$r5;20r\x1B\\");

        expect_response(&mut t, Decrqss::Decslrm, b"\x1BP0$r\x1B\\");
        t.modes.set(Mode::EnableLeftAndRightMargin, true);
        t.scrolling_region.left = 2;
        t.scrolling_region.right = 69;
        expect_response(&mut t, Decrqss::Decslrm, b"\x1BP1$r3;70s\x1B\\");
    }

    #[test]
    fn decrqss_largest_response_fits_fixed_buffer() {
        let mut t = term(80, 24);

        t.set_attribute(Attribute::Bold).unwrap();
        t.set_attribute(Attribute::Faint).unwrap();
        t.set_attribute(Attribute::Italic).unwrap();
        t.set_attribute(Attribute::Underline(Underline::Dashed)).unwrap();
        t.set_attribute(Attribute::Blink).unwrap();
        t.set_attribute(Attribute::Inverse).unwrap();
        t.set_attribute(Attribute::Invisible).unwrap();
        t.set_attribute(Attribute::Strikethrough).unwrap();
        t.set_attribute(Attribute::Overline).unwrap();
        t.set_attribute(Attribute::DirectColorFg(Rgb { r: 255, g: 255, b: 255 })).unwrap();
        t.set_attribute(Attribute::DirectColorBg(Rgb { r: 255, g: 255, b: 255 })).unwrap();

        let mut buf = [0u8; Decrqss::MAX_RESPONSE_BYTES];
        let encoded = Decrqss::Sgr.encode(&t, &mut buf).unwrap();

        let expected: &[u8] = b"\x1BP1$r0;1;2;3;4:5;53;5;7;8;9;38:2::255:255:255;48:2::255:255:255m\x1B\\";
        assert_eq!(
            String::from_utf8_lossy(expected),
            String::from_utf8_lossy(encoded)
        );
        assert!(encoded.len() <= Decrqss::MAX_RESPONSE_BYTES);
    }

    #[test]
    fn tmux_enter_and_implicit_exit() {
        if !TMUX_CONTROL_MODE {
            return;
        }

        let mut h = Handler::default();

        {
            let cmd = h.hook(dcs(b"", &[1000], b'p')).unwrap();
            assert_eq!(Command::Tmux(ControlNotification::Enter), cmd);
        }

        {
            let cmd = h.unhook().unwrap();
            assert_eq!(Command::Tmux(ControlNotification::Exit), cmd);
        }
    }
}
