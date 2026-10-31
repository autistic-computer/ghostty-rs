//! Port of `src/macos/foundation/dictionary.zig`.
//!
//! STATUS: ported

use std::ffi::c_void;
use std::ptr;

use super::c;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CFDictionaryRef`
    Dictionary
);

/// The result of [`Dictionary::get_keys_and_values`].
pub struct KeysAndValues {
    pub keys: Vec<*const c_void>,
    pub values: Vec<*const c_void>,
}

impl Dictionary {
    pub fn create(
        keys: Option<&[*const c_void]>,
        values: Option<&[*const c_void]>,
    ) -> Result<&'static Dictionary, Error> {
        if keys.is_some() || values.is_some() {
            assert!(keys.is_some());
            assert!(values.is_some());
            assert!(keys.unwrap().len() == values.unwrap().len());
        }

        // SAFETY: the key/value slices are valid and of equal length.
        unsafe {
            created(c::CFDictionaryCreate(
                ptr::null(),
                keys.map_or(ptr::null(), |s| s.as_ptr()),
                values.map_or(ptr::null(), |s| s.as_ptr()),
                keys.map_or(0, |s| s.len()) as c::CFIndex,
                ptr::addr_of!(c::kCFTypeDictionaryKeyCallBacks),
                ptr::addr_of!(c::kCFTypeDictionaryValueCallBacks),
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn get_count(&self) -> usize {
        // SAFETY: self is live.
        unsafe { c::CFDictionaryGetCount(self.as_ptr()) as usize }
    }

    /// # Safety
    /// The value for `key`, if any, must be a `V`.
    pub unsafe fn get_value<V>(&self, key: *const c_void) -> Option<&V> {
        // SAFETY: self is live; the value type is guaranteed by the caller.
        unsafe { (c::CFDictionaryGetValue(self.as_ptr(), key) as *const V).as_ref() }
    }

    pub fn get_keys_and_values(&self) -> KeysAndValues {
        let count = self.get_count();
        let mut keys = vec![ptr::null(); count];
        let mut values = vec![ptr::null(); count];
        // SAFETY: both buffers hold `count` pointers.
        unsafe {
            c::CFDictionaryGetKeysAndValues(self.as_ptr(), keys.as_mut_ptr(), values.as_mut_ptr());
        }
        KeysAndValues { keys, values }
    }
}

opaque_type!(
    /// `CFMutableDictionaryRef`
    MutableDictionary
);

impl MutableDictionary {
    pub fn create(cap: usize) -> Result<&'static MutableDictionary, Error> {
        // SAFETY: plain constructor with the standard CF type callbacks.
        unsafe {
            created(c::CFDictionaryCreateMutable(
                ptr::null(),
                cap as c::CFIndex,
                ptr::addr_of!(c::kCFTypeDictionaryKeyCallBacks),
                ptr::addr_of!(c::kCFTypeDictionaryValueCallBacks),
            ))
        }
    }

    pub fn create_mutable_copy(
        cap: usize,
        src: &Dictionary,
    ) -> Result<&'static MutableDictionary, Error> {
        // SAFETY: src is live.
        unsafe {
            created(c::CFDictionaryCreateMutableCopy(
                ptr::null(),
                cap as c::CFIndex,
                src.as_ptr(),
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    /// View as an immutable dictionary (`@ptrCast`).
    pub fn as_dictionary(&self) -> &Dictionary {
        // SAFETY: a mutable dictionary is a dictionary.
        unsafe { &*(self as *const Self as *const Dictionary) }
    }

    /// # Safety
    /// `key` and `value` must be live CF objects.
    pub unsafe fn set_value(&self, key: *const c_void, value: *const c_void) {
        // SAFETY: self is live; key and value are CF objects (retained).
        unsafe { c::CFDictionarySetValue(self.as_ptr(), key, value) }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{String, StringEncoding};
    use super::*;

    #[test]
    fn dictionary() {
        let str = String::create_with_bytes(b"hello", StringEncoding::Unicode, false).unwrap();

        // SAFETY: reading framework constants.
        let (purgeable, volume) = unsafe { (c::kCFURLIsPurgeableKey, c::kCFURLIsVolumeKey) };
        let keys = [purgeable];
        let values = [str.as_ptr() as *const c_void];
        let dict = Dictionary::create(Some(&keys), Some(&values)).unwrap();

        assert_eq!(1usize, dict.get_count());
        // SAFETY: the values are strings.
        unsafe {
            assert!(dict.get_value::<String>(purgeable).is_some());
            assert!(dict.get_value::<String>(volume).is_none());
        }

        dict.release();
        str.release();
    }

    #[test]
    fn mutable_dictionary() {
        let dict = MutableDictionary::create(0).unwrap();

        let str = String::create_with_bytes(b"hello", StringEncoding::Unicode, false).unwrap();

        // SAFETY: reading framework constants.
        let (purgeable, volume) = unsafe { (c::kCFURLIsPurgeableKey, c::kCFURLIsVolumeKey) };
        // SAFETY: both are live CF objects.
        unsafe { dict.set_value(purgeable, str.as_ptr()) };

        {
            let imm = dict.as_dictionary();
            assert_eq!(1usize, imm.get_count());
            // SAFETY: the values are strings.
            unsafe {
                assert!(imm.get_value::<String>(purgeable).is_some());
                assert!(imm.get_value::<String>(volume).is_none());
            }
        }

        str.release();
        dict.release();
    }
}
