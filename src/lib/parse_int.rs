//! Port of `src/lib/parse_int.zig`.
//!
//! STATUS: ported
//!
//! Integer parsing for terminal protocols, without Zig digit separators.
//!
//! Adapted from Zig's `std.fmt.parseInt` (MIT license). See
//! src/lib/compat/README.md for license and details.

use std::fmt;

/// Errors from [`parse_int`] (Zig: `std.fmt.ParseIntError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseIntError {
    /// The result cannot fit in the type specified.
    Overflow,
    /// The input was empty or contained an invalid character.
    InvalidCharacter,
}

impl fmt::Display for ParseIntError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseIntError::Overflow => f.write_str("Overflow"),
            ParseIntError::InvalidCharacter => f.write_str("InvalidCharacter"),
        }
    }
}

impl std::error::Error for ParseIntError {}

/// Integer types that [`parse_int`] can produce.
pub trait ParseIntTarget: Sized + Copy {
    const SIGNED: bool;
    const BITS: u32;
    fn from_i128(v: i128) -> Self;
    fn from_u128(v: u128) -> Self;
}

macro_rules! impl_target {
    ($($t:ty: $signed:expr),*) => {
        $(impl ParseIntTarget for $t {
            const SIGNED: bool = $signed;
            const BITS: u32 = <$t>::BITS;
            fn from_i128(v: i128) -> Self {
                v as $t
            }
            fn from_u128(v: u128) -> Self {
                v as $t
            }
        })*
    };
}

impl_target!(
    u8: false, u16: false, u32: false, u64: false, u128: false, usize: false,
    i8: true, i16: true, i32: true, i64: true, i128: true, isize: true
);

/// Parse ASCII digits in the given base. Signed types allow a leading sign;
/// unsigned types accept digits only.
///
/// This exists because `std.fmt.parseInt` implements Zig integer literal
/// syntax rather than what terminal protocols specify: it ignores `_` digit
/// separators (so `4_2` parses as 42) and accepts a leading `+` or `-` even
/// for unsigned types. Accepting that input makes us diverge from other
/// terminals, so don't replace this with a more lenient parser. (Rust's
/// `str::parse` also accepts a leading `+` for unsigned types.)
pub fn parse_int<T: ParseIntTarget>(value: &[u8], base: u8) -> Result<T, ParseIntError> {
    if T::SIGNED {
        parse_int_bits_signed(value, base, T::BITS).map(T::from_i128)
    } else {
        parse_int_bits_unsigned(value, base, T::BITS).map(T::from_u128)
    }
}

