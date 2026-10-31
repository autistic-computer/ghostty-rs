//! Port of `pkg/oniguruma/types.zig`.
//!
//! STATUS: ported

use std::marker::{PhantomData, PhantomPinned};
use std::os::raw::c_uint;

use super::c;

/// An Oniguruma encoding (`OnigEncodingType`), only used by reference.
///
/// Zig exposes the built-in encodings as `pub const utf8: *Encoding = ...`
/// constants; Rust cannot take the address of an extern static in a
/// constant, so they are associated functions (`Encoding::utf8()`).
#[repr(C)]
pub struct Encoding {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

macro_rules! encodings {
    ($($name:ident = $c:ident,)*) => {
        impl Encoding {
            $(
                pub fn $name() -> &'static Encoding {
                    // SAFETY: the encoding tables are static data in
                    // libonig; only their address is taken here.
                    unsafe { &*(&raw const c::$c as *const Encoding) }
                }
            )*
        }
    };
}

encodings! {
    ascii = OnigEncodingASCII,
    iso_8859_1 = OnigEncodingISO_8859_1,
    iso_8859_2 = OnigEncodingISO_8859_2,
    iso_8859_3 = OnigEncodingISO_8859_3,
    iso_8859_4 = OnigEncodingISO_8859_4,
    iso_8859_5 = OnigEncodingISO_8859_5,
    iso_8859_6 = OnigEncodingISO_8859_6,
    iso_8859_7 = OnigEncodingISO_8859_7,
    iso_8859_8 = OnigEncodingISO_8859_8,
    iso_8859_9 = OnigEncodingISO_8859_9,
    iso_8859_10 = OnigEncodingISO_8859_10,
    iso_8859_11 = OnigEncodingISO_8859_11,
    iso_8859_13 = OnigEncodingISO_8859_13,
    iso_8859_14 = OnigEncodingISO_8859_14,
    iso_8859_15 = OnigEncodingISO_8859_15,
    iso_8859_16 = OnigEncodingISO_8859_16,
    utf8 = OnigEncodingUTF8,
    utf16_be = OnigEncodingUTF16_BE,
    utf16_le = OnigEncodingUTF16_LE,
    utf32_be = OnigEncodingUTF32_BE,
    utf32_le = OnigEncodingUTF32_LE,
    euc_jp = OnigEncodingEUC_JP,
    euc_tw = OnigEncodingEUC_TW,
    euc_kr = OnigEncodingEUC_KR,
    euc_cn = OnigEncodingEUC_CN,
    sjis = OnigEncodingSJIS,
    koi8 = OnigEncodingKOI8,
    koi8_r = OnigEncodingKOI8_R,
    cp1251 = OnigEncodingCP1251,
    big5 = OnigEncodingBIG5,
    gb18030 = OnigEncodingGB18030,
}

impl Encoding {
    #[inline]
    pub fn cval(&self) -> c::OnigEncoding {
        self as *const Encoding as c::OnigEncoding
    }
}

/// An Oniguruma syntax (`OnigSyntaxType`), only used by reference. See
/// [`Encoding`] for why the Zig constants are functions.
#[repr(C)]
pub struct Syntax {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

macro_rules! syntaxes {
    ($($name:ident = $c:ident,)*) => {
        impl Syntax {
            $(
                pub fn $name() -> &'static Syntax {
                    // SAFETY: the syntax tables are static data in libonig;
                    // only their address is taken here.
                    unsafe { &*(&raw const c::$c as *const Syntax) }
                }
            )*
        }
    };
}

syntaxes! {
    default = OnigSyntaxOniguruma,
    asis = OnigSyntaxASIS,
    posix_basic = OnigSyntaxPosixBasic,
    posix_extended = OnigSyntaxPosixExtended,
    emacs = OnigSyntaxEmacs,
    grep = OnigSyntaxGrep,
    gnu_regex = OnigSyntaxGnuRegex,
    java = OnigSyntaxJava,
    perl = OnigSyntaxPerl,
    perl_ng = OnigSyntaxPerl_NG,
    ruby = OnigSyntaxRuby,
    oniguruma = OnigSyntaxOniguruma,
}

impl Syntax {
    #[inline]
    pub fn cval(&self) -> *mut c::OnigSyntaxType {
        self as *const Syntax as *mut c::OnigSyntaxType
    }
}

