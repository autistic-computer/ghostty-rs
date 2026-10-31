//! Port of `src/lib/allocator/wasm.zig`.
//!
//! STATUS: ported
//!
//! Wasm allocation conveniences for caller-owned storage and opaque out slots.
//!
//! The primary use case for this is Wasm builds. Ghostty relies a lot on
//! pointers to various types for ABI compatibility and creating those pointers
//! in Wasm is tedious. This file contains the small set of functions exposed by
//! the Wasm module without changing the API from the C library.
//!
//! Given these are convenience methods, they always use the default allocator.
//! If a caller is using a custom allocator, they have the expertise to
//! allocate these types manually using their custom allocator.

use std::ffi::c_void;

use super::{default, ZigAllocator};
use crate::lib::c_abi;

/// log2 of `c_abi::MAX_ALIGNMENT` (Zig: `std.mem.Alignment.fromByteUnits`).
const WASM_ALIGNMENT: u8 = c_abi::MAX_ALIGNMENT.trailing_zeros() as u8;

/// A nullable opaque C handle stored in a constructor out-parameter slot.
pub type Opaque = *mut c_void;

/// Allocate `len` bytes of uninitialized, caller-owned Wasm ABI storage.
///
/// The returned pointer is aligned for any fundamental C ABI type and must be
/// released with `free_bytes` using the same `len`. Returns null when `len` is
/// zero or allocation fails.
pub extern "C" fn alloc_bytes(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::null_mut();
    }
    default(None).alloc(len, WASM_ALIGNMENT, 0)
}

/// Release storage returned by `alloc_bytes`.
///
/// `len` must exactly match the allocation length. A null pointer is ignored.
pub extern "C" fn free_bytes(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    if len == 0 {
        return;
    }
    default(None).free(ptr, len, WASM_ALIGNMENT, 0);
}

/// Allocate a null-initialized slot for an opaque constructor out-parameter.
///
/// The slot may be reused after each value is removed with `take_opaque`. It
/// must eventually be released with `free_opaque`.
pub extern "C" fn alloc_opaque() -> *mut Opaque {
    let ptr = alloc_bytes(std::mem::size_of::<Opaque>());
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    let result = ptr as *mut Opaque;
    // SAFETY: freshly allocated, suitably aligned storage for one pointer.
    unsafe { result.write(std::ptr::null_mut()) };
    result
}

/// Release a slot returned by `alloc_opaque` without freeing its stored handle.
///
/// Call the handle's type-specific destructor before freeing a populated slot.
/// A null slot pointer is ignored.
pub extern "C" fn free_opaque(ptr: *mut Opaque) {
    free_bytes(ptr as *mut u8, std::mem::size_of::<Opaque>());
}

/// Remove and return the handle in an opaque out-parameter slot.
///
/// The slot is reset to null so it can be safely reused for another
/// constructor. This does not free either the handle or the slot. Returns null
/// when the slot pointer is null or the slot is empty.
pub extern "C" fn take_opaque(ptr: *mut Opaque) -> Opaque {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: caller passes a slot from `alloc_opaque`.
    unsafe {
        let result = *ptr;
        *ptr = std::ptr::null_mut();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_allocation() {
        let ptr = alloc_bytes(1);
        assert!(!ptr.is_null());

        assert_eq!(0usize, ptr as usize % c_abi::MAX_ALIGNMENT as usize);
        // SAFETY: one byte was allocated.
        unsafe {
            *ptr = 42;
            assert_eq!(42u8, *ptr);
        }

        assert!(alloc_bytes(0).is_null());
        free_bytes(std::ptr::null_mut(), 0);
        free_bytes(ptr, 1);
    }

    #[test]
    fn opaque_slots_are_initialized_and_reusable() {
        let slot = alloc_opaque();
        assert!(!slot.is_null());

        assert!(take_opaque(slot).is_null());

        let mut value: u8 = 42;
        let value_ptr = &mut value as *mut u8 as *mut c_void;
        // SAFETY: slot is a valid opaque slot.
        unsafe { *slot = value_ptr };
        assert_eq!(value_ptr, take_opaque(slot));
        assert!(take_opaque(slot).is_null());
        free_opaque(slot);
    }
}
