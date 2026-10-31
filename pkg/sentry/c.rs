//! Port of `src/sentry/c.zig`.
//!
//! STATUS: ported
//!
//! Hand-written declarations for the parts of `sentry.h` (the Sentry native
//! SDK) that Ghostty uses; the Zig build generates these with translate-c.
//! The library is linked as the system `sentry` library.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void};

/// `sentry_value_t`: a 64-bit tagged union.
#[repr(C)]
#[derive(Clone, Copy)]
pub union sentry_value_t {
    pub _bits: u64,
    pub _double: f64,
}

/// `sentry_uuid_t`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct sentry_uuid_t {
    pub bytes: [c_char; 16],
}

#[repr(C)]
pub struct sentry_options_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct sentry_transport_t {
    _private: [u8; 0],
}

#[repr(C)]
pub struct sentry_envelope_t {
    _private: [u8; 0],
}

/// `sentry_level_t`
pub type sentry_level_t = c_int;
pub const SENTRY_LEVEL_DEBUG: sentry_level_t = -1;
pub const SENTRY_LEVEL_INFO: sentry_level_t = 0;
pub const SENTRY_LEVEL_WARNING: sentry_level_t = 1;
pub const SENTRY_LEVEL_ERROR: sentry_level_t = 2;
pub const SENTRY_LEVEL_FATAL: sentry_level_t = 3;

pub type sentry_event_function_t = Option<
    unsafe extern "C" fn(
        event: sentry_value_t,
        hint: *mut c_void,
        closure: *mut c_void,
    ) -> sentry_value_t,
>;

pub type sentry_transport_send_func_t =
    Option<unsafe extern "C" fn(envelope: *mut sentry_envelope_t, state: *mut c_void)>;

pub type sentry_transport_free_func_t = Option<unsafe extern "C" fn(state: *mut c_void)>;

#[link(name = "sentry")]
extern "C" {
    // Options / lifecycle
    pub fn sentry_options_new() -> *mut sentry_options_t;
    pub fn sentry_options_free(opts: *mut sentry_options_t);
    pub fn sentry_options_set_dsn(opts: *mut sentry_options_t, dsn: *const c_char);
    pub fn sentry_options_set_release_n(
        opts: *mut sentry_options_t,
        release: *const c_char,
        release_len: usize,
    );
    pub fn sentry_options_set_transport(
        opts: *mut sentry_options_t,
        transport: *mut sentry_transport_t,
    );
    pub fn sentry_options_set_before_send(
        opts: *mut sentry_options_t,
        func: sentry_event_function_t,
        data: *mut c_void,
    );
    pub fn sentry_options_set_database_path_n(
        opts: *mut sentry_options_t,
        path: *const c_char,
        path_len: usize,
    );
    pub fn sentry_options_set_debug(opts: *mut sentry_options_t, debug: c_int);
    pub fn sentry_init(options: *mut sentry_options_t) -> c_int;
    pub fn sentry_close() -> c_int;

    // Scope
    pub fn sentry_capture_event(event: sentry_value_t) -> sentry_uuid_t;
    pub fn sentry_set_context_n(key: *const c_char, key_len: usize, value: sentry_value_t);
    pub fn sentry_remove_context_n(key: *const c_char, key_len: usize);
    pub fn sentry_set_tag_n(
        key: *const c_char,
        key_len: usize,
        value: *const c_char,
        value_len: usize,
    );
    pub fn sentry_free(ptr: *mut c_void);

    // Values
    pub fn sentry_value_new_message_event_n(
        level: sentry_level_t,
        logger: *const c_char,
        logger_len: usize,
        text: *const c_char,
        text_len: usize,
    ) -> sentry_value_t;
    pub fn sentry_value_new_object() -> sentry_value_t;
    pub fn sentry_value_new_string_n(value: *const c_char, value_len: usize) -> sentry_value_t;
    pub fn sentry_value_new_bool(value: c_int) -> sentry_value_t;
    pub fn sentry_value_new_int32(value: i32) -> sentry_value_t;
    pub fn sentry_value_incref(value: sentry_value_t);
    pub fn sentry_value_decref(value: sentry_value_t);
    pub fn sentry_value_is_null(value: sentry_value_t) -> c_int;
    pub fn sentry_value_set_by_key_n(
        value: sentry_value_t,
        k: *const c_char,
        k_len: usize,
        v: sentry_value_t,
    ) -> c_int;
    pub fn sentry_value_get_by_key_n(
        value: sentry_value_t,
        k: *const c_char,
        k_len: usize,
    ) -> sentry_value_t;

    // Transport
    pub fn sentry_transport_new(send_func: sentry_transport_send_func_t)
        -> *mut sentry_transport_t;
    pub fn sentry_transport_free(transport: *mut sentry_transport_t);
    pub fn sentry_transport_set_state(transport: *mut sentry_transport_t, state: *mut c_void);
    pub fn sentry_transport_set_free_func(
        transport: *mut sentry_transport_t,
        free_func: sentry_transport_free_func_t,
    );

    // Envelope
    pub fn sentry_envelope_free(envelope: *mut sentry_envelope_t);
    pub fn sentry_envelope_serialize(
        envelope: *const sentry_envelope_t,
        size_out: *mut usize,
    ) -> *mut c_char;
    pub fn sentry_envelope_write_to_file_n(
        envelope: *const sentry_envelope_t,
        path: *const c_char,
        path_len: usize,
    ) -> c_int;
    pub fn sentry_envelope_get_event(envelope: *const sentry_envelope_t) -> sentry_value_t;

    // UUID
    pub fn sentry_uuid_new_v4() -> sentry_uuid_t;
    pub fn sentry_uuid_is_nil(uuid: *const sentry_uuid_t) -> c_int;
    pub fn sentry_uuid_as_string(uuid: *const sentry_uuid_t, str: *mut c_char);
}
