//! Port of `src/terminal/c/sgr.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: the core `sgr.Attribute` C representation
//! (`Attribute.C`/`Attribute.CValue`, built by `lib.TaggedUnion`) is empty
//! in the Rust core because it is the application (`.zig` target) build.
//! The C API therefore declares the C ABI types here: [`AttributeTag`],
//! [`Underline`], [`AttributeCValue`] (only the non-void arms plus the
//! `[8]u64` padding, which is what the C header declares) and
//! [`AttributeC`], with [`attribute_cval`] converting a core attribute.

use std::ffi::c_int;

use super::result::Result;
use super::types::{Field, HandleAllocator};
use crate::lib::allocator::Allocator as CAllocator;
use crate::terminal::color::RgbC;
use crate::terminal::parser::SepList;
use crate::terminal::sgr;

const LOG_TARGET: &str = "sgr";

crate::c_api_enum! {
    /// C: GhosttySgrAttributeTag (Zig: `sgr.Attribute.Tag`).
    pub enum AttributeTag {
        Unset = 0,
        Unknown = 1,
        Bold = 2,
        ResetBold = 3,
        Italic = 4,
        ResetItalic = 5,
        Faint = 6,
        Underline = 7,
        UnderlineColor = 8,
        UnderlineColor256 = 9 => "256_underline_color",
        ResetUnderlineColor = 10,
        Overline = 11,
        ResetOverline = 12,
        Blink = 13,
        ResetBlink = 14,
        Inverse = 15,
        ResetInverse = 16,
        Invisible = 17,
        ResetInvisible = 18,
        Strikethrough = 19,
        ResetStrikethrough = 20,
        DirectColorFg = 21,
        DirectColorBg = 22,
        Bg8 = 23 => "8_bg",
        Fg8 = 24 => "8_fg",
        ResetFg = 25,
        ResetBg = 26,
        BrightBg8 = 27 => "8_bright_bg",
        BrightFg8 = 28 => "8_bright_fg",
        Bg256 = 29 => "256_bg",
        Fg256 = 30 => "256_fg",
    }
}

crate::c_api_enum! {
    /// C: GhosttySgrUnderline (Zig: `sgr.Attribute.Underline`).
    pub enum Underline {
        None = 0,
        Single = 1,
        Double = 2,
        Curly = 3,
        Dotted = 4,
        Dashed = 5,
    }
}

/// C: GhosttySgrAttributeValue (Zig: `sgr.Attribute.CValue`).
#[repr(C)]
#[derive(Clone, Copy)]
pub union AttributeCValue {
    pub unknown: sgr::UnknownC,
    pub underline: c_int,
    pub underline_color: RgbC,
    pub underline_color_256: u8,
    pub direct_color_fg: RgbC,
    pub direct_color_bg: RgbC,
    pub bg_8: u8,
    pub fg_8: u8,
    pub bright_bg_8: u8,
    pub bright_fg_8: u8,
    pub bg_256: u8,
    pub fg_256: u8,
    pub _padding: [u64; 8],
}

// For ABI compatibility, we expect that this is our union size.
const _: () = assert!(std::mem::size_of::<AttributeCValue>() == std::mem::size_of::<[u64; 8]>());

impl AttributeCValue {
    /// Manifest fields (Zig: `writeMappedUnionFields`).
    pub fn fields() -> Vec<Field> {
        use super::types::named;
        use crate::c_api_field as field;
        vec![
            field!(union AttributeCValue, unknown, named("GhosttySgrUnknown")),
            field!(union AttributeCValue, underline, named("GhosttySgrUnderline")),
            field!(union AttributeCValue, underline_color, named("GhosttyColorRgb")),
            field!(union AttributeCValue, underline_color_256, named("GhosttyColorPaletteIndex")),
            field!(union AttributeCValue, direct_color_fg, named("GhosttyColorRgb")),
            field!(union AttributeCValue, direct_color_bg, named("GhosttyColorRgb")),
            field!(union AttributeCValue, bg_8, named("GhosttyColorPaletteIndex")),
            field!(union AttributeCValue, fg_8, named("GhosttyColorPaletteIndex")),
            field!(union AttributeCValue, bright_bg_8, named("GhosttyColorPaletteIndex")),
            field!(union AttributeCValue, bright_fg_8, named("GhosttyColorPaletteIndex")),
            field!(union AttributeCValue, bg_256, named("GhosttyColorPaletteIndex")),
            field!(union AttributeCValue, fg_256, named("GhosttyColorPaletteIndex")),
            field!(union AttributeCValue, _padding, super::types::array("u64", 8)),
        ]
    }
}

/// C: GhosttySgrAttribute (Zig: `sgr.Attribute.C`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AttributeC {
    pub tag: AttributeTag,
    pub value: AttributeCValue,
}

