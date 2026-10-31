//! Port of `src/datastruct/comparison.zig`.
//!
//! STATUS: ported
//!
//! The contents of this file is largely based on testing.zig from the Zig 0.15.1
//! stdlib, distributed under the MIT license, copyright (c) Zig contributors
//!
//! The Zig version uses comptime reflection to walk arbitrary types. The
//! Rust port expresses the same per-type rules as traits:
//!
//! * [`DeepEqual`] — implemented here for the primitive and container
//!   types the Zig switch handles; structs/enums implement it by hand
//!   (the Zig "`equal` decl"), or field-by-field with
//!   [`impl_deep_equal_fields!`](crate::impl_deep_equal_fields), or via
//!   `PartialEq` with [`impl_deep_equal_via_eq!`](crate::impl_deep_equal_via_eq).
//! * [`ApproxEqual`] — the same for `expect_approx_equal`.

use std::ffi::{CStr, CString};
use std::fmt::Debug;

/// A deep equality comparison function that works for most types. We
/// add types as necessary. It defers to `deep_equal` impls on types.
pub fn deep_equal<T: DeepEqual + ?Sized>(old: &T, new: &T) -> bool {
    old.deep_equal(new)
}

/// Types comparable with [`deep_equal`].
pub trait DeepEqual {
    fn deep_equal(&self, other: &Self) -> bool;
}

macro_rules! deep_equal_by_eq {
    ($($t:ty),* $(,)?) => {
        $(impl DeepEqual for $t {
            #[inline]
            #[allow(clippy::float_cmp)]
            fn deep_equal(&self, other: &Self) -> bool {
                self == other
            }
        })*
    };
}

// .void, .bool, .int, .float
deep_equal_by_eq!(
    (),
    bool,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    f32,
    f64,
    char,
    str,
    String,
    CStr,
    CString,
);

// []const T, [N]T, Vec<T>
impl<T: DeepEqual> DeepEqual for [T] {
    fn deep_equal(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        self.iter().zip(other.iter()).all(|(a, b)| a.deep_equal(b))
    }
}

impl<T: DeepEqual, const N: usize> DeepEqual for [T; N] {
    fn deep_equal(&self, other: &Self) -> bool {
        self.as_slice().deep_equal(other.as_slice())
    }
}

impl<T: DeepEqual> DeepEqual for Vec<T> {
    fn deep_equal(&self, other: &Self) -> bool {
        self.as_slice().deep_equal(other.as_slice())
    }
}

// .optional
impl<T: DeepEqual> DeepEqual for Option<T> {
    fn deep_equal(&self, other: &Self) -> bool {
        match (self, other) {
            (None, None) => true,
            (Some(a), Some(b)) => a.deep_equal(b),
            _ => false,
        }
    }
}

impl<T: DeepEqual + ?Sized> DeepEqual for &T {
    fn deep_equal(&self, other: &Self) -> bool {
        (**self).deep_equal(*other)
    }
}

impl<T: DeepEqual + ?Sized> DeepEqual for Box<T> {
    fn deep_equal(&self, other: &Self) -> bool {
        (**self).deep_equal(&**other)
    }
}

/// Implement [`DeepEqual`] for a struct as a recursive field-by-field
/// compare (the Zig fallback for structs without an `equal` decl).
#[macro_export]
macro_rules! impl_deep_equal_fields {
    ($t:ty { $($field:ident),* $(,)? }) => {
        impl $crate::datastruct::comparison::DeepEqual for $t {
            fn deep_equal(&self, other: &Self) -> bool {
                true $(&& $crate::datastruct::comparison::DeepEqual::deep_equal(&self.$field, &other.$field))*
            }
        }
    };
}

/// Implement [`DeepEqual`] using `PartialEq` (useful for enums whose
/// variants are all plain values).
#[macro_export]
macro_rules! impl_deep_equal_via_eq {
    ($($t:ty),* $(,)?) => {
        $(impl $crate::datastruct::comparison::DeepEqual for $t {
            fn deep_equal(&self, other: &Self) -> bool {
                self == other
            }
        })*
    };
}

