//! Port of `src/sentry/value.zig`.
//!
//! STATUS: ported

use super::c;
use super::level::Level;

/// sentry_value_t
#[derive(Clone, Copy)]
pub struct Value {
    /// The underlying value. This is a union that could be represented with
    /// an extern union but I don't want to risk C ABI issues so we wrap it
    /// in a struct.
    pub value: c::sentry_value_t,
}

impl Value {
    pub fn init_message_event(
        level: Level,
        logger: Option<&[u8]>,
        message: impl AsRef<[u8]>,
    ) -> Value {
        let message = message.as_ref();
        // SAFETY: the pointer/length pairs are valid (or null/0).
        Value {
            value: unsafe {
                c::sentry_value_new_message_event_n(
                    level.to_int(),
                    logger.map_or(std::ptr::null(), |v| v.as_ptr().cast()),
                    logger.map_or(0, |v| v.len()),
                    message.as_ptr().cast(),
                    message.len(),
                )
            },
        }
    }

    pub fn init_object() -> Value {
        // SAFETY: plain FFI constructor.
        Value {
            value: unsafe { c::sentry_value_new_object() },
        }
    }

    pub fn init_string(value: impl AsRef<[u8]>) -> Value {
        let value = value.as_ref();
        // SAFETY: the pointer/length pair is valid.
        Value {
            value: unsafe { c::sentry_value_new_string_n(value.as_ptr().cast(), value.len()) },
        }
    }

    pub fn init_bool(value: bool) -> Value {
        // SAFETY: plain FFI constructor.
        Value {
            value: unsafe { c::sentry_value_new_bool(value as i32) },
        }
    }

    pub fn init_int32(value: i32) -> Value {
        // SAFETY: plain FFI constructor.
        Value {
            value: unsafe { c::sentry_value_new_int32(value) },
        }
    }

    pub fn decref(self) {
        // SAFETY: the value is a valid sentry value.
        unsafe { c::sentry_value_decref(self.value) }
    }

    /// Increment the reference count. (The Zig version declares a `Value`
    /// return type but never returns one; this returns `self`.)
    pub fn incref(self) -> Value {
        // SAFETY: the value is a valid sentry value.
        unsafe { c::sentry_value_incref(self.value) };
        self
    }

    pub fn is_null(self) -> bool {
        // SAFETY: the value is a valid sentry value.
        unsafe { c::sentry_value_is_null(self.value) != 0 }
    }

    /// sentry_value_set_by_key_n
    pub fn set(self, key: impl AsRef<[u8]>, value: Value) {
        let key = key.as_ref();
        // SAFETY: the key pointer/length pair is valid; ownership of
        // `value` moves into the object.
        unsafe {
            c::sentry_value_set_by_key_n(self.value, key.as_ptr().cast(), key.len(), value.value);
        }
    }

    /// sentry_value_get_by_key_n
    pub fn get(self, key: impl AsRef<[u8]>) -> Option<Value> {
        let key = key.as_ref();
        // SAFETY: the key pointer/length pair is valid.
        let val = Value {
            value: unsafe {
                c::sentry_value_get_by_key_n(self.value, key.as_ptr().cast(), key.len())
            },
        };
        if val.is_null() {
            return None;
        }
        Some(val)
    }
}
