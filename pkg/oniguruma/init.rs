//! Port of `pkg/oniguruma/init.zig`.
//!
//! STATUS: ported

use std::os::raw::c_int;

use super::c;
use super::errors::{self, Error};
use super::types::Encoding;

/// Call once per process to initialize Oniguruma. This should be given
/// the encodings that the program will use.
pub fn init(encs: &[&Encoding]) -> Result<(), Error> {
    // SAFETY: `&Encoding` has the layout of `OnigEncoding`; Oniguruma only
    // reads the array.
    errors::convert_error(unsafe {
        c::onig_initialize(encs.as_ptr() as *mut c::OnigEncoding, encs.len() as c_int)
    })?;
    Ok(())
}

pub fn deinit() {
    // SAFETY: no preconditions.
    unsafe {
        c::onig_end();
    }
}