/// Error returned by [`expect_approx_equal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApproxEqualError {
    TestExpectedApproxEqual,
}

impl std::fmt::Display for ApproxEqualError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TestExpectedApproxEqual")
    }
}

impl std::error::Error for ApproxEqualError {}

/// Types comparable with [`expect_approx_equal`]: approximate comparison
/// for floats and equality for everything else.
pub trait ApproxEqual: Debug {
    fn expect_approx_equal_inner(&self, actual: &Self) -> Result<(), ApproxEqualError>;
}

/// Generic, recursive equality testing utility using approximate comparison for
/// floats and equality for everything else
///
/// Based on `testing.expectEqual` and `testing.expectEqualSlices`.
///
/// The relative tolerance is currently hardcoded to `sqrt(eps(float_type))`.
pub fn expect_approx_equal<T: ApproxEqual + ?Sized>(
    expected: &T,
    actual: &T,
) -> Result<(), ApproxEqualError> {
    expected.expect_approx_equal_inner(actual)
}

/// Zig's `std.math.approxEqRel`.
fn approx_eq_rel_f64(x: f64, y: f64, tolerance: f64) -> bool {
    debug_assert!(tolerance > 0.0);
    // Fast path for equal values (and signed zeros and infinites).
    if x == y {
        return true;
    }
    if x.is_nan() || y.is_nan() {
        return false;
    }
    (x - y).abs() <= x.abs().max(y.abs()) * tolerance
}

fn approx_eq_rel_f32(x: f32, y: f32, tolerance: f32) -> bool {
    debug_assert!(tolerance > 0.0);
    if x == y {
        return true;
    }
    if x.is_nan() || y.is_nan() {
        return false;
    }
    (x - y).abs() <= x.abs().max(y.abs()) * tolerance
}

// check approximate equality for floats
impl ApproxEqual for f32 {
    fn expect_approx_equal_inner(&self, actual: &Self) -> Result<(), ApproxEqualError> {
        let sqrt_eps = f32::EPSILON.sqrt();
        if !approx_eq_rel_f32(*self, *actual, sqrt_eps) {
            print(format_args!(
                "expected approximately {:?}, found {:?}\n",
                self, actual
            ));
            return Err(ApproxEqualError::TestExpectedApproxEqual);
        }
        Ok(())
    }
}

impl ApproxEqual for f64 {
    fn expect_approx_equal_inner(&self, actual: &Self) -> Result<(), ApproxEqualError> {
        let sqrt_eps = f64::EPSILON.sqrt();
        if !approx_eq_rel_f64(*self, *actual, sqrt_eps) {
            print(format_args!(
                "expected approximately {:?}, found {:?}\n",
                self, actual
            ));
            return Err(ApproxEqualError::TestExpectedApproxEqual);
        }
        Ok(())
    }
}

macro_rules! approx_equal_by_eq {
    ($($t:ty),* $(,)?) => {
        $(impl ApproxEqual for $t {
            fn expect_approx_equal_inner(&self, actual: &Self) -> Result<(), ApproxEqualError> {
                // fall back to expectEqual for everything else
                if self != actual {
                    print(format_args!("expected {:?}, found {:?}\n", self, actual));
                    return Err(ApproxEqualError::TestExpectedApproxEqual);
                }
                Ok(())
            }
        })*
    };
}

approx_equal_by_eq!(
    (),
    bool,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    char,
    str,
    String,
);

