//! Port of `pkg/fontconfig/lang_set.zig`.
//!
//! STATUS: ported

use std::ffi::CStr;
use std::ptr::NonNull;

use super::c;

/// Zig: `opaque` used as `*LangSet`. A copyable, non-null handle to an
/// `FcLangSet`; it must be released explicitly with [`LangSet::destroy`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LangSet(NonNull<c::FcLangSet>);

impl LangSet {
    pub fn create() -> LangSet {
        // SAFETY: no preconditions.
        LangSet(NonNull::new(unsafe { c::FcLangSetCreate() }).expect("FcLangSetCreate failed"))
    }

    /// # Safety
    /// The handle (and its copies) must not be used afterwards.
    pub unsafe fn destroy(self) {
        c::FcLangSetDestroy(self.cval());
    }

    pub fn add_lang(self, lang: &CStr) -> bool {
        // SAFETY: valid handle and NUL-terminated string.
        unsafe {
            c::FcLangSetAdd(self.cval(), lang.as_ptr() as *const c::FcChar8)
                == c::FcTrue as c::FcBool
        }
    }

    pub fn has_lang(self, lang: &CStr) -> bool {
        // SAFETY: valid handle and NUL-terminated string.
        unsafe {
            c::FcLangSetHasLang(self.cval_const(), lang.as_ptr() as *const c::FcChar8)
                == c::FcLangEqual
        }
    }

    /// Wraps a raw, non-null `FcLangSet` pointer.
    ///
    /// # Safety
    /// `ptr` must be a valid `FcLangSet`.
    pub unsafe fn from_raw(ptr: *mut c::FcLangSet) -> LangSet {
        LangSet(NonNull::new(ptr).expect("null FcLangSet"))
    }

    #[inline]
    pub fn cval(self) -> *mut c::_FcLangSet {
        self.0.as_ptr()
    }

    #[inline]
    pub fn cval_const(self) -> *const c::_FcLangSet {
        self.0.as_ptr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create() {
        let fs = LangSet::create();
        assert!(!fs.has_lang(c"und-zsye"));
        // SAFETY: not used afterwards.
        unsafe { fs.destroy() };
    }

    #[test]
    fn has_lang_exact_match() {
        // Test exact match: langset with "en-US" should return true for "en-US"
        let fs = LangSet::create();
        assert!(fs.add_lang(c"en-US"));
        assert!(fs.has_lang(c"en-US"));

        // Test exact match: langset with "und-zsye" should return true for "und-zsye"
        let fs_emoji = LangSet::create();
        assert!(fs_emoji.add_lang(c"und-zsye"));
        assert!(fs_emoji.has_lang(c"und-zsye"));

        // Test mismatch: langset with "en-US" should return false for "fr"
        assert!(!fs.has_lang(c"fr"));

        // Test partial match: langset with "en-US" should return false for "en-GB"
        // (different territory, but we only want exact matches)
        assert!(!fs.has_lang(c"en-GB"));

        // SAFETY: not used afterwards.
        unsafe {
            fs_emoji.destroy();
            fs.destroy();
        }
    }
}
