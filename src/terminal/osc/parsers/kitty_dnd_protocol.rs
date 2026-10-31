//! Port of `src/terminal/osc/parsers/kitty_dnd_protocol.zig`.
//!
//! STATUS: ported
//!
//! Kitty's drag and drop protocol (OSC 72)
//!
//! This only captures the raw metadata and payload for the OSC. The
//! actual protocol grammar (metadata keys, event types, chunking) is
//! implemented in `terminal/kitty/dnd.zig` and its submodules, since the
//! protocol requires stateful handling that doesn't belong in the
//! stateless OSC parser.
//!
//! Specification: https://sw.kovidgoyal.net/kitty/dnd-protocol/

use crate::terminal::osc::{Command, Parser, State, Terminator};

/// A Kitty drag and drop protocol command (OSC 72).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Osc<'a> {
    /// The raw metadata that was received. Parse with
    /// `kitty.dnd.Metadata.parse`.
    pub metadata: &'a [u8],

    /// The raw payload. Its meaning and encoding depend on the event
    /// type (`t` metadata key). Null when the OSC had no `;` after the
    /// metadata; an empty payload is distinct from no payload.
    pub payload: Option<&'a [u8]>,

    /// The terminator used for this OSC, so any response can match it.
    pub terminator: Terminator,
}

/// We don't currently support encoding this to C in any way.
pub type OscC = ();

impl Osc<'_> {
    pub fn cval(&self) -> OscC {}
}

pub fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    debug_assert!(parser.state == State::Osc72);

    let Some(cap) = parser.capture.as_ref() else {
        parser.state = State::Invalid;
        return None;
    };

    let data = cap.trailing();

    let (metadata, payload) = match data.iter().position(|&c| c == b';') {
        None => (data, None),
        Some(sep) => (&data[..sep], Some(&data[sep + 1..])),
    };

    Some(Command::KittyDndProtocol(Osc {
        metadata,
        payload,
        terminator: Terminator::init(terminator_ch),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::osc::Allocator;

    fn dnd(input: &[u8], terminator: u8, check: impl FnOnce(Osc<'_>)) {
        let mut p = Parser::init(Some(Allocator::GLOBAL));
        for &ch in input {
            p.next(ch);
        }
        match p.end(Some(terminator)) {
            Some(Command::KittyDndProtocol(c)) => check(c),
            other => panic!("expected kitty_dnd_protocol, got {:?}", other),
        }
    }

    #[test]
    fn osc_72_metadata_only_no_payload() {
        dnd(b"72;t=a", 0x1b, |c| {
            assert_eq!(b"t=a", c.metadata);
            assert!(c.payload.is_none());
        });
    }

    #[test]
    fn osc_72_metadata_and_empty_payload() {
        dnd(b"72;t=a;", 0x1b, |c| {
            assert_eq!(b"t=a", c.metadata);
            assert_eq!(b"", c.payload.unwrap());
        });
    }

    #[test]
    fn osc_72_metadata_and_non_empty_payload() {
        dnd(b"72;t=a:i=5;text/plain text/uri-list", 0x1b, |c| {
            assert_eq!(b"t=a:i=5", c.metadata);
            assert_eq!(b"text/plain text/uri-list", c.payload.unwrap());
        });
    }

    #[test]
    fn osc_72_empty_metadata_with_payload() {
        dnd(b"72;;payload", 0x1b, |c| {
            assert_eq!(b"", c.metadata);
            assert_eq!(b"payload", c.payload.unwrap());
        });
    }

    #[test]
    fn osc_72_bel_terminator_recorded() {
        dnd(b"72;t=q", 0x07, |c| {
            assert_eq!(Terminator::Bel, c.terminator);
        });
    }
}
