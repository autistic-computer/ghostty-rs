//! Port of `src/datastruct/untouched_pool.zig`.
//!
//! STATUS: ported
//!
//! The item allocator is any [`GlobalAlloc`] (Zig: an `Allocator` value),
//! defaulting to [`System`]. The general-purpose allocator used for the
//! free list is Rust's global allocator (a `Vec`). Zig's explicit `deinit`
//! is `Drop`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::marker::PhantomData;
use std::ptr::NonNull;

/// Allocation failure (Zig: `Allocator.Error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    OutOfMemory,
}

impl std::fmt::Display for AllocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutOfMemory")
    }
}

impl std::error::Error for AllocError {}

/// Mirrors `std.heap.ArenaAllocator.ResetMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    /// Releases all allocated memory.
    FreeAll,
    /// Keeps all allocated memory for reuse.
    RetainCapacity,
    /// Keeps at most this many bytes of allocated memory for reuse.
    RetainWithLimit(usize),
}

/// A fixed-size item pool whose bookkeeping never touches item memory.
///
/// std.heap.MemoryPool keeps its free list inside the items intrusively,
/// meaning it has to touch allocated item memory. For memory that is
/// demand-paged, that write forces the OS to create a physical page
/// for nothing (to mark it free!).
///
/// This pool keeps the free list in a separate array allocated from a
/// general-purpose allocator and allocates every item individually from
/// the item allocator with the requested alignment. The pool never reads
/// or writes an item until it is needed.
///
/// Contracts:
///
///   - The pool never zeroes. Callers that need zeroed items must use an
///     item allocator that returns zeroed memory and must zero (or
///     decommit) an item before destroy().
///   - destroy() never allocates and never fails: create() reserves a
///     free-list slot for every live item before allocating a new one.
///   - The pool tracks free items only. Every item must be returned with
///     destroy() or release() before reset() or drop, or it leaks.
///     This is asserted in debug builds.
///
/// The tradeoff is that this isn't as fast as std.heap.MemoryPool, its
/// not as cache friendly. But benchmarks show that the cost is minimal
/// and if the tradeoff of not touching the memory is important, then
/// this pays off.
///
/// `ALIGNMENT` is the requested alignment in bytes (0 means the natural
/// alignment of `Item`).
pub struct UntouchedPool<Item, const ALIGNMENT: usize = 0, A: GlobalAlloc = System> {
    /// The allocator items are allocated from.
    pub allocator: A,

    /// Free items. The most recently destroyed item is handed out first.
    pub free: Vec<NonNull<Item>>,

    /// Number of items currently allocated from the item allocator,
    /// free or in use. `free.capacity() >= live` always holds so that
    /// destroy() never has to grow the free list.
    pub live: usize,

    _marker: PhantomData<Item>,
}

/// Pointer to a pool item (Zig: `*align(item_alignment) Item`).
pub type ItemPtr<Item> = NonNull<Item>;

impl<Item, const ALIGNMENT: usize, A: GlobalAlloc> UntouchedPool<Item, ALIGNMENT, A> {
    pub const ITEM_SIZE: usize = std::mem::size_of::<Item>();
    pub const ITEM_ALIGNMENT: usize = {
        let natural = std::mem::align_of::<Item>();
        std::assert!(ALIGNMENT == 0 || ALIGNMENT.is_power_of_two());
        if ALIGNMENT > natural {
            ALIGNMENT
        } else {
            natural
        }
    };

    const LAYOUT: Layout = {
        std::assert!(
            Self::ITEM_SIZE > 0,
            "UntouchedPool items must not be zero-sized"
        );
        match Layout::from_size_align(Self::ITEM_SIZE, Self::ITEM_ALIGNMENT) {
            Ok(l) => l,
            Err(_) => panic!("invalid item layout"),
        }
    };

    /// Create a pool with `preheat` items already allocated.
    pub fn init_capacity(item_alloc: A, preheat: usize) -> Result<Self, AllocError> {
        let mut free = Vec::new();
        free.try_reserve_exact(preheat)
            .map_err(|_| AllocError::OutOfMemory)?;
        let mut pool = Self {
            allocator: item_alloc,
            free,
            live: 0,
            _marker: PhantomData,
        };

        // On error, `pool` is dropped which frees the preheated items.
        for _ in 0..preheat {
            let item = pool.alloc_item()?;
            pool.free.push(item);
        }

        Ok(pool)
    }

