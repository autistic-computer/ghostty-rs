//! Port of `src/macos/foundation/data.zig`.
//!
//! STATUS: ported

use std::ptr;

use super::c;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CFDataRef`
    Data
);

impl Data {
    /// Wrap `data` without copying it; `data` must outlive the object.
    pub fn create_with_bytes_no_copy(data: &[u8]) -> Result<&'static Data, Error> {
        // SAFETY: data is a valid slice; kCFAllocatorNull means CF won't free it.
        unsafe {
            created(c::CFDataCreateWithBytesNoCopy(
                ptr::null(),
                data.as_ptr(),
                data.len() as c::CFIndex,
                c::kCFAllocatorNull,
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn get_pointer(&self) -> *const u8 {
        // SAFETY: self is live.
        unsafe { c::CFDataGetBytePtr(self.as_ptr()) }
    }

    pub fn get_length(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::CFDataGetLength(self.as_ptr()) as usize }
    }

    /// The bytes as a slice (`getPointer()[0..getLength()]`).
    pub fn as_slice(&self) -> &[u8] {
        let len = self.get_length();
        if len == 0 {
            return &[];
        }
        // SAFETY: CF guarantees the pointer is valid for len bytes while
        // self is alive.
        unsafe { std::slice::from_raw_parts(self.get_pointer(), len) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data() {
        let raw = b"hello world";
        let data = Data::create_with_bytes_no_copy(raw).unwrap();
        assert_eq!(data.as_slice(), raw);
        data.release();
    }
}
