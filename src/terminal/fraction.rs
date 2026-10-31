//! Port of `src/terminal/fraction.zig`.
//!
//! STATUS: ported
//!
//! Parsing of decimal fractions in the unit interval [0, 1].
//!
//! Escape sequences that carry fractional parameters (XTerm `rgbi:`
//! color specifications, glyph protocol padding) only ever need plain
//! decimal values such as "0.5". Using a full float parser for these
//! pulls the full correctly-rounded float parser and its lookup tables
//! into the binary (~26KB). This implements only the restricted subset
//! we need.

/// Parse a decimal fraction in the range [0, 1] inclusive.
///
/// Accepts an optional leading `+` or `-` sign followed by decimal
/// digits with an optional decimal point ("0.5", ".5", "1.", "1"); at
/// least one digit is required. Exponent and hexadecimal float syntax
/// are not supported. Returns null for invalid syntax and for any value
/// outside [0, 1] (so negative values other than -0 are rejected).
///
/// Results are correctly rounded for inputs of up to 15 fractional
/// digits; further digits are validated but ignored, which can move the
/// result by less than 1e-15.
pub fn parse(value: &[u8]) -> Option<f64> {
    let mut v = value;
    let mut negative = false;
    if !v.is_empty() {
        match v[0] {
            b'+' => v = &v[1..],
            b'-' => {
                negative = true;
                v = &v[1..];
            }
            _ => {}
        }
    }

    let mut i: usize = 0;
    let int_part: f64 = {
        let mut int_part: f64 = 0.0;
        while i < v.len() && v[i] != b'.' {
            let d = v[i];
            if !(b'0'..=b'9').contains(&d) {
                return None;
            }
            int_part = int_part * 10.0 + (d - b'0') as f64;
            i += 1;
        }
        int_part
    };

    let mut digits: usize = i;
    let mut frac: u64 = 0;
    let mut scale: u64 = 1;
    if i < v.len() {
        debug_assert!(v[i] == b'.');
        i += 1;
        while i < v.len() {
            let d = v[i];
            if !(b'0'..=b'9').contains(&d) {
                return None;
            }
            // Stop accumulating after 15 digits so that frac and scale
            // both stay exactly representable in f64 (and cannot
            // overflow), making the division below round only once.
            if scale < 1_000_000_000_000_000 {
                frac = frac * 10 + (d - b'0') as u64;
                scale *= 10;
            }
            digits += 1;
            i += 1;
        }
    }
    if digits == 0 {
        return None;
    }

    // frac and scale are both exact in f64 (below 2^53 and a power of
    // ten no greater than 1e15), so this division rounds only once.
    let magnitude = int_part + (frac as f64) / (scale as f64);
    let result = if negative { -magnitude } else { magnitude };

    // Range check. Written so that -0 passes and any out-of-range or
    // non-finite value fails.
    if !(result >= 0.0 && result <= 1.0) {
        return None;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse() {
        // Plain values
        assert_eq!(0.0, parse(b"0").unwrap());
        assert_eq!(1.0, parse(b"1").unwrap());
        assert_eq!(0.5, parse(b"0.5").unwrap());
        assert_eq!(0.25, parse(b"0.25").unwrap());
        assert_eq!(1.0, parse(b"1.0").unwrap());
        assert_eq!(1.0, parse(b"1.000000").unwrap());
        assert_eq!(0.0, parse(b"0.0").unwrap());

        // Partial forms
        assert_eq!(0.5, parse(b".5").unwrap());
        assert_eq!(1.0, parse(b"1.").unwrap());
        assert_eq!(0.0, parse(b"00.00").unwrap());

        // Signs
        assert_eq!(0.5, parse(b"+0.5").unwrap());
        assert_eq!(0.0, parse(b"-0").unwrap());
        assert_eq!(0.0, parse(b"-0.000").unwrap());
        assert_eq!(None, parse(b"-0.5"));
        assert_eq!(None, parse(b"-1"));

        // Matches std float parsing rounding for typical inputs.
        assert_eq!("0.3".parse::<f64>().unwrap(), parse(b"0.3").unwrap());
        assert_eq!(
            "0.123456789012345".parse::<f64>().unwrap(),
            parse(b"0.123456789012345").unwrap()
        );

        // Digits past the 15th are ignored: long fractions stay finite and
        // equal their 15-digit truncation.
        let mut overlong_fraction = [b'3'; 400];
        overlong_fraction[..2].copy_from_slice(b"0.");
        assert_eq!(
            parse(b"0.333333333333333").unwrap(),
            parse(&overlong_fraction).unwrap()
        );

        // Out of range
        assert_eq!(None, parse(b"1.0000001"));
        assert_eq!(None, parse(b"2"));
        assert_eq!(None, parse(b"255"));
        let overlong_int = [b'1'; 400];
        assert_eq!(None, parse(&overlong_int));

        // Invalid syntax
        assert_eq!(None, parse(b""));
        assert_eq!(None, parse(b"."));
        assert_eq!(None, parse(b"+"));
        assert_eq!(None, parse(b"-"));
        assert_eq!(None, parse(b"+."));
        assert_eq!(None, parse(b"abc"));
        assert_eq!(None, parse(b"0.5x"));
        assert_eq!(None, parse(b"0x0.8"));
        assert_eq!(None, parse(b"0..5"));
        assert_eq!(None, parse(b"0.5.5"));
        assert_eq!(None, parse(b" 0.5"));
        assert_eq!(None, parse(b"0.5 "));
        assert_eq!(None, parse(b"1e-1"));
        assert_eq!(None, parse(b"nan"));
        assert_eq!(None, parse(b"inf"));
    }
}
