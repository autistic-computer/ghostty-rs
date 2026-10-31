//! Port of `src/datastruct/wasm_page_pool.zig`.
//!
//! STATUS: ported
//!
//! The pool only exists on `wasm32`. It is also compiled for native unit
//! tests, where wasm linear memory growth is emulated with page-aligned
//! allocations that are never returned (just like linear memory, which
//! cannot shrink), so the free-list logic stays tested.

#![cfg(any(target_arch = "wasm32", test))]

use std::any::TypeId;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::Mutex;

pub use super::untouched_pool::{AllocError, ResetMode};

/// The wasm page size (`std.heap.page_size_min` on wasm).
pub const WASM_PAGE_SIZE: usize = 64 * 1024;

/// Freed items, shared by every pool of the same Item type in the module
/// instance. Zig uses a container-level `var` per instantiation; Rust has
/// no generic statics so the list heads are keyed by the item type. The
/// value is the address of the first free item (0 = empty); each free
/// item stores the address of the next one in its first bytes (an
/// intrusive singly linked list, like `std.SinglyLinkedList`).
///
/// The Ghostty wasm modules are single threaded; the mutex only exists to
/// make the static sound (and native tests safe).
static FREE_LISTS: Mutex<Option<HashMap<TypeId, usize>>> = Mutex::new(None);

/// A memory pool of wasm-page-multiple-sized items backed directly by
/// wasm linear memory, for wasm targets only.
///
/// This is necessary for two reasons: the std.heap.MemoryPool grows 1.5x
/// at each growth point. The backing allocator for that is usually a GPA
/// which is the BrkAllocator for wasm. This grows by power-of-two
/// big-allocation slots. If you pair these together you get a massive
/// permanent linear memory growth. Native (non-wasm) targets don't care
/// because unused virtual mappings are effectively free, but this isn't
/// exactly true for Wasm runtimes.
///
/// This pool instead grows exactly size_of::<Item>() bytes of fresh linear
/// memory per item with memory.grow (which also guarantees wasm-page
/// alignment and zeroing) and recycles freed items through a free list
/// that is container-level, i.e. shared by every pool of the same Item
/// type in the module instance. Every freed item is immediately
/// reusable by every pool, and each item costs exactly size_of::<Item>()
/// reserved bytes, ever.
///
/// The "shared by every pool of the same Item type" is really important:
/// the normal Ghostty memory pool is per-terminal. This one is per-module.
/// The Ghostty wasm modules are not multi-threaded so this doesn't require
/// any synchronization. But this means that all terminals share one pool
/// so memory doesn't balloon like crazy that way either.
///
/// Requirements on Item:
///
///   - size_of::<Item>() must be a nonzero multiple of the wasm page size
///     (64KiB), since memory.grow allocates in whole pages. This is
///     also what makes the pool exact-fit: any other size would strand
///     the remainder of the last page.
///   - Natural alignment must be at most the wasm page size.
///
/// Free-list items are dirty except that their first pointer-size bytes
/// hold the intrusive free-list node, exactly like the std MemoryPool.
/// Callers that need zeroed items must zero them (fresh items from
/// memory.grow are zero by the wasm spec; recycled items retain
/// whatever the caller left, so zero either on destroy or on create).
///
/// The API mirrors the native page pool (`UntouchedPool`:
/// init_capacity/reset/create/destroy plus the allocator field),
/// so PageList can swap them at compile time.
pub struct WasmPagePool<Item: 'static, A = ()> {
    /// The allocator this managed pool was initialized with,
    /// carried for interface compatibility with the std managed
    /// memory pools. Pool items never come from it (they come
    /// from memory.grow), but callers may use it for related
    /// allocations that don't fit the pool.
    pub allocator: A,

    _marker: PhantomData<Item>,
}

/// Pointer to a pool item (Zig: `*align(page_size_min) Item`).
pub type ItemPtr<Item> = NonNull<Item>;

