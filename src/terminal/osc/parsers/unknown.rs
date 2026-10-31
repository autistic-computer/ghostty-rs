//! Port of `src/terminal/osc/parsers/unknown.zig`.
//!
//! STATUS: ported

use crate::terminal::osc::{Command, Parser, State, Terminator, Unknown};

/// Build `Command::Unknown` from the bytes collected for a sequence whose
/// number the parser does not implement. Only reached when
/// `Parser::unknown_max_bytes` is nonzero.
pub fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    debug_assert!(parser.state == State::Unknown || parser.state == State::UnknownTruncated);

    let truncated = parser.state == State::UnknownTruncated;
    let cap = parser.capture.as_ref().unwrap();
    Some(Command::Unknown(Unknown {
        content: cap.trailing(),
        truncated,
        terminator: Terminator::init(terminator_ch),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::osc::{Allocator, Backing};

    const CAN: u8 = 0x18;
    const SUB: u8 = 0x1A;

    fn unknown<'a>(cmd: Option<Command<'a>>) -> Unknown<'a> {
        match cmd {
            Some(Command::Unknown(u)) => u,
            other => panic!("expected unknown, got {:?}", other),
        }
    }

    #[test]
    fn osc_unknown_prefix_state_names_are_the_bytes_consumed() {
        // unknown_or_invalid recovers the consumed identifier from the prefix
        // state's name, so every prefix state must be reachable by feeding
        // its name.
        for state in State::ALL {
            match state {
                State::Start | State::Invalid | State::Unknown | State::UnknownTruncated => {}
                _ => {
                    let mut p = Parser::init(None);
                    for &ch in state.as_str().as_bytes() {
                        p.next(ch);
                    }
                    assert_eq!(state, p.state);
                }
            }
        }
    }

    #[test]
    fn osc_unknown_disabled_by_default() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));

        assert_eq!(0usize, p.unknown_max_bytes);
        p.next_slice(b"7400;status=busy");
        assert_eq!(State::Invalid, p.state);
        assert!(p.capture.is_none());
        assert!(p.end(Some(0x07)).is_none());

        // A sequence ending on a partial number is dropped too.
        p.reset();
        p.next_slice(b"77");
        assert!(p.end(Some(0x07)).is_none());
    }

    #[test]
    fn osc_unknown_reported() {
        struct Case {
            input: &'static [u8],
            terminator_ch: Option<u8>,
            terminator: Terminator,
        }
        const fn c(input: &'static [u8]) -> Case {
            Case {
                input,
                terminator_ch: Some(0x1B),
                terminator: Terminator::St,
            }
        }
        let cases = [
            // Unsupported number with a body.
            c(b"7400;status=busy"),
            // Unsupported number with no body, BEL terminated.
            Case {
                input: b"7400",
                terminator_ch: Some(0x07),
                terminator: Terminator::Bel,
            },
            // Bridge states that only lead to longer identifiers.
            c(b"3;x"),
            c(b"30;x"),
            c(b"300;x"),
            c(b"55;x"),
            c(b"552;x"),
            c(b"6;x"),
            c(b"77;x"),
            // Bridge states with no body end on the prefix itself.
            c(b"3"),
            c(b"300"),
            Case {
                input: b"77",
                terminator_ch: Some(0x07),
                terminator: Terminator::Bel,
            },
            // Known number extended by a digit.
            c(b"1338;x"),
            c(b"1339"),
            // Leading zero is not a known number.
            c(b"0133;x"),
            // Non-numeric identifier.
            c(b"I;name"),
            // Known number followed by junk before ';'.
            c(b"133 x"),
            // Empty identifier.
            c(b";x"),
            // A null terminator is treated as ST.
            Case {
                input: b"7400;x",
                terminator_ch: None,
                terminator: Terminator::St,
            },
        ];

        for case in cases {
            let mut p = Parser::init(Some(Allocator::GLOBAL));
            p.unknown_max_bytes = 64;

            p.next_slice(case.input);
            let cmd = unknown(p.end(case.terminator_ch));
            assert_eq!(case.input, cmd.content);
            assert!(!cmd.truncated);
            assert_eq!(case.terminator, cmd.terminator);
        }
    }

    #[test]
    fn osc_unknown_supported_commands_are_unchanged() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));
        p.unknown_max_bytes = 64;

        // A supported command still parses as itself.
        p.next_slice(b"0;title");
        let cmd = p.end(Some(0x07)).unwrap();
        assert_eq!(Command::ChangeWindowTitle(b"title"), cmd);

        // A known number with a malformed body is not reported as unknown.
        p.reset();
        p.next_slice(b"9;4;9");
        if let Some(c) = p.end(Some(0x07)) {
            assert!(!matches!(c, Command::Unknown(_)));
        }

        // A known number with an unsupported sub-command is not reported.
        p.reset();
        p.next_slice(b"1337;SetUserVar=foo=YmFy");
        if let Some(c) = p.end(Some(0x07)) {
            assert!(!matches!(c, Command::Unknown(_)));
        }

        // A known number without a body is not reported.
        p.reset();
        p.next_slice(b"7");
        if let Some(c) = p.end(Some(0x07)) {
            assert!(!matches!(c, Command::Unknown(_)));
        }

        // An empty OSC has no identifier and is not reported.
        p.reset();
        assert!(p.end(Some(0x07)).is_none());
    }

    #[test]
    fn osc_unknown_aborted_sequences_are_dropped() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));
        p.unknown_max_bytes = 64;

        p.next_slice(b"7400;x");
        assert!(p.end(Some(CAN)).is_none());

        p.reset();
        p.next_slice(b"7400;x");
        assert!(p.end(Some(SUB)).is_none());

        // Bridge prefixes are dropped on abort too.
        p.reset();
        p.next_slice(b"77");
        assert!(p.end(Some(CAN)).is_none());
    }

    #[test]
    fn osc_unknown_truncated_at_the_limit() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));
        p.unknown_max_bytes = 8;

        // Exactly at the limit is not truncated.
        p.next_slice(b"7400;abc");
        let cmd = unknown(p.end(Some(0x07)));
        assert_eq!(b"7400;abc", cmd.content);
        assert!(!cmd.truncated);

        // One byte over is truncated but still reported.
        p.reset();
        p.next_slice(b"7400;abcd");
        let cmd = unknown(p.end(Some(0x07)));
        assert_eq!(b"7400;abc", cmd.content);
        assert!(cmd.truncated);

        // Later input is discarded, both by slice and by byte.
        p.reset();
        p.next_slice(b"7400;abcdef");
        p.next_slice(b"ghi");
        p.next(b'j');
        let cmd = unknown(p.end(Some(0x07)));
        assert_eq!(b"7400;abc", cmd.content);
        assert!(cmd.truncated);
    }

    #[test]
    fn osc_unknown_limit_smaller_than_the_identifier() {
        let mut p = Parser::init(Some(Allocator::GLOBAL));
        p.unknown_max_bytes = 2;

        // The prefix itself overflows while beginning the capture.
        p.next_slice(b"7400;x");
        let cmd = unknown(p.end(Some(0x07)));
        assert_eq!(b"74", cmd.content);
        assert!(cmd.truncated);

        // The prefix fits but the unrecognized byte does not.
        p.reset();
        p.next_slice(b"77x");
        let cmd = unknown(p.end(Some(0x07)));
        assert_eq!(b"77", cmd.content);
        assert!(cmd.truncated);

        // A bridge prefix longer than the limit at end.
        p.reset();
        p.unknown_max_bytes = 1;
        p.next_slice(b"77");
        let cmd = unknown(p.end(Some(0x07)));
        assert_eq!(b"7", cmd.content);
        assert!(cmd.truncated);
    }

    #[test]
    fn osc_unknown_fixed_tier_does_not_allocate() {
        let mut p = Parser::init(Some(Allocator::FAILING));
        p.unknown_max_bytes = Parser::MAX_BUF;

        p.next_slice(b"7400;");
        assert_eq!(Backing::Fixed, p.capture.as_ref().unwrap().backing);
        assert_eq!(Parser::MAX_BUF, p.capture.as_ref().unwrap().max_bytes);
        p.next_slice(b"x");
        let cmd = unknown(p.end(Some(0x07)));
        assert_eq!(b"7400;x", cmd.content);
    }

    #[test]
    fn osc_unknown_allocating_tier() {
        let limit = Parser::MAX_BUF * 2;

        let mut p = Parser::init(Some(Allocator::GLOBAL));
        p.unknown_max_bytes = limit;

        p.next_slice(b"7400;");
        assert_eq!(Backing::Allocating, p.capture.as_ref().unwrap().backing);

        let body = vec![b'a'; limit];

        // Fill to exactly the limit, including the 5 identifier bytes.
        p.next_slice(&body[..limit - 5]);
        assert_eq!(State::Unknown, p.state);
        assert_eq!(limit, p.capture.as_ref().unwrap().buffer_len());

        // One more byte truncates without growing the allocation.
        p.next_slice(b"a");
        assert_eq!(State::UnknownTruncated, p.state);
        assert_eq!(limit, p.capture.as_ref().unwrap().buffer_len());

        let cmd = unknown(p.end(Some(0x07)));
        assert!(cmd.truncated);
        assert_eq!(limit, cmd.content.len());
        assert!(cmd.content.starts_with(b"7400;aaa"));
    }

    #[test]
    fn osc_unknown_large_limit_without_allocator_uses_fixed_buffer() {
        let mut p = Parser::init(None);
        p.unknown_max_bytes = Parser::MAX_BUF * 2;

        p.next_slice(b"7400;");
        assert_eq!(Backing::Fixed, p.capture.as_ref().unwrap().backing);

        let body = [b'a'; Parser::MAX_BUF];
        p.next_slice(&body);

        let cmd = unknown(p.end(Some(0x07)));
        assert!(cmd.truncated);
        assert_eq!(Parser::MAX_BUF, cmd.content.len());
    }

    #[test]
    fn osc_unknown_allocation_failure_falls_back_to_fixed_buffer() {
        let mut p = Parser::init(Some(Allocator::FAILING));
        p.unknown_max_bytes = Parser::MAX_BUF * 2;

        p.next_slice(b"7400;x");
        assert_eq!(Backing::Fixed, p.capture.as_ref().unwrap().backing);
        let cmd = unknown(p.end(Some(0x07)));
        assert_eq!(b"7400;x", cmd.content);
    }

    #[test]
    fn osc_unknown_allocation_failure_while_growing_truncates() {
        // Allow the initial capacity allocation and fail the first growth,
        // whether it is attempted in place or by a new allocation.
        let failing = Allocator::failing(1, 0);

        let mut p = Parser::init(Some(failing));
        p.unknown_max_bytes = Parser::MAX_BUF * 2;

        p.next_slice(b"7400;");
        assert_eq!(Backing::Allocating, p.capture.as_ref().unwrap().backing);

        let body = [b'a'; Parser::MAX_BUF];
        p.next_slice(&body);
        assert_eq!(State::UnknownTruncated, p.state);
        let retained = p.capture.as_ref().unwrap().trailing().len();
        assert!(retained <= Parser::MAX_BUF);

        // Later bytes must not be appended after the gap, even if an
        // allocation would now succeed.
        for a in [
            p.alloc.as_mut().unwrap(),
            p.capture.as_mut().unwrap().alloc.as_mut().unwrap(),
        ] {
            a.fail_index = usize::MAX;
            a.resize_fail_index = usize::MAX;
        }
        p.next_slice(b"zz");
        p.next(b'z');

        let cmd = unknown(p.end(Some(0x07)));
        assert!(cmd.truncated);
        assert_eq!(retained, cmd.content.len());
        assert!(!cmd.content.contains(&b'z'));
    }

    #[test]
    fn osc_unknown_next_slice_matches_per_byte_parsing() {
        let inputs: [&[u8]; 5] = [
            b"7400;status=busy:jobs=4",
            b"I;name",
            b"1338;x",
            b"77",
            b"7400;abcdefghijklmnop",
        ];

        for input in inputs {
            // Per-byte reference.
            let mut r = Parser::init(Some(Allocator::GLOBAL));
            r.unknown_max_bytes = 12;
            for &ch in input {
                r.next(ch);
            }
            let expected = unknown(r.end(Some(0x07)));
            let expected_content = expected.content.to_vec();
            let expected_truncated = expected.truncated;

            // Every two-way split of the input must parse identically.
            for split in 0..input.len() + 1 {
                let mut p = Parser::init(Some(Allocator::GLOBAL));
                p.unknown_max_bytes = 12;
                p.next_slice(&input[..split]);
                p.next_slice(&input[split..]);

                let cmd = unknown(p.end(Some(0x07)));
                assert_eq!(&expected_content[..], cmd.content);
                assert_eq!(expected_truncated, cmd.truncated);
            }
        }
    }
}
