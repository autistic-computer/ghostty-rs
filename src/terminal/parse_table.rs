//! Port of `src/terminal/parse_table.zig`.
//!
//! STATUS: ported
//!
//! The primary export of this file is "TABLE", which contains a
//! compile-time-generated state transition table for VT emulation.
//!
//! This is based on the vt100.net state machine:
//! https://vt100.net/emu/dec_ansi_parser
//! But has some modifications:
//!
//!   * csi_param accepts the colon character (':') since the SGR command
//!     accepts colon as a valid parameter value.
//!

use crate::terminal::parser::{State, TransitionAction as Action};

/// The number of parser states.
const MAX_STATE: usize = State::COUNT;

/// The state transition table. The type is [u8][State]Transition but
/// generated at compile time to be exactly-sized.
pub static TABLE: Table = gen_table();

/// Table is the type of the state table.
pub type Table = [[Transition; MAX_STATE]; 256];

/// OptionalTable is private to this file. We use this to accumulate and
/// detect invalid transitions created.
type OptionalTable = [[Option<Transition>; MAX_STATE]; 256];

// Transition is the transition to take within the table
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub state: State,
    pub action: Action,
}

/// Function to generate the full state transition table for VT emulation.
const fn gen_table() -> Table {
    // We accumulate using an "optional" table so we can detect duplicates.
    let mut result: OptionalTable = [[None; MAX_STATE]; 256];

    // anywhere transitions
    let mut s: usize = 0;
    while s < MAX_STATE {
        let source = State::ALL[s];

        // anywhere => ground
        single(&mut result, 0x18, source, State::Ground, Action::Execute);
        single(&mut result, 0x1A, source, State::Ground, Action::Execute);
        range(&mut result, 0x80, 0x8F, source, State::Ground, Action::Execute);
        range(&mut result, 0x91, 0x97, source, State::Ground, Action::Execute);
        single(&mut result, 0x99, source, State::Ground, Action::Execute);
        single(&mut result, 0x9A, source, State::Ground, Action::Execute);
        single(&mut result, 0x9C, source, State::Ground, Action::None);

        // anywhere => escape
        single(&mut result, 0x1B, source, State::Escape, Action::None);

        // anywhere => sos_pm_apc_string
        single(&mut result, 0x98, source, State::SosPmApcString, Action::None);
        single(&mut result, 0x9E, source, State::SosPmApcString, Action::None);
        single(&mut result, 0x9F, source, State::SosPmApcString, Action::None);

        // anywhere => csi_entry
        single(&mut result, 0x9B, source, State::CsiEntry, Action::None);

        // anywhere => dcs_entry
        single(&mut result, 0x90, source, State::DcsEntry, Action::None);

        // anywhere => osc_string
        single(&mut result, 0x9D, source, State::OscString, Action::None);

        s += 1;
    }

    // ground
    {
        // events
        single(&mut result, 0x19, State::Ground, State::Ground, Action::Execute);
        range(&mut result, 0, 0x17, State::Ground, State::Ground, Action::Execute);
        range(&mut result, 0x1C, 0x1F, State::Ground, State::Ground, Action::Execute);
        range(&mut result, 0x20, 0x7F, State::Ground, State::Ground, Action::Print);
    }

    // escape_intermediate
    {
        let source = State::EscapeIntermediate;

        single(&mut result, 0x19, source, source, Action::Execute);
        range(&mut result, 0, 0x17, source, source, Action::Execute);
        range(&mut result, 0x1C, 0x1F, source, source, Action::Execute);
        range(&mut result, 0x20, 0x2F, source, source, Action::Collect);
        single(&mut result, 0x7F, source, source, Action::Ignore);

        // => ground
        range(&mut result, 0x30, 0x7E, source, State::Ground, Action::EscDispatch);
    }

    // sos_pm_apc_string
    {
        let source = State::SosPmApcString;

        // events
        single(&mut result, 0x19, source, source, Action::ApcPut);
        range(&mut result, 0, 0x17, source, source, Action::ApcPut);
        range(&mut result, 0x1C, 0x1F, source, source, Action::ApcPut);
        range(&mut result, 0x20, 0x7F, source, source, Action::ApcPut);
    }

    // escape
    {
        let source = State::Escape;

        // events
        single(&mut result, 0x19, source, source, Action::Execute);
        range(&mut result, 0, 0x17, source, source, Action::Execute);
        range(&mut result, 0x1C, 0x1F, source, source, Action::Execute);
        single(&mut result, 0x7F, source, source, Action::Ignore);

        // => ground
        range(&mut result, 0x30, 0x4F, source, State::Ground, Action::EscDispatch);
        range(&mut result, 0x51, 0x57, source, State::Ground, Action::EscDispatch);
        range(&mut result, 0x60, 0x7E, source, State::Ground, Action::EscDispatch);
        single(&mut result, 0x59, source, State::Ground, Action::EscDispatch);
        single(&mut result, 0x5A, source, State::Ground, Action::EscDispatch);
        single(&mut result, 0x5C, source, State::Ground, Action::EscDispatch);

        // => escape_intermediate
        range(&mut result, 0x20, 0x2F, source, State::EscapeIntermediate, Action::Collect);

        // => sos_pm_apc_string
        single(&mut result, 0x58, source, State::SosPmApcString, Action::None);
        single(&mut result, 0x5E, source, State::SosPmApcString, Action::None);
        single(&mut result, 0x5F, source, State::SosPmApcString, Action::None);

        // => dcs_entry
        single(&mut result, 0x50, source, State::DcsEntry, Action::None);

        // => csi_entry
        single(&mut result, 0x5B, source, State::CsiEntry, Action::None);

        // => osc_string
        single(&mut result, 0x5D, source, State::OscString, Action::None);
    }

    // dcs_entry
    {
        let source = State::DcsEntry;

        // events
        single(&mut result, 0x19, source, source, Action::Ignore);
        range(&mut result, 0, 0x17, source, source, Action::Ignore);
        range(&mut result, 0x1C, 0x1F, source, source, Action::Ignore);
        single(&mut result, 0x7F, source, source, Action::Ignore);

        // => dcs_intermediate
        range(&mut result, 0x20, 0x2F, source, State::DcsIntermediate, Action::Collect);

        // => dcs_ignore
        single(&mut result, 0x3A, source, State::DcsIgnore, Action::None);

        // => dcs_param
        range(&mut result, 0x30, 0x39, source, State::DcsParam, Action::Param);
        single(&mut result, 0x3B, source, State::DcsParam, Action::Param);
        range(&mut result, 0x3C, 0x3F, source, State::DcsParam, Action::Collect);

        // => dcs_passthrough
        range(&mut result, 0x40, 0x7E, source, State::DcsPassthrough, Action::None);
    }

    // dcs_intermediate
    {
        let source = State::DcsIntermediate;

        // events
        single(&mut result, 0x19, source, source, Action::Ignore);
        range(&mut result, 0, 0x17, source, source, Action::Ignore);
        range(&mut result, 0x1C, 0x1F, source, source, Action::Ignore);
        range(&mut result, 0x20, 0x2F, source, source, Action::Collect);
        single(&mut result, 0x7F, source, source, Action::Ignore);

        // => dcs_ignore
        range(&mut result, 0x30, 0x3F, source, State::DcsIgnore, Action::None);

        // => dcs_passthrough
        range(&mut result, 0x40, 0x7E, source, State::DcsPassthrough, Action::None);
    }

    // dcs_ignore
    {
        let source = State::DcsIgnore;

        // events
        single(&mut result, 0x19, source, source, Action::Ignore);
        range(&mut result, 0, 0x17, source, source, Action::Ignore);
        range(&mut result, 0x1C, 0x1F, source, source, Action::Ignore);

        // High bytes are ignored payload data, overriding the
        // "anywhere" C1 transitions. See dcs_passthrough below for more.
        // In dcs_ignore the additional concern is that a UTF-8 payload in
        // an ignored DCS could otherwise begin a live sequence mid-string
        // (e.g. 0x9B => csi_entry).
        range(&mut result, 0x80, 0xFF, source, source, Action::Ignore);
    }

    // dcs_param
    {
        let source = State::DcsParam;

        // events
        single(&mut result, 0x19, source, source, Action::Ignore);
        range(&mut result, 0, 0x17, source, source, Action::Ignore);
        range(&mut result, 0x1C, 0x1F, source, source, Action::Ignore);
        range(&mut result, 0x30, 0x39, source, source, Action::Param);
        single(&mut result, 0x3B, source, source, Action::Param);
        single(&mut result, 0x7F, source, source, Action::Ignore);

        // => dcs_ignore
        single(&mut result, 0x3A, source, State::DcsIgnore, Action::None);
        range(&mut result, 0x3C, 0x3F, source, State::DcsIgnore, Action::None);

        // => dcs_intermediate
        range(&mut result, 0x20, 0x2F, source, State::DcsIntermediate, Action::Collect);

        // => dcs_passthrough
        range(&mut result, 0x40, 0x7E, source, State::DcsPassthrough, Action::None);
    }

    // dcs_passthrough
    {
        let source = State::DcsPassthrough;

        // events
        single(&mut result, 0x19, source, source, Action::Put);
        range(&mut result, 0, 0x17, source, source, Action::Put);
        range(&mut result, 0x1C, 0x1F, source, source, Action::Put);
        range(&mut result, 0x20, 0x7E, source, source, Action::Put);
        single(&mut result, 0x7F, source, source, Action::Ignore);

        // High bytes are payload data, overriding the "anywhere" C1
        // transitions, matching how osc_string handles them below.
        // Ghostty is UTF-8 only, and DCS payloads carry UTF-8 text
        // (e.g. tmux control mode pane content): without this, a
        // UTF-8 continuation byte in the C1 range terminates or
        // corrupts the string and 0xA0-0xFF are silently dropped.
        //
        // This includes 0x9C (8-bit ST) on purpose: a raw 0x9C is
        // indistinguishable from a UTF-8 continuation byte ("Ü" is
        // 0xC3 0x9C), and Ghostty doesn't honor 8-bit C1 controls in
        // the ground state either (they go through UTF-8 decoding).
        // DCS strings terminate via 7-bit ST (ESC \) and abort via
        // CAN/SUB, which are unaffected here.
        range(&mut result, 0x80, 0xFF, source, source, Action::Put);
    }

    // csi_param
    {
        let source = State::CsiParam;

        // events
        single(&mut result, 0x19, source, source, Action::Execute);
        range(&mut result, 0, 0x17, source, source, Action::Execute);
        range(&mut result, 0x1C, 0x1F, source, source, Action::Execute);
        range(&mut result, 0x30, 0x39, source, source, Action::Param);
        single(&mut result, 0x3A, source, source, Action::Param);
        single(&mut result, 0x3B, source, source, Action::Param);
        single(&mut result, 0x7F, source, source, Action::Ignore);

        // => ground
        range(&mut result, 0x40, 0x7E, source, State::Ground, Action::CsiDispatch);

        // => csi_ignore
        range(&mut result, 0x3C, 0x3F, source, State::CsiIgnore, Action::None);

        // => csi_intermediate
        range(&mut result, 0x20, 0x2F, source, State::CsiIntermediate, Action::Collect);
    }

    // csi_ignore
    {
        let source = State::CsiIgnore;

        // events
        single(&mut result, 0x19, source, source, Action::Execute);
        range(&mut result, 0, 0x17, source, source, Action::Execute);
        range(&mut result, 0x1C, 0x1F, source, source, Action::Execute);
        range(&mut result, 0x20, 0x3F, source, source, Action::Ignore);
        single(&mut result, 0x7F, source, source, Action::Ignore);

        // => ground
        range(&mut result, 0x40, 0x7E, source, State::Ground, Action::None);
    }

    // csi_intermediate
    {
        let source = State::CsiIntermediate;

        // events
        single(&mut result, 0x19, source, source, Action::Execute);
        range(&mut result, 0, 0x17, source, source, Action::Execute);
        range(&mut result, 0x1C, 0x1F, source, source, Action::Execute);
        range(&mut result, 0x20, 0x2F, source, source, Action::Collect);
        single(&mut result, 0x7F, source, source, Action::Ignore);

        // => ground
        range(&mut result, 0x40, 0x7E, source, State::Ground, Action::CsiDispatch);

        // => csi_ignore
        range(&mut result, 0x30, 0x3F, source, State::CsiIgnore, Action::None);
    }

    // csi_entry
    {
        let source = State::CsiEntry;

        // events
        single(&mut result, 0x19, source, source, Action::Execute);
        range(&mut result, 0, 0x17, source, source, Action::Execute);
        range(&mut result, 0x1C, 0x1F, source, source, Action::Execute);
        single(&mut result, 0x7F, source, source, Action::Ignore);

        // => ground
        range(&mut result, 0x40, 0x7E, source, State::Ground, Action::CsiDispatch);

        // => csi_ignore
        single(&mut result, 0x3A, source, State::CsiIgnore, Action::None);

        // => csi_intermediate
        range(&mut result, 0x20, 0x2F, source, State::CsiIntermediate, Action::Collect);

        // => csi_param
        range(&mut result, 0x30, 0x39, source, State::CsiParam, Action::Param);
        single(&mut result, 0x3B, source, State::CsiParam, Action::Param);
        range(&mut result, 0x3C, 0x3F, source, State::CsiParam, Action::Collect);
    }

    // osc_string
    {
        let source = State::OscString;

        // events
        single(&mut result, 0x19, source, source, Action::Ignore);
        range(&mut result, 0, 0x06, source, source, Action::Ignore);
        range(&mut result, 0x08, 0x17, source, source, Action::Ignore);
        range(&mut result, 0x1C, 0x1F, source, source, Action::Ignore);
        range(&mut result, 0x20, 0xFF, source, source, Action::OscPut);

        // XTerm accepts either BEL  or ST  for terminating OSC
        // sequences, and when returning information, uses the same
        // terminator used in a query.
        single(&mut result, 0x07, source, State::Ground, Action::None);
    }

    // Create our immutable version
    let mut final_table: Table = [[transition(State::Ground, Action::None); MAX_STATE]; 256];
    let mut i: usize = 0;
    while i < 256 {
        let mut j: usize = 0;
        while j < MAX_STATE {
            final_table[i][j] = match result[i][j] {
                Some(t) => t,
                None => transition(State::ALL[j], Action::None),
            };
            j += 1;
        }
        i += 1;
    }

    final_table
}

