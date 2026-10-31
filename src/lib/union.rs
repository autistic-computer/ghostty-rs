//! Port of `src/lib/union.zig`.
//!
//! STATUS: ported
//!
//! # Usage for later ports
//!
//! Zig's `lib.TaggedUnion(target, Union, options)` augments a Zig tagged
//! union with a C ABI compatible `extern struct { tag, value }` whose
//! `value` is an `extern union` of every payload plus a fixed-size
//! `_padding` member that pins the union size for ABI stability.
//!
//! The Rust port is the [`lib_tagged_union!`] macro. Given an existing
//! C tag enum (created with [`crate::lib_enum!`] using the `c` target so
//! values stay stable when tags are added) it declares:
//!
//! * the Rust `enum` (the Zig union),
//! * `CValue`: a `#[repr(C)] union` with one field per tag (named after the
//!   C header field) plus `_padding`,
//! * `C`: a `#[repr(C)] struct { tag, value }`,
//! * `fn tag(&self)`, `fn cval(&self) -> C` and `C::c_field_rename(tag)`,
//! * a compile-time assertion that `CValue` is exactly the padding size.
//!
//! ```ignore
//! crate::lib_enum! { pub enum ActionTag: c { Quit, NewWindow, SetTitle } }
//! crate::lib_tagged_union! {
//!     padding = [u64; 2];
//!     pub enum Action: ActionTag => ActionC, ActionCValue {
//!         Quit => quit,
//!         NewWindow(u32) => new_window as "surface",
//!         SetTitle(*const u8) => set_title,
//!     }
//! }
//! ```
//!
//! Payload types convert to their C representation through [`CRepr`]
//! (Zig: a payload type's `C` decl and `cval()` method); primitives and
//! raw pointers map to themselves, `lib_enum!` C enums implement it too.
//! The Zig-only `Value(tag)` type function has no Rust counterpart: use
//! the enum variant's payload type directly.

/// The C representation of a tagged-union payload. Zig: a payload type may
/// declare `pub const C` and `pub fn cval(self) C`; otherwise the type is
/// used as-is.
pub trait CRepr {
    type C: Copy;
    fn cval(&self) -> Self::C;
}

macro_rules! identity_crepr {
    ($($t:ty),*) => {
        $(impl CRepr for $t {
            type C = $t;
            fn cval(&self) -> $t {
                *self
            }
        })*
    };
}

identity_crepr!(
    (),
    bool,
    u8,
    u16,
    u32,
    u64,
    usize,
    i8,
    i16,
    i32,
    i64,
    isize,
    f32,
    f64
);

impl<T> CRepr for *const T {
    type C = *const T;
    fn cval(&self) -> *const T {
        *self
    }
}

impl<T> CRepr for *mut T {
    type C = *mut T;
    fn cval(&self) -> *mut T {
        *self
    }
}

impl<T: Copy, const N: usize> CRepr for [T; N] {
    type C = [T; N];
    fn cval(&self) -> [T; N] {
        *self
    }
}

