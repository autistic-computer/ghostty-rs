//! Port of `src/terminal/Parser.zig`.
//!
//! STATUS: ported
//!
//! VT-series parser for escape and control sequences.
//!
//! This is implemented directly as the state machine described on
//! vt100.net: https://vt100.net/emu/dec_ansi_parser
//!
//! Rust note: the actions returned by [`Parser::next`] borrow the parser
//! (intermediates, params and OSC data are only valid until the next call),
//! which the Zig version documents and Rust enforces with a lifetime.

use std::fmt;

use crate::terminal::osc;
use crate::terminal::parse_table::TABLE;

const LOG_TARGET: &str = "parser";

/// States for the state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiIntermediate,
    CsiParam,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    SosPmApcString,
}

impl State {
    /// The number of states.
    pub const COUNT: usize = 14;

    /// All states, in declaration (backing integer) order.
    pub const ALL: [State; Self::COUNT] = [
        State::Ground,
        State::Escape,
        State::EscapeIntermediate,
        State::CsiEntry,
        State::CsiIntermediate,
        State::CsiParam,
        State::CsiIgnore,
        State::DcsEntry,
        State::DcsParam,
        State::DcsIntermediate,
        State::DcsPassthrough,
        State::DcsIgnore,
        State::OscString,
        State::SosPmApcString,
    ];
}

/// Transition action is an action that can be taken during a state
/// transition. This is more of an internal action, not one used by
/// end users, typically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransitionAction {
    None,
    Ignore,
    Print,
    Execute,
    Collect,
    Param,
    EscDispatch,
    CsiDispatch,
    Put,
    OscPut,
    ApcPut,
}

/// Action is the action that a caller of the parser is expected to
/// take as a result of some input character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action<'a> {
    /// Draw character to the screen. This is a unicode codepoint (Zig `u21`).
    Print(u32),

    /// Execute the C0 or C1 function.
    Execute(u8),

    /// Execute the CSI command. Note that pointers within this
    /// structure are only valid until the next call to "next".
    CsiDispatch(Csi<'a>),

    /// Execute the ESC command.
    EscDispatch(Esc<'a>),

    /// Execute the OSC command.
    OscDispatch(osc::Command<'a>),

    /// DCS-related events.
    DcsHook(Dcs<'a>),
    DcsPut(u8),
    DcsUnhook,

    /// APC data
    ApcStart,
    ApcPut(u8),
    ApcEnd,
}

/// The tag of an [`Action`] (Zig: `Action.Tag`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionTag {
    Print,
    Execute,
    CsiDispatch,
    EscDispatch,
    OscDispatch,
    DcsHook,
    DcsPut,
    DcsUnhook,
    ApcStart,
    ApcPut,
    ApcEnd,
}

impl ActionTag {
    /// The Zig tag name.
    pub fn as_str(self) -> &'static str {
        match self {
            ActionTag::Print => "print",
            ActionTag::Execute => "execute",
            ActionTag::CsiDispatch => "csi_dispatch",
            ActionTag::EscDispatch => "esc_dispatch",
            ActionTag::OscDispatch => "osc_dispatch",
            ActionTag::DcsHook => "dcs_hook",
            ActionTag::DcsPut => "dcs_put",
            ActionTag::DcsUnhook => "dcs_unhook",
            ActionTag::ApcStart => "apc_start",
            ActionTag::ApcPut => "apc_put",
            ActionTag::ApcEnd => "apc_end",
        }
    }
}

impl Action<'_> {
    /// The tag of this action.
    pub fn tag(&self) -> ActionTag {
        match self {
            Action::Print(_) => ActionTag::Print,
            Action::Execute(_) => ActionTag::Execute,
            Action::CsiDispatch(_) => ActionTag::CsiDispatch,
            Action::EscDispatch(_) => ActionTag::EscDispatch,
            Action::OscDispatch(_) => ActionTag::OscDispatch,
            Action::DcsHook(_) => ActionTag::DcsHook,
            Action::DcsPut(_) => ActionTag::DcsPut,
            Action::DcsUnhook => ActionTag::DcsUnhook,
            Action::ApcStart => ActionTag::ApcStart,
            Action::ApcPut(_) => ActionTag::ApcPut,
            Action::ApcEnd => ActionTag::ApcEnd,
        }
    }
}