impl AttributeC {
    /// Returns the public C value-union field name for `tag`, or `None`
    /// when the tag has no value field (Zig: `cFieldRename`).
    pub fn c_field_rename(tag: AttributeTag) -> Option<&'static str> {
        use AttributeTag as T;
        Some(match tag {
            T::Unknown => "unknown",
            T::Underline => "underline",
            T::UnderlineColor => "underline_color",
            T::UnderlineColor256 => "underline_color_256",
            T::DirectColorFg => "direct_color_fg",
            T::DirectColorBg => "direct_color_bg",
            T::Bg8 => "bg_8",
            T::Fg8 => "fg_8",
            T::BrightBg8 => "bright_bg_8",
            T::BrightFg8 => "bright_fg_8",
            T::Bg256 => "bg_256",
            T::Fg256 => "fg_256",
            _ => return None,
        })
    }
}

/// Convert a core attribute to its C representation (Zig:
/// `Attribute.cval`).
pub fn attribute_cval(attr: &sgr::Attribute<'_>) -> AttributeC {
    use sgr::Attribute as A;
    // SAFETY: the C value union is plain old data; an all-zero bit pattern
    // is valid for every field and keeps padding bytes deterministic.
    let mut value: AttributeCValue = unsafe { std::mem::zeroed() };
    match attr {
        A::Unknown(u) => value.unknown = u.cval(),
        A::Underline(u) => value.underline = u.cval(),
        A::UnderlineColor(rgb) => value.underline_color = rgb.cval(),
        A::UnderlineColor256(idx) => value.underline_color_256 = *idx,
        A::DirectColorFg(rgb) => value.direct_color_fg = rgb.cval(),
        A::DirectColorBg(rgb) => value.direct_color_bg = rgb.cval(),
        A::Bg8(name) => value.bg_8 = name.cval(),
        A::Fg8(name) => value.fg_8 = name.cval(),
        A::BrightBg8(name) => value.bright_bg_8 = name.cval(),
        A::BrightFg8(name) => value.bright_fg_8 = name.cval(),
        A::Bg256(idx) => value.bg_256 = *idx,
        A::Fg256(idx) => value.fg_256 = *idx,
        _ => {}
    }
    AttributeC {
        tag: AttributeTag::from_int(attr.tag() as c_int).expect("core and C SGR tags line up"),
        value,
    }
}

/// Wrapper around parser that tracks the allocator for C API usage. The
/// core parser borrows its parameters, so the wrapper owns them and
/// rebuilds the parser view on each `next`.
pub struct ParserWrapper {
    params: Vec<u16>,
    params_sep: SepList,
    idx: usize,
    alloc: HandleAllocator,
}

/// C: GhosttySgrParser
pub type Parser = *mut ParserWrapper;

/// C: ghostty_sgr_new
#[export_name = "ghostty_sgr_new"]
pub extern "C" fn new(alloc_: Option<&CAllocator>, result: &mut Parser) -> Result {
    let alloc = HandleAllocator::new(alloc_);
    let ptr = alloc.create(ParserWrapper {
        params: Vec::new(),
        params_sep: SepList::EMPTY,
        idx: 0,
        alloc,
    });
    if ptr.is_null() {
        return Result::OutOfMemory;
    }
    *result = ptr;
    Result::Success
}

/// C: ghostty_sgr_free
///
/// # Safety
/// `parser_` must be NULL or a parser from [`new`] not used afterwards.
#[export_name = "ghostty_sgr_free"]
pub unsafe extern "C" fn free(parser_: Parser) {
    let Some(wrapper) = parser_.as_ref() else {
        return;
    };
    let alloc = wrapper.alloc;
    alloc.destroy(parser_);
}

/// C: ghostty_sgr_reset
///
/// # Safety
/// `parser_` must be NULL or a valid parser.
#[export_name = "ghostty_sgr_reset"]
pub unsafe extern "C" fn reset(parser_: Parser) {
    let Some(wrapper) = parser_.as_mut() else {
        return;
    };
    wrapper.idx = 0;
}

/// C: ghostty_sgr_set_params
///
/// # Safety
/// `params` must be valid for `len` reads, and a non-null `seps_` too.
#[export_name = "ghostty_sgr_set_params"]
pub unsafe extern "C" fn set_params(parser_: Parser, params: *const u16, seps_: *const u8, len: usize) -> Result {
    let Some(wrapper) = parser_.as_mut() else {
        return Result::InvalidValue;
    };

    // Copy our new parameters
    let mut params_vec = Vec::new();
    if params_vec.try_reserve_exact(len).is_err() {
        return Result::OutOfMemory;
    }
    params_vec.extend_from_slice(super::types::slice_or_empty(params, len));
    wrapper.params = params_vec;

    // If we have separators, set that state too.
    wrapper.params_sep = SepList::EMPTY;
    if !seps_.is_null() {
        if len > SepList::BIT_LENGTH {
            log::warn!(
                target: LOG_TARGET,
                "ghostty_sgr_set_params: separators length {} exceeds max supported length {}",
                len,
                SepList::BIT_LENGTH,
            );
            return Result::InvalidValue;
        }

        for (i, &sep) in std::slice::from_raw_parts(seps_, len).iter().enumerate() {
            if sep == b':' {
                wrapper.params_sep.set(i);
            }
        }
    }

    // Reset our parsing state
    wrapper.idx = 0;

    Result::Success
}