    /// Free items so that the retained free items fit `mode`. Every
    /// item must have been returned with destroy() or release().
    ///
    /// This mirrors std.heap.MemoryPool.reset; it always succeeds
    /// (returns true) because nothing is reallocated.
    pub fn reset(&mut self, mode: ResetMode) -> bool {
        debug_assert_eq!(self.live, self.free.len());
        let retain_bytes: usize = match mode {
            ResetMode::FreeAll => 0,
            ResetMode::RetainCapacity => return true,
            ResetMode::RetainWithLimit(limit) => limit,
        };

        let retain_items = retain_bytes / Self::ITEM_SIZE;
        while self.free.len() > retain_items {
            let item = self.free.pop().unwrap();
            // SAFETY: free items were allocated from `allocator` with LAYOUT.
            unsafe {
                self.allocator
                    .dealloc(item.as_ptr() as *mut u8, Self::LAYOUT)
            };
            self.live -= 1;
        }

        true
    }

    /// Return the number of items that are allocated but not in use.
    /// These wait in the free list until `create` hands them out again.
    pub fn free_count(&self) -> usize {
        self.free.len()
    }

    /// Get an item. This pops a free item without touching it or,
    /// when none is free, allocates a new one from the item
    /// allocator.
    pub fn create(&mut self) -> Result<ItemPtr<Item>, AllocError> {
        if let Some(item) = self.free.pop() {
            return Ok(item);
        }

        // Reserve the free-list slot for the new item before
        // allocating it so that destroy() can never fail.
        let need = self.live + 1;
        if self.free.capacity() < need {
            self.free
                .try_reserve(need - self.free.len())
                .map_err(|_| AllocError::OutOfMemory)?;
        }
        self.alloc_item()
    }

    /// Return an item to the free list for reuse. The item is not
    /// modified or zeroed so it is up to the caller.
    pub fn destroy(&mut self, item: ItemPtr<Item>) {
        debug_assert!(self.free.len() < self.live);
        debug_assert!(self.free.len() < self.free.capacity());
        // Never reallocates: capacity >= live > len.
        self.free.push(item);
    }

    /// Return an item straight to the item allocator instead of the
    /// free list. This is for teardown paths that would otherwise
    /// have to zero an item only for it to be freed moments later.
    pub fn release(&mut self, item: ItemPtr<Item>) {
        debug_assert!(self.free.len() < self.live);
        // SAFETY: live items were allocated from `allocator` with LAYOUT.
        unsafe {
            self.allocator
                .dealloc(item.as_ptr() as *mut u8, Self::LAYOUT)
        };
        self.live -= 1;
    }

    fn alloc_item(&mut self) -> Result<ItemPtr<Item>, AllocError> {
        debug_assert!(self.free.capacity() > self.live);
        // SAFETY: LAYOUT has a non-zero size.
        let memory = unsafe { self.allocator.alloc(Self::LAYOUT) };
        let ptr = NonNull::new(memory as *mut Item).ok_or(AllocError::OutOfMemory)?;
        self.live += 1;
        Ok(ptr)
    }
}

