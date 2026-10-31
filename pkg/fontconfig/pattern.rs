//! Port of `pkg/fontconfig/pattern.zig`.
//!
//! STATUS: ported

use std::ffi::CStr;
use std::marker::PhantomData;
use std::os::raw::c_int;
use std::ptr::NonNull;

use super::c;
use super::common::{Property, Result};
use super::error::Error;
use super::object_set::ObjectSet;
use super::value::{Value, ValueBinding};

/// Zig: `opaque` used as `*Pattern`. A copyable, non-null handle to an
/// `FcPattern`; it must be released explicitly with [`Pattern::destroy`]
/// (unless ownership was passed to fontconfig, e.g. via `FontSet::add`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pattern(NonNull<c::FcPattern>);

impl Pattern {
    pub fn create() -> Pattern {
        // SAFETY: no preconditions.
        Pattern(NonNull::new(unsafe { c::FcPatternCreate() }).expect("FcPatternCreate failed"))
    }

    pub fn parse(str: &CStr) -> Pattern {
        // SAFETY: NUL-terminated string.
        Pattern(
            NonNull::new(unsafe { c::FcNameParse(str.as_ptr() as *const c::FcChar8) })
                .expect("FcNameParse failed"),
        )
    }

    /// # Safety
    /// The handle (and its copies) must not be used afterwards.
    pub unsafe fn destroy(self) {
        c::FcPatternDestroy(self.cval());
    }

    pub fn default_substitute(self) {
        // SAFETY: valid handle.
        unsafe { c::FcDefaultSubstitute(self.cval()) }
    }

    pub fn add(self, prop: Property, value: Value<'_>, append: bool) -> bool {
        // SAFETY: valid handle; fontconfig copies the value.
        unsafe {
            c::FcPatternAdd(
                self.cval(),
                prop.cval().as_ptr(),
                value.cval(),
                if append { c::FcTrue } else { c::FcFalse } as c::FcBool,
            ) == c::FcTrue as c::FcBool
        }
    }

    pub fn get(&self, prop: Property, id: u32) -> std::result::Result<Value<'_>, Error> {
        // SAFETY: valid handle and out pointer; on success the value points
        // into the pattern.
        unsafe {
            let mut val: c::_FcValue = std::mem::zeroed();
            Result::from_raw(c::FcPatternGet(
                self.cval(),
                prop.cval().as_ptr(),
                id as c_int,
                &mut val,
            ))
            .to_error()?;

            Ok(Value::init(&val))
        }
    }

    pub fn delete(self, prop: Property) -> bool {
        // SAFETY: valid handle.
        unsafe { c::FcPatternDel(self.cval(), prop.cval().as_ptr()) == c::FcTrue as c::FcBool }
    }

    pub fn filter(self, os: ObjectSet) -> Pattern {
        // SAFETY: valid handles.
        Pattern(
            NonNull::new(unsafe { c::FcPatternFilter(self.cval(), os.cval()) })
                .expect("FcPatternFilter failed"),
        )
    }

    pub fn object_iterator(&self) -> ObjectIterator<'_> {
        ObjectIterator {
            pat: self.cval(),
            iter: None,
            _marker: PhantomData,
        }
    }

    pub fn print(self) {
        // SAFETY: valid handle.
        unsafe { c::FcPatternPrint(self.cval()) }
    }

    /// Wraps a raw, non-null `FcPattern` pointer.
    ///
    /// # Safety
    /// `ptr` must be a valid `FcPattern`.
    pub unsafe fn from_raw(ptr: *mut c::FcPattern) -> Pattern {
        Pattern(NonNull::new(ptr).expect("null FcPattern"))
    }

    #[inline]
    pub fn cval(self) -> *mut c::_FcPattern {
        self.0.as_ptr()
    }
}

