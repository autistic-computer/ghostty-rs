//! Port of `src/macos/dispatch/data.zig`.
//!
//! STATUS: ported

use std::ffi::c_void;

use super::c;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `dispatch_data_t`
    Data
);

impl Data {
    pub const DESTRUCTOR_DEFAULT: *mut c_void = c::DISPATCH_DATA_DESTRUCTOR_DEFAULT;

    /// Create a dispatch data object. With the default destructor the
    /// bytes are copied.
    ///
    /// # Safety
    /// `queue` must be null or a dispatch queue and `destructor` null or a
    /// dispatch block.
    pub unsafe fn create(
        data: &[u8],
        queue: *mut c_void,
        destructor: *mut c_void,
    ) -> Result<&'static Data, Error> {
        // SAFETY: data is a valid slice; the rest is guaranteed by the caller.
        unsafe {
            created(c::dispatch_data_create(
                data.as_ptr() as *const c_void,
                data.len(),
                queue,
                destructor,
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }
}