/// Zig: `packed struct(c_uint)` of `ONIG_OPTION_*` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Option {
    pub ignorecase: bool,
    pub extend: bool,
    pub multiline: bool,
    pub singleline: bool,
    pub find_longest: bool,
    pub find_not_empty: bool,
    pub negate_singleline: bool,
    pub dont_capture_group: bool,
    pub capture_group: bool,
    // search time
    pub notbol: bool,
    pub noteol: bool,
    pub posix_region: bool,
    pub check_validity_of_string: bool,
    // compile time
    pub ignorecase_is_ascii: bool,
    pub word_is_ascii: bool,
    pub digit_is_ascii: bool,
    pub space_is_ascii: bool,
    pub posix_is_ascii: bool,
    pub text_segment_extended_grapheme_cluster: bool,
    pub text_segment_word: bool,
    // search time
    pub not_begin_string: bool,
    pub not_end_string: bool,
    pub not_begin_position: bool,
    pub callback_each_match: bool,
    pub match_whole_string: bool,
}

impl Option {
    /// The `ONIG_OPTION_*` bit mask (Zig: `@bitCast(self)`).
    ///
    /// Deviation: the Zig packed struct places the fields sequentially,
    /// but in `oniguruma.h` `ONIG_OPTION_IGNORECASE_IS_ASCII` (and every
    /// flag after it) is shifted by two further bits. Each field is mapped
    /// to its C constant here so the flags reach Oniguruma correctly.
    pub fn int(self) -> c_uint {
        let flags: [(bool, i32); 25] = [
            (self.ignorecase, c::ONIG_OPTION_IGNORECASE),
            (self.extend, c::ONIG_OPTION_EXTEND),
            (self.multiline, c::ONIG_OPTION_MULTILINE),
            (self.singleline, c::ONIG_OPTION_SINGLELINE),
            (self.find_longest, c::ONIG_OPTION_FIND_LONGEST),
            (self.find_not_empty, c::ONIG_OPTION_FIND_NOT_EMPTY),
            (self.negate_singleline, c::ONIG_OPTION_NEGATE_SINGLELINE),
            (self.dont_capture_group, c::ONIG_OPTION_DONT_CAPTURE_GROUP),
            (self.capture_group, c::ONIG_OPTION_CAPTURE_GROUP),
            (self.notbol, c::ONIG_OPTION_NOTBOL),
            (self.noteol, c::ONIG_OPTION_NOTEOL),
            (self.posix_region, c::ONIG_OPTION_POSIX_REGION),
            (
                self.check_validity_of_string,
                c::ONIG_OPTION_CHECK_VALIDITY_OF_STRING,
            ),
            (self.ignorecase_is_ascii, c::ONIG_OPTION_IGNORECASE_IS_ASCII),
            (self.word_is_ascii, c::ONIG_OPTION_WORD_IS_ASCII),
            (self.digit_is_ascii, c::ONIG_OPTION_DIGIT_IS_ASCII),
            (self.space_is_ascii, c::ONIG_OPTION_SPACE_IS_ASCII),
            (self.posix_is_ascii, c::ONIG_OPTION_POSIX_IS_ASCII),
            (
                self.text_segment_extended_grapheme_cluster,
                c::ONIG_OPTION_TEXT_SEGMENT_EXTENDED_GRAPHEME_CLUSTER,
            ),
            (self.text_segment_word, c::ONIG_OPTION_TEXT_SEGMENT_WORD),
            (self.not_begin_string, c::ONIG_OPTION_NOT_BEGIN_STRING),
            (self.not_end_string, c::ONIG_OPTION_NOT_END_STRING),
            (self.not_begin_position, c::ONIG_OPTION_NOT_BEGIN_POSITION),
            (self.callback_each_match, c::ONIG_OPTION_CALLBACK_EACH_MATCH),
            (self.match_whole_string, c::ONIG_OPTION_MATCH_WHOLE_STRING),
        ];
        let mut v: c_uint = 0;
        for (set, bit) in flags {
            if set {
                v |= bit as c_uint;
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order() {
        let opt = Option {
            extend: true,
            ..Default::default()
        };
        assert_eq!(c::ONIG_OPTION_EXTEND as c_uint, opt.int());
    }
}
