//! Port of `pkg/fontconfig/char_set.zig`.
//!
//! STATUS: ported

use std::ptr::NonNull;

use super::c;

/// Zig: `opaque` used as `*CharSet`. A copyable, non-null handle to an
/// `FcCharSet`; it must be released explicitly with [`CharSet::destroy`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharSet(NonNull<c::FcCharSet>);

impl CharSet {
    pub fn create() -> CharSet {
        // SAFETY: no preconditions.
        CharSet(NonNull::new(unsafe { c::FcCharSetCreate() }).expect("FcCharSetCreate failed"))
    }

    /// # Safety
    /// The handle (and its copies) must not be used afterwards.
    pub unsafe fn destroy(self) {
        c::FcCharSetDestroy(self.cval());
    }

    pub fn add_char(self, cp: u32) -> bool {
        // SAFETY: valid handle.
        unsafe { c::FcCharSetAddChar(self.cval(), cp) == c::FcTrue as c::FcBool }
    }

    pub fn has_char(self, cp: u32) -> bool {
        // SAFETY: valid handle.
        unsafe { c::FcCharSetHasChar(self.cval_const(), cp) == c::FcTrue as c::FcBool }
    }

    /// Wraps a raw, non-null `FcCharSet` pointer.
    ///
    /// # Safety
    /// `ptr` must be a valid `FcCharSet`.
    pub unsafe fn from_raw(ptr: *mut c::FcCharSet) -> CharSet {
        CharSet(NonNull::new(ptr).expect("null FcCharSet"))
    }

    #[inline]
    pub fn cval(self) -> *mut c::_FcCharSet {
        self.0.as_ptr()
    }

    #[inline]
    pub fn cval_const(self) -> *const c::_FcCharSet {
        self.0.as_ptr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create() {
        let fs = CharSet::create();

        assert!(!fs.has_char(0x20));
        assert!(fs.add_char(0x20));
        assert!(fs.has_char(0x20));

        // SAFETY: not used afterwards.
        unsafe { fs.destroy() };
    }
}
