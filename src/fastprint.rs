//! Port of `src/fastprint.zig`.
//!
//! STATUS: ported
//!
//! Fastprint has fast printing routines that are significantly
//! faster than going through std.fmt.

/// Two-digit lookup, equivalent to Zig's `std.fmt.digits2`.
#[inline(always)]
fn digits2(v: u8) -> [u8; 2] {
    debug_assert!(v < 100);
    [b'0' + v / 10, b'0' + v % 10]
}

/// Integer types supported by [`print_decimal`]. The Zig version switches
/// on the comptime type; this trait plays the same role.
pub trait PrintDecimal: Copy {
    fn print_decimal(self, buf: &mut [u8]) -> usize;
}

impl PrintDecimal for u8 {
    #[inline]
    fn print_decimal(self, buf: &mut [u8]) -> usize {
        let v = self;
        if v >= 100 {
            buf[0] = b'0' + v / 100;
            buf[1..3].copy_from_slice(&digits2(v % 100));
            return 3;
        }
        if v >= 10 {
            buf[0..2].copy_from_slice(&digits2(v));
            return 2;
        }
        buf[0] = b'0' + v;
        1
    }
}

/// Zig's `u21` (a Unicode scalar value width). Values must be
/// `<= 0x1F_FFFF`.
impl PrintDecimal for u32 {
    #[inline]
    fn print_decimal(self, buf: &mut [u8]) -> usize {
        debug_assert!(self <= 0x1F_FFFF);
        // Maximum u21 is 2097151: at most 7 digits.
        let mut tmp = [0u8; 7];
        let mut val: u32 = self;
        let mut i: usize = tmp.len();
        loop {
            i -= 1;
            tmp[i] = b'0' + (val % 10) as u8;
            val /= 10;
            if val == 0 {
                break;
            }
        }
        let n = tmp.len() - i;
        buf[..n].copy_from_slice(&tmp[i..]);
        n
    }
}

/// Print a decimal type T. The buffer is expected to be large enough so if
/// necessary use comptime with a buffer-too-large to determine your
/// max size needed. Returns the length written.
///
/// Supported types: `u8` and `u32` (standing in for Zig's `u21`).
#[inline]
pub fn print_decimal<T: PrintDecimal>(buf: &mut [u8], v: T) -> usize {
    v.print_decimal(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_print_decimal() {
        let mut buf = [0u8; 16];

        // u8: exercise 1, 2, and 3 digit values including boundaries.
        for v in [0u8, 1, 9, 10, 99, 100, 255] {
            let expected = v.to_string();
            let len = print_decimal(&mut buf, v);
            assert_eq!(expected.as_bytes(), &buf[..len]);
        }

        // u21: exercise digit count boundaries up to the maximum.
        for v in [0u32, 9, 10, 128, 65535, 1114111, 0x1F_FFFF] {
            let expected = v.to_string();
            let len = print_decimal(&mut buf, v);
            assert_eq!(expected.as_bytes(), &buf[..len]);
        }
    }
}
