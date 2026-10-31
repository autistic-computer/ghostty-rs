//! Port of `src/simd/codepoint_width.zig`.
//!
//! STATUS: ported
//!
//! Deviation: the Zig SIMD build calls `codepoint_width.cpp` (Highway range
//! tables). This port uses the Zig scalar path (the uucode `width` field),
//! which differs from the C++ tables: those return 1 for every codepoint
//! `<= 0xFF` (controls included) and predate Unicode 18.

use crate::unicode::uucode;

pub fn codepoint_width(cp: u32) -> i8 {
    if cp > uucode::config::MAX_CODE_POINT {
        return 1;
    }
    uucode::width(cp) as i8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codepoint_width_basic() {
        assert_eq!(1i8, codepoint_width('a' as u32));
        assert_eq!(1i8, codepoint_width(0x100)); // Ā
        assert_eq!(2i8, codepoint_width(0x3400)); // 㐀
        assert_eq!(2i8, codepoint_width(0x2E3A)); // ⸺
        assert_eq!(2i8, codepoint_width(0x1F1E6)); // 🇦
        assert_eq!(2i8, codepoint_width(0x4E00)); // 一
        assert_eq!(2i8, codepoint_width(0xF900)); // 豈
        assert_eq!(2i8, codepoint_width(0x20000)); // 𠀀
        assert_eq!(2i8, codepoint_width(0x30000)); // 𠀀
    }

    // The Zig "codepointWidth matches uucode" test is commented out upstream
    // (it compares the C++ SIMD tables to uucode). Since this port uses the
    // uucode width directly, the equivalent check is that it agrees with the
    // unicode props table (which is also derived from uucode) everywhere.
    #[test]
    fn codepoint_width_matches_uucode() {
        for cp in (0xFFu32 + 1)..=0x1FFFFF {
            let expected = if cp > uucode::config::MAX_CODE_POINT {
                1
            } else {
                crate::unicode::TABLE.get(cp).width as i8
            };
            assert_eq!(expected, codepoint_width(cp), "mismatch cp=U+{cp:x}");
        }
    }
}
