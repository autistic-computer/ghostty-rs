//! Port of `pkg/freetype/computations.zig`.
//!
//! STATUS: ported

use super::c;

/// Compute (a*b)/0x10000 with maximum accuracy. Its main use is to multiply
/// a given value by a 16.16 fixed-point factor.
pub fn mul_fix(a: i32, b: i32) -> i32 {
    // SAFETY: FT_MulFix is a pure arithmetic function.
    unsafe { c::FT_MulFix(a as c::FT_Long, b as c::FT_Long) as i32 }
}