// recurse into containers
impl<T: ApproxEqual> ApproxEqual for [T] {
    fn expect_approx_equal_inner(&self, actual: &Self) -> Result<(), ApproxEqualError> {
        let diff_index: usize = 'diff_index: {
            let shortest = self.len().min(actual.len());
            let mut index: usize = 0;
            while index < shortest {
                if expect_approx_equal(&actual[index], &self[index]).is_err() {
                    break 'diff_index index;
                }
                index += 1;
            }
            if self.len() == actual.len() {
                return Ok(());
            }
            shortest
        };
        print(format_args!(
            "slices not approximately equal. first significant difference occurs at index {} (0x{:X})\n",
            diff_index, diff_index
        ));
        Err(ApproxEqualError::TestExpectedApproxEqual)
    }
}

// .array and .vector (Zig vectors are arrays in Rust)
impl<T: ApproxEqual, const N: usize> ApproxEqual for [T; N] {
    fn expect_approx_equal_inner(&self, actual: &Self) -> Result<(), ApproxEqualError> {
        self.as_slice().expect_approx_equal_inner(actual.as_slice())
    }
}

impl<T: ApproxEqual> ApproxEqual for Vec<T> {
    fn expect_approx_equal_inner(&self, actual: &Self) -> Result<(), ApproxEqualError> {
        self.as_slice().expect_approx_equal_inner(actual.as_slice())
    }
}

// unwrap optionals
impl<T: ApproxEqual> ApproxEqual for Option<T> {
    fn expect_approx_equal_inner(&self, actual: &Self) -> Result<(), ApproxEqualError> {
        match (self, actual) {
            (Some(e), Some(a)) => expect_approx_equal(e, a),
            (None, None) => Ok(()),
            // we only reach this point if there's at least one null,
            // in which case expectEqual is all we need
            _ => {
                print(format_args!("expected {:?}, found {:?}\n", self, actual));
                Err(ApproxEqualError::TestExpectedApproxEqual)
            }
        }
    }
}

impl<T: ApproxEqual + ?Sized> ApproxEqual for &T {
    fn expect_approx_equal_inner(&self, actual: &Self) -> Result<(), ApproxEqualError> {
        (**self).expect_approx_equal_inner(*actual)
    }
}

// Anonymous structs (tuples): field by field.
macro_rules! approx_equal_tuple {
    ($($name:ident $idx:tt),+) => {
        impl<$($name: ApproxEqual),+> ApproxEqual for ($($name,)+) {
            fn expect_approx_equal_inner(&self, actual: &Self) -> Result<(), ApproxEqualError> {
                $(expect_approx_equal(&self.$idx, &actual.$idx)?;)+
                Ok(())
            }
        }
    };
}

approx_equal_tuple!(A 0);
approx_equal_tuple!(A 0, B 1);
approx_equal_tuple!(A 0, B 1, C 2);
approx_equal_tuple!(A 0, B 1, C 2, D 3);
approx_equal_tuple!(A 0, B 1, C 2, D 3, E 4);
approx_equal_tuple!(A 0, B 1, C 2, D 3, E 4, F 5);

/// Implement [`ApproxEqual`] for a struct as a recursive field-by-field
/// approximate compare (the Zig `.struct` case).
#[macro_export]
macro_rules! impl_approx_equal_fields {
    ($t:ty { $($field:ident),* $(,)? }) => {
        impl $crate::datastruct::comparison::ApproxEqual for $t {
            fn expect_approx_equal_inner(
                &self,
                actual: &Self,
            ) -> Result<(), $crate::datastruct::comparison::ApproxEqualError> {
                $($crate::datastruct::comparison::expect_approx_equal(&self.$field, &actual.$field)?;)*
                Ok(())
            }
        }
    };
}

