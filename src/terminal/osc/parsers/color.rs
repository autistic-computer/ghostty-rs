//! Port of `src/terminal/osc/parsers/color.zig`.
//!
//! STATUS: ported

use crate::lib::parse_int::parse_int_bits_unsigned;
use crate::terminal::color::{Dynamic as DynamicColor, Rgb, Special as SpecialColor};
use crate::terminal::osc::{ColorOperation, Command, Parser, State, Terminator};

const LOG_TARGET: &str = "osc_color";

/// Errors from [`parse_color`]. Allocation failure (`OutOfMemory`) cannot
/// happen with the global allocator but is kept to mirror the Zig error set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    OutOfMemory,
    MissingOperation,
}

/// The possible operations we support for colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    Osc4,
    Osc5,
    Osc10,
    Osc11,
    Osc12,
    Osc13,
    Osc14,
    Osc15,
    Osc16,
    Osc17,
    Osc18,
    Osc19,
    Osc104,
    Osc105,
    Osc110,
    Osc111,
    Osc112,
    Osc113,
    Osc114,
    Osc115,
    Osc116,
    Osc117,
    Osc118,
    Osc119,
}

/// Parse OSCs 4, 5, 10-19, 104, 105, 110-119
pub fn parse(parser: &mut Parser, terminator_ch: Option<u8>) -> Option<Command<'_>> {
    if parser.alloc.is_none() {
        parser.state = State::Invalid;
        return None;
    }
    // Check and make sure that we're parsing the correct OSCs
    let op = match parser.state {
        State::Osc4 => Operation::Osc4,
        State::Osc5 => Operation::Osc5,
        State::Osc10 => Operation::Osc10,
        State::Osc11 => Operation::Osc11,
        State::Osc12 => Operation::Osc12,
        State::Osc13 => Operation::Osc13,
        State::Osc14 => Operation::Osc14,
        State::Osc15 => Operation::Osc15,
        State::Osc16 => Operation::Osc16,
        State::Osc17 => Operation::Osc17,
        State::Osc18 => Operation::Osc18,
        State::Osc19 => Operation::Osc19,
        State::Osc104 => Operation::Osc104,
        State::Osc105 => Operation::Osc105,
        State::Osc110 => Operation::Osc110,
        State::Osc111 => Operation::Osc111,
        State::Osc112 => Operation::Osc112,
        State::Osc113 => Operation::Osc113,
        State::Osc114 => Operation::Osc114,
        State::Osc115 => Operation::Osc115,
        State::Osc116 => Operation::Osc116,
        State::Osc117 => Operation::Osc117,
        State::Osc118 => Operation::Osc118,
        State::Osc119 => Operation::Osc119,
        _ => {
            parser.state = State::Invalid;
            return None;
        }
    };
    // If we've collected any extra data parse that, otherwise use an empty
    // string.
    let data: &[u8] = match parser.capture.as_ref() {
        Some(cap) => cap.trailing(),
        None => b"",
    };
    let requests = match parse_color(op, data) {
        Ok(list) => list,
        Err(err) => {
            log::info!(
                target: LOG_TARGET,
                "failed to parse OSC {} color request err={:?} data={}",
                parser.state,
                err,
                String::from_utf8_lossy(data)
            );
            List::new()
        }
    };
    Some(Command::ColorOperation(ColorOperation {
        op,
        requests,
        terminator: Terminator::init(terminator_ch),
    }))
}

/// Zig `std.mem.tokenizeScalar(u8, buf, ';')`: split skipping empty tokens.
fn tokenize(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    buf.split(|&c| c == b';').filter(|t| !t.is_empty())
}

