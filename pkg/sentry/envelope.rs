//! Port of `src/sentry/envelope.zig`.
//!
//! STATUS: ported

use std::fmt;

use super::c;
use super::value::Value;

/// `error{WriteFailed}`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteFailed;

impl fmt::Display for WriteFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WriteFailed")
    }
}

impl std::error::Error for WriteFailed {}

/// sentry_envelope_t
///
/// An opaque type: only ever used behind a pointer handed out by Sentry.
#[repr(C)]
pub struct Envelope {
    _private: [u8; 0],
}

impl Envelope {
    fn ptr(&self) -> *mut c::sentry_envelope_t {
        self as *const Self as *mut c::sentry_envelope_t
    }

    /// Free the envelope. `self` must not be used afterwards.
    pub fn deinit(&self) {
        // SAFETY: self is a live envelope owned by the caller.
        unsafe { c::sentry_envelope_free(self.ptr()) }
    }

    pub fn write_to_file(&self, path: &[u8]) -> Result<(), WriteFailed> {
        // SAFETY: the path pointer/length pair is valid.
        if unsafe {
            c::sentry_envelope_write_to_file_n(self.ptr(), path.as_ptr().cast(), path.len())
        } != 0
        {
            return Err(WriteFailed);
        }
        Ok(())
    }

    /// Serialize the envelope. The returned buffer is allocated by Sentry
    /// and must be freed with [`super::free`].
    pub fn serialize(&self) -> *mut [u8] {
        let mut len: usize = 0;
        // SAFETY: self is a live envelope; len is a valid out pointer.
        let ptr = unsafe { c::sentry_envelope_serialize(self.ptr(), &mut len) };
        assert!(!ptr.is_null());
        std::ptr::slice_from_raw_parts_mut(ptr.cast::<u8>(), len)
    }

    pub fn event(&self) -> Option<Value> {
        // SAFETY: self is a live envelope.
        let val = Value {
            value: unsafe { c::sentry_envelope_get_event(self.ptr()) },
        };
        if val.is_null() {
            return None;
        }
        Some(val)
    }
}
