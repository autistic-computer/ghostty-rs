//! Port of `pkg/freetype/tag.zig`.
//!
//! STATUS: ported

/// FT_Tag
///
/// Zig: `packed struct(u32) { d, c, b, a }`; `a` is the most significant
/// byte of the `u32` value. Use [`Tag::to_int`]/[`Tag::from_int`] for the
/// `@bitCast` to/from `u32`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tag {
    pub d: u8,
    pub c: u8,
    pub b: u8,
    pub a: u8,
}

impl Tag {
    pub const fn init(v: &[u8; 4]) -> Tag {
        Tag {
            a: v[0],
            b: v[1],
            c: v[2],
            d: v[3],
        }
    }

    /// Converts the ID to a string. The return value is only valid
    /// for the lifetime of the self pointer.
    pub const fn str(self) -> [u8; 4] {
        [self.a, self.b, self.c, self.d]
    }

    /// `@bitCast(tag)` to the `u32` (FT_Tag) value.
    pub const fn to_int(self) -> u32 {
        ((self.a as u32) << 24) | ((self.b as u32) << 16) | ((self.c as u32) << 8) | self.d as u32
    }

    /// `@bitCast(value)` from a `u32` (FT_Tag) value.
    pub const fn from_int(v: u32) -> Tag {
        Tag {
            a: (v >> 24) as u8,
            b: (v >> 16) as u8,
            c: (v >> 8) as u8,
            d: v as u8,
        }
    }
}
