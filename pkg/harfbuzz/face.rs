//! Port of `pkg/harfbuzz/face.zig`.
//!
//! STATUS: ported

use super::c;

/// A font face is an object that represents a single face from within a font family.
///
/// More precisely, a font face represents a single face in a binary font file.
/// Font faces are typically built from a binary blob and a face index.
/// Font faces are used to create fonts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Face {
    pub handle: *mut c::hb_face_t,
}

impl Face {
    /// Decreases the reference count on a face object. When the reference
    /// count reaches zero, the face is destroyed, freeing all memory.
    ///
    /// # Safety
    /// The handle (and its copies) must not be used afterwards unless other
    /// references are still held.
    pub unsafe fn destroy(&mut self) {
        c::hb_face_destroy(self.handle);
    }
}
