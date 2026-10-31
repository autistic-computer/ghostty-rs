//! Port of `src/macos/foundation/number.zig`.
//!
//! STATUS: ported

use std::ffi::c_void;
use std::ptr;

use super::c;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CFNumberRef`
    Number
);

impl Number {
    /// Create a number. `T` must be the value type of `type_` (see
    /// [`NumberType::value_size`]); this is a comptime check in Zig and an
    /// assertion here.
    pub fn create<T: Copy>(type_: NumberType, value: &T) -> Result<&'static Number, Error> {
        assert_eq!(type_.value_size(), Some(std::mem::size_of::<T>()));
        // SAFETY: value points to a T of the size CF expects for type_.
        unsafe {
            created(c::CFNumberCreate(
                ptr::null(),
                type_ as c::CFNumberType,
                value as *const T as *const c_void,
            ))
        }
    }

    pub fn get_value<T: Copy>(&self, t: NumberType, ptr: &mut T) -> bool {
        assert_eq!(t.value_size(), Some(std::mem::size_of::<T>()));
        // SAFETY: ptr points to a T of the size CF writes for t.
        unsafe {
            c::CFNumberGetValue(
                self.as_ptr(),
                t as c::CFNumberType,
                ptr as *mut T as *mut c_void,
            ) == 1
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }
}

#[repr(isize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberType {
    Sint8 = c::kCFNumberSInt8Type,
    Sint16 = c::kCFNumberSInt16Type,
    Sint32 = c::kCFNumberSInt32Type,
    Sint64 = c::kCFNumberSInt64Type,
    Float32 = c::kCFNumberFloat32Type,
    Float64 = c::kCFNumberFloat64Type,
    Char = c::kCFNumberCharType,
    Short = c::kCFNumberShortType,
    Int = c::kCFNumberIntType,
    Long = c::kCFNumberLongType,
    LongLong = c::kCFNumberLongLongType,
    Float = c::kCFNumberFloatType,
    Double = c::kCFNumberDoubleType,
    CfIndex = c::kCFNumberCFIndexType,
    NsInteger = c::kCFNumberNSIntegerType,
    CgFloat = c::kCFNumberCGFloatType,
}

impl NumberType {
    /// The size of the Zig `ValueType()` for this number type (`None` for
    /// the types the Zig code leaves as `unreachable // TODO`).
    pub fn value_size(self) -> Option<usize> {
        use std::ffi::{c_int, c_long, c_longlong, c_short};
        use std::mem::size_of;
        Some(match self {
            NumberType::Sint8 => size_of::<i8>(),
            NumberType::Sint16 => size_of::<i16>(),
            NumberType::Sint32 => size_of::<i32>(),
            NumberType::Sint64 => size_of::<i64>(),
            NumberType::Float32 => size_of::<f32>(),
            NumberType::Float64 => size_of::<f64>(),
            NumberType::Char => size_of::<u8>(),
            NumberType::Short => size_of::<c_short>(),
            NumberType::Int => size_of::<c_int>(),
            NumberType::Long => size_of::<c_long>(),
            NumberType::LongLong => size_of::<c_longlong>(),
            NumberType::Float => size_of::<f32>(),
            NumberType::Double => size_of::<f64>(),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number() {
        let inner: i8 = 42;
        let v = Number::create(NumberType::Sint8, &inner).unwrap();

        let mut result: i8 = 0;
        assert!(v.get_value(NumberType::Sint8, &mut result));
        assert_eq!(result, inner);
        v.release();
    }
}