/// Parse any color operation string. This should NOT include the operation
/// itself, but only the body of the operation. e.g. for "4;a;b;c" the body
/// should be "a;b;c" and the operation should be set accordingly.
///
/// Color parsing is fairly complicated so we pull this out to a specialized
/// function rather than go through our OSC parsing state machine. This is
/// much slower and requires more memory (since we need to buffer the full
/// request) but grants us an easier to understand and testable implementation.
///
/// If color changing ends up being a bottleneck we can optimize this later.
fn parse_color(op: Operation, buf: &[u8]) -> Result<List, ParseError> {
    let mut it = tokenize(buf);
    let it = &mut it;
    use DynamicColor as D;
    Ok(match op {
        Operation::Osc4 => parse_get_set_ansi_color(Operation::Osc4, it),
        Operation::Osc5 => parse_get_set_ansi_color(Operation::Osc5, it),
        Operation::Osc104 => parse_reset_ansi_color(Operation::Osc104, it),
        Operation::Osc105 => parse_reset_ansi_color(Operation::Osc105, it),
        Operation::Osc10 => parse_get_set_dynamic_color(D::Foreground, it),
        Operation::Osc11 => parse_get_set_dynamic_color(D::Background, it),
        Operation::Osc12 => parse_get_set_dynamic_color(D::Cursor, it),
        Operation::Osc13 => parse_get_set_dynamic_color(D::PointerForeground, it),
        Operation::Osc14 => parse_get_set_dynamic_color(D::PointerBackground, it),
        Operation::Osc15 => parse_get_set_dynamic_color(D::TektronixForeground, it),
        Operation::Osc16 => parse_get_set_dynamic_color(D::TektronixBackground, it),
        Operation::Osc17 => parse_get_set_dynamic_color(D::HighlightBackground, it),
        Operation::Osc18 => parse_get_set_dynamic_color(D::TektronixCursor, it),
        Operation::Osc19 => parse_get_set_dynamic_color(D::HighlightForeground, it),
        Operation::Osc110 => parse_reset_dynamic_color(D::Foreground, it),
        Operation::Osc111 => parse_reset_dynamic_color(D::Background, it),
        Operation::Osc112 => parse_reset_dynamic_color(D::Cursor, it),
        Operation::Osc113 => parse_reset_dynamic_color(D::PointerForeground, it),
        Operation::Osc114 => parse_reset_dynamic_color(D::PointerBackground, it),
        Operation::Osc115 => parse_reset_dynamic_color(D::TektronixForeground, it),
        Operation::Osc116 => parse_reset_dynamic_color(D::TektronixBackground, it),
        Operation::Osc117 => parse_reset_dynamic_color(D::HighlightBackground, it),
        Operation::Osc118 => parse_reset_dynamic_color(D::TektronixCursor, it),
        Operation::Osc119 => parse_reset_dynamic_color(D::HighlightForeground, it),
    })
}

/// Parse a color index as a Zig `u9`.
fn parse_u9(s: &[u8]) -> Option<u16> {
    parse_int_bits_unsigned(s, 10, 9).ok().map(|v| v as u16)
}

/// Map an OSC 4/104 index (0-255 palette, 256-260 special) or an OSC
/// 5/105 index (special) to a target.
fn ansi_target(op: Operation, color: u16) -> Option<Target> {
    match op {
        // OSC5 maps directly to the Special enum.
        Operation::Osc5 | Operation::Osc105 => {
            if color > 7 {
                return None;
            }
            Some(Target::Special(SpecialColor::from_int(color as u64)?))
        }

        // OSC4 maps 0-255 to palette, 256-259 to special offset
        // by the palette count.
        Operation::Osc4 | Operation::Osc104 => {
            if color <= u8::MAX as u16 {
                Some(Target::Palette(color as u8))
            } else {
                let v = color - 256;
                if v > 7 {
                    return None;
                }
                Some(Target::Special(SpecialColor::from_int(v as u64)?))
            }
        }

        _ => unreachable!(),
    }
}

/// OSC 4/5
fn parse_get_set_ansi_color<'a>(op: Operation, it: &mut impl Iterator<Item = &'a [u8]>) -> List {
    // Note: in ANY error scenario below we return the accumulated results.
    // This matches the xterm behavior (see misc.c ChangeAnsiColorRequest)

    let mut result = List::new();
    loop {
        // We expect a `c; spec` pair. If either doesn't exist then
        // we return the results up to this point.
        let Some(color_str) = it.next() else {
            return result;
        };
        let Some(spec_str) = it.next() else {
            return result;
        };

        // Color must be numeric. u9 because that'll fit our palette + special
        let Some(color) = parse_u9(color_str) else {
            return result;
        };

        // Parse the color.
        let Some(target) = ansi_target(op, color) else {
            return result;
        };

        // "?" always results in a query.
        if spec_str == b"?" {
            result.push(Request::Query(target));
            continue;
        }

        let Ok(rgb) = Rgb::parse(spec_str) else {
            return result;
        };
        result.push(Request::Set(ColoredTarget { target, color: rgb }));
    }
}