impl<Item: 'static, A> WasmPagePool<Item, A> {
    pub const ITEM_SIZE: usize = std::mem::size_of::<Item>();

    const CHECK: () = {
        // Items are whole wasm pages
        std::assert!(Self::ITEM_SIZE > 0);
        std::assert!(Self::ITEM_SIZE % WASM_PAGE_SIZE == 0);
        std::assert!(std::mem::align_of::<Item>() <= WASM_PAGE_SIZE);
        // Free items store the intrusive node in their bytes.
        std::assert!(Self::ITEM_SIZE >= std::mem::size_of::<usize>());
    };

    pub fn init_capacity(allocator: A, preheat: usize) -> Result<Self, AllocError> {
        #[allow(clippy::let_unit_value)]
        let () = Self::CHECK;

        // Preheating is intentionally a no-op: item creation is a
        // cheap memory.grow or free-list pop. And we want to avoid
        // the preallocation mem cost.
        let _ = preheat;
        Ok(Self {
            allocator,
            _marker: PhantomData,
        })
    }

    pub fn reset(&mut self, mode: ResetMode) -> bool {
        // There is nothing to trim: linear memory cannot shrink,
        // so retaining every freed item for reuse is always the
        // right policy on wasm. As with deinit, live items must be
        // destroyed by the caller first.
        let _ = mode;
        true
    }

    /// Return the number of unused items this pool holds, which is
    /// always zero. Freed items go to a free list shared by every pool
    /// of this item type in the module, so no single pool owns them.
    pub fn free_count(&self) -> usize {
        0
    }

    pub fn create(&mut self) -> Result<ItemPtr<Item>, AllocError> {
        // If we have a free item, use it.
        {
            let mut lists = FREE_LISTS.lock().unwrap_or_else(|e| e.into_inner());
            let lists = lists.get_or_insert_with(HashMap::new);
            let head = lists.entry(TypeId::of::<Item>()).or_insert(0);
            if *head != 0 {
                let node = *head as *mut usize;
                // SAFETY: free items store the next free address in their
                // first word (written in destroy).
                *head = unsafe { node.read() };
                return Ok(NonNull::new(node as *mut Item).unwrap());
            }
        }

        // Grow linear memory by exactly one item.
        let pages = Self::ITEM_SIZE / WASM_PAGE_SIZE;
        let ptr = memory_grow(pages).ok_or(AllocError::OutOfMemory)?;
        Ok(ptr.cast())
    }

    pub fn destroy(&mut self, ptr: ItemPtr<Item>) {
        let mut lists = FREE_LISTS.lock().unwrap_or_else(|e| e.into_inner());
        let lists = lists.get_or_insert_with(HashMap::new);
        let head = lists.entry(TypeId::of::<Item>()).or_insert(0);
        let node = ptr.as_ptr() as *mut usize;
        // SAFETY: the item is at least pointer sized and page aligned.
        unsafe { node.write(*head) };
        *head = node as usize;
    }
}

/// Grow linear memory by `pages` wasm pages, returning the start of the
/// new memory (zeroed by the wasm spec).
#[cfg(target_arch = "wasm32")]
fn memory_grow(pages: usize) -> Option<NonNull<u8>> {
    let page_index = core::arch::wasm32::memory_grow(0, pages);
    if page_index == usize::MAX {
        return None;
    }
    NonNull::new((page_index * WASM_PAGE_SIZE) as *mut u8)
}

/// Native emulation of `memory.grow` for unit tests: zeroed, page-aligned
/// memory that is never returned.
#[cfg(not(target_arch = "wasm32"))]
fn memory_grow(pages: usize) -> Option<NonNull<u8>> {
    let layout =
        std::alloc::Layout::from_size_align(pages * WASM_PAGE_SIZE, WASM_PAGE_SIZE).ok()?;
    // SAFETY: non-zero size.
    NonNull::new(unsafe { std::alloc::alloc_zeroed(layout) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_page_pool() {
        type Pool = WasmPagePool<[u8; 64 * 1024]>;
        let mut pool = Pool::init_capacity((), 0).unwrap();

        let a = pool.create().unwrap();
        let b = pool.create().unwrap();
        assert_ne!(a, b);
        assert_eq!(0, a.as_ptr() as usize % WASM_PAGE_SIZE);

        // Freed items are recycled, most recent first.
        pool.destroy(b);
        assert_eq!(b, pool.create().unwrap());

        // The free list is shared across pools of the same Item type.
        let mut pool2 = Pool::init_capacity((), 0).unwrap();
        pool.destroy(a);
        assert_eq!(a, pool2.create().unwrap());

        pool.destroy(a);
        pool.destroy(b);
    }
}
