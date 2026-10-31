//! Port of `pkg/harfbuzz/font.zig`.
//!
//! STATUS: ported

use std::os::raw::c_int;

use super::c;
use super::errors::Error;
use super::face::Face;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Font {
    pub handle: *mut c::hb_font_t,
}

impl Font {
    /// Constructs a new font object from the specified face.
    pub fn create(face: Face) -> Result<Font, Error> {
        // SAFETY: valid face handle.
        let handle = unsafe { c::hb_font_create(face.handle) };
        if handle.is_null() {
            return Err(Error::HarfbuzzFailed);
        }
        Ok(Font { handle })
    }

    /// Decreases the reference count on the given font object. When the
    /// reference count reaches zero, the font is destroyed, freeing all memory.
    ///
    /// # Safety
    /// The handle (and its copies) must not be used afterwards unless other
    /// references are still held.
    pub unsafe fn destroy(&mut self) {
        c::hb_font_destroy(self.handle);
    }

    pub fn set_scale(&mut self, x: u32, y: u32) {
        // SAFETY: valid font handle.
        unsafe { c::hb_font_set_scale(self.handle, x as c_int, y as c_int) }
    }
}