/// OSC 104/105: Reset ANSI Colors
fn parse_reset_ansi_color<'a>(op: Operation, it: &mut impl Iterator<Item = &'a [u8]>) -> List {
    // Note: xterm stops parsing the reset list on any error, but we're
    // more flexible and try the next value. This matches the behavior of
    // Kitty and I don't see a downside to being more flexible here. Hopefully
    // no one depends on the exact behavior of xterm.

    let mut result = List::new();
    loop {
        let Some(color_str) = it.next() else {
            // If no parameters are given, we reset the full table.
            if result.is_empty() {
                result.push(match op {
                    Operation::Osc104 => Request::ResetPalette,
                    Operation::Osc105 => Request::ResetSpecial,
                    _ => unreachable!(),
                });
            }
            return result;
        };

        // Empty color strings are ignored, not treated as an error.
        if color_str.is_empty() {
            continue;
        }

        // Color must be numeric. u9 because that'll fit our palette + special
        let Some(color) = parse_u9(color_str) else {
            continue;
        };

        // Parse the color.
        let Some(target) = ansi_target(op, color) else {
            continue;
        };

        result.push(Request::Reset(target));
    }
}

/// OSC 10-19: Get/Set Dynamic Colors
fn parse_get_set_dynamic_color<'a>(
    start: DynamicColor,
    it: &mut impl Iterator<Item = &'a [u8]>,
) -> List {
    // Note: in ANY error scenario below we return the accumulated results.
    // This matches the xterm behavior (see misc.c ChangeColorsRequest)

    let mut result = List::new();
    let mut color = start;
    loop {
        let Some(spec_str) = it.next() else {
            return result;
        };

        if spec_str == b"?" {
            result.push(Request::Query(Target::Dynamic(color)));
        } else {
            let Ok(rgb) = Rgb::parse(spec_str) else {
                return result;
            };
            result.push(Request::Set(ColoredTarget {
                target: Target::Dynamic(color),
                color: rgb,
            }));
        }

        // Each successive value uses the next color so long as it exists.
        let Some(next) = color.next() else {
            return result;
        };
        color = next;
    }
}

/// OSC 110-119: Reset Dynamic Colors
fn parse_reset_dynamic_color<'a>(
    color: DynamicColor,
    it: &mut impl Iterator<Item = &'a [u8]>,
) -> List {
    let mut result = List::new();
    if it.next().is_some() {
        return result;
    }
    result.push(Request::Reset(Target::Dynamic(color)));
    result
}

/// The list of requests in a single color OSC. In most cases, OSC
/// 4/104/etc. send very few.
///
/// Zig uses a `SegmentedList(Request, 2)` to avoid copying; a `Vec` is
/// used here.
pub type List = Vec<Request>;