impl<Item, const ALIGNMENT: usize, A: GlobalAlloc> Drop for UntouchedPool<Item, ALIGNMENT, A> {
    /// Free all items and the free list. Every item must have been
    /// returned with destroy() or release().
    fn drop(&mut self) {
        if !std::thread::panicking() {
            debug_assert_eq!(self.live, self.free.len());
        }
        for item in self.free.drain(..) {
            // SAFETY: free items were allocated from `allocator` with LAYOUT.
            unsafe {
                self.allocator
                    .dealloc(item.as_ptr() as *mut u8, Self::LAYOUT)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Minimum OS page size (`std.heap.page_size_min`).
    const PAGE_SIZE_MIN: usize = 4096;

    /// Test item: one minimum OS page, page-aligned, like a terminal page.
    type TestPool<A = System> = UntouchedPool<[u8; PAGE_SIZE_MIN], PAGE_SIZE_MIN, A>;

    #[test]
    fn untouched_pool_create_destroy_reuse() {
        let mut pool: TestPool = TestPool::init_capacity(System, 0).unwrap();

        let a = pool.create().unwrap();
        let b = pool.create().unwrap();
        let c = pool.create().unwrap();
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
        assert_eq!(0, pool.free_count());
        assert_eq!(0, a.as_ptr() as usize % PAGE_SIZE_MIN);

        // Freed items are recycled, most recent first.
        pool.destroy(a);
        pool.destroy(b);
        assert_eq!(2, pool.free_count());
        assert_eq!(b, pool.create().unwrap());
        assert_eq!(a, pool.create().unwrap());
        assert_eq!(0, pool.free_count());

        pool.destroy(a);
        pool.destroy(b);
        pool.destroy(c);
    }

    /// `std.heap.FixedBufferAllocator` equivalent over a borrowed buffer.
    struct FixedBufferAllocator {
        buffer: *mut u8,
        len: usize,
        end_index: Cell<usize>,
    }

    unsafe impl GlobalAlloc for &FixedBufferAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let base = self.buffer as usize;
            let start = (base + self.end_index.get()).next_multiple_of(layout.align()) - base;
            let end = start + layout.size();
            if end > self.len {
                return std::ptr::null_mut();
            }
            self.end_index.set(end);
            self.buffer.add(start)
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
    }

    #[test]
    fn untouched_pool_create_and_destroy_never_touch_items() {
        const PREHEAT: usize = 4;
        let item_size = TestPool::<System>::ITEM_SIZE;

        // Back the item allocator with memory we can inspect.
        let layout = Layout::from_size_align(PREHEAT * item_size, PAGE_SIZE_MIN).unwrap();
        let backing = unsafe { std::alloc::alloc(layout) };
        assert!(!backing.is_null());
        let fba = FixedBufferAllocator {
            buffer: backing,
            len: layout.size(),
            end_index: Cell::new(0),
        };

        {
            let mut pool: TestPool<&FixedBufferAllocator> =
                UntouchedPool::init_capacity(&fba, PREHEAT).unwrap();
            assert_eq!(PREHEAT * item_size, fba.end_index.get());

            // Lay the sentinel down after preheat. The sentinel must
            // differ from any fill pattern so that any write is visible.
            let sentinel: u8 = 0x5A;
            unsafe { std::ptr::write_bytes(backing, sentinel, layout.size()) };
            let all_sentinel =
                |p: *const u8, n: usize| (0..n).all(|i| unsafe { *p.add(i) } == sentinel);

            // Every preheated item comes out untouched and without allocating.
            let mut items = [NonNull::<[u8; PAGE_SIZE_MIN]>::dangling(); PREHEAT];
            for item in items.iter_mut() {
                *item = pool.create().unwrap();
                assert_eq!(PREHEAT * item_size, fba.end_index.get());
                assert!(all_sentinel(item.as_ptr() as *const u8, item_size));
            }

            // Destroying and re-creating doesn't touch them either.
            for item in items {
                pool.destroy(item);
            }
            assert!(all_sentinel(backing, layout.size()));
            for item in items.iter_mut() {
                *item = pool.create().unwrap();
                assert!(all_sentinel(item.as_ptr() as *const u8, item_size));
            }
            for item in items {
                pool.destroy(item);
            }
        }

        unsafe { std::alloc::dealloc(backing, layout) };
    }

    #[test]
    fn untouched_pool_destroy_never_allocates_from_the_general_allocator() {
        // Rust's Vec can't swap in a failing allocator, so check the
        // equivalent property: destroy never grows (reallocates) the
        // free list because create already reserved the slot.
        let mut pool: TestPool = TestPool::init_capacity(System, 0).unwrap();

        let mut items = Vec::new();
        for _ in 0..64 {
            items.push(pool.create().unwrap());
        }
        let cap = pool.free.capacity();
        let buf = pool.free.as_ptr();
        assert!(cap >= items.len());
        for item in items.iter() {
            pool.destroy(*item);
        }
        assert_eq!(cap, pool.free.capacity());
        assert_eq!(buf, pool.free.as_ptr());
        assert_eq!(items.len(), pool.free.len());
    }

    #[test]
    fn untouched_pool_reset_retains_at_most_the_limit() {
        let mut pool: TestPool = TestPool::init_capacity(System, 4).unwrap();
        let item_size = TestPool::<System>::ITEM_SIZE;

        // Free everything above the limit
        assert!(pool.reset(ResetMode::RetainWithLimit(2 * item_size)));
        assert_eq!(2, pool.free.len());
        assert_eq!(2, pool.live);

        // retain_capacity keeps everything
        assert!(pool.reset(ResetMode::RetainCapacity));
        assert_eq!(2, pool.free.len());

        // Retained items are handed out without allocating; new items
        // beyond them are allocated as needed.
        let a = pool.create().unwrap();
        let b = pool.create().unwrap();
        assert_eq!(2, pool.live);
        let c = pool.create().unwrap();
        assert_eq!(3, pool.live);
        pool.destroy(a);
        pool.destroy(b);
        pool.destroy(c);

        assert!(pool.reset(ResetMode::FreeAll));
        assert_eq!(0, pool.free.len());
        assert_eq!(0, pool.live);
    }

    #[test]
    fn untouched_pool_release_frees_immediately() {
        let mut pool: TestPool = TestPool::init_capacity(System, 1).unwrap();

        let a = pool.create().unwrap();
        let b = pool.create().unwrap();
        assert_eq!(2, pool.live);
        pool.release(a);
        assert_eq!(1, pool.live);
        assert_eq!(0, pool.free.len());
        pool.destroy(b);
        assert_eq!(1, pool.free.len());
    }
}
