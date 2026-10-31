//! Port of `pkg/oniguruma/match_param.zig`.
//!
//! STATUS: ported

use std::os::raw::c_ulong;

use super::c;
use super::errors::{self, Error};

/// Owns an `OnigMatchParam`; freed on drop (Zig: explicit `deinit`).
#[derive(Debug)]
pub struct MatchParam {
    pub value: *mut c::OnigMatchParam,
}

// SAFETY: the match param is exclusively owned and not tied to a thread.
unsafe impl Send for MatchParam {}

impl MatchParam {
    pub fn init() -> Result<MatchParam, Error> {
        // SAFETY: no preconditions.
        let value = unsafe { c::onig_new_match_param() };
        if value.is_null() {
            return Err(Error::Memory);
        }
        Ok(MatchParam { value })
    }

    pub fn set_retry_limit_in_search(&mut self, limit: usize) -> Result<(), Error> {
        // SAFETY: valid match param.
        errors::convert_error(unsafe {
            c::onig_set_retry_limit_in_search_of_match_param(self.value, limit as c_ulong)
        })?;
        Ok(())
    }
}

impl Drop for MatchParam {
    fn drop(&mut self) {
        // SAFETY: owned match param, freed exactly once.
        unsafe { c::onig_free_match_param(self.value) }
    }
}