/// Zig: `Pattern.ObjectIterator`.
pub struct ObjectIterator<'a> {
    pat: *mut c::_FcPattern,
    iter: Option<c::_FcPatternIter>,
    _marker: PhantomData<&'a Pattern>,
}

impl<'a> ObjectIterator<'a> {
    /// Move to the next object, returns true if there is another
    /// object and false otherwise. If this is the first call, this
    /// will be the first object.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> bool {
        // SAFETY: valid pattern handle and iterator storage.
        unsafe {
            // Null means our first iterator
            let Some(iter) = self.iter.as_mut() else {
                // If we have no objects, do not create iterator
                if c::FcPatternObjectCount(self.pat) == 0 {
                    return false;
                }

                let mut iter: c::_FcPatternIter = std::mem::zeroed();
                c::FcPatternIterStart(self.pat, &mut iter);
                debug_assert!(
                    c::FcPatternIterIsValid(self.pat, &mut iter) == c::FcTrue as c::FcBool
                );
                self.iter = Some(iter);

                // Return right away because the fontconfig iterator pattern
                // is do/while.
                return true;
            };

            c::FcPatternIterNext(self.pat, iter) == c::FcTrue as c::FcBool
        }
    }

    pub fn object(&mut self) -> &'a [u8] {
        let iter = self.iter.as_mut().expect("iterator not started");
        // SAFETY: valid pattern and iterator; the object name is owned by
        // fontconfig and outlives the pattern.
        unsafe { CStr::from_ptr(c::FcPatternIterGetObject(self.pat, iter)).to_bytes() }
    }

    pub fn value_len(&mut self) -> usize {
        let iter = self.iter.as_mut().expect("iterator not started");
        // SAFETY: valid pattern and iterator.
        unsafe { c::FcPatternIterValueCount(self.pat, iter) as usize }
    }

    pub fn value_iterator(&mut self) -> ValueIterator<'_> {
        let pat = self.pat;
        let iter = self.iter.as_mut().expect("iterator not started");
        // SAFETY: valid pattern and iterator.
        let max = unsafe { c::FcPatternIterValueCount(pat, iter) };
        ValueIterator {
            pat,
            iter,
            max,
            id: 0,
        }
    }
}

/// Zig: `Pattern.ValueIterator`.
pub struct ValueIterator<'a> {
    pat: *mut c::_FcPattern,
    iter: &'a mut c::_FcPatternIter,
    max: c_int,
    id: c_int,
}

/// Zig: `Pattern.ValueIterator.Entry`.
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    pub result: Result,
    pub value: Value<'a>,
    pub binding: ValueBinding,
}

impl<'a> ValueIterator<'a> {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Entry<'_>> {
        if self.id >= self.max {
            return None;
        }
        // SAFETY: valid pattern and iterator; id < value count.
        unsafe {
            let mut value: c::_FcValue = std::mem::zeroed();
            let mut binding: c::FcValueBinding = 0;
            let result =
                c::FcPatternIterGetValue(self.pat, self.iter, self.id, &mut value, &mut binding);
            self.id += 1;

            Some(Entry {
                result: Result::from_raw(result),
                binding: ValueBinding::from_raw(binding),
                value: Value::init(&value),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::common::Weight;
    use super::*;

    #[test]
    fn create() {
        let pat = Pattern::create();

        assert!(pat.add(Property::Family, Value::String(c"monospace"), false));
        assert!(pat.add(Property::Weight, Value::Integer(Weight::Bold as i32), false));

        {
            let val = pat.get(Property::Family, 0).unwrap();
            let Value::String(s) = val else {
                panic!("expected string, got {val:?}");
            };
            assert_eq!(c"monospace", s);
        }

        // SAFETY: not used afterwards.
        unsafe { pat.destroy() };
    }

    #[test]
    fn name_parse() {
        let pat = Pattern::parse(c":monospace");
        pat.default_substitute();
        // SAFETY: not used afterwards.
        unsafe { pat.destroy() };
    }
}
