//! Port of `src/lib/struct.zig`.
//!
//! STATUS: ported
//!
//! # Usage for later ports
//!
//! Zig's `lib.Struct(target, Zig)` returns `Zig` unchanged for the Zig
//! target and recreates it with an `extern` (C) layout for the C target,
//! widening packed `bool` bit fields to full bytes. Rust cannot rebuild a
//! type by reflection, so the port is the [`lib_struct!`] macro: it
//! declares the native struct and a `#[repr(C)]` companion with the same
//! fields in the same order, plus `From` conversions both ways. Packed
//! Zig structs are normally ported as plain Rust structs (or explicit
//! bit-packed integers, see PORTING.md), so their `bool` fields already
//! occupy a full byte in the C companion, exactly like `Struct(.c, ...)`.
//!
//! ```ignore
//! crate::lib_struct! {
//!     /// Docs.
//!     #[derive(Debug, Clone, Copy, PartialEq)]
//!     pub struct Options / OptionsC {
//!         pub flag: bool,
//!         pub value: u8,
//!     }
//! }
//! ```
//!
//! For C structs with an explicit leading `size` field (ABI-versioned
//! structs) use [`struct_sized_field_fits!`] (Zig: `sizedFieldFits`).

/// Declare a native struct and its C ABI (`#[repr(C)]`) companion with
/// identical fields. See the module documentation.
#[macro_export]
macro_rules! lib_struct {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident / $cname:ident {
            $($(#[$fmeta:meta])* $fvis:vis $field:ident : $ty:ty),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis struct $name {
            $($(#[$fmeta])* $fvis $field: $ty,)*
        }

        $(#[$meta])*
        #[repr(C)]
        $vis struct $cname {
            $($(#[$fmeta])* $fvis $field: $ty,)*
        }

        impl ::core::convert::From<$name> for $cname {
            fn from(v: $name) -> Self {
                $cname { $($field: v.$field,)* }
            }
        }

        impl ::core::convert::From<$cname> for $name {
            fn from(v: $cname) -> Self {
                $name { $($field: v.$field,)* }
            }
        }
    };
}

/// Returns true if a struct with size `size` can set a field that starts
/// at `offset` and is `field_size` bytes long (if it fits within the size).
/// This is used for ABI compatibility for structs that have an explicit
/// size field.
pub const fn sized_field_fits_raw(size: usize, offset: usize, field_size: usize) -> bool {
    size >= offset + field_size
}

/// Size of the pointee type of a raw pointer. Helper for
/// [`struct_sized_field_fits!`].
pub const fn size_of_pointee<T>(_: *const T) -> usize {
    std::mem::size_of::<T>()
}

/// Returns true if a struct of type `T` with size `size` can set
/// field `field` (if it fits within the size). This is used for ABI
/// compatibility for structs that have an explicit size field.
///
/// Zig: `sizedFieldFits(T, size, "field")`.
#[macro_export]
macro_rules! struct_sized_field_fits {
    ($T:ty, $size:expr, $field:ident) => {{
        let offset = ::core::mem::offset_of!($T, $field);
        let uninit = ::core::mem::MaybeUninit::<$T>::uninit();
        let ptr = uninit.as_ptr();
        // SAFETY: `addr_of!` only computes the field address; the
        // uninitialized memory is never read.
        let field_ptr = unsafe { ::core::ptr::addr_of!((*ptr).$field) };
        let field_size = $crate::lib::r#struct::size_of_pointee(field_ptr);
        $crate::lib::r#struct::sized_field_fits_raw($size, offset, field_size)
    }};
}

#[cfg(test)]
mod tests {
    use crate::struct_sized_field_fits;
    use std::mem::{offset_of, size_of};

    #[repr(C)]
    struct Sized {
        size: usize,
        a: u8,
        b: u32,
    }

    #[test]
    fn sized_field_fits_boundary_checks() {
        let size_required = offset_of!(Sized, size) + size_of::<usize>();
        let a_required = offset_of!(Sized, a) + size_of::<u8>();
        let b_required = offset_of!(Sized, b) + size_of::<u32>();

        assert!(struct_sized_field_fits!(Sized, size_required, size));
        assert!(!struct_sized_field_fits!(Sized, size_required - 1, size));

        assert!(struct_sized_field_fits!(Sized, a_required, a));
        assert!(!struct_sized_field_fits!(Sized, a_required - 1, a));

        assert!(struct_sized_field_fits!(Sized, b_required, b));
        assert!(!struct_sized_field_fits!(Sized, b_required - 1, b));
    }

    #[test]
    fn sized_field_fits_respects_alignment_padding() {
        let up_to_padding = offset_of!(Sized, b);
        assert!(struct_sized_field_fits!(Sized, up_to_padding, a));
        assert!(!struct_sized_field_fits!(Sized, up_to_padding, b));
    }

    crate::lib_struct! {
        #[derive(Debug, Clone, Copy, PartialEq)]
        struct Packed / PackedC {
            flag1: bool,
            flag2: bool,
            value: u8,
        }
    }

    #[test]
    fn packed_struct_converts_to_extern_with_full_size_bools() {
        let z = Packed {
            flag1: true,
            flag2: false,
            value: 7,
        };
        let c: PackedC = z.into();
        assert_eq!(1, size_of::<bool>());
        assert_eq!(0, offset_of!(PackedC, flag1));
        assert_eq!(1, offset_of!(PackedC, flag2));
        assert_eq!(2, offset_of!(PackedC, value));
        assert_eq!(3, size_of::<PackedC>());
        assert_eq!(z, Packed::from(c));
    }
}
