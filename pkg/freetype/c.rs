//! Port of `pkg/freetype/c.zig`.
//!
//! STATUS: ported
//!
//! The Zig package `@cImport`s `freetype-zig.h` (ft2build.h, FT_FREETYPE_H,
//! FT_TRUETYPE_TABLES_H, ftmm.h, ftoutln.h, ftsnames.h, ttnameid.h,
//! ftbitmap.h, ftbbox.h). The Rust equivalent is `c_bindings.rs`, generated
//! by rust-bindgen 0.73 from the same header against the system freetype2
//! (allowlist `FT_.*|TT_.*|FREETYPE_.*|ft_.*|TTAG_.*`, enum constants without
//! the enum name prefix, macro constants typed `i32`), linked via pkg-config
//! in `build.rs`.
//!
//! translate-c also turns a few function-like C macros into functions; those
//! that ghostty uses are written by hand below.

#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    clippy::all
)]

include!("c_bindings.rs");

/// `FT_LOAD_TARGET_(x)`
#[inline]
pub const fn FT_LOAD_TARGET_(x: i32) -> i32 {
    (x & 15) << 16
}

pub const FT_LOAD_TARGET_NORMAL: i32 = FT_LOAD_TARGET_(FT_RENDER_MODE_NORMAL as i32);
pub const FT_LOAD_TARGET_LIGHT: i32 = FT_LOAD_TARGET_(FT_RENDER_MODE_LIGHT as i32);
pub const FT_LOAD_TARGET_MONO: i32 = FT_LOAD_TARGET_(FT_RENDER_MODE_MONO as i32);
pub const FT_LOAD_TARGET_LCD: i32 = FT_LOAD_TARGET_(FT_RENDER_MODE_LCD as i32);
pub const FT_LOAD_TARGET_LCD_V: i32 = FT_LOAD_TARGET_(FT_RENDER_MODE_LCD_V as i32);

/// `FT_LOAD_TARGET_MODE(x)`
#[inline]
pub const fn FT_LOAD_TARGET_MODE(x: i32) -> FT_Render_Mode {
    ((x >> 16) & 15) as FT_Render_Mode
}

#[inline]
unsafe fn face_flags(face: FT_Face) -> FT_Long {
    (*face).face_flags
}

/// `FT_HAS_HORIZONTAL(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_HAS_HORIZONTAL(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_HORIZONTAL as FT_Long != 0
}

/// `FT_HAS_VERTICAL(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_HAS_VERTICAL(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_VERTICAL as FT_Long != 0
}

/// `FT_HAS_KERNING(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_HAS_KERNING(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_KERNING as FT_Long != 0
}

/// `FT_IS_SCALABLE(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_IS_SCALABLE(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_SCALABLE as FT_Long != 0
}

/// `FT_IS_SFNT(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_IS_SFNT(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_SFNT as FT_Long != 0
}

/// `FT_IS_FIXED_WIDTH(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_IS_FIXED_WIDTH(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_FIXED_WIDTH as FT_Long != 0
}

/// `FT_HAS_FIXED_SIZES(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_HAS_FIXED_SIZES(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_FIXED_SIZES as FT_Long != 0
}

/// `FT_HAS_GLYPH_NAMES(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_HAS_GLYPH_NAMES(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_GLYPH_NAMES as FT_Long != 0
}

/// `FT_HAS_MULTIPLE_MASTERS(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_HAS_MULTIPLE_MASTERS(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_MULTIPLE_MASTERS as FT_Long != 0
}

/// `FT_IS_NAMED_INSTANCE(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_IS_NAMED_INSTANCE(face: FT_Face) -> bool {
    (*face).face_index & 0x7FFF0000 != 0
}

/// `FT_IS_VARIATION(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_IS_VARIATION(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_VARIATION as FT_Long != 0
}

/// `FT_IS_CID_KEYED(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_IS_CID_KEYED(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_CID_KEYED as FT_Long != 0
}

/// `FT_IS_TRICKY(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_IS_TRICKY(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_TRICKY as FT_Long != 0
}

/// `FT_HAS_COLOR(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_HAS_COLOR(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_COLOR as FT_Long != 0
}

/// `FT_HAS_SVG(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_HAS_SVG(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_SVG as FT_Long != 0
}

/// `FT_HAS_SBIX(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_HAS_SBIX(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_SBIX as FT_Long != 0
}

/// `FT_HAS_SBIX_OVERLAY(face)`
///
/// # Safety
/// `face` must be a valid `FT_Face`.
#[inline]
pub unsafe fn FT_HAS_SBIX_OVERLAY(face: FT_Face) -> bool {
    face_flags(face) & FT_FACE_FLAG_SBIX_OVERLAY as FT_Long != 0
}

/// `FT_MAKE_TAG(a, b, c, d)`
#[inline]
pub const fn FT_MAKE_TAG(a: u8, b: u8, c: u8, d: u8) -> FT_Tag {
    ((a as FT_Tag) << 24) | ((b as FT_Tag) << 16) | ((c as FT_Tag) << 8) | (d as FT_Tag)
}
