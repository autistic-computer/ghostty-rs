//! Port of `pkg/fontconfig/range.zig`.
//!
//! STATUS: ported

use std::ptr::NonNull;

use super::c;

/// Zig: `opaque` used as `*Range`. A copyable, non-null handle to an
/// `FcRange`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range(NonNull<c::FcRange>);

impl Range {
    /// # Safety
    /// The handle (and its copies) must not be used afterwards.
    pub unsafe fn destroy(self) {
        c::FcRangeDestroy(self.cval());
    }

    /// Wraps a raw, non-null `FcRange` pointer.
    ///
    /// # Safety
    /// `ptr` must be a valid `FcRange`.
    pub unsafe fn from_raw(ptr: *mut c::FcRange) -> Range {
        Range(NonNull::new(ptr).expect("null FcRange"))
    }

    #[inline]
    pub fn cval(self) -> *mut c::_FcRange {
        self.0.as_ptr()
    }
}