/// The list of separators used for CSI params. The value of the
/// bit can be mapped to Sep. The index of this bit set specifies
/// the separator AFTER that param. For example: 0;4:3 would have
/// index 1 set.
///
/// Zig: `std.bit_set.Static(MAX_PARAMS)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SepList {
    mask: u32,
}

impl SepList {
    pub const EMPTY: SepList = SepList { mask: 0 };

    /// The number of bits in the set.
    pub const BIT_LENGTH: usize = MAX_PARAMS;

    pub fn is_set(&self, idx: usize) -> bool {
        debug_assert!(idx < MAX_PARAMS);
        self.mask & (1u32 << idx) != 0
    }

    pub fn set(&mut self, idx: usize) {
        debug_assert!(idx < MAX_PARAMS);
        self.mask |= 1u32 << idx;
    }

    pub fn unset(&mut self, idx: usize) {
        debug_assert!(idx < MAX_PARAMS);
        self.mask &= !(1u32 << idx);
    }

    pub fn count(&self) -> usize {
        self.mask.count_ones() as usize
    }

    /// The raw bit mask.
    pub fn mask(&self) -> u32 {
        self.mask
    }
}

/// The separator used for CSI params.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Sep {
    Semicolon = 0,
    Colon = 1,
}

/// A CSI command (Zig: `Action.CSI`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Csi<'a> {
    pub intermediates: &'a [u8],
    pub params: &'a [u16],
    pub params_sep: SepList,
    pub r#final: u8,
}

// Implement formatter for logging
impl fmt::Display for Csi<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ESC [ {} {:?} {}",
            String::from_utf8_lossy(self.intermediates),
            self.params,
            self.r#final as char
        )
    }
}

/// An ESC command (Zig: `Action.ESC`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Esc<'a> {
    pub intermediates: &'a [u8],
    pub r#final: u8,
}

// Implement formatter for logging
impl fmt::Display for Esc<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ESC {} {}",
            String::from_utf8_lossy(self.intermediates),
            self.r#final as char
        )
    }
}

/// A DCS hook (Zig: `Action.DCS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dcs<'a> {
    pub intermediates: &'a [u8],
    pub params: &'a [u16],
    pub r#final: u8,
}

impl<'a> Dcs<'a> {
    /// A DCS with no intermediates or params (the Zig field defaults).
    pub fn new(r#final: u8) -> Dcs<'a> {
        Dcs {
            intermediates: b"",
            params: &[],
            r#final,
        }
    }
}

/// C representation of [`Dcs`] (Zig: `Action.DCS.C`).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DcsC {
    pub intermediates: *const u8,
    pub intermediates_len: usize,
    pub params: *const u16,
    pub params_len: usize,
    pub r#final: u8,
}

// Implement formatter for logging. This prints custom formats for some of
// our primitives.
impl fmt::Display for Action<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "terminal.Parser.Action{{ .{} = ", self.tag().as_str())?;
        match self {
            // Unicode
            Action::Print(cp) => match char::from_u32(*cp) {
                Some(ch) => write!(f, "'{}' (U+{:X})", ch, cp)?,
                None => write!(f, "'\u{FFFD}' (U+{:X})", cp)?,
            },

            // Byte
            Action::Execute(b) | Action::DcsPut(b) | Action::ApcPut(b) => {
                write!(f, "0x{:x}", b)?
            }

            // Note: we don't do ASCII (u8) because there are a lot
            // of invisible characters we don't want to handle right
            // now.

            // All others do the default behavior.
            Action::CsiDispatch(csi) => write!(f, "{:?}", csi)?,
            Action::EscDispatch(esc) => write!(f, "{:?}", esc)?,
            Action::OscDispatch(cmd) => write!(f, "{:?}", cmd.key())?,
            Action::DcsHook(dcs) => write!(f, "{:?}", dcs)?,
            Action::DcsUnhook | Action::ApcStart | Action::ApcEnd => f.write_str("void")?,
        }
        f.write_str(" }")
    }
}

/// Maximum number of intermediate characters during parsing. This is
/// 4 because we also use the intermediates array for UTF8 decoding which
/// can be at most 4 bytes.
pub const MAX_INTERMEDIATE: usize = 4;

