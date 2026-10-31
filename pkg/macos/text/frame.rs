//! Port of `src/macos/text/frame.zig`.
//!
//! STATUS: ported

use super::c;
use crate::pkg::macos::foundation;
use crate::pkg::macos::graphics;
use crate::pkg::macos::opaque_type;

opaque_type!(
    /// `CTFrameRef`
    Frame
);

impl Frame {
    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn get_line_origins(&self, range: foundation::Range, points: &mut [graphics::Point]) {
        // SAFETY: points is a valid buffer; Point is layout-compatible with
        // CGPoint. (Like the C API, `range` must fit in `points`.)
        unsafe {
            c::CTFrameGetLineOrigins(
                self.as_ptr(),
                range.cval(),
                points.as_mut_ptr() as *mut c::CGPoint,
            )
        }
    }

    pub fn get_lines(&self) -> &foundation::Array {
        // SAFETY: self is live; the array is owned by self.
        unsafe { &*(c::CTFrameGetLines(self.as_ptr()) as *const foundation::Array) }
    }
}

// See framesetter tests...