/// Copy of testing.print (not public)
fn print(args: std::fmt::Arguments<'_>) {
    eprint!("{}", args);
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests based on the `expectEqual` tests in the Zig stdlib
    #[test]
    fn expect_approx_equal_union_enum() {
        #[derive(Debug)]
        enum T {
            #[allow(dead_code)]
            A(i32),
            B(f32),
        }
        impl ApproxEqual for T {
            fn expect_approx_equal_inner(&self, actual: &Self) -> Result<(), ApproxEqualError> {
                // tags must be equal, then compare the payloads
                match (self, actual) {
                    (T::A(e), T::A(a)) => expect_approx_equal(e, a),
                    (T::B(e), T::B(a)) => expect_approx_equal(e, a),
                    _ => Err(ApproxEqualError::TestExpectedApproxEqual),
                }
            }
        }

        let b10 = T::B(10.0);
        let b10plus = T::B(10.000001);

        expect_approx_equal(&b10, &b10plus).unwrap();
    }

    #[test]
    fn expect_approx_equal_nested_array() {
        let a: [[f32; 2]; 2] = [[1.0, 0.0], [0.0, 1.0]];

        let b: [[f32; 2]; 2] = [[1.000001, 0.0], [0.0, 0.999999]];

        expect_approx_equal(&a, &b).unwrap();
    }

    #[test]
    fn expect_approx_equal_vector() {
        let a: [f32; 4] = [4.0; 4];
        let b: [f32; 4] = [4.000001; 4];

        expect_approx_equal(&a, &b).unwrap();
    }

    #[test]
    fn expect_approx_equal_struct() {
        let a = (1, 1.0f32);
        let b = (1, 0.999999f32);

        expect_approx_equal(&a, &b).unwrap();
    }

    #[test]
    fn deep_equal_void() {
        assert!(deep_equal(&(), &()));
    }

    #[test]
    fn deep_equal_bool() {
        assert!(deep_equal(&true, &true));
        assert!(deep_equal(&false, &false));
        assert!(!deep_equal(&true, &false));
        assert!(!deep_equal(&false, &true));
    }

    #[test]
    fn deep_equal_int() {
        assert!(deep_equal::<i32>(&42, &42));
        assert!(deep_equal::<i32>(&-100, &-100));
        assert!(!deep_equal::<i32>(&42, &43));
        assert!(deep_equal::<u64>(&0, &0));
        assert!(!deep_equal::<u64>(&0, &1));
    }

    #[test]
    fn deep_equal_float() {
        assert!(deep_equal::<f32>(&1.0, &1.0));
        assert!(!deep_equal::<f32>(&1.0, &1.1));
        assert!(deep_equal::<f64>(&3.14159, &3.14159));
        assert!(!deep_equal::<f64>(&3.14159, &3.14158));
    }

    #[test]
    fn deep_equal_enum() {
        #[derive(Debug, PartialEq)]
        #[allow(dead_code)]
        enum Color {
            Red,
            Green,
            Blue,
        }
        crate::impl_deep_equal_via_eq!(Color);
        assert!(deep_equal(&Color::Red, &Color::Red));
        assert!(deep_equal(&Color::Blue, &Color::Blue));
        assert!(!deep_equal(&Color::Red, &Color::Green));
        assert!(!deep_equal(&Color::Green, &Color::Blue));
    }

    #[test]
    fn deep_equal_const_u8_slice() {
        let f = deep_equal::<[u8]>;
        assert!(f(b"hello", b"hello"));
        assert!(f(b"", b""));
        assert!(!f(b"hello", b"world"));
        assert!(!f(b"hello", b"hell"));
        assert!(!f(b"hello", b"hello!"));
    }

    #[test]
    fn deep_equal_sentinel_u8_slice() {
        let f = deep_equal::<CStr>;
        assert!(f(c"foo", c"foo"));
        assert!(!f(c"foo", c"bar"));
        assert!(!f(c"foo", c"fo"));
    }

    #[test]
    fn deep_equal_slice_of_sentinel_slices() {
        let a: &[&CStr] = &[c"one", c"two", c"three"];
        let b: &[&CStr] = &[c"one", c"two", c"three"];
        let c: &[&CStr] = &[c"one", c"two"];
        let d: &[&CStr] = &[c"one", c"two", c"four"];
        let e: &[&CStr] = &[];

        assert!(deep_equal(a, b));
        assert!(!deep_equal(a, c));
        assert!(!deep_equal(a, d));
        assert!(deep_equal(e, e));
        assert!(!deep_equal(a, e));
    }

    #[test]
    fn deep_equal_optional() {
        assert!(deep_equal::<Option<i32>>(&None, &None));
        assert!(deep_equal(&Some(42), &Some(42)));
        assert!(!deep_equal(&None, &Some(42)));
        assert!(!deep_equal(&Some(42), &None));
        assert!(!deep_equal(&Some(42), &Some(43)));
    }

    #[derive(Debug)]
    struct Nested {
        x: i32,
        y: i32,
    }
    crate::impl_deep_equal_fields!(Nested { x, y });

    #[test]
    fn deep_equal_optional_nested() {
        assert!(deep_equal::<Option<Nested>>(&None, &None));
        assert!(deep_equal(
            &Some(Nested { x: 1, y: 2 }),
            &Some(Nested { x: 1, y: 2 })
        ));
        assert!(!deep_equal(
            &Some(Nested { x: 1, y: 2 }),
            &Some(Nested { x: 1, y: 3 })
        ));
        assert!(!deep_equal(&Some(Nested { x: 1, y: 2 }), &None));
    }

    #[test]
    fn deep_equal_array() {
        assert!(deep_equal::<[i32; 3]>(&[1, 2, 3], &[1, 2, 3]));
        assert!(!deep_equal::<[i32; 3]>(&[1, 2, 3], &[1, 2, 4]));
        assert!(!deep_equal::<[i32; 3]>(&[1, 2, 3], &[0, 2, 3]));
        assert!(deep_equal::<[i32; 0]>(&[], &[]));
    }

    #[test]
    fn deep_equal_nested_array() {
        let a: [[i32; 2]; 2] = [[1, 2], [3, 4]];
        let b: [[i32; 2]; 2] = [[1, 2], [3, 4]];
        let c: [[i32; 2]; 2] = [[1, 2], [3, 5]];

        assert!(deep_equal(&a, &b));
        assert!(!deep_equal(&a, &c));
    }

    #[test]
    fn deep_equal_struct() {
        struct Point {
            x: i32,
            y: i32,
        }
        crate::impl_deep_equal_fields!(Point { x, y });
        assert!(deep_equal(&Point { x: 10, y: 20 }, &Point { x: 10, y: 20 }));
        assert!(!deep_equal(
            &Point { x: 10, y: 20 },
            &Point { x: 10, y: 21 }
        ));
        assert!(!deep_equal(
            &Point { x: 10, y: 20 },
            &Point { x: 11, y: 20 }
        ));
    }

    #[test]
    fn deep_equal_struct_nested() {
        struct Inner {
            value: i32,
        }
        crate::impl_deep_equal_fields!(Inner { value });
        struct Outer {
            a: Inner,
            b: Inner,
        }
        crate::impl_deep_equal_fields!(Outer { a, b });

        let x = Outer {
            a: Inner { value: 1 },
            b: Inner { value: 2 },
        };
        let y = Outer {
            a: Inner { value: 1 },
            b: Inner { value: 2 },
        };
        let z = Outer {
            a: Inner { value: 1 },
            b: Inner { value: 3 },
        };

        assert!(deep_equal(&x, &y));
        assert!(!deep_equal(&x, &z));
    }

    #[test]
    fn deep_equal_struct_with_equal_decl() {
        struct Custom {
            value: i32,
        }
        impl DeepEqual for Custom {
            fn deep_equal(&self, other: &Self) -> bool {
                self.value.rem_euclid(10) == other.value.rem_euclid(10)
            }
        }

        assert!(deep_equal(&Custom { value: 5 }, &Custom { value: 15 }));
        assert!(deep_equal(&Custom { value: 100 }, &Custom { value: 0 }));
        assert!(!deep_equal(&Custom { value: 5 }, &Custom { value: 6 }));
    }

    #[test]
    fn deep_equal_union() {
        enum Value {
            Int(i32),
            Float(f32),
            None,
        }
        // Tagged union fallback: tags must match, then compare payloads.
        impl DeepEqual for Value {
            fn deep_equal(&self, other: &Self) -> bool {
                match (self, other) {
                    (Value::Int(a), Value::Int(b)) => deep_equal(a, b),
                    (Value::Float(a), Value::Float(b)) => deep_equal(a, b),
                    (Value::None, Value::None) => true,
                    _ => false,
                }
            }
        }

        assert!(deep_equal(&Value::Int(42), &Value::Int(42)));
        assert!(!deep_equal(&Value::Int(42), &Value::Int(43)));
        assert!(!deep_equal(&Value::Int(42), &Value::Float(42.0)));
        assert!(deep_equal(&Value::None, &Value::None));
        assert!(!deep_equal(&Value::None, &Value::Int(0)));
    }

    #[test]
    fn deep_equal_union_with_equal_decl() {
        enum Value {
            Num(i32),
            Str(&'static [u8]),
        }
        impl DeepEqual for Value {
            fn deep_equal(&self, other: &Self) -> bool {
                match self {
                    Value::Num(n) => match other {
                        Value::Num(m) => n.rem_euclid(10) == m.rem_euclid(10),
                        _ => false,
                    },
                    Value::Str(s) => match other {
                        Value::Str(t) => s.len() == t.len(),
                        _ => false,
                    },
                }
            }
        }

        assert!(deep_equal(&Value::Num(5), &Value::Num(25)));
        assert!(!deep_equal(&Value::Num(5), &Value::Num(6)));
        assert!(deep_equal(&Value::Str(b"abc"), &Value::Str(b"xyz")));
        assert!(!deep_equal(&Value::Str(b"abc"), &Value::Str(b"ab")));
    }

    #[test]
    fn deep_equal_array_of_structs() {
        struct Item {
            id: i32,
            name: &'static [u8],
        }
        crate::impl_deep_equal_fields!(Item { id, name });
        let a = [
            Item {
                id: 1,
                name: b"one",
            },
            Item {
                id: 2,
                name: b"two",
            },
        ];
        let b = [
            Item {
                id: 1,
                name: b"one",
            },
            Item {
                id: 2,
                name: b"two",
            },
        ];
        let c = [
            Item {
                id: 1,
                name: b"one",
            },
            Item {
                id: 2,
                name: b"TWO",
            },
        ];

        assert!(deep_equal(&a, &b));
        assert!(!deep_equal(&a, &c));
    }

    #[test]
    fn deep_equal_struct_with_optional_field() {
        struct Config {
            name: &'static [u8],
            port: Option<u16>,
        }
        crate::impl_deep_equal_fields!(Config { name, port });

        let cfg = |name: &'static [u8], port| Config { name, port };
        assert!(deep_equal(
            &cfg(b"app", Some(8080)),
            &cfg(b"app", Some(8080))
        ));
        assert!(deep_equal(&cfg(b"app", None), &cfg(b"app", None)));
        assert!(!deep_equal(&cfg(b"app", Some(8080)), &cfg(b"app", None)));
        assert!(!deep_equal(
            &cfg(b"app", Some(8080)),
            &cfg(b"app", Some(8081))
        ));
    }

    #[test]
    fn deep_equal_struct_with_array_field() {
        struct Data {
            values: [i32; 3],
        }
        crate::impl_deep_equal_fields!(Data { values });

        assert!(deep_equal(
            &Data { values: [1, 2, 3] },
            &Data { values: [1, 2, 3] }
        ));
        assert!(!deep_equal(
            &Data { values: [1, 2, 3] },
            &Data { values: [1, 2, 4] }
        ));
    }
}
