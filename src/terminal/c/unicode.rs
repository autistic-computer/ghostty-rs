//! Port of `src/terminal/c/unicode.zig`.
//!
//! STATUS: ported

use crate::unicode as unicode_pkg;

/// C: ghostty_unicode_codepoint_width
#[export_name = "ghostty_unicode_codepoint_width"]
pub extern "C" fn codepoint_width(cp: u32) -> u8 {
    if cp > 0x10FFFF {
        return 1;
    }
    unicode_pkg::codepoint_width(cp)
}

/// C: ghostty_unicode_grapheme_width
///
/// # Safety
/// A non-null `cps` must be valid for `len` reads.
#[export_name = "ghostty_unicode_grapheme_width"]
pub unsafe extern "C" fn grapheme_width(cps: *const u32, len: usize, width: Option<&mut u8>) -> usize {
    if len == 0 {
        if let Some(ptr) = width {
            *ptr = 0;
        }
        return 0;
    }

    if cps.is_null() {
        if let Some(out) = width {
            *out = 0;
        }
        return 0;
    }
    let result = unicode_pkg::grapheme::grapheme_width(std::slice::from_raw_parts(cps, len));
    if let Some(out) = width {
        *out = result.width;
    }
    result.len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codepoint_width_narrow() {
        assert_eq!(1, codepoint_width('a' as u32));
    }

    #[test]
    fn codepoint_width_wide() {
        assert_eq!(2, codepoint_width(0x4E00));
    }

    #[test]
    fn codepoint_width_zero() {
        assert_eq!(0, codepoint_width(0x0301));
    }

    #[test]
    fn codepoint_width_out_of_range() {
        assert_eq!(1, codepoint_width(0x110000));
        assert_eq!(1, codepoint_width(u32::MAX));
    }

    #[test]
    fn grapheme_width_empty() {
        let mut width: u8 = 42;
        assert_eq!(0, unsafe { grapheme_width(std::ptr::null(), 0, Some(&mut width)) });
        assert_eq!(0, width);
    }

    #[test]
    fn grapheme_width_null_width() {
        let cps = [0x2764u32, 0xFE0F];
        assert_eq!(2, unsafe { grapheme_width(cps.as_ptr(), cps.len(), None) });
    }

    #[test]
    fn grapheme_width_out_of_range() {
        let mut width: u8 = 0;
        let invalid_first = [0x110000u32, 0x0301];
        assert_eq!(1, unsafe {
            grapheme_width(invalid_first.as_ptr(), invalid_first.len(), Some(&mut width))
        });
        assert_eq!(1, width);

        let invalid_second = ['a' as u32, 0x110000];
        assert_eq!(1, unsafe {
            grapheme_width(invalid_second.as_ptr(), invalid_second.len(), Some(&mut width))
        });
        assert_eq!(1, width);
    }

    #[test]
    fn grapheme_width_emoji_sequence() {
        let mut width: u8 = 0;
        let cps = [0x1F468u32, 0x200D, 0x1F469, 0x200D, 0x1F467];
        assert_eq!(cps.len(), unsafe { grapheme_width(cps.as_ptr(), cps.len(), Some(&mut width)) });
        assert_eq!(2, width);
    }
}
