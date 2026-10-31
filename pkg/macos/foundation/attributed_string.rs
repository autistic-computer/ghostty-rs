//! Port of `src/macos/foundation/attributed_string.zig`.
//!
//! STATUS: ported

use std::ffi::c_void;
use std::ptr;

use super::c;
use super::dictionary::Dictionary;
use super::string::String;
use super::Range;
use crate::pkg::macos::{created, opaque_type, CFType, Error};

opaque_type!(
    /// `CFAttributedStringRef`
    AttributedString
);

impl AttributedString {
    pub fn create(
        str: &String,
        attributes: &Dictionary,
    ) -> Result<&'static AttributedString, Error> {
        // SAFETY: str and attributes are live.
        unsafe {
            created(c::CFAttributedStringCreate(
                ptr::null(),
                str.as_ptr(),
                attributes.as_ptr(),
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn get_length(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::CFAttributedStringGetLength(self.as_ptr()) as usize }
    }

    pub fn get_string(&self) -> &String {
        // SAFETY: self is live; the string is owned by self.
        unsafe { &*(c::CFAttributedStringGetString(self.as_ptr()) as *const String) }
    }
}

/// An attribute key: anything with a `key()` (like `text.StringAttribute`
/// or `text.FontAttribute`) or a CF string itself.
pub trait AttributeKey {
    fn key_ptr(&self) -> *const c_void;
}

impl AttributeKey for String {
    fn key_ptr(&self) -> *const c_void {
        self.as_ptr()
    }
}

impl<T: AttributeKey + ?Sized> AttributeKey for &T {
    fn key_ptr(&self) -> *const c_void {
        (**self).key_ptr()
    }
}

opaque_type!(
    /// `CFMutableAttributedStringRef`
    MutableAttributedString
);

impl MutableAttributedString {
    pub fn create(cap: usize) -> Result<&'static MutableAttributedString, Error> {
        // SAFETY: plain constructor.
        unsafe {
            created(c::CFAttributedStringCreateMutable(
                ptr::null(),
                cap as c::CFIndex,
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    /// View as an immutable attributed string (`@ptrCast`).
    pub fn as_attributed_string(&self) -> &AttributedString {
        // SAFETY: a mutable attributed string is an attributed string.
        unsafe { &*(self as *const Self as *const AttributedString) }
    }

    pub fn replace_string(&self, range: Range, replacement: &String) {
        // SAFETY: self and replacement are live.
        unsafe {
            c::CFAttributedStringReplaceString(self.as_ptr(), range.cval(), replacement.as_ptr())
        }
    }

    /// Set an attribute. `value` may be null.
    ///
    /// # Safety
    /// `value` must be null or a live CF object.
    pub unsafe fn set_attribute(&self, range: Range, key: impl AttributeKey, value: *const c_void) {
        // SAFETY: self is live; key is a CF string.
        unsafe {
            c::CFAttributedStringSetAttribute(self.as_ptr(), range.cval(), key.key_ptr(), value)
        }
    }

    /// Set an attribute to a CF object.
    pub fn set_attribute_value(&self, range: Range, key: impl AttributeKey, value: &dyn CFType) {
        // SAFETY: value is a live CF object.
        unsafe { self.set_attribute(range, key, value.as_ptr()) };
    }

    pub fn get_length(&self) -> usize {
        self.as_attributed_string().get_length()
    }
}

#[cfg(test)]
mod tests {
    use super::super::StringEncoding;
    use super::*;
    use crate::pkg::macos::text;

    #[test]
    fn mutable_attributed_string() {
        let str = MutableAttributedString::create(0).unwrap();

        {
            let rep = String::create_with_bytes(b"hello", StringEncoding::Utf8, false).unwrap();
            str.replace_string(Range::init(0, 0), rep);
            rep.release();
        }

        // SAFETY: null values are allowed.
        unsafe {
            str.set_attribute(Range::init(0, 0), text::FontAttribute::Url, ptr::null());
            str.set_attribute(
                Range::init(0, 0),
                text::FontAttribute::Name.key(),
                ptr::null(),
            );
        }
        str.release();
    }
}
