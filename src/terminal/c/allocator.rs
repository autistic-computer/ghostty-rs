//! Port of `src/terminal/c/allocator.zig`.
//!
//! STATUS: ported

use crate::lib::allocator::{self as lib_alloc, Allocator as CAllocator, ZigAllocator};

/// Allocate a buffer of `len` bytes using the given allocator
/// (or the default allocator if NULL).
///
/// Returns a pointer to the allocated buffer, or NULL if the
/// allocation failed or `len` is zero.
#[export_name = "ghostty_alloc"]
pub extern "C" fn alloc(alloc_: Option<&CAllocator>, len: usize) -> *mut u8 {
    // Zig's empty allocation pointer is not safe to expose to foreign runtimes.
    if len == 0 {
        return std::ptr::null_mut();
    }
    let allocator = lib_alloc::default(alloc_);
    allocator.alloc(len, 0, 0)
}

/// Free memory that was allocated by a libghostty-vt function.
///
/// This must be used to free buffers returned by functions like
/// `format_alloc`. Pass the same allocator (or NULL for the default)
/// that was used for the allocation.
#[export_name = "ghostty_free"]
pub extern "C" fn free(alloc_: Option<&CAllocator>, ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    let allocator = lib_alloc::default(alloc_);
    allocator.free(ptr, len, 0, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;

    #[test]
    fn alloc_returns_non_null() {
        let ptr = alloc(Some(&C_ALLOCATOR), 16);
        assert!(!ptr.is_null());
        free(Some(&C_ALLOCATOR), ptr, 16);
    }

    #[test]
    fn alloc_with_null_allocator() {
        let ptr = alloc(None, 8);
        assert!(!ptr.is_null());
        free(None, ptr, 8);
    }

    #[test]
    fn alloc_zero_length() {
        let ptr = alloc(Some(&C_ALLOCATOR), 0);
        assert!(ptr.is_null());
        free(Some(&C_ALLOCATOR), ptr, 0);
    }

    #[test]
    fn free_null_pointer() {
        free(Some(&C_ALLOCATOR), std::ptr::null_mut(), 0);
    }

    #[test]
    fn free_allocated_memory() {
        let allocator = lib_alloc::default(Some(&C_ALLOCATOR));
        let mem = allocator.alloc(16, 0, 0);
        assert!(!mem.is_null());
        free(Some(&C_ALLOCATOR), mem, 16);
    }

    #[test]
    fn free_with_null_allocator() {
        // null allocator falls back to the default
        let allocator = lib_alloc::default(None);
        let mem = allocator.alloc(8, 0, 0);
        assert!(!mem.is_null());
        free(None, mem, 8);
    }
}