/// Create a tagged union with a C ABI and fixed-size C value union. See
/// the module documentation for the syntax.
#[macro_export]
macro_rules! lib_tagged_union {
    (
        padding = $padding:ty;
        $(#[$meta:meta])*
        $vis:vis enum $name:ident : $tag:ident => $cname:ident, $cvalue:ident {
            $(
                $(#[$vmeta:meta])*
                $variant:ident $(($ty:ty))? => $cfield:ident $(as $rename:literal)?
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis enum $name {
            $($(#[$vmeta])* $variant $(($ty))?,)*
        }

        /// The C ABI compatible union value type.
        #[repr(C)]
        #[derive(Clone, Copy)]
        #[allow(non_snake_case)]
        $vis union $cvalue {
            $(pub $cfield: $crate::lib_tagged_union!(@cty $($ty)?),)*
            pub _padding: $padding,
        }

        /// The C ABI compatible tagged union type.
        #[repr(C)]
        #[derive(Clone, Copy)]
        $vis struct $cname {
            pub tag: $tag,
            pub value: $cvalue,
        }

        // For ABI compatibility, we expect that this is our union size.
        const _: () = assert!(
            ::core::mem::size_of::<$cvalue>() == ::core::mem::size_of::<$padding>(),
            "TaggedUnion CValue size does not match expected fixed size"
        );

        impl $cname {
            /// Returns the public C value-union field name for `tag`, or
            /// `None` when the tag has no value field. This is metadata
            /// only; it does not rename fields in the C value union.
            #[allow(unreachable_patterns)]
            pub fn c_field_rename(tag: $tag) -> Option<&'static str> {
                $(
                    if tag == $tag::$variant {
                        return $crate::lib_tagged_union!(@rename $cfield [$($ty)?] [$($rename)?]);
                    }
                )*
                None
            }
        }

        impl $name {
            /// The tag of this value.
            pub fn tag(&self) -> $tag {
                match self {
                    $($crate::lib_tagged_union!(@pat $name $variant [v] $($ty)?) => $tag::$variant,)*
                }
            }

            /// Convert to the C tagged union.
            #[allow(unused_variables)]
            pub fn cval(&self) -> $cname {
                // SAFETY: the C value union is plain old data; an all-zero
                // bit pattern is valid for every field and keeps padding
                // bytes deterministic.
                let mut value: $cvalue = unsafe { ::core::mem::zeroed() };
                match self {
                    $($crate::lib_tagged_union!(@pat $name $variant [v] $($ty)?) => {
                        value.$cfield = $crate::lib_tagged_union!(@conv [v] $($ty)?);
                    })*
                }
                $cname {
                    tag: self.tag(),
                    value,
                }
            }
        }
    };

    (@rename $cfield:ident [$($ty:ty)?] [$rename:literal]) => { Some($rename) };
    (@rename $cfield:ident [] []) => { None };
    (@rename $cfield:ident [$ty:ty] []) => {
        if ::core::mem::size_of::<<$ty as $crate::lib::union::CRepr>::C>() == 0 {
            None
        } else {
            Some(stringify!($cfield))
        }
    };
    (@cty) => { () };
    (@cty $ty:ty) => { <$ty as $crate::lib::union::CRepr>::C };
    (@pat $name:ident $variant:ident [$v:ident]) => { $name::$variant };
    (@pat $name:ident $variant:ident [$v:ident] $ty:ty) => { $name::$variant($v) };
    (@conv [$v:ident]) => { () };
    (@conv [$v:ident] $ty:ty) => { $crate::lib::union::CRepr::cval($v) };
}

#[cfg(test)]
mod tests {
    use crate::lib::r#enum::LibEnum;
    use crate::{lib_enum, lib_tagged_union};

    mod matching {
        crate::lib_enum! { pub enum Tag: c { A, B } }
        crate::lib_tagged_union! {
            padding = u64;
            pub enum U: Tag => UC, UCValue {
                A(u32) => a,
                B(u64) => b,
            }
        }
    }

    #[test]
    fn tagged_union_matching_size() {
        assert_eq!(8, std::mem::size_of::<matching::UCValue>());
    }

    mod padded {
        crate::lib_enum! { pub enum Tag: c { A } }
        crate::lib_tagged_union! {
            padding = u64;
            pub enum U: Tag => UC, UCValue {
                A(u32) => a,
            }
        }
    }

    #[test]
    fn tagged_union_padded_size() {
        assert_eq!(8, std::mem::size_of::<padded::UCValue>());
    }

    #[test]
    fn tagged_union_c_conversion() {
        use matching::*;
        let c = U::A(42).cval();
        assert_eq!(Tag::A, c.tag);
        // SAFETY: the tag says `a` is active.
        assert_eq!(42, unsafe { c.value.a });
    }

    mod custom {
        crate::lib_enum! { pub enum Tag: c { A, B, None } }
        crate::lib_tagged_union! {
            padding = u64;
            pub enum U: Tag => UC, UCValue {
                A(u32) => a as "value",
                B(u32) => b as "value",
                None => none,
            }
        }
    }

    #[test]
    fn tagged_union_custom_c_value_fields() {
        use custom::*;
        // The union keeps one field per tag; renames are metadata only.
        let v: UCValue = unsafe { std::mem::zeroed() };
        let _ = unsafe { (v.a, v.b, v.none) };
        assert_eq!(Some("value"), UC::c_field_rename(Tag::A));
        assert_eq!(None, UC::c_field_rename(Tag::None));

        let a = U::A(42).cval();
        assert_eq!(Tag::A, a.tag);
        assert_eq!(42, unsafe { a.value.a });

        let none = U::None.cval();
        assert_eq!(Tag::None, none.tag);
        assert_eq!(2, none.tag.backing());
    }
}