/// A single operation related to the terminal color palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Set(ColoredTarget),
    Query(Target),
    Reset(Target),
    ResetPalette,
    ResetSpecial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Palette(u8),
    Special(SpecialColor),
    Dynamic(DynamicColor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColoredTarget {
    pub target: Target,
    pub color: Rgb,
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgb = Rgb { r: 255, g: 0, b: 0 };
    const BLUE: Rgb = Rgb { r: 0, g: 0, b: 255 };

    fn set(target: Target, color: Rgb) -> Request {
        Request::Set(ColoredTarget { target, color })
    }

    #[test]
    fn osc_4_empty_param() {
        let mut p = Parser::init(None);

        for &ch in b"4;;" {
            p.next(ch);
        }

        assert!(p.end(Some(0x1b)).is_none());
    }

    #[test]
    fn osc_color_indexes_reject_digit_separators() {
        for op in [Operation::Osc4, Operation::Osc5] {
            let list = parse_color(op, b"1;red;0_3;blue;2;green").unwrap();
            // Invalid indexes stop get/set requests at the valid prefix.
            assert_eq!(1, list.len());
        }

        for op in [Operation::Osc104, Operation::Osc105] {
            let list = parse_color(op, b"1;0_3;2").unwrap();
            // Invalid indexes are skipped in reset requests.
            assert_eq!(2, list.len());
        }
    }

    #[test]
    fn osc_4() {
        // Test every palette index
        for idx in 0..u8::MAX as usize {
            // Simple color set
            // printf '\e]4;0;red\\'
            {
                let body = format!("{};red", idx);
                let list = parse_color(Operation::Osc4, body.as_bytes()).unwrap();
                assert_eq!(1, list.len());
                assert_eq!(set(Target::Palette(idx as u8), RED), list[0]);
            }

            // Simple color query
            // printf '\e]4;0;?\\'
            {
                let body = format!("{};?", idx);
                let list = parse_color(Operation::Osc4, body.as_bytes()).unwrap();
                assert_eq!(1, list.len());
                assert_eq!(Request::Query(Target::Palette(idx as u8)), list[0]);
            }

            // Trailing invalid data produces results up to that point
            // printf '\e]4;0;red;\e\\'
            {
                let body = format!("{};red;", idx);
                let list = parse_color(Operation::Osc4, body.as_bytes()).unwrap();
                assert_eq!(1, list.len());
                assert_eq!(set(Target::Palette(idx as u8), RED), list[0]);
            }

            // Whitespace doesn't produce a working value in xterm but we
            // allow it because Kitty does and it seems harmless.
            //
            // printf '\e]4;0;red \e\\'
            {
                let body = format!("{};red ", idx);
                let list = parse_color(Operation::Osc4, body.as_bytes()).unwrap();
                assert_eq!(1, list.len());
                assert_eq!(set(Target::Palette(idx as u8), RED), list[0]);
            }
        }

        // Test every special color
        for i in 0..SpecialColor::COUNT {
            let special = SpecialColor::from_int(i as u64).unwrap();

            // Simple color set
            // printf '\e]4;256;red\\'
            let body = format!("{};red", 256 + i);
            let list = parse_color(Operation::Osc4, body.as_bytes()).unwrap();
            assert_eq!(1, list.len());
            assert_eq!(set(Target::Special(special), RED), list[0]);
        }
    }

    #[test]
    fn osc_5() {
        // Test every special color
        for i in 0..SpecialColor::COUNT {
            let special = SpecialColor::from_int(i as u64).unwrap();

            // Simple color set
            // printf '\e]4;256;red\\'
            let body = format!("{};red", i);
            let list = parse_color(Operation::Osc5, body.as_bytes()).unwrap();
            assert_eq!(1, list.len());
            assert_eq!(set(Target::Special(special), RED), list[0]);
        }
    }

    #[test]
    fn osc_4_multiple_requests() {
        // printf '\e]4;0;red;1;blue\e\\'
        {
            let list = parse_color(Operation::Osc4, b"0;red;1;blue").unwrap();
            assert_eq!(2, list.len());
            assert_eq!(set(Target::Palette(0), RED), list[0]);
            assert_eq!(set(Target::Palette(1), BLUE), list[1]);
        }

        // Multiple requests with same index overwrite each other
        // printf '\e]4;0;red;0;blue\e\\'
        {
            let list = parse_color(Operation::Osc4, b"0;red;0;blue").unwrap();
            assert_eq!(2, list.len());
            assert_eq!(set(Target::Palette(0), RED), list[0]);
            assert_eq!(set(Target::Palette(0), BLUE), list[1]);
        }
    }

    #[test]
    fn osc_104() {
        // Test every palette index
        for idx in 0..u8::MAX as usize {
            // Simple color set
            // printf '\e]104;0\\'
            let body = format!("{}", idx);
            let list = parse_color(Operation::Osc104, body.as_bytes()).unwrap();
            assert_eq!(1, list.len());
            assert_eq!(Request::Reset(Target::Palette(idx as u8)), list[0]);
        }

        // Test every special color
        for i in 0..SpecialColor::COUNT {
            let special = SpecialColor::from_int(i as u64).unwrap();

            // Simple color set
            // printf '\e]104;256\\'
            let body = format!("{}", 256 + i);
            let list = parse_color(Operation::Osc104, body.as_bytes()).unwrap();
            assert_eq!(1, list.len());
            assert_eq!(Request::Reset(Target::Special(special)), list[0]);
        }
    }

    #[test]
    fn osc_104_empty_index() {
        let list = parse_color(Operation::Osc104, b"0;;1").unwrap();
        assert_eq!(2, list.len());
        assert_eq!(Request::Reset(Target::Palette(0)), list[0]);
        assert_eq!(Request::Reset(Target::Palette(1)), list[1]);
    }

    #[test]
    fn osc_104_invalid_index() {
        let list = parse_color(Operation::Osc104, b"ffff;1").unwrap();
        assert_eq!(1, list.len());
        assert_eq!(Request::Reset(Target::Palette(1)), list[0]);
    }

    #[test]
    fn osc_104_reset_all() {
        let list = parse_color(Operation::Osc104, b"").unwrap();
        assert_eq!(1, list.len());
        assert_eq!(Request::ResetPalette, list[0]);
    }

    #[test]
    fn osc_105_reset_all() {
        let list = parse_color(Operation::Osc105, b"").unwrap();
        assert_eq!(1, list.len());
        assert_eq!(Request::ResetSpecial, list[0]);
    }

    const DYNAMIC_OPS: [(DynamicColor, Operation, Operation); 10] = [
        (DynamicColor::Foreground, Operation::Osc10, Operation::Osc110),
        (DynamicColor::Background, Operation::Osc11, Operation::Osc111),
        (DynamicColor::Cursor, Operation::Osc12, Operation::Osc112),
        (DynamicColor::PointerForeground, Operation::Osc13, Operation::Osc113),
        (DynamicColor::PointerBackground, Operation::Osc14, Operation::Osc114),
        (DynamicColor::TektronixForeground, Operation::Osc15, Operation::Osc115),
        (DynamicColor::TektronixBackground, Operation::Osc16, Operation::Osc116),
        (DynamicColor::HighlightBackground, Operation::Osc17, Operation::Osc117),
        (DynamicColor::TektronixCursor, Operation::Osc18, Operation::Osc118),
        (DynamicColor::HighlightForeground, Operation::Osc19, Operation::Osc119),
    ];

    // OSC 10-19: Get/Set Dynamic Colors
    #[test]
    fn osc_10_to_19_dynamic() {
        for (color, op, _) in DYNAMIC_OPS {
            // Example script:
            // printf '\e]10;red\e\\'
            let list = parse_color(op, b"red").unwrap();
            assert_eq!(1, list.len());
            assert_eq!(set(Target::Dynamic(color), RED), list[0]);
        }
    }

    #[test]
    fn osc_10_to_19_dynamic_multiple() {
        // Example script:
        // printf '\e]11;red;blue\e\\'
        let list = parse_color(Operation::Osc11, b"red;blue").unwrap();
        assert_eq!(2, list.len());
        assert_eq!(set(Target::Dynamic(DynamicColor::Background), RED), list[0]);
        assert_eq!(set(Target::Dynamic(DynamicColor::Cursor), BLUE), list[1]);
    }

    // OSC 110-119: Reset Dynamic Colors
    #[test]
    fn osc_110_to_119_reset_dynamic() {
        for (color, _, op) in DYNAMIC_OPS {
            // Example script:
            // printf '\e]110\e\\'
            {
                let list = parse_color(op, b"").unwrap();
                assert_eq!(1, list.len());
                assert_eq!(Request::Reset(Target::Dynamic(color)), list[0]);
            }

            // xterm allows a trailing semicolon. script to verify:
            //
            // printf '\e]110;\e\\'
            {
                let list = parse_color(op, b";").unwrap();
                assert_eq!(1, list.len());
                assert_eq!(Request::Reset(Target::Dynamic(color)), list[0]);
            }

            // xterm does NOT allow any whitespace
            //
            // printf '\e]110 \e\\'
            {
                let list = parse_color(op, b" ").unwrap();
                assert_eq!(0, list.len());
            }
        }
    }
}
