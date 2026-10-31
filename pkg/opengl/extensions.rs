//! Port of `pkg/opengl/extensions.zig`.
//!
//! STATUS: ported

use std::ffi::CStr;

use super::c;
use super::errors::{self, Error};
use super::glad;

/// Returns the number of extensions.
pub fn len() -> Result<u32, Error> {
    let mut n: c::GLint = 0;
    glad::call!(GetIntegerv(c::GL_NUM_EXTENSIONS, &mut n));
    errors::get_error()?;
    Ok(n as u32)
}

/// Returns an iterator for the extensions.
pub fn iterator() -> Result<Iterator, Error> {
    Ok(Iterator { len: len()?, i: 0 })
}

/// Iterator for the available extensions.
#[derive(Debug, Clone, Copy, Default)]
pub struct Iterator {
    /// The total number of extensions.
    pub len: c::GLuint,
    pub i: c::GLuint,
}

impl Iterator {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<&'static [u8]>, Error> {
        if self.i >= self.len {
            return Ok(None);
        }
        let res = glad::call!(GetStringi(c::GL_EXTENSIONS, self.i));
        errors::get_error()?;
        self.i += 1;
        // SAFETY: GL returns a static NUL-terminated string.
        Ok(Some(unsafe { CStr::from_ptr(res as *const _) }.to_bytes()))
    }
}
