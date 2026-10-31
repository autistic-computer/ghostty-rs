//! Port of `src/macos/text/run.zig`.
//!
//! STATUS: ported

use super::c;
use crate::pkg::macos::graphics;
use crate::pkg::macos::opaque_type;

opaque_type!(
    /// `CTRunRef`
    Run
);

/// The whole run (`.{ .location = 0, .length = 0 }`).
const ALL: c::CFRange = c::CFRange {
    location: 0,
    length: 0,
};

impl Run {
    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn get_glyph_count(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::CTRunGetGlyphCount(self.as_ptr()) as usize }
    }

    /// # Safety helper
    /// Build a slice from a run-owned pointer, `None` if null.
    fn slice<T>(&self, ptr: *const T) -> Option<&[T]> {
        let len = self.get_glyph_count();
        if len == 0 {
            return Some(&[]);
        }
        if ptr.is_null() {
            return None;
        }
        // SAFETY: CoreText guarantees the buffer holds glyph-count elements
        // and lives as long as the run.
        Some(unsafe { std::slice::from_raw_parts(ptr, len) })
    }

    pub fn get_glyphs_ptr(&self) -> Option<&[graphics::Glyph]> {
        // SAFETY: self is live.
        self.slice(unsafe { c::CTRunGetGlyphsPtr(self.as_ptr()) })
    }

    pub fn get_glyphs(&self) -> Vec<graphics::Glyph> {
        let mut buf = vec![0; self.get_glyph_count()];
        // SAFETY: buf holds glyph-count elements.
        unsafe { c::CTRunGetGlyphs(self.as_ptr(), ALL, buf.as_mut_ptr()) };
        buf
    }

    pub fn get_positions_ptr(&self) -> Option<&[graphics::Point]> {
        // SAFETY: self is live; Point is layout-compatible with CGPoint.
        self.slice(unsafe { c::CTRunGetPositionsPtr(self.as_ptr()) } as *const graphics::Point)
    }

    pub fn get_positions(&self) -> Vec<graphics::Point> {
        let mut buf = vec![graphics::Point::default(); self.get_glyph_count()];
        // SAFETY: buf holds glyph-count elements; Point is layout-compatible.
        unsafe { c::CTRunGetPositions(self.as_ptr(), ALL, buf.as_mut_ptr() as *mut c::CGPoint) };
        buf
    }

    pub fn get_advances_ptr(&self) -> Option<&[graphics::Size]> {
        // SAFETY: self is live; Size is layout-compatible with CGSize.
        self.slice(unsafe { c::CTRunGetAdvancesPtr(self.as_ptr()) } as *const graphics::Size)
    }

    pub fn get_advances(&self) -> Vec<graphics::Size> {
        let mut buf = vec![graphics::Size::default(); self.get_glyph_count()];
        // SAFETY: buf holds glyph-count elements; Size is layout-compatible.
        unsafe { c::CTRunGetAdvances(self.as_ptr(), ALL, buf.as_mut_ptr() as *mut c::CGSize) };
        buf
    }

    pub fn get_string_indices_ptr(&self) -> Option<&[usize]> {
        // SAFETY: self is live; CFIndex has the size of usize.
        self.slice(unsafe { c::CTRunGetStringIndicesPtr(self.as_ptr()) } as *const usize)
    }

    pub fn get_string_indices(&self) -> Vec<usize> {
        let mut buf = vec![0usize; self.get_glyph_count()];
        // SAFETY: buf holds glyph-count elements; CFIndex has usize's size.
        unsafe {
            c::CTRunGetStringIndices(self.as_ptr(), ALL, buf.as_mut_ptr() as *mut c::CFIndex)
        };
        buf
    }

    pub fn get_status(&self) -> Status {
        // SAFETY: self is live.
        Status::from_bits(unsafe { c::CTRunGetStatus(self.as_ptr()) })
    }
}

/// https://developer.apple.com/documentation/coretext/ctrunstatus?language=objc
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Status {
    pub right_to_left: bool,
    pub non_monotonic: bool,
    pub has_non_identity_matrix: bool,
}

impl Status {
    /// `@bitCast` from the C value.
    pub fn from_bits(v: c::CTRunStatus) -> Status {
        Status {
            right_to_left: v & c::kCTRunStatusRightToLeft != 0,
            non_monotonic: v & c::kCTRunStatusNonMonotonic != 0,
            has_non_identity_matrix: v & c::kCTRunStatusHasNonIdentityMatrix != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bits() {
        let s = Status::from_bits(c::kCTRunStatusRightToLeft | c::kCTRunStatusHasNonIdentityMatrix);
        assert!(s.right_to_left);
        assert!(!s.non_monotonic);
        assert!(s.has_non_identity_matrix);
    }
}
