//! Port of `src/sentry/uuid.zig`.
//!
//! STATUS: ported

use super::c;

/// sentry_uuid_t
#[derive(Clone, Copy)]
pub struct UUID {
    pub value: c::sentry_uuid_t,
}

impl UUID {
    pub fn init() -> UUID {
        // SAFETY: plain FFI constructor.
        UUID {
            value: unsafe { c::sentry_uuid_new_v4() },
        }
    }

    pub fn is_nil(&self) -> bool {
        // SAFETY: the pointer is to a valid uuid.
        unsafe { c::sentry_uuid_is_nil(&self.value) != 0 }
    }

    /// The hyphenated string form (36 bytes; the Zig version returns a
    /// `[36:0]u8`).
    pub fn string(&self) -> [u8; 36] {
        let mut buf = [0u8; 37];
        // SAFETY: sentry writes 36 characters plus a NUL terminator.
        unsafe { c::sentry_uuid_as_string(&self.value, buf.as_mut_ptr().cast()) };
        let mut out = [0u8; 36];
        out.copy_from_slice(&buf[..36]);
        out
    }
}
