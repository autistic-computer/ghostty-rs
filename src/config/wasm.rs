//! Port of `src/config/Wasm.zig`.
//!
//! STATUS: ported

#![cfg(target_arch = "wasm32")]

use super::config::Config;
use crate::cli::args;

const LOG_TARGET: &str = "config";

/// Create a new configuration filled with the initial default values.
#[no_mangle]
pub extern "C" fn config_new() -> *mut Config {
    Box::into_raw(Box::new(Config::default()))
}

/// # Safety
///
/// `ptr` must be null or a config returned by [`config_new`].
#[no_mangle]
pub unsafe extern "C" fn config_free(ptr: *mut Config) {
    if !ptr.is_null() {
        // SAFETY: the caller guarantees this came from Box::into_raw.
        let mut v = unsafe { Box::from_raw(ptr) };
        v.deinit();
    }
}

/// Load the configuration from a string in the same format as
/// the file-based syntax for the desktop version of the terminal.
///
/// # Safety
///
/// `self_` must be a valid config and `str` valid for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn config_load_string(self_: *mut Config, str: *const u8, len: usize) {
    // SAFETY: the caller guarantees the pointers are valid.
    let (this, s) = unsafe { (&mut *self_, std::slice::from_raw_parts(str, len)) };
    if let Err(err) = config_load_string_(this, s) {
        log::error!(target: LOG_TARGET, "error loading config err={}", err);
    }
}

fn config_load_string_(self_: &mut Config, str: &[u8]) -> Result<(), args::Error> {
    let mut iter = args::LineIterator::init(std::io::Cursor::new(str));
    args::parse(self_, &mut iter)
}

/// # Safety
///
/// `self_` must be a valid config.
#[no_mangle]
pub unsafe extern "C" fn config_finalize(self_: *mut Config) {
    // SAFETY: the caller guarantees self_ is valid.
    let this = unsafe { &mut *self_ };
    if let Err(err) = this.finalize() {
        log::error!(target: LOG_TARGET, "error finalizing config err={}", err);
    }
}
