//! Port of `src/sentry/main.zig`.
//!
//! STATUS: ported
//!
//! Only compiled with the `sentry` Cargo feature, which links the system
//! Sentry native SDK library (`libsentry`).

#![cfg(feature = "sentry")]

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "c.rs"]
pub mod c;
#[path = "envelope.rs"]
pub mod envelope;
#[path = "level.rs"]
pub mod level;
#[path = "transport.rs"]
pub mod transport;
#[path = "uuid.rs"]
pub mod uuid;
#[path = "value.rs"]
pub mod value;
// @generated-mods-end

use std::ffi::c_void;

pub use envelope::Envelope;
pub use level::Level;
pub use transport::Transport;
pub use uuid::UUID;
pub use value::Value;

pub fn capture_event(value: Value) -> Option<UUID> {
    // SAFETY: the value is a valid sentry value; ownership moves to sentry.
    let uuid = UUID {
        value: unsafe { c::sentry_capture_event(value.value) },
    };
    if uuid.is_nil() {
        return None;
    }
    Some(uuid)
}

pub fn set_context(key: impl AsRef<[u8]>, value: Value) {
    let key = key.as_ref();
    // SAFETY: the key pointer/length pair is valid.
    unsafe { c::sentry_set_context_n(key.as_ptr().cast(), key.len(), value.value) }
}

pub fn remove_context(key: impl AsRef<[u8]>) {
    let key = key.as_ref();
    // SAFETY: the key pointer/length pair is valid.
    unsafe { c::sentry_remove_context_n(key.as_ptr().cast(), key.len()) }
}

pub fn set_tag(key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) {
    let (key, value) = (key.as_ref(), value.as_ref());
    // SAFETY: the pointer/length pairs are valid strings.
    unsafe {
        c::sentry_set_tag_n(
            key.as_ptr().cast(),
            key.len(),
            value.as_ptr().cast(),
            value.len(),
        )
    }
}

/// Free memory allocated by Sentry.
///
/// # Safety
/// `ptr` must have been allocated by Sentry and not freed already.
pub unsafe fn free(ptr: *mut c_void) {
    // SAFETY: guaranteed by the caller.
    unsafe { c::sentry_free(ptr) }
}
