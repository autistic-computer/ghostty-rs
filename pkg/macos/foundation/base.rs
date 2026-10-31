//! Port of `src/macos/foundation/base.zig`.
//!
//! STATUS: ported

use super::c;

#[repr(isize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonResult {
    Less = -1,
    Equal = 0,
    Greater = 1,
}

impl ComparisonResult {
    /// `@fromBackingInt`
    pub fn from_int(v: c::CFComparisonResult) -> ComparisonResult {
        match v {
            v if v < 0 => ComparisonResult::Less,
            0 => ComparisonResult::Equal,
            _ => ComparisonResult::Greater,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Range {
    pub location: c::CFIndex,
    pub length: c::CFIndex,
}

impl Range {
    pub fn init(loc: usize, len: usize) -> Range {
        Range {
            location: loc as c::CFIndex,
            length: len as c::CFIndex,
        }
    }

    pub fn cval(self) -> c::CFRange {
        c::CFRange {
            location: self.location,
            length: self.length,
        }
    }
}

/// A four character code, stored like the Zig `packed struct(u32)` with
/// `d` in the low byte and `a` in the high byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FourCharCode {
    pub d: u8,
    pub c: u8,
    pub b: u8,
    pub a: u8,
}

impl FourCharCode {
    pub fn init(v: &[u8; 4]) -> FourCharCode {
        FourCharCode {
            a: v[0],
            b: v[1],
            c: v[2],
            d: v[3],
        }
    }

    /// Converts the ID to a string.
    pub fn str(self) -> [u8; 4] {
        [self.a, self.b, self.c, self.d]
    }

    /// `@bitCast` to `u32`.
    pub fn to_bits(self) -> u32 {
        u32::from_le_bytes([self.d, self.c, self.b, self.a])
    }

    /// `@bitCast` from `u32`.
    pub fn from_bits(v: u32) -> FourCharCode {
        let [d, c, b, a] = v.to_le_bytes();
        FourCharCode { d, c, b, a }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_char_code() {
        let v = FourCharCode::init(b"head");
        assert_eq!(&v.str(), b"head");
        assert_eq!(v.to_bits(), u32::from_be_bytes(*b"head"));
        assert_eq!(FourCharCode::from_bits(v.to_bits()), v);
    }
}