/// C: ghostty_sgr_next
///
/// # Safety
/// `parser_` must be NULL or a valid parser. Unknown attributes point into
/// the parser's parameters and are valid until the next `set_params`.
#[export_name = "ghostty_sgr_next"]
pub unsafe extern "C" fn next(parser_: Parser, result: &mut AttributeC) -> bool {
    let Some(wrapper) = parser_.as_mut() else {
        return false;
    };
    let mut parser = sgr::Parser {
        params: &wrapper.params,
        params_sep: wrapper.params_sep,
        idx: wrapper.idx,
    };
    let attr = parser.next();
    wrapper.idx = parser.idx;
    if let Some(attr) = attr {
        *result = attribute_cval(&attr);
        return true;
    }

    false
}

/// C: ghostty_sgr_unknown_full
#[export_name = "ghostty_sgr_unknown_full"]
pub extern "C" fn unknown_full(unknown: sgr::UnknownC, ptr: Option<&mut *const u16>) -> usize {
    if let Some(p) = ptr {
        *p = unknown.full_ptr;
    }
    unknown.full_len
}

/// C: ghostty_sgr_unknown_partial
#[export_name = "ghostty_sgr_unknown_partial"]
pub extern "C" fn unknown_partial(unknown: sgr::UnknownC, ptr: Option<&mut *const u16>) -> usize {
    if let Some(p) = ptr {
        *p = unknown.partial_ptr;
    }
    unknown.partial_len
}

/// C: ghostty_sgr_attribute_tag
#[export_name = "ghostty_sgr_attribute_tag"]
pub extern "C" fn attribute_tag(attr: AttributeC) -> AttributeTag {
    attr.tag
}

/// C: ghostty_sgr_attribute_value
#[export_name = "ghostty_sgr_attribute_value"]
pub extern "C" fn attribute_value(attr: &mut AttributeC) -> &mut AttributeCValue {
    &mut attr.value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;

    #[test]
    fn alloc() {
        let mut p: Parser = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut p));
        unsafe { free(p) };
    }

    #[test]
    fn simple_params_no_seps() {
        let mut p: Parser = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut p));

        unsafe {
            let params = [1u16];
            assert_eq!(Result::Success, set_params(p, params.as_ptr(), std::ptr::null(), 1));

            // Set it twice on purpose to make sure we don't leak.
            assert_eq!(Result::Success, set_params(p, params.as_ptr(), std::ptr::null(), 1));

            // Verify we get bold
            let mut attr: AttributeC = std::mem::zeroed();
            assert!(next(p, &mut attr));
            assert_eq!(AttributeTag::Bold, attr.tag);

            // Nothing else
            assert!(!next(p, &mut attr));

            free(p);
        }
    }

    /// Rust port: the C tags and the core tags must line up, and the
    /// unknown accessors return the captured parameters.
    #[test]
    fn unknown_and_values() {
        let mut p: Parser = std::ptr::null_mut();
        assert_eq!(Result::Success, new(None, &mut p));
        unsafe {
            let params = [38u16, 2, 1, 2, 3, 4, 999];
            let seps = *b":::::;;";
            assert_eq!(Result::Success, set_params(p, params.as_ptr(), seps.as_ptr(), params.len()));
            let mut attr: AttributeC = std::mem::zeroed();
            assert!(next(p, &mut attr));
            assert_eq!(AttributeTag::DirectColorFg, attribute_tag(attr));
            let v = attribute_value(&mut attr);
            assert_eq!(2, v.direct_color_fg.r);
            assert_eq!(3, v.direct_color_fg.g);
            assert_eq!(4, v.direct_color_fg.b);

            assert!(next(p, &mut attr));
            assert_eq!(AttributeTag::Unknown, attr.tag);
            let mut full: *const u16 = std::ptr::null();
            let len = unknown_full(attr.value.unknown, Some(&mut full));
            assert_eq!(params.len(), len);
            let mut partial: *const u16 = std::ptr::null();
            let plen = unknown_partial(attr.value.unknown, Some(&mut partial));
            assert_eq!(&[999], std::slice::from_raw_parts(partial, plen));
            free(p);
        }

        for (_, tag) in <AttributeTag as crate::lib::Enum>::FIELDS {
            let _ = AttributeC::c_field_rename(*tag);
        }
    }
}
