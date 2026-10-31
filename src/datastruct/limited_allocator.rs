//! Port of `src/datastruct/limited_allocator.zig`.
//!
//! STATUS: ported
//!
//! Zig passes allocators explicitly, so a limiting wrapper can be handed
//! to any decoder. The closest Rust equivalent is a [`GlobalAlloc`]
//! wrapper (usable directly or as a `#[global_allocator]`), plus checked
//! helpers for code that allocates through `Vec`, which is how Rust
//! decoders ported from Zig take an allocation limit.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::TryReserveError;
use std::fmt;

/// Allocation failure (Zig: `error.OutOfMemory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    OutOfMemory,
}

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OutOfMemory")
    }
}

impl std::error::Error for AllocError {}

impl From<TryReserveError> for AllocError {
    fn from(_: TryReserveError) -> Self {
        AllocError::OutOfMemory
    }
}

/// An allocator that rejects any single allocation or resize larger than a
/// configured byte limit. The limit applies to each request independently,
/// not to the total amount of memory currently allocated.
pub struct LimitedAllocator<A: GlobalAlloc = System> {
    pub child: A,
    pub limit: usize,

    /// Set after an allocation, resize, or remap is rejected for exceeding
    /// the limit. This remains set until the caller clears it.
    pub limit_exceeded: Cell<bool>,
}

impl LimitedAllocator<System> {
    /// Limit allocations made from the system allocator.
    pub const fn new(limit: usize) -> Self {
        Self::init(System, limit)
    }
}

impl<A: GlobalAlloc> LimitedAllocator<A> {
    pub const fn init(child: A, limit: usize) -> Self {
        Self {
            child,
            limit,
            limit_exceeded: Cell::new(false),
        }
    }

    fn check(&self, size: usize) -> bool {
        if size <= self.limit {
            return true;
        }
        self.limit_exceeded.set(true);
        false
    }

    /// Allocate `layout` (Zig vtable `alloc`). Returns null if the request
    /// exceeds the limit (without consulting the child) or the child fails.
    ///
    /// # Safety
    /// Same contract as [`GlobalAlloc::alloc`].
    pub unsafe fn raw_alloc(&self, layout: Layout) -> *mut u8 {
        if !self.check(layout.size()) {
            return std::ptr::null_mut();
        }
        self.child.alloc(layout)
    }

    /// Attempt to resize an allocation in place (Zig vtable `resize`).
    ///
    /// `GlobalAlloc` has no in-place resize, so after the limit check this
    /// only succeeds for a no-op resize; callers fall back to
    /// [`LimitedAllocator::raw_remap`] exactly like Zig callers do when
    /// `resize` returns false.
    ///
    /// # Safety
    /// `ptr`/`layout` must describe a live allocation from this allocator.
    pub unsafe fn raw_resize(&self, ptr: *mut u8, layout: Layout, new_len: usize) -> bool {
        let _ = ptr;
        if !self.check(new_len) {
            return false;
        }
        new_len == layout.size()
    }

    /// Resize, possibly moving the allocation (Zig vtable `remap`).
    /// Returns null if the new size exceeds the limit (the old allocation
    /// is left untouched) or the child fails.
    ///
    /// # Safety
    /// Same contract as [`GlobalAlloc::realloc`].
    pub unsafe fn raw_remap(&self, ptr: *mut u8, layout: Layout, new_len: usize) -> *mut u8 {
        if !self.check(new_len) {
            return std::ptr::null_mut();
        }
        self.child.realloc(ptr, layout, new_len)
    }

    /// Free an allocation (Zig vtable `free`).
    ///
    /// # Safety
    /// Same contract as [`GlobalAlloc::dealloc`].
    pub unsafe fn raw_free(&self, ptr: *mut u8, layout: Layout) {
        self.child.dealloc(ptr, layout);
    }