/// Maximum number of CSI parameters. This is arbitrary. Practically, the
/// only CSI command that uses more than 3 parameters is the SGR command
/// which can be infinitely long. 24 is a reasonable limit based on empirical
/// data. This used to be 16 but Kakoune has a SGR command that uses 17
/// parameters.
///
/// We could in the future make this the static limit and then allocate after
/// but that's a lot more work and practically its so rare to exceed this
/// number. I implore TUI authors to not use more than this number of CSI
/// params, but I suspect we'll introduce a slow path with heap allocation
/// one day.
pub const MAX_PARAMS: usize = 24;

/// An action computed before borrowing the parser's buffers.
#[derive(Debug, Clone, Copy)]
enum Pending {
    Print(u8),
    Execute(u8),
    CsiDispatch(u8),
    EscDispatch(u8),
    DcsHook(u8),
    DcsPut(u8),
    DcsUnhook,
    ApcStart,
    ApcPut(u8),
    ApcEnd,
}

/// The VT parser.
#[derive(Debug)]
pub struct Parser {
    /// Current state of the state machine
    pub state: State,

    /// Intermediate tracking.
    pub intermediates: [u8; MAX_INTERMEDIATE],
    pub intermediates_idx: u8,

    /// Param tracking, building
    pub params: [u16; MAX_PARAMS],
    pub params_sep: SepList,
    pub params_idx: u8,
    pub param_acc: u16,
    pub param_acc_idx: u8,

    /// Parser for OSC sequences
    pub osc_parser: osc::Parser,
}

impl Default for Parser {
    fn default() -> Self {
        Self::init()
    }
}

impl Parser {
    pub fn init() -> Parser {
        Parser {
            state: State::Ground,
            intermediates_idx: 0,
            params_sep: SepList::EMPTY,
            params_idx: 0,
            param_acc: 0,
            param_acc_idx: 0,
            osc_parser: osc::Parser::init(None),

            intermediates: [0; MAX_INTERMEDIATE],
            params: [0; MAX_PARAMS],
        }
    }

    pub fn deinit(&mut self) {
        self.osc_parser.deinit();
    }

    /// Next consumes the next character c and returns the actions to execute.
    /// Up to 3 actions may need to be executed -- in order -- representing
    /// the state exit, transition, and entry actions.
    pub fn next(&mut self, c: u8) -> [Option<Action<'_>>; 3] {
        let effect = TABLE[c as usize][self.state as usize];

        // log.info("next: {x}", .{c});

        let next_state = effect.state;
        let action = effect.action;

        let prev_state = self.state;
        let changed = prev_state != next_state;

        // After generating the actions, we set our next state.
        self.state = next_state;

        // When going from one state to another, the actions take place in this order:
        //
        // 1. exit action from old state
        // 2. transition action
        // 3. entry action to new state
        //
        // The transition and entry actions are performed first here so
        // that the exit action (which may borrow the OSC parser's data)
        // can be produced last. This doesn't change behavior: the exit
        // action only touches the OSC parser when leaving the OSC string
        // state, while the transition and entry actions only touch it
        // when staying in or entering that state.

        // 2. transition action
        let transition = self.do_action(action, c);

        // 3. Entry depends on new state
        let entry: Option<Pending> = if !changed {
            None
        } else {
            match next_state {
                State::Escape | State::DcsEntry | State::CsiEntry => {
                    self.clear();
                    None
                }
                State::OscString => {
                    self.osc_parser.reset();
                    None
                }
                State::DcsPassthrough => 'dcs_hook: {
                    // Ignore too many parameters
                    if self.params_idx as usize >= MAX_PARAMS {
                        break 'dcs_hook None;
                    }
                    // Finalize parameters
                    if self.param_acc_idx > 0 {
                        self.params[self.params_idx as usize] = self.param_acc;
                        self.params_idx += 1;
                    }
                    Some(Pending::DcsHook(c))
                }
                State::SosPmApcString => Some(Pending::ApcStart),
                _ => None,
            }
        };