/// Parse into a signed integer of arbitrary bit width `bits` (1..=128),
/// e.g. Zig's `i3`. The result is returned widened to `i128`.
pub fn parse_int_bits_signed(value: &[u8], base: u8, bits: u32) -> Result<i128, ParseIntError> {
    assert!((2..=36).contains(&base));
    assert!((1..=128).contains(&bits));
    if value.is_empty() {
        return Err(ParseIntError::InvalidCharacter);
    }
    let (rest, neg) = match value[0] {
        b'+' => (&value[1..], false),
        b'-' => (&value[1..], true),
        _ => (value, false),
    };
    if rest.is_empty() {
        return Err(ParseIntError::InvalidCharacter);
    }

    let signed_range = |bits: u32| -> (i128, i128) {
        if bits == 128 {
            (i128::MIN, i128::MAX)
        } else {
            (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
        }
    };
    let (min, max) = signed_range(bits);

    // Like Zig, accumulate in an integer of at least 8 bits and report
    // overflow as soon as the accumulator overflows.
    let (acc_min, acc_max) = signed_range(bits.max(8));
    let check = |v: Option<i128>| -> Result<i128, ParseIntError> {
        match v {
            Some(v) if v >= acc_min && v <= acc_max => Ok(v),
            _ => Err(ParseIntError::Overflow),
        }
    };

    let mut accumulate: i128 = 0;
    for &c in rest {
        let digit = char_to_digit(c, base)?;
        accumulate = check(accumulate.checked_mul(base as i128))?;
        accumulate = check(if neg {
            accumulate.checked_sub(digit as i128)
        } else {
            accumulate.checked_add(digit as i128)
        })?;
    }

    if accumulate < min || accumulate > max {
        return Err(ParseIntError::Overflow);
    }
    Ok(accumulate)
}

/// Parse into an unsigned integer of arbitrary bit width `bits`
/// (1..=128). The result is returned widened to `u128`.
pub fn parse_int_bits_unsigned(value: &[u8], base: u8, bits: u32) -> Result<u128, ParseIntError> {
    assert!((2..=36).contains(&base));
    assert!((1..=128).contains(&bits));
    if value.is_empty() {
        return Err(ParseIntError::InvalidCharacter);
    }

    let unsigned_max = |bits: u32| -> u128 {
        if bits == 128 {
            u128::MAX
        } else {
            (1u128 << bits) - 1
        }
    };
    let max = unsigned_max(bits);

    // Like Zig, accumulate in an integer of at least 8 bits and report
    // overflow as soon as the accumulator overflows.
    let acc_max = unsigned_max(bits.max(8));
    let check = |v: Option<u128>| -> Result<u128, ParseIntError> {
        match v {
            Some(v) if v <= acc_max => Ok(v),
            _ => Err(ParseIntError::Overflow),
        }
    };

    let mut accumulate: u128 = 0;
    for &c in value {
        let digit = char_to_digit(c, base)?;
        accumulate = check(accumulate.checked_mul(base as u128))?;
        accumulate = check(accumulate.checked_add(digit as u128))?;
    }

    if accumulate > max {
        return Err(ParseIntError::Overflow);
    }
    Ok(accumulate)
}

/// Zig: `std.fmt.parseInt`, i.e. Zig integer literal syntax, for the
/// places where Ghostty deliberately uses it instead of [`parse_int`]
/// (config values, tmux output, cache files): an optional leading `+` or
/// `-` for every type (a negative non-zero value overflows an unsigned
/// type), `_` digit separators between digits, and with `base == 0` a
/// `0b`/`0o`/`0x` prefix selects the base (default 10).
///
/// Rust-only addition (the Zig code calls the standard library directly);
/// supports integer types of up to 64 bits.
pub fn std_parse_int<T: ParseIntTarget>(buf: &[u8], base: u8) -> Result<T, ParseIntError> {
    assert!(T::BITS <= 64);
    let (min, max): (i128, i128) = if T::SIGNED {
        (-(1i128 << (T::BITS - 1)), (1i128 << (T::BITS - 1)) - 1)
    } else {
        (0, (1i128 << T::BITS) - 1)
    };

    if buf.is_empty() {
        return Err(ParseIntError::InvalidCharacter);
    }
    let (buf, neg) = match buf[0] {
        b'+' => (&buf[1..], false),
        b'-' => (&buf[1..], true),
        _ => (buf, false),
    };
    if buf.is_empty() {
        return Err(ParseIntError::InvalidCharacter);
    }

    let mut buf_base = base;
    let mut buf_start = buf;
    if base == 0 {
        buf_base = 10;
        if buf.len() > 2 && buf[0] == b'0' {
            match buf[1].to_ascii_lowercase() {
                b'b' => {
                    buf_base = 2;
                    buf_start = &buf[2..];
                }
                b'o' => {
                    buf_base = 8;
                    buf_start = &buf[2..];
                }
                b'x' => {
                    buf_base = 16;
                    buf_start = &buf[2..];
                }
                _ => {}
            }
        }
    }

    if buf_start[0] == b'_' || buf_start[buf_start.len() - 1] == b'_' {
        return Err(ParseIntError::InvalidCharacter);
    }

    // Like Zig, accumulate in an integer of at least 8 bits and report
    // overflow as soon as the accumulator overflows.
    let (acc_min, acc_max): (i128, i128) = if T::SIGNED {
        let bits = T::BITS.max(8);
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << T::BITS.max(8)) - 1)
    };
    let check = |v: i128| -> Result<i128, ParseIntError> {
        if v < acc_min || v > acc_max {
            return Err(ParseIntError::Overflow);
        }
        Ok(v)
    };

    let mut accumulate: i128 = 0;
    for &c in buf_start {
        if c == b'_' {
            continue;
        }
        let digit = i128::from(char_to_digit(c, buf_base)?);
        if accumulate != 0 {
            accumulate = check(accumulate * i128::from(buf_base))?;
        } else if neg {
            // The first digit of a negative number.
            accumulate = check(-digit)?;
            continue;
        }
        accumulate = check(if neg {
            accumulate - digit
        } else {
            accumulate + digit
        })?;
    }

    if accumulate < min || accumulate > max {
        return Err(ParseIntError::Overflow);
    }
    Ok(T::from_i128(accumulate))
}

