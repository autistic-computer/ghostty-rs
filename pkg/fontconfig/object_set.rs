//! Port of `pkg/fontconfig/object_set.zig`.
//!
//! STATUS: ported

use std::ptr::NonNull;

use super::c;
use super::common::Property;

/// Zig: `opaque` used as `*ObjectSet`. A copyable, non-null handle to an
/// `FcObjectSet`; it must be released explicitly with [`ObjectSet::destroy`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectSet(NonNull<c::FcObjectSet>);

impl ObjectSet {
    pub fn create() -> ObjectSet {
        // SAFETY: no preconditions.
        ObjectSet(
            NonNull::new(unsafe { c::FcObjectSetCreate() }).expect("FcObjectSetCreate failed"),
        )
    }

    /// # Safety
    /// The handle (and its copies) must not be used afterwards.
    pub unsafe fn destroy(self) {
        c::FcObjectSetDestroy(self.cval());
    }

    pub fn add(self, p: Property) -> bool {
        // SAFETY: valid handle and NUL-terminated static string.
        unsafe { c::FcObjectSetAdd(self.cval(), p.cval().as_ptr()) == c::FcTrue as c::FcBool }
    }

    #[inline]
    pub fn cval(self) -> *mut c::_FcObjectSet {
        self.0.as_ptr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create() {
        let os = ObjectSet::create();
        assert!(os.add(Property::Family));
        // SAFETY: not used afterwards.
        unsafe { os.destroy() };
    }
}