const fn single(t: &mut OptionalTable, c: u8, s0: State, s1: State, a: Action) {
    let s0_int = s0 as usize;

    // TODO: enable this but it thinks we're in runtime right now
    // if (t[c][s0_int]) |existing| {
    //     @compileError("transition set multiple times");
    // }

    t[c as usize][s0_int] = Some(transition(s1, a));
}

const fn range(t: &mut OptionalTable, from: u8, to: u8, s0: State, s1: State, a: Action) {
    let mut i = from;
    loop {
        single(t, i, s0, s1, a);
        // If 'to' is 0xFF, our next pass will overflow. Return early to prevent
        // the loop from executing it's continue expression
        if i == to {
            break;
        }
        i += 1;
    }
}

const fn transition(state: State, action: Action) -> Transition {
    Transition { state, action }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_builds() {
        // This forces evaluation of the table, so we're just testing
        // that it succeeds in creation.
        let _ = &TABLE;
    }

    #[test]
    fn dcs_passthrough_high_bytes_are_payload_data() {
        // Bytes 0x80-0xFF within a DCS string are payload data, not C1
        // controls. This includes 0x9C (8-bit ST): a raw 0x9C is
        // indistinguishable from a UTF-8 continuation byte (e.g. "Ü" is
        // 0xC3 0x9C) and Ghostty doesn't support 8-bit C1 controls
        // anywhere else.
        for c in 0x80..0x100 {
            let entry = TABLE[c][State::DcsPassthrough as usize];
            assert_eq!(State::DcsPassthrough, entry.state);
            assert_eq!(Action::Put, entry.action);
        }
    }

    #[test]
    fn dcs_ignore_high_bytes_are_ignored_payload_data() {
        // Same as dcs_passthrough: a UTF-8 payload inside an ignored DCS
        // must not trigger "anywhere" C1 transitions (e.g. 0x9B beginning
        // a CSI mid-string).
        for c in 0x80..0x100 {
            let entry = TABLE[c][State::DcsIgnore as usize];
            assert_eq!(State::DcsIgnore, entry.state);
            assert_eq!(Action::Ignore, entry.action);
        }
    }

    #[test]
    fn dcs_passthrough_esc_can_and_sub_still_exit() {
        // 7-bit ST (ESC \) is the DCS terminator and CAN/SUB abort, so
        // these must continue to leave dcs_passthrough. dcs_unhook is
        // emitted by the parser on any transition out of dcs_passthrough.
        let esc = TABLE[0x1B][State::DcsPassthrough as usize];
        assert_eq!(State::Escape, esc.state);

        let can = TABLE[0x18][State::DcsPassthrough as usize];
        assert_eq!(State::Ground, can.state);

        let sub = TABLE[0x1A][State::DcsPassthrough as usize];
        assert_eq!(State::Ground, sub.state);
    }
}