        // 1. Exit depends on current state
        let exit: Option<Action<'_>> = if !changed {
            None
        } else {
            match prev_state {
                State::OscString => self.osc_parser.end(Some(c)).map(Action::OscDispatch),
                State::DcsPassthrough => Some(Action::DcsUnhook),
                State::SosPmApcString => Some(Action::ApcEnd),
                _ => None,
            }
        };

        let intermediates: &[u8] = &self.intermediates[..self.intermediates_idx as usize];
        let params: &[u16] = &self.params[..self.params_idx as usize];
        let params_sep = self.params_sep;
        let materialize = |p: Pending| -> Action<'_> {
            match p {
                Pending::Print(c) => Action::Print(c as u32),
                Pending::Execute(c) => Action::Execute(c),
                Pending::CsiDispatch(c) => Action::CsiDispatch(Csi {
                    intermediates,
                    params,
                    params_sep,
                    r#final: c,
                }),
                Pending::EscDispatch(c) => Action::EscDispatch(Esc {
                    intermediates,
                    r#final: c,
                }),
                Pending::DcsHook(c) => Action::DcsHook(Dcs {
                    intermediates,
                    params,
                    r#final: c,
                }),
                Pending::DcsPut(c) => Action::DcsPut(c),
                Pending::DcsUnhook => Action::DcsUnhook,
                Pending::ApcStart => Action::ApcStart,
                Pending::ApcPut(c) => Action::ApcPut(c),
                Pending::ApcEnd => Action::ApcEnd,
            }
        };

        [exit, transition.map(materialize), entry.map(materialize)]
    }

    #[inline]
    pub fn collect(&mut self, c: u8) {
        if self.intermediates_idx as usize >= MAX_INTERMEDIATE {
            log::warn!(target: LOG_TARGET, "invalid intermediates count");
            return;
        }

        self.intermediates[self.intermediates_idx as usize] = c;
        self.intermediates_idx += 1;
    }

    #[inline]
    fn do_action(&mut self, action: TransitionAction, c: u8) -> Option<Pending> {
        match action {
            TransitionAction::None | TransitionAction::Ignore => None,
            TransitionAction::Print => Some(Pending::Print(c)),
            TransitionAction::Execute => Some(Pending::Execute(c)),
            TransitionAction::Collect => {
                self.collect(c);
                None
            }
            TransitionAction::Param => {
                // Semicolon separates parameters. If we encounter a semicolon
                // we need to store and move on to the next parameter.
                if c == b';' || c == b':' {
                    // Ignore too many parameters
                    if self.params_idx as usize >= MAX_PARAMS {
                        return None;
                    }

                    // Set param final value
                    self.params[self.params_idx as usize] = self.param_acc;
                    if c == b':' {
                        self.params_sep.set(self.params_idx as usize);
                    }
                    self.params_idx += 1;

                    // Reset current param value to 0
                    self.param_acc = 0;
                    self.param_acc_idx = 0;
                    return None;
                }

                // A numeric value. Add it to our accumulator.
                self.param_acc = self.param_acc.saturating_mul(10);
                self.param_acc = self.param_acc.saturating_add((c - b'0') as u16);

                // Increment our accumulator index. If we overflow then
                // we're out of bounds and we exit immediately.
                let (idx, overflow) = self.param_acc_idx.overflowing_add(1);
                self.param_acc_idx = idx;
                if overflow {
                    return None;
                }

                // The client is expected to perform no action.
                None
            }
            TransitionAction::OscPut => {
                self.osc_parser.next(c);
                None
            }
            TransitionAction::CsiDispatch => {
                // Ignore too many parameters
                if self.params_idx as usize >= MAX_PARAMS {
                    return None;
                }

                // Finalize parameters if we have one
                if self.param_acc_idx > 0 {
                    self.params[self.params_idx as usize] = self.param_acc;
                    self.params_idx += 1;
                }

                // We only allow colon or mixed separators for the 'm' command.
                if c != b'm' && self.params_sep.count() > 0 {
                    self.warn_csi_sep_mismatch(c);
                    return None;
                }

                Some(Pending::CsiDispatch(c))
            }
            TransitionAction::EscDispatch => Some(Pending::EscDispatch(c)),
            TransitionAction::Put => Some(Pending::DcsPut(c)),
            TransitionAction::ApcPut => Some(Pending::ApcPut(c)),
        }
    }

    /// Log a warning for a CSI dispatch with colon/mixed separators on a
    /// non-'m' command.
    ///
    /// This is noinline on purpose so that this unlikely (cold) behavior
    /// doesn't bloat the hot dispatch path which has been measured to actually
    /// affect both binary size and performance due to icache busts.
    #[cold]
    #[inline(never)]
    fn warn_csi_sep_mismatch(&self, c: u8) {
        let csi = Csi {
            intermediates: &self.intermediates[..self.intermediates_idx as usize],
            params: &self.params[..self.params_idx as usize],
            params_sep: self.params_sep,
            r#final: c,
        };
        log::warn!(
            target: LOG_TARGET,
            "CSI colon or mixed separators only allowed for 'm' command, got: {}",
            csi
        );
    }

    #[inline]
    pub fn clear(&mut self) {
        self.intermediates_idx = 0;
        self.params_idx = 0;
        self.params_sep = SepList::EMPTY;
        self.param_acc = 0;
        self.param_acc_idx = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::osc::color::{Operation, Request, Target};
    use crate::terminal::osc::{Allocator, Command, Terminator};

    fn feed_silent(p: &mut Parser, input: &[u8]) {
        for &c in input {
            let a = p.next(c);
            assert!(a[0].is_none());
            assert!(a[1].is_none());
            assert!(a[2].is_none());
        }
    }

    fn csi<'a>(a: &'a [Option<Action<'a>>; 3]) -> &'a Csi<'a> {
        match &a[1] {
            Some(Action::CsiDispatch(d)) => d,
            other => panic!("expected csi_dispatch, got {:?}", other),
        }
    }

    #[test]
    fn basic() {
        let mut p = Parser::init();
        let _ = p.next(0x9E);
        assert_eq!(State::SosPmApcString, p.state);
        let _ = p.next(0x9C);
        assert_eq!(State::Ground, p.state);

        {
            let a = p.next(b'a');
            assert!(a[0].is_none());
            assert!(matches!(a[1], Some(Action::Print(_))));
            assert!(a[2].is_none());
        }
        assert_eq!(State::Ground, p.state);

        {
            let a = p.next(0x19);
            assert!(a[0].is_none());
            assert!(matches!(a[1], Some(Action::Execute(_))));
            assert!(a[2].is_none());
        }
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn esc_esc_paren_b() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        let _ = p.next(b'(');

        let a = p.next(b'B');
        assert!(a[0].is_none());
        assert!(a[2].is_none());

        let Some(Action::EscDispatch(d)) = &a[1] else {
            panic!("expected esc_dispatch");
        };
        assert_eq!(b'B', d.r#final);
        assert_eq!(1, d.intermediates.len());
        assert_eq!(b'(', d.intermediates[0]);
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn csi_esc_bracket_h() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        let _ = p.next(0x5B);

        let a = p.next(0x48);
        assert!(a[0].is_none());
        assert!(a[2].is_none());

        let d = csi(&a);
        assert_eq!(0x48, d.r#final);
        assert_eq!(0, d.params.len());
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn csi_esc_bracket_1_semicolon_4_h() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        let _ = p.next(0x5B);
        let _ = p.next(0x31); // 1
        let _ = p.next(0x3B); // ;
        let _ = p.next(0x34); // 4

        let a = p.next(0x48); // H
        assert!(a[0].is_none());
        assert!(a[2].is_none());

        let d = csi(&a);
        assert_eq!(b'H', d.r#final);
        assert_eq!(2, d.params.len());
        assert_eq!(1u16, d.params[0]);
        assert_eq!(4u16, d.params[1]);
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn csi_sgr_esc_bracket_38_colon_2_m() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        let _ = p.next(b'[');
        let _ = p.next(b'3');
        let _ = p.next(b'8');
        let _ = p.next(b':');
        let _ = p.next(b'2');

        let a = p.next(b'm');
        assert!(a[0].is_none());
        assert!(a[2].is_none());

        let d = csi(&a);
        assert_eq!(b'm', d.r#final);
        assert_eq!(2, d.params.len());
        assert_eq!(38u16, d.params[0]);
        assert!(d.params_sep.is_set(0));
        assert_eq!(2u16, d.params[1]);
        assert!(!d.params_sep.is_set(1));
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn csi_sgr_colon_followed_by_semicolon() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        feed_silent(&mut p, b"[48:2");

        {
            let a = p.next(b'm');
            assert!(a[0].is_none());
            assert!(matches!(a[1], Some(Action::CsiDispatch(_))));
            assert!(a[2].is_none());
        }
        assert_eq!(State::Ground, p.state);

        let _ = p.next(0x1B);
        let _ = p.next(b'[');
        {
            let a = p.next(b'H');
            assert!(a[0].is_none());
            assert!(matches!(a[1], Some(Action::CsiDispatch(_))));
            assert!(a[2].is_none());
        }
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn csi_sgr_mixed_colon_and_semicolon() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        feed_silent(&mut p, b"[38:5:1;48:5:0");

        {
            let a = p.next(b'm');
            assert!(a[0].is_none());
            assert!(matches!(a[1], Some(Action::CsiDispatch(_))));
            assert!(a[2].is_none());
        }
        assert_eq!(State::Ground, p.state);
    }

    fn check_params(d: &Csi<'_>, expected: &[(u16, bool)]) {
        assert_eq!(expected.len(), d.params.len());
        for (i, &(value, sep)) in expected.iter().enumerate() {
            assert_eq!(value, d.params[i], "param {}", i);
            assert_eq!(sep, d.params_sep.is_set(i), "sep {}", i);
        }
    }

    #[test]
    fn csi_sgr_esc_bracket_48_colon_2_m() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        feed_silent(&mut p, b"[48:2:240:143:104");

        let a = p.next(b'm');
        assert!(a[0].is_none());
        assert!(a[2].is_none());

        let d = csi(&a);
        assert_eq!(b'm', d.r#final);
        check_params(
            d,
            &[(48, true), (2, true), (240, true), (143, true), (104, false)],
        );
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn csi_sgr_esc_bracket_4_colon_3_m_colon() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        let _ = p.next(b'[');
        let _ = p.next(b'4');
        let _ = p.next(b':');
        let _ = p.next(b'3');

        let a = p.next(b'm');
        assert!(a[0].is_none());
        assert!(a[2].is_none());

        let d = csi(&a);
        assert_eq!(b'm', d.r#final);
        check_params(d, &[(4, true), (3, false)]);
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn csi_sgr_with_many_blank_and_colon() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        feed_silent(&mut p, b"[58:2::240:143:104");

        let a = p.next(b'm');
        assert!(a[0].is_none());
        assert!(a[2].is_none());

        let d = csi(&a);
        assert_eq!(b'm', d.r#final);
        check_params(
            d,
            &[
                (58, true),
                (2, true),
                (0, true),
                (240, true),
                (143, true),
                (104, false),
            ],
        );
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    // This is from a Kakoune actual SGR sequence.
    #[test]
    fn csi_sgr_mixed_colon_and_semicolon_with_blank() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        feed_silent(&mut p, b"[;4:3;38;2;175;175;215;58:2::190:80:70");

        let a = p.next(b'm');
        assert!(a[0].is_none());
        assert!(a[2].is_none());

        let d = csi(&a);
        assert_eq!(b'm', d.r#final);
        check_params(
            d,
            &[
                (0, false),
                (4, true),
                (3, false),
                (38, false),
                (2, false),
                (175, false),
                (175, false),
                (215, false),
                (58, true),
                (2, true),
                (0, true),
                (190, true),
                (80, true),
                (70, false),
            ],
        );
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    // This is from a Kakoune actual SGR sequence also.
    #[test]
    fn csi_sgr_mixed_colon_and_semicolon_setting_underline_bg_fg() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        feed_silent(&mut p, b"[4:3;38;2;51;51;51;48;2;170;170;170;58;2;255;97;136");

        let a = p.next(b'm');
        assert!(a[0].is_none());
        assert!(a[2].is_none());

        let d = csi(&a);
        assert_eq!(b'm', d.r#final);
        check_params(
            d,
            &[
                (4, true),
                (3, false),
                (38, false),
                (2, false),
                (51, false),
                (51, false),
                (51, false),
                (48, false),
                (2, false),
                (170, false),
                (170, false),
                (170, false),
                (58, false),
                (2, false),
                (255, false),
                (97, false),
                (136, false),
            ],
        );
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn csi_colon_for_non_m_final() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        feed_silent(&mut p, b"[38:2h");

        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn csi_request_mode_decrqm() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        feed_silent(&mut p, b"[?2026$");

        let a = p.next(b'p');
        assert!(a[0].is_none());
        assert!(a[2].is_none());

        let d = csi(&a);
        assert_eq!(b'p', d.r#final);
        assert_eq!(2, d.intermediates.len());
        assert_eq!(1, d.params.len());
        assert_eq!(b'?', d.intermediates[0]);
        assert_eq!(b'$', d.intermediates[1]);
        assert_eq!(2026u16, d.params[0]);
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn csi_change_cursor() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        feed_silent(&mut p, b"[3 ");

        let a = p.next(b'q');
        assert!(a[0].is_none());
        assert!(a[2].is_none());

        let d = csi(&a);
        assert_eq!(b'q', d.r#final);
        assert_eq!(1, d.intermediates.len());
        assert_eq!(1, d.params.len());
        assert_eq!(b' ', d.intermediates[0]);
        assert_eq!(3u16, d.params[0]);
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn osc_change_window_title() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        let _ = p.next(b']');
        let _ = p.next(b'0');
        let _ = p.next(b';');
        let _ = p.next(b'a');
        let _ = p.next(b'b');
        let _ = p.next(b'c');

        let a = p.next(0x07); // BEL
        assert!(a[1].is_none());
        assert!(a[2].is_none());

        let Some(Action::OscDispatch(cmd)) = &a[0] else {
            panic!("expected osc_dispatch");
        };
        assert_eq!(&Command::ChangeWindowTitle(b"abc"), cmd);
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn osc_change_window_title_end_in_esc() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        let _ = p.next(b']');
        let _ = p.next(b'0');
        let _ = p.next(b';');
        let _ = p.next(b'a');
        let _ = p.next(b'b');
        let _ = p.next(b'c');

        {
            let a = p.next(0x1B);
            assert!(a[1].is_none());
            assert!(a[2].is_none());

            let Some(Action::OscDispatch(cmd)) = &a[0] else {
                panic!("expected osc_dispatch");
            };
            assert_eq!(&Command::ChangeWindowTitle(b"abc"), cmd);
        }
        let _ = p.next(b'\\');
        assert_eq!(State::Ground, p.state);
    }

    // https://github.com/darrenstarr/VtNetCore/pull/14
    // Saw this on HN, decided to add a test case because why not.
    #[test]
    fn osc_112_incomplete_sequence() {
        let mut p = Parser::init();
        p.osc_parser.alloc = Some(Allocator::GLOBAL);

        let _ = p.next(0x1B);
        let _ = p.next(b']');
        let _ = p.next(b'1');
        let _ = p.next(b'1');
        let _ = p.next(b'2');

        let a = p.next(0x07);
        assert!(a[1].is_none());
        assert!(a[2].is_none());

        let Some(Action::OscDispatch(Command::ColorOperation(op))) = &a[0] else {
            panic!("expected color_operation");
        };
        assert_eq!(Terminator::Bel, op.terminator);
        assert_eq!(Operation::Osc112, op.op);
        assert_eq!(1, op.requests.len());
        let mut it = op.requests.iter();
        {
            let req = it.next().unwrap();
            assert!(matches!(req, Request::Reset(_)));
            assert_eq!(
                Request::Reset(Target::Dynamic(crate::terminal::color::Dynamic::Cursor)),
                *req
            );
        }
        assert!(it.next().is_none());
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn osc_104_empty() {
        let mut p = Parser::init();
        p.osc_parser.alloc = Some(Allocator::GLOBAL);

        let _ = p.next(0x1B);
        let _ = p.next(b']');
        let _ = p.next(b'1');
        let _ = p.next(b'0');
        let _ = p.next(b'4');

        let a = p.next(0x07);
        assert!(a[1].is_none());
        assert!(a[2].is_none());

        let Some(Action::OscDispatch(Command::ColorOperation(op))) = &a[0] else {
            panic!("expected color_operation");
        };
        assert_eq!(Terminator::Bel, op.terminator);
        assert_eq!(Operation::Osc104, op.op);
        assert_eq!(1, op.requests.len());
        let mut it = op.requests.iter();
        assert_eq!(Some(&Request::ResetPalette), it.next());
        assert!(it.next().is_none());
        drop(a);
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn osc_105_empty() {
        let mut p = Parser::init();
        p.osc_parser.alloc = Some(Allocator::GLOBAL);

        for &c in b"\x1b]105" {
            let _ = p.next(c);
        }

        let a = p.next(0x07);
        let Some(Action::OscDispatch(Command::ColorOperation(op))) = &a[0] else {
            panic!("expected color_operation");
        };
        assert_eq!(Operation::Osc105, op.op);
    }

    #[test]
    fn csi_too_many_params() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        let _ = p.next(b'[');
        for _ in 0..100 {
            let _ = p.next(b'1');
            let _ = p.next(b';');
        }
        let _ = p.next(b'1');

        {
            let a = p.next(b'C');
            assert!(a[0].is_none());
            assert!(a[1].is_none());
            assert!(a[2].is_none());
        }
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn csi_sgr_with_up_to_our_max_parameters() {
        for max in 1..MAX_PARAMS + 1 {
            let mut p = Parser::init();
            let _ = p.next(0x1B);
            let _ = p.next(b'[');

            for _ in 0..max - 1 {
                let _ = p.next(b'1');
                let _ = p.next(b';');
            }
            let _ = p.next(b'2');

            let a = p.next(b'H');
            assert!(a[0].is_none());
            assert!(a[2].is_none());

            let d = csi(&a);
            assert_eq!(max, d.params.len());
            assert_eq!(2u16, d.params[max - 1]);
            drop(a);
            assert_eq!(State::Ground, p.state);
        }
    }

    #[test]
    fn csi_sgr_beyond_our_max_drops_it() {
        // Has to be +2 for the loops below
        let max = MAX_PARAMS + 2;

        let mut p = Parser::init();
        let _ = p.next(0x1B);
        let _ = p.next(b'[');

        for _ in 0..max - 1 {
            let _ = p.next(b'1');
            let _ = p.next(b';');
        }
        let _ = p.next(b'2');

        {
            let a = p.next(b'H');
            assert!(a[0].is_none());
            assert!(a[1].is_none());
            assert!(a[2].is_none());
        }
        assert_eq!(State::Ground, p.state);
    }

    #[test]
    fn dcs_xtgettcap() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        feed_silent(&mut p, b"P+");

        let a = p.next(b'q');
        assert!(a[0].is_none());
        assert!(a[1].is_none());

        let Some(Action::DcsHook(hook)) = &a[2] else {
            panic!("expected dcs_hook");
        };
        assert_eq!(b"+", hook.intermediates);
        assert_eq!(&[] as &[u16], hook.params);
        assert_eq!(b'q', hook.r#final);
        drop(a);
        assert_eq!(State::DcsPassthrough, p.state);
    }

    #[test]
    fn dcs_params() {
        let mut p = Parser::init();
        let _ = p.next(0x1B);
        feed_silent(&mut p, b"P1000");

        let a = p.next(b'p');
        assert!(a[0].is_none());
        assert!(a[1].is_none());

        let Some(Action::DcsHook(hook)) = &a[2] else {
            panic!("expected dcs_hook");
        };
        assert_eq!(&[1000u16], hook.params);
        assert_eq!(b'p', hook.r#final);
        drop(a);
        assert_eq!(State::DcsPassthrough, p.state);
    }

    #[test]
    fn dcs_too_many_params() {
        // Regression test for a crash found by fuzzing (afl). When a DCS
        // sequence has more than MAX_PARAMS parameters and param_acc_idx > 0,
        // entering dcs_passthrough wrote to params[params_idx] without a
        // bounds check, causing an out-of-bounds access.
        let mut p = Parser::init();
        let _ = p.next(0x1B); // ESC
        let _ = p.next(b'P'); // DCS entry

        // Feed a digit then MAX_PARAMS semicolons to fill all param slots.
        let _ = p.next(b'6');
        for _ in 0..MAX_PARAMS {
            let _ = p.next(b';');
        }
        // Feed another digit so param_acc_idx > 0 while params_idx == MAX_PARAMS.
        let _ = p.next(b'7');

        // A final byte triggers entry to dcs_passthrough. The DCS should
        // be dropped entirely, consistent with how CSI handles overflow.
        let a = p.next(b'p');
        assert!(a[0].is_none());
        assert!(a[1].is_none());
        assert!(a[2].is_none());
    }

    #[test]
    fn action_format() {
        assert_eq!(
            "terminal.Parser.Action{ .print = 'a' (U+61) }",
            format!("{}", Action::Print('a' as u32))
        );
        assert_eq!(
            "terminal.Parser.Action{ .execute = 0x7 }",
            format!("{}", Action::Execute(7))
        );
    }
}
