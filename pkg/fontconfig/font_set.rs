//! Port of `pkg/fontconfig/font_set.zig`.
//!
//! STATUS: ported

use std::ptr::NonNull;

use super::c;
use super::pattern::Pattern;

/// Zig: `opaque` used as `*FontSet`. A copyable, non-null handle to an
/// `FcFontSet`; it must be released explicitly with [`FontSet::destroy`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontSet(NonNull<c::FcFontSet>);

impl FontSet {
    pub fn create() -> FontSet {
        // SAFETY: no preconditions.
        FontSet(NonNull::new(unsafe { c::FcFontSetCreate() }).expect("FcFontSetCreate failed"))
    }

    /// Destroys the font set and every pattern in it.
    ///
    /// # Safety
    /// The handle (and its copies, and the patterns in the set) must not be
    /// used afterwards.
    pub unsafe fn destroy(self) {
        c::FcFontSetDestroy(self.cval());
    }

    pub fn fonts(&self) -> &[Pattern] {
        // SAFETY: valid handle; `fonts` holds `nfont` non-null patterns and
        // Pattern is a transparent non-null pattern pointer.
        unsafe {
            let s = &*self.cval();
            if s.fonts.is_null() {
                return &[];
            }
            std::slice::from_raw_parts(s.fonts as *const Pattern, s.nfont as usize)
        }
    }

    pub fn add(self, pat: Pattern) -> bool {
        // SAFETY: valid handles; the set takes ownership of the pattern.
        unsafe { c::FcFontSetAdd(self.cval(), pat.cval()) == c::FcTrue as c::FcBool }
    }

    pub fn print(self) {
        // SAFETY: valid handle.
        unsafe { c::FcFontSetPrint(self.cval()) }
    }

    /// Wraps a raw, non-null `FcFontSet` pointer.
    ///
    /// # Safety
    /// `ptr` must be a valid `FcFontSet`.
    pub unsafe fn from_raw(ptr: *mut c::FcFontSet) -> FontSet {
        FontSet(NonNull::new(ptr).expect("null FcFontSet"))
    }

    #[inline]
    pub fn cval(self) -> *mut c::_FcFontSet {
        self.0.as_ptr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create() {
        let fs = FontSet::create();
        assert_eq!(0usize, fs.fonts().len());
        // SAFETY: not used afterwards.
        unsafe { fs.destroy() };
    }
}
