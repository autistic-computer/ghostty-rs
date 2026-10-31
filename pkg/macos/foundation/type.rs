//! Port of `src/macos/foundation/type.zig`.
//!
//! STATUS: ported

use std::ffi::c_void;

/// Release a Core Foundation object.
///
/// # Safety
/// `cf` must be a live CF object owned (+1) by the caller.
pub unsafe fn cf_release(cf: *mut c_void) {
    // SAFETY: guaranteed by the caller.
    unsafe { super::c::CFRelease(cf) }
}

/// Retain a Core Foundation object.
///
/// # Safety
/// `cf` must be a live CF object.
pub unsafe fn cf_retain(cf: *mut c_void) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        super::c::CFRetain(cf);
    }
}