    /// Allocate a `Vec<T>` of `n` default values, subject to the limit.
    /// This is the `alloc.alloc(T, n)` used by decoders. The `Vec`
    /// helpers allocate from the global allocator (stable `Vec` can't use
    /// a custom allocator); only the limit check applies.
    pub fn alloc<T: Clone + Default>(&self, n: usize) -> Result<Vec<T>, AllocError> {
        let bytes = n
            .checked_mul(std::mem::size_of::<T>())
            .ok_or(AllocError::OutOfMemory)?;
        if !self.check(bytes) {
            return Err(AllocError::OutOfMemory);
        }
        let mut v = Vec::new();
        v.try_reserve_exact(n)?;
        v.resize(n, T::default());
        Ok(v)
    }

    /// Grow (or shrink) the capacity of `v` to hold `new_len` elements,
    /// subject to the limit. Returns false if rejected.
    pub fn remap_vec<T>(&self, v: &mut Vec<T>, new_len: usize) -> bool {
        let Some(bytes) = new_len.checked_mul(std::mem::size_of::<T>()) else {
            self.limit_exceeded.set(true);
            return false;
        };
        if !self.check(bytes) {
            return false;
        }
        if new_len > v.len() {
            v.try_reserve_exact(new_len - v.len()).is_ok()
        } else {
            v.truncate(new_len);
            v.shrink_to(new_len);
            true
        }
    }
}

// SAFETY: forwards to the child allocator after a size check; returning
// null is the allowed failure mode.
unsafe impl<A: GlobalAlloc> GlobalAlloc for LimitedAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.raw_alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.raw_free(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        self.raw_remap(ptr, layout, new_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Equivalent of `std.testing.FailingAllocator` with `fail_index = 0`.
    struct FailingAllocator {
        has_induced_failure: Cell<bool>,
    }

    unsafe impl GlobalAlloc for FailingAllocator {
        unsafe fn alloc(&self, _layout: Layout) -> *mut u8 {
            self.has_induced_failure.set(true);
            std::ptr::null_mut()
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
    }

    #[test]
    fn limited_allocator_allows_allocations_through_the_limit() {
        let limited = LimitedAllocator::new(8);
        let layout = Layout::array::<u8>(8).unwrap();
        let data = unsafe { limited.raw_alloc(layout) };
        assert!(!data.is_null());
        unsafe { limited.raw_free(data, layout) };

        let v: Vec<u8> = limited.alloc(8).unwrap();
        assert_eq!(v.len(), 8);

        assert!(!limited.limit_exceeded.get());
    }

    #[test]
    fn limited_allocator_rejects_allocations_before_the_child() {
        let failing = FailingAllocator {
            has_induced_failure: Cell::new(false),
        };
        let limited = LimitedAllocator::init(failing, 8);

        let layout = Layout::array::<u8>(9).unwrap();
        assert!(unsafe { limited.raw_alloc(layout) }.is_null());
        assert_eq!(Err(AllocError::OutOfMemory), limited.alloc::<u8>(9));
        assert!(limited.limit_exceeded.get());
        assert!(!limited.child.has_induced_failure.get());
    }

    #[test]
    fn limited_allocator_rejects_oversized_resize_and_remap() {
        let limited = LimitedAllocator::new(8);
        let layout = Layout::array::<u8>(8).unwrap();
        let data = unsafe { limited.raw_alloc(layout) };
        assert!(!data.is_null());

        assert!(!unsafe { limited.raw_resize(data, layout, 9) });
        assert!(limited.limit_exceeded.get());

        limited.limit_exceeded.set(false);
        assert!(unsafe { limited.raw_remap(data, layout, 9) }.is_null());
        assert!(limited.limit_exceeded.get());

        unsafe { limited.raw_free(data, layout) };

        // Same through the Vec helpers.
        limited.limit_exceeded.set(false);
        let mut v: Vec<u8> = limited.alloc(8).unwrap();
        assert!(!limited.remap_vec(&mut v, 9));
        assert!(limited.limit_exceeded.get());
    }
}
