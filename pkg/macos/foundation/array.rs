//! Port of `src/macos/foundation/array.zig`.
//!
//! STATUS: ported

use std::ffi::c_void;
use std::ptr;

use super::base::{ComparisonResult, Range};
use super::c;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CFArrayRef`
    Array
);

impl Array {
    /// Create an array of raw pointers (no retain/release callbacks, like
    /// the Zig version which passes null callbacks).
    pub fn create<T>(values: &[&T]) -> Result<&'static Array, Error> {
        // SAFETY: values is a valid slice of pointers.
        unsafe {
            created(c::CFArrayCreate(
                ptr::null(),
                values.as_ptr() as *const *const c_void,
                values.len() as c::CFIndex,
                ptr::null(),
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live array owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn get_count(&self) -> usize {
        // SAFETY: self is a live array.
        unsafe { c::CFArrayGetCount(self.as_ptr()) as usize }
    }

    /// Note the return type is actually a `*const T` but we strip the
    /// constness so that further API calls work correctly. The Foundation
    /// API doesn't properly mark things const/non-const.
    ///
    /// # Safety
    /// The value at `idx` must be a `T` and `idx` must be in bounds.
    pub unsafe fn get_value_at_index<T>(&self, idx: usize) -> &'static T {
        // SAFETY: guaranteed by the caller.
        unsafe { &*(c::CFArrayGetValueAtIndex(self.as_ptr(), idx as c::CFIndex) as *const T) }
    }
}

opaque_type!(
    /// `CFMutableArrayRef`
    MutableArray
);

/// A comparator for [`MutableArray::sort_values`].
pub type Comparator<Elem, Context> =
    unsafe extern "C" fn(a: *const Elem, b: *const Elem, context: *mut Context) -> ComparisonResult;

impl MutableArray {
    pub fn create() -> Result<&'static MutableArray, Error> {
        // SAFETY: plain constructor with the standard CF type callbacks.
        unsafe {
            created(c::CFArrayCreateMutable(
                ptr::null(),
                0,
                ptr::addr_of!(c::kCFTypeArrayCallBacks),
            ))
        }
    }

    pub fn create_copy(array: &Array) -> Result<&'static MutableArray, Error> {
        // SAFETY: array is live.
        unsafe { created(c::CFArrayCreateMutableCopy(ptr::null(), 0, array.as_ptr())) }
    }

    pub fn release(&self) {
        // SAFETY: self is a live array owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    /// View as an immutable array (`@ptrCast`).
    pub fn as_array(&self) -> &Array {
        // SAFETY: a mutable array is an array.
        unsafe { &*(self as *const Self as *const Array) }
    }

    pub fn append_value<Elem>(&self, value: &Elem) {
        // SAFETY: self is live; value is a valid pointer.
        unsafe { c::CFArrayAppendValue(self.as_ptr(), value as *const Elem as *const c_void) }
    }

    pub fn remove_value(&self, idx: usize) {
        // SAFETY: self is live.
        unsafe { c::CFArrayRemoveValueAtIndex(self.as_ptr(), idx as c::CFIndex) }
    }

    pub fn sort_values<Elem, Context>(
        &self,
        context: Option<&mut Context>,
        comparator: Option<Comparator<Elem, Context>>,
    ) {
        let count = self.as_array().get_count();
        // SAFETY: the comparator's pointer arguments have the same ABI as
        // CFComparatorFunction's, and ComparisonResult is CFIndex-sized.
        unsafe {
            c::CFArraySortValues(
                self.as_ptr(),
                Range::init(0, count).cval(),
                std::mem::transmute::<Option<Comparator<Elem, Context>>, c::CFComparatorFunction>(
                    comparator,
                ),
                context.map_or(ptr::null_mut(), |c| c as *mut Context as *mut c_void),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array() {
        let str = b"hello";
        let values = [&str[0], &str[1]];
        let arr = Array::create(&values).unwrap();

        assert_eq!(2usize, arr.get_count());

        {
            // SAFETY: the array holds u8 pointers.
            let ch: &u8 = unsafe { arr.get_value_at_index(0) };
            assert_eq!(b'h', *ch);
        }

        // Can make it mutable
        let mutable = MutableArray::create_copy(arr).unwrap();
        mutable.release();
        arr.release();
    }

    #[test]
    fn array_sorting() {
        let str = b"hello";
        let values = [&str[0], &str[1]];
        let arr = Array::create(&values).unwrap();
        let mutable = MutableArray::create_copy(arr).unwrap();

        unsafe extern "C" fn compare(a: *const u8, b: *const u8, _: *mut ()) -> ComparisonResult {
            // SAFETY: the array holds u8 pointers.
            let (a, b) = unsafe { (*a, *b) };
            if a > b {
                return ComparisonResult::Greater;
            }
            if a == b {
                return ComparisonResult::Equal;
            }
            ComparisonResult::Less
        }
        mutable.sort_values::<u8, ()>(None, Some(compare));

        // SAFETY: the array holds u8 pointers.
        unsafe {
            let ch: &u8 = mutable.as_array().get_value_at_index(0);
            assert_eq!(b'e', *ch);
            let ch: &u8 = mutable.as_array().get_value_at_index(1);
            assert_eq!(b'h', *ch);
        }

        mutable.release();
        arr.release();
    }
}
