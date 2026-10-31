//! Port of `src/font/nerd_font_attributes.zig`.
//!
//! STATUS: ported
//!
//! Nerd Font glyph constraints.
//!
//! `table` is a 3-level codepoint lookup, same shape as
//! `unicode/symbols_table.zig`. Pages with no constrained glyphs share
//! one stage2 block and return null after the stage1 load.

use super::glyph::{Constraint, ConstraintAlign, ConstraintHeight, ConstraintSize};
use super::nerd_font_tables as generated;
use crate::unicode::lut::Tables;

static STAGE3: [Option<Constraint>; generated::STAGE3_LEN] =
    generated::stage3!(Constraint, ConstraintSize, ConstraintAlign, ConstraintHeight);

/// 3-level codepoint lookup. Same shape as `unicode/symbols_table.zig`.
pub static TABLE: Tables<'static, Option<Constraint>> = Tables {
    stage1: &generated::STAGE1,
    stage2: &generated::STAGE2,
    stage3: &STAGE3,
};

// lut.zig pages are 256 codepoints. The generator stores the all-null
// page at stage2 offset 0, and stage3 index 0 is null.
const EMPTY_BLOCK: u16 = 0;

const _: () = {
    // One stage1 entry for every high byte of a u21.
    assert!(generated::STAGE1.len() == 0x2000, "stage1 must cover every u21 page");
    assert!(generated::STAGE3_LEN != 0);
    let mut i = 0;
    while i < 256 {
        assert!(
            generated::STAGE2[i] == 0,
            "empty nerd-font page must be stage2 offset 0"
        );
        i += 1;
    }
};

/// Nerd Font constraint for `cp`, or null when the codepoint has none.
/// Pages with no constrained glyphs return after the stage1 load.
///
/// `cp` is a `u21` in Zig; values above `0x1FFFFF` are not valid.
#[inline]
pub fn get_constraint(cp: u32) -> Option<Constraint> {
    debug_assert!(cp <= 0x1F_FFFF);
    if TABLE.stage1[(cp >> 8) as usize] == EMPTY_BLOCK {
        return None;
    }
    TABLE.get(cp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage3_first_is_null() {
        // stage3[0] must be null.
        assert!(STAGE3[0].is_none());
    }

    #[test]
    fn constraints_lookup_table() {
        assert!(get_constraint('A' as u32).is_none());
        assert!(get_constraint(0x30A2).is_none()); // katakana
        assert!(get_constraint(0xFF66).is_none()); // halfwidth katakana
        assert!(get_constraint(0x2630).is_some());
        assert!(get_constraint(0x110000).is_none());
        assert!(get_constraint(0x1F_FFFF).is_none());

        let mut cp: u32 = 0;
        while cp <= 0x10FFFF {
            let scalar = cp;
            if TABLE.stage1[(scalar >> 8) as usize] == EMPTY_BLOCK {
                assert!(get_constraint(scalar).is_none());
                cp = (cp & !0xFFu32) + 256;
                continue;
            }
            let full = TABLE.get(scalar);
            let fast = get_constraint(scalar);
            if let Some(value) = full {
                assert_eq!(value, fast.unwrap());
            } else {
                assert!(fast.is_none());
            }
            cp += 1;
        }
    }
}