/// Zig: `std.fmt.charToDigit`.
pub fn char_to_digit(c: u8, base: u8) -> Result<u8, ParseIntError> {
    let value = match c {
        b'0'..=b'9' => c - b'0',
        b'A'..=b'Z' => c - b'A' + 10,
        b'a'..=b'z' => c - b'a' + 10,
        _ => return Err(ParseIntError::InvalidCharacter),
    };
    if value >= base {
        return Err(ParseIntError::InvalidCharacter);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ParseIntError::*;

    #[test]
    fn protocol_integer_parsing() {
        assert_eq!(Ok(42u8), parse_int::<u8>(b"042", 10));
        assert_eq!(Ok(255u8), parse_int::<u8>(b"fF", 16));
        assert_eq!(Ok(42i32), parse_int::<i32>(b"+42", 10));
        assert_eq!(Ok(-2147483648i32), parse_int::<i32>(b"-2147483648", 10));
        assert_eq!(Ok(2147483647i32), parse_int::<i32>(b"2147483647", 10));
        assert_eq!(Ok(-4), parse_int_bits_signed(b"-4", 10, 3));
        assert_eq!(Err(Overflow), parse_int_bits_signed(b"4", 10, 3));
        assert_eq!(Err(Overflow), parse_int::<u8>(b"256", 10));
        assert_eq!(Err(Overflow), parse_int::<i32>(b"2147483648", 10));
        assert_eq!(Err(Overflow), parse_int::<i32>(b"-2147483649", 10));
        for value in [
            &b""[..],
            b"4_2",
            b"4__2",
            b"_42",
            b"42_",
            b" 42",
            b"42 ",
            b"0x2a",
            b"4.2",
            b"4e2",
            b"\xff",
        ] {
            assert_eq!(Err(InvalidCharacter), parse_int::<u8>(value, 10));
            assert_eq!(Err(InvalidCharacter), parse_int::<i32>(value, 10));
        }
        for value in [&b"+42"[..], b"-0", b"-42"] {
            assert_eq!(Err(InvalidCharacter), parse_int::<u8>(value, 10));
        }
        for value in [&b"+"[..], b"-", b"-4_2", b"+4__2"] {
            assert_eq!(Err(InvalidCharacter), parse_int::<i32>(value, 10));
        }
        assert_eq!(Err(InvalidCharacter), parse_int::<u16>(b"f_f", 16));
        assert_eq!(Err(InvalidCharacter), parse_int::<u16>(b"0xff", 16));

        // Overflow is reported as soon as the accumulator overflows, before
        // later invalid characters, like Zig.
        assert_eq!(Err(Overflow), parse_int::<u8>(b"1000x", 10));
    }

    // Port-specific: `std_parse_int` follows Zig's `std.fmt.parseInt`.
    #[test]
    fn std_parse_int_zig_literal_syntax() {
        assert_eq!(Ok(42u8), std_parse_int::<u8>(b"42", 10));
        assert_eq!(Ok(42u8), std_parse_int::<u8>(b"+42", 10));
        assert_eq!(Ok(42u8), std_parse_int::<u8>(b"4_2", 10));
        assert_eq!(Ok(0u8), std_parse_int::<u8>(b"-0", 10));
        assert_eq!(Err(Overflow), std_parse_int::<u8>(b"-1", 10));
        assert_eq!(Err(Overflow), std_parse_int::<u8>(b"256", 10));
        assert_eq!(Ok(-128i8), std_parse_int::<i8>(b"-128", 10));
        assert_eq!(Err(Overflow), std_parse_int::<i8>(b"128", 10));
        assert_eq!(
            Ok(i64::MIN),
            std_parse_int::<i64>(b"-9223372036854775808", 10)
        );
        assert_eq!(
            Ok(u64::MAX),
            std_parse_int::<u64>(b"18446744073709551615", 10)
        );
        assert_eq!(
            Err(Overflow),
            std_parse_int::<u64>(b"18446744073709551616", 10)
        );
        assert_eq!(Ok(255u8), std_parse_int::<u8>(b"0xff", 0));
        assert_eq!(Ok(5u8), std_parse_int::<u8>(b"0b101", 0));
        assert_eq!(Ok(8u8), std_parse_int::<u8>(b"0o10", 0));
        assert_eq!(Ok(10u8), std_parse_int::<u8>(b"010", 0));
        for value in [&b""[..], b"+", b"-", b"_1", b"1_", b"0x_1", b" 1", b"1x"] {
            assert_eq!(Err(InvalidCharacter), std_parse_int::<u8>(value, 0));
        }
    }
}
