//! Port of `src/sentry/transport.zig`.
//!
//! STATUS: ported

use std::ffi::c_void;

use super::c;
use super::envelope::Envelope;

pub type SendFunc = unsafe extern "C" fn(envelope: *mut Envelope, state: *mut c_void);
pub type FreeFunc = unsafe extern "C" fn(state: *mut c_void);

/// sentry_transport_t
///
/// An opaque type: only ever used behind a pointer handed out by Sentry.
#[repr(C)]
pub struct Transport {
    _private: [u8; 0],
}

impl Transport {
    fn ptr(&self) -> *mut c::sentry_transport_t {
        self as *const Self as *mut c::sentry_transport_t
    }

    /// Create a new transport. The returned reference is valid until it is
    /// passed to Sentry (which takes ownership) or [`Transport::deinit`].
    pub fn init(f: SendFunc) -> &'static Transport {
        // SAFETY: `Envelope` is an opaque stand-in for `sentry_envelope_t`, so
        // the two function pointer types have the same ABI.
        let f: unsafe extern "C" fn(*mut c::sentry_envelope_t, *mut c_void) =
            unsafe { std::mem::transmute(f) };
        // SAFETY: plain FFI constructor.
        let ptr = unsafe { c::sentry_transport_new(Some(f)) };
        assert!(!ptr.is_null());
        // SAFETY: non-null transport allocated by Sentry.
        unsafe { &*(ptr as *const Transport) }
    }

    pub fn deinit(&self) {
        // SAFETY: self is a live transport owned by the caller.
        unsafe { c::sentry_transport_free(self.ptr()) }
    }

    pub fn set_state(&self, state: *mut c_void) {
        // SAFETY: self is a live transport.
        unsafe { c::sentry_transport_set_state(self.ptr(), state) }
    }

    pub fn set_state_free_func(&self, f: FreeFunc) {
        // SAFETY: self is a live transport.
        unsafe { c::sentry_transport_set_free_func(self.ptr(), Some(f)) }
    }

    /// The raw pointer, for `sentry_options_set_transport`.
    pub fn as_ptr(&self) -> *mut c::sentry_transport_t {
        self.ptr()
    }
}
