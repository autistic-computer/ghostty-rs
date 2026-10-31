//! Port of `src/terminal/PageList.zig`.
//!
//! STATUS: ported
//!
//! Maintains a linked list of pages to make up a terminal screen
//! and provides higher level operations on top of those pages to
//! make it slightly easier to work with.
//!
//! Notes on the port:
//! - Nodes are linked by raw pointers ([`NodePtr`], Zig `*List.Node`)
//!   through the intrusive [`DoublyLinkedList`]. Nodes live in the
//!   PageList's node pool; a node pointer is valid while the node is in a
//!   PageList (or a [`Builder`]/[`PageAllocation`]) and becomes dangling
//!   once the node is destroyed. Tracked pins are raw pointers
//!   ([`PinPtr`], Zig `*Pin`) into the pin pool that PageList operations
//!   update in place. The module-private helpers `nd`/`pinr` dereference
//!   them; every use relies on the PageList invariant that every node
//!   reachable from `pages` and every tracked pin is live, and that the
//!   caller holds exclusive access to the PageList (it is not
//!   thread-safe, exactly like the Zig version).
//! - [`Pin`] methods are safe functions with the same contract as Zig: a
//!   pin is only valid until the PageList is modified (unless it is
//!   tracked). Using an invalid pin is undefined behavior. Accessors that
//!   return pointers into page memory return raw pointers (like
//!   `Page::get_row_and_cell`).
//! - Allocators: Zig passes a general-purpose allocator and selects a page
//!   allocator for page memory. Here [`Allocator`] is a small copyable
//!   handle over a [`GlobalAlloc`] (the Rust global allocator by
//!   default, or an OS page allocator for page memory). Containers that
//!   use Rust's global allocator (the tracked pin set, the pool free
//!   lists) call [`Allocator::check`] where Zig would allocate through
//!   the general-purpose allocator, so tests can inject failures.
//! - The page pool is a local [`PagePool`] with the semantics of
//!   `datastruct::UntouchedPool([std_size]u8)` because `std_size` is only
//!   known at runtime in Rust (`Page::layout` is not a `const fn`). The
//!   wasm page pool is not used (native targets only).
//! - Explicit `deinit` became `Drop` for [`PageList`], [`MemoryPool`],
//!   [`Builder`] and [`PageAllocation`] (they release their pages and
//!   pins). [`PageAllocation`] keeps a raw pointer to its destination
//!   PageList, which must not move or be dropped while it exists.
//! - The `Clone` options struct keeps its Zig name; the trait is imported
//!   anonymously so method calls still resolve.
//! - `eraseRow`/`eraseRowBounded` return `()` (their Zig error set is
//!   empty), `clone` returns [`CloneError`] (Zig's inferred
//!   `OutOfMemory || CloneFromError`).

use core::clone::Clone as _;
use core::fmt;
use core::ptr::NonNull;
use std::alloc::{GlobalAlloc, Layout as AllocLayout, System};
use std::collections::HashMap;
use std::sync::OnceLock;

use indexmap::IndexSet;

use crate::datastruct::intrusive_linked_list::DoublyLinkedList;
use crate::datastruct::untouched_pool::{self, ResetMode, UntouchedPool};
use crate::fastmem;
use crate::simd;
use crate::terminal::build_options;
use crate::terminal::compress as compression;
use crate::terminal::highlight;
use crate::terminal::hyperlink;
use crate::terminal::kitty::graphics_unicode;
use crate::terminal::mem::{self as terminal_mem, DecommitMode, PAGE_SIZE_MIN};
use crate::terminal::page::{self as pagepkg, cell_fields, field_mask, Capacity, Page, PageMemory, Row};
use crate::terminal::point;
use crate::terminal::size::{self, AllocError, CellCountInt, OffsetBuf};
use crate::terminal::style as stylepkg;
use crate::tripwire;

const LOG_TARGET: &str = "page_list";

/// The number of pages we preheat the page pool with. For operating systems
/// that support it, pages are demand-paged (see PagePool) so this only
/// costs us address space. For other operating systems, we don't preheat.
const PAGE_PREHEAT: usize = 4;

/// The number of nodes we preheat the node pool with. Unlike pages, nodes
/// are ordinary heap memory so every idle preheated node costs real
/// memory. A new PageList needs exactly one node for its first page, so
/// we preheat that one and let the pool grow on demand.
const NODE_PREHEAT: usize = 1;

/// The number of pins we preheat the pin pool with: the viewport pin that
/// every PageList tracks and the cursor pin that every Screen tracks.
/// Selections, searches, and so on grow the pool on demand.
const PIN_PREHEAT: usize = 2;

/// The list of pages in the screen. These are expected to be in order
/// where the first page is the topmost page (scrollback) and the last is
/// the bottommost page (the current active page).
pub type List = DoublyLinkedList<Node>;

/// A pointer to a node in the list (Zig: `*List.Node`).
pub type NodePtr = NonNull<Node>;

/// A pointer to a (tracked) pin (Zig: `*Pin`).
pub type PinPtr = NonNull<Pin>;

/// Dereference a node pointer.
///
/// # Safety
///
/// The node must be live (see the module notes) and not otherwise
/// accessed through a conflicting reference while the result is used.
#[inline(always)]
unsafe fn nd<'a>(node: NodePtr) -> &'a mut Node {
    unsafe { &mut *node.as_ptr() }
}

/// Dereference a pin pointer.
///
/// # Safety
///
/// The pin must be live (tracked or the viewport pin).
#[inline(always)]
unsafe fn pinr<'a>(p: PinPtr) -> &'a mut Pin {
    unsafe { &mut *p.as_ptr() }
}

/// Convert an untouched pool error.
#[inline(always)]
fn pool_err(_: untouched_pool::AllocError) -> AllocError {
    AllocError::OutOfMemory
}

// ---------------------------------------------------------------------------
// Allocators
// ---------------------------------------------------------------------------

/// A copyable allocator handle (Zig: `std.mem.Allocator`).
///
/// With no backend this is the Rust global allocator.
#[derive(Clone, Copy)]
pub struct Allocator {
    backend: Option<&'static (dyn GlobalAlloc + Sync)>,
}

impl fmt::Debug for Allocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.backend {
            None => f.write_str("Allocator(global)"),
            Some(b) => write!(f, "Allocator({:p})", b as *const _ as *const u8),
        }
    }
}

impl Default for Allocator {
    fn default() -> Self {
        Allocator::GLOBAL
    }
}

impl Allocator {
    /// The Rust global allocator.
    pub const GLOBAL: Allocator = Allocator { backend: None };

    /// An allocator backed by the given [`GlobalAlloc`].
    pub const fn new(backend: &'static (dyn GlobalAlloc + Sync)) -> Allocator {
        Allocator { backend: Some(backend) }
    }

    /// The operating system page allocator (Zig: `std.heap.page_allocator`,
    /// or the tagged mach page allocator on macOS). Memory it returns is
    /// zeroed and page aligned.
    pub fn page() -> Allocator {
        Allocator::new(&OS_PAGE_ALLOCATOR)
    }

    /// Allocate `len` bytes with the given alignment. A zero length
    /// returns a dangling, aligned pointer.
    pub fn alloc_bytes(&self, len: usize, align: usize) -> Result<NonNull<u8>, AllocError> {
        if len == 0 {
            // SAFETY: align is nonzero.
            return Ok(unsafe { NonNull::new_unchecked(align as *mut u8) });
        }
        let layout = AllocLayout::from_size_align(len, align).map_err(|_| AllocError::OutOfMemory)?;
        // SAFETY: nonzero size.
        let ptr = unsafe { self.alloc(layout) };
        NonNull::new(ptr).ok_or(AllocError::OutOfMemory)
    }

    /// Free memory returned by [`Allocator::alloc_bytes`].
    ///
    /// # Safety
    ///
    /// `ptr`, `len` and `align` must match a previous `alloc_bytes` call on
    /// an equivalent allocator.
    pub unsafe fn free_bytes(&self, ptr: NonNull<u8>, len: usize, align: usize) {
        if len == 0 {
            return;
        }
        let layout = AllocLayout::from_size_align(len, align).unwrap();
        unsafe { self.dealloc(ptr.as_ptr(), layout) };
    }

    /// Model an allocation made by a container that uses Rust's global
    /// allocator (Zig would allocate through this allocator). With the
    /// default backend this never fails and does nothing; with a custom
    /// backend this performs (and immediately frees) a one-byte
    /// allocation so failure injection sees it.
    pub fn check(&self) -> Result<(), AllocError> {
        match self.backend {
            None => Ok(()),
            Some(b) => {
                let layout = AllocLayout::from_size_align(1, 1).unwrap();
                // SAFETY: nonzero size.
                let p = unsafe { b.alloc(layout) };
                if p.is_null() {
                    return Err(AllocError::OutOfMemory);
                }
                // SAFETY: just allocated with this layout.
                unsafe { b.dealloc(p, layout) };
                Ok(())
            }
        }
    }
}

// SAFETY: forwards to the backend (or the system allocator).
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: AllocLayout) -> *mut u8 {
        unsafe {
            match self.backend {
                Some(b) => b.alloc(layout),
                None => System.alloc(layout),
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: AllocLayout) {
        unsafe {
            match self.backend {
                Some(b) => b.dealloc(ptr, layout),
                None => System.dealloc(ptr, layout),
            }
        }
    }
}

/// The OS page allocator (see [`Allocator::page`]).
pub struct OsPageAllocator;

/// The single OS page allocator instance.
pub static OS_PAGE_ALLOCATOR: OsPageAllocator = OsPageAllocator;

// SAFETY: mmap/munmap (or the system allocator) of whole pages.
unsafe impl GlobalAlloc for OsPageAllocator {
    unsafe fn alloc(&self, layout: AllocLayout) -> *mut u8 {
        #[cfg(target_os = "macos")]
        {
            // On macOS we want to tag our memory so we can assign it to our
            // core terminal usage.
            use crate::os::mach;
            return mach::TaggedPageAllocator::map(
                layout.size(),
                layout.align().trailing_zeros() as u8,
                mach::VMTag::APPLICATION_SPECIFIC_1,
            );
        }

        #[cfg(all(unix, not(target_os = "macos")))]
        {
            debug_assert!(layout.align() <= PAGE_SIZE_MIN);
            // SAFETY: anonymous private mapping; zeroed by the OS.
            let ptr = unsafe {
                libc::mmap(
                    core::ptr::null_mut(),
                    layout.size(),
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                return core::ptr::null_mut();
            }
            return ptr.cast();
        }

        #[cfg(not(unix))]
        {
            return unsafe { System.alloc_zeroed(layout) };
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: AllocLayout) {
        #[cfg(target_os = "macos")]
        {
            crate::os::mach::TaggedPageAllocator::unmap(ptr, layout.size());
        }

        #[cfg(all(unix, not(target_os = "macos")))]
        {
            // SAFETY: unmapping our own mapping.
            unsafe { libc::munmap(ptr.cast(), layout.size()) };
        }

        #[cfg(not(unix))]
        {
            unsafe { System.dealloc(ptr, layout) };
        }
    }
}

/// Returns the allocator used for underlying page allocations.
///
/// `alloc` is the caller-provided allocator. Zig uses it on native
/// freestanding targets (which Rust doesn't support) and the testing
/// allocator in tests; the port always uses the OS page allocator.
#[inline]
fn page_allocator(_alloc: Allocator) -> Allocator {
    Allocator::page()
}

// ---------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------

/// A single node within the PageList linked list.
#[derive(Debug)]
pub struct Node {
    pub prev: Option<NodePtr>,
    pub next: Option<NodePtr>,
    pub data: NodeData,
    pub serial: u64,

    /// How the backing memory of the embedded Page was allocated. Pool-owned
    /// memory is always a full standard-size item from the memory
    /// pool, regardless of the page layout size. Heap-owned memory
    /// is allocated directly with the page allocator and is exactly
    /// `Page.memory.len` bytes.
    ///
    /// This must never be inferred from the memory length: heap-owned
    /// pages can be smaller than the standard size (e.g. compacted
    /// pages), and returning one to the pool would corrupt it.
    ///
    /// This has no default on purpose so that every construction site
    /// is forced to make an explicit decision.
    pub owned: Owned,
}

crate::impl_intrusive_node!(Node);

/// The physical representation of the page contents (Zig: `Node.Data`).
///
/// Both states retain the same `Page.memory` virtual mapping and the same
/// `Page` metadata. A resident page can access the mapping directly since
/// it is, well, resident.
///
/// A compressed page owns an encoded copy while the physical memory
/// behind the raw mapping is discarded. HOWEVER, we retain the
/// virtual allocation so that restoration is infallible.
#[derive(Debug)]
pub enum NodeData {
    Resident(Page),
    Compressed(compression::Page),
}

/// (Zig: `Node.Owned`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owned {
    Pool,
    Heap,
}

/// The backing-memory representation currently stored by this node.
/// (Zig: `Node.Storage`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    Resident,
    Compressed,
}

/// Read-only page with full memory access that doesn't change
/// this node's storage state: if a node is compressed it remains
/// compressed. (Zig: `Node.PreservedPage`.)
///
/// Resident pages are borrowed directly. So they're basically free.
///
/// Compressed pages are decoded into an independently owned Page
/// by the caller so callers can inspect their contents. Because resident
/// pages are still borrowed, you have to continue to have exclusive
/// access to the PageList during this operation.
#[derive(Debug)]
pub enum PreservedPage {
    Borrowed(*const Page),
    Owned { page: Page, alloc: Allocator },
}

impl PreservedPage {
    /// Return the read-only page represented by this value.
    pub fn page(&self) -> &Page {
        match self {
            // SAFETY: borrowed from a live node (see type docs).
            PreservedPage::Borrowed(p) => unsafe { &**p },
            PreservedPage::Owned { page, .. } => page,
        }
    }

    /// Release storage owned by this preserved page.
    pub fn deinit(&mut self) {
        match self {
            PreservedPage::Borrowed(_) => {}
            // The clone buffer came from the caller's allocator rather
            // than Page's OS allocator, so it must not use Page.deinit.
            PreservedPage::Owned { page, alloc } => {
                // SAFETY: allocated in `page_preserving_state`.
                unsafe {
                    alloc.free_bytes(
                        NonNull::new_unchecked(page.memory.ptr()),
                        page.memory.len(),
                        PAGE_SIZE_MIN,
                    )
                };
            }
        }
        *self = PreservedPage::Borrowed(core::ptr::null());
    }
}

/// (Zig: `Node.RestoreMode`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreMode {
    /// Decode the compressed representation back into the raw mapping.
    Preserve,

    /// Discard the compressed representation without decoding it. The
    /// caller must not read the page contents before overwriting them.
    Discard,
}

impl Node {
    /// Return the terminal page stored in this node.
    ///
    /// WARNING: This will DECOMPRESS compressed pages! Only use this if
    /// you need access to the underlying memory. If you only need access to
    /// metadata (row, col counts etc) then use the other metadata functions.
    #[inline]
    pub fn page(&mut self) -> &mut Page {
        if let NodeData::Compressed(_) = self.data {
            return self.restore(RestoreMode::Preserve);
        }
        self.page_assume_resident()
    }

    /// Return the terminal page only when its raw memory is resident.
    ///
    /// Unlike `page`, this never restores a compressed node. This is useful
    /// for diagnostics which can display node metadata without touching the
    /// discarded mapping, but may inspect page contents when they are already
    /// available.
    #[inline]
    pub fn page_if_resident(&mut self) -> Option<&mut Page> {
        match &mut self.data {
            NodeData::Resident(p) => Some(p),
            NodeData::Compressed(_) => None,
        }
    }

    /// Return read-only page contents without changing this node's storage.
    ///
    /// `alloc` is unused for a resident node. A compressed node uses it for an
    /// exact-sized, page-aligned decode buffer owned by the returned value.
    ///
    /// The caller must call `PreservedPage.deinit` when finished. See
    /// `PreservedPage` for the synchronization required by its borrowed
    /// resident representation.
    pub fn page_preserving_state(&self, alloc: Allocator) -> Result<PreservedPage, AllocError> {
        match &self.data {
            NodeData::Resident(p) => Ok(PreservedPage::Borrowed(p as *const Page)),
            NodeData::Compressed(compressed) => {
                let len = compressed.page.memory.len();
                let ptr = alloc.alloc_bytes(len, PAGE_SIZE_MIN)?;
                // SAFETY: fresh allocation of `len` bytes.
                let memory = unsafe { PageMemory::from_raw_parts(ptr.as_ptr(), len) };

                // SAFETY: `memory` is fresh, aligned and doesn't overlap.
                let page = match unsafe { compressed.clone_buf(memory) } {
                    Ok(p) => p,
                    Err(err) => {
                        // The encoded data was produced by our codec and remains
                        // immutable while compressed. Failure is internal
                        // corruption, matching the normal restoration boundary.
                        // SAFETY: allocated above.
                        unsafe { alloc.free_bytes(ptr, len, PAGE_SIZE_MIN) };
                        log::error!(target: LOG_TARGET, "failed to clone compressed page err={:?}", err);
                        panic!("failed to clone compressed terminal page");
                    }
                };

                Ok(PreservedPage::Owned { page, alloc })
            }
        }
    }

    /// Return the node's backing-memory representation without restoring it.
    #[inline]
    pub fn storage(&self) -> Storage {
        match self.data {
            NodeData::Resident(_) => Storage::Resident,
            NodeData::Compressed(_) => Storage::Compressed,
        }
    }

    /// Return a page which the caller knows is already resident.
    ///
    /// This avoids the representation check in hot paths which already hold
    /// live pointers into the page mapping. Such pointers are only valid while
    /// the page is resident. Prefer `page` unless the caller can establish
    /// that invariant independently.
    #[inline]
    pub fn page_assume_resident(&mut self) -> &mut Page {
        match &mut self.data {
            NodeData::Resident(p) => p,
            NodeData::Compressed(_) => unreachable!("page is not resident"),
        }
    }

    /// Return the number of populated rows without accessing page memory.
    #[inline]
    pub fn rows(&self) -> CellCountInt {
        self.metadata().size.rows
    }

    /// Return the current column count without accessing page memory.
    #[inline]
    pub fn cols(&self) -> CellCountInt {
        self.metadata().size.cols
    }

    /// Return the page capacity without accessing page memory.
    #[inline]
    pub fn capacity(&self) -> Capacity {
        self.metadata().capacity
    }

    /// Return the embedded Page metadata without restoring its memory.
    #[inline]
    pub fn metadata(&self) -> &Page {
        match &self.data {
            NodeData::Resident(p) => p,
            NodeData::Compressed(c) => &c.page,
        }
    }

    /// Restore this node to the resident representation.
    ///
    /// Preserve mode reconstructs the page contents and is used by `page`.
    /// Discard mode skips decoding for callers which will overwrite or destroy
    /// the page. Both modes recommit the retained mapping, free the encoded
    /// allocation, and leave the node in a valid resident state.
    #[inline(never)]
    fn restore(&mut self, mode: RestoreMode) -> &mut Page {
        let compressed = match &mut self.data {
            NodeData::Resident(_) => return self.page_assume_resident(),
            NodeData::Compressed(c) => c,
        };

        // Decommit only discarded the physical pages. Recommit prepares the
        // still-reserved mapping for decoding or reuse by the caller.
        // SAFETY: the retained mapping is owned by this node.
        terminal_mem::recommit(unsafe { compressed.page.memory.as_mut_slice() });

        let restored = match mode {
            RestoreMode::Preserve => match compressed.restore() {
                Ok(p) => p,
                Err(err) => {
                    // Compressed nodes retain the immutable encoding and exact
                    // output mapping used to create them. Any decode error is an
                    // internal codec bug or memory corruption.
                    log::error!(target: LOG_TARGET, "failed to restore compressed page err={:?}", err);
                    panic!("failed to restore compressed terminal page");
                }
            },
            RestoreMode::Discard => compressed.page,
        };

        // Remove our compressed data
        compressed.deinit();

        // We're now a resident, non-compressed node.
        self.data = NodeData::Resident(restored);
        self.page_assume_resident()
    }

    #[inline]
    pub fn is_compressed(&self) -> bool {
        matches!(self.data, NodeData::Compressed(_))
    }
}

// ---------------------------------------------------------------------------
// Pools
// ---------------------------------------------------------------------------

/// The memory pool we get page nodes from.
///
/// We don't use a std memory pool here because it is backed by an arena
/// and we end up paying a lot of wasted memory for the growth factor when
/// in practice we don't usually use many nodes.
///
/// We don't need the "untouched" property of our UntouchedPool but
/// this gives us a GPA-allocated pool so we reuse it here.
pub type NodePool = UntouchedPool<Node, 0, Allocator>;

/// The pool of tracked pins (Zig: `std.heap.MemoryPool(Pin)`).
pub type PinPool = UntouchedPool<Pin, 0, Allocator>;

/// The standard page capacity that we use as a starting point for
/// all pages. This is chosen as a sane default that fits most terminal
/// usage to support using our pool.
pub const STD_CAPACITY: Capacity = pagepkg::STD_CAPACITY;

/// The byte size required for a standard page.
pub fn std_size() -> usize {
    static STD_SIZE: OnceLock<usize> = OnceLock::new();
    *STD_SIZE.get_or_init(|| Page::layout(STD_CAPACITY).total_size)
}

/// A pointer to a page pool item (Zig: `PagePool.ItemPtr`).
pub type PagePoolItemPtr = NonNull<u8>;

/// The memory pool we use for page memory buffers. We use a separate pool
/// so we can allocate these with a page allocator. We have to use a page
/// allocator because we need memory that is zero-initialized and page-aligned.
///
/// Untouched pools never read/write to the items so that we can
/// use demand-paging on operating systems that support it. This makes
/// it so that an idle item in the pool costs no physical memory,
/// only virtual memory.
///
/// Contract for PageList is that every path that returns an item
/// to the pool must zero it.
///
/// This mirrors `datastruct.UntouchedPool([std_size]u8, page_size_min)`;
/// see the module notes for why it is a separate type.
pub struct PagePool {
    /// The allocator items are allocated from.
    pub allocator: Allocator,

    /// Free items. The most recently destroyed item is handed out first.
    free: Vec<PagePoolItemPtr>,

    /// Number of items currently allocated from the item allocator,
    /// free or in use.
    live: usize,
}

impl PagePool {
    /// The alignment of an item.
    pub const ITEM_ALIGNMENT: usize = PAGE_SIZE_MIN;

    /// The size of an item (Zig: `PagePool.item_size`).
    #[inline]
    pub fn item_size() -> usize {
        std_size()
    }

    fn layout() -> AllocLayout {
        AllocLayout::from_size_align(Self::item_size(), Self::ITEM_ALIGNMENT).unwrap()
    }

    /// Create a pool with `preheat` items already allocated.
    pub fn init_capacity(item_alloc: Allocator, preheat: usize) -> Result<PagePool, AllocError> {
        let mut free = Vec::new();
        free.try_reserve_exact(preheat).map_err(|_| AllocError::OutOfMemory)?;
        let mut pool = PagePool {
            allocator: item_alloc,
            free,
            live: 0,
        };
        for _ in 0..preheat {
            let item = pool.alloc_item()?;
            pool.free.push(item);
        }
        Ok(pool)
    }

    /// Free items so that the retained free items fit `mode`. Every
    /// item must have been returned with destroy() or release().
    pub fn reset(&mut self, mode: ResetMode) -> bool {
        debug_assert_eq!(self.live, self.free.len());
        let retain_bytes: usize = match mode {
            ResetMode::FreeAll => 0,
            ResetMode::RetainCapacity => return true,
            ResetMode::RetainWithLimit(limit) => limit,
        };

        let retain_items = retain_bytes / Self::item_size();
        while self.free.len() > retain_items {
            let item = self.free.pop().unwrap();
            // SAFETY: free items were allocated from `allocator` with layout().
            unsafe { self.allocator.dealloc(item.as_ptr(), Self::layout()) };
            self.live -= 1;
        }
        true
    }

    /// Return the number of items that are allocated but not in use.
    pub fn free_count(&self) -> usize {
        self.free.len()
    }

    /// Get an item. This pops a free item without touching it or,
    /// when none is free, allocates a new one from the item allocator.
    pub fn create(&mut self) -> Result<PagePoolItemPtr, AllocError> {
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
    pub fn destroy(&mut self, item: PagePoolItemPtr) {
        debug_assert!(self.free.len() < self.live);
        self.free.push(item);
    }

    /// Return an item straight to the item allocator instead of the
    /// free list.
    pub fn release(&mut self, item: PagePoolItemPtr) {
        debug_assert!(self.free.len() < self.live);
        // SAFETY: live items were allocated from `allocator` with layout().
        unsafe { self.allocator.dealloc(item.as_ptr(), Self::layout()) };
        self.live -= 1;
    }

    fn alloc_item(&mut self) -> Result<PagePoolItemPtr, AllocError> {
        // SAFETY: nonzero size.
        let memory = unsafe { self.allocator.alloc(Self::layout()) };
        let ptr = NonNull::new(memory).ok_or(AllocError::OutOfMemory)?;
        self.live += 1;
        Ok(ptr)
    }
}

impl Drop for PagePool {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            debug_assert_eq!(self.live, self.free.len());
        }
        for item in self.free.drain(..) {
            // SAFETY: free items were allocated from `allocator` with layout().
            unsafe { self.allocator.dealloc(item.as_ptr(), Self::layout()) };
        }
    }
}

/// View a page pool item as a byte slice.
///
/// # Safety
///
/// `item` must be a live page pool item.
#[inline]
unsafe fn pool_item<'a>(item: PagePoolItemPtr) -> &'a mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(item.as_ptr(), std_size()) }
}

/// List of pins, known as "tracked" pins. These are pins that are kept
/// up to date automatically through page-modifying operations.
pub type PinSet = IndexSet<PinPtr>;

/// The pool of memory used for a pagelist. This can be shared between
/// multiple pagelists but it is not threadsafe.
pub struct MemoryPool {
    pub alloc: Allocator,
    pub nodes: NodePool,
    pub pages: PagePool,
    pub pins: PinPool,
}

impl MemoryPool {
    pub fn init(gen_alloc: Allocator, page_alloc: Allocator, preheat: usize) -> Result<MemoryPool, AllocError> {
        let nodes = NodePool::init_capacity(gen_alloc, NODE_PREHEAT).map_err(pool_err)?;
        let pages = PagePool::init_capacity(page_alloc, preheat)?;
        let pins = PinPool::init_capacity(gen_alloc, PIN_PREHEAT).map_err(pool_err)?;
        Ok(MemoryPool {
            alloc: gen_alloc,
            nodes,
            pages,
            pins,
        })
    }

    pub fn reset(&mut self, mode: ResetMode) {
        self.pages.reset(mode);
        self.nodes.reset(mode);
        self.pins.reset(mode);
    }
}

// ---------------------------------------------------------------------------
// PageList
// ---------------------------------------------------------------------------

/// Maintains a linked list of pages to make up a terminal screen.
pub struct PageList {
    /// The memory pool we get page nodes, pages from.
    pub pool: MemoryPool,

    /// The list of pages in the screen.
    pub pages: List,

    /// The next globally unique reference generation for this PageList. A
    /// generation is assigned whenever a page is allocated, reused as new, or
    /// changed in place such that existing page coordinates are no longer stable.
    ///
    /// The serial number can be used to detect whether the page is identical
    /// to the page that was originally referenced by a pointer. Since we reuse
    /// and pool memory, pointer stability is not guaranteed, but the serial
    /// will always be different for different page generations.
    ///
    /// Developer note: we never do overflow checking on this. If we created
    /// a new page every second it'd take 584 billion years to overflow. We're
    /// going to risk it.
    pub page_serial: u64,

    /// The first serial in the current whole-list validity epoch. Only `reset`
    /// advances this value, immediately before rebuilding every page. It is not
    /// the generation of the first page or the exact minimum live generation.
    /// Page generations are not monotonic in list order: replacement and split
    /// operations can put a fresh generation before older live pages.
    ///
    /// A generation below this epoch is definitely invalid, allowing O(1)
    /// rejection in `nodeIsValid` and bulk removal of pre-reset search results.
    /// A generation at or above it is only potentially valid and must still be
    /// checked against the live list before its coordinates are used.
    pub page_serial_epoch: u64,

    /// Byte size of the raw backing mappings owned by active page nodes. This is
    /// logical scrollback accounting and does not change while a mapping is
    /// decommitted. It excludes encoded storage and unused preheated pool items.
    pub page_size: usize,

    /// Continuation state for incremental page compression. This allows
    /// compress(.incremental) to work. More details on all that there and
    /// in the state struct.
    pub page_compression: IncrementalCompressionState,

    /// A node available for immediate reuse by createPage, bypassing the
    /// memory pool round trip. This is only set during column reflow
    /// (resizeCols), which destroys one source page for roughly every
    /// destination page it creates: returning a page buffer to the pool
    /// decommits it and taking one back recommits it, and that syscall
    /// pair per page is a significant part of reflow cost. Always null
    /// outside of an in-progress reflow.
    pub recycle_node: Option<NodePtr>,

    /// Limits for scrollback.
    pub limits: Limits,

    /// The total number of rows represented by this PageList. This is used
    /// specifically for scrollbar information so we can have the total size.
    pub total_rows: usize,

    /// The list of tracked pins. These are kept up to date automatically.
    pub tracked_pins: PinSet,

    /// The top-left of certain parts of the screen that are frequently
    /// accessed so we don't have to traverse the linked list to find them.
    ///
    /// For other tags, don't need this:
    ///   - screen: pages.first
    ///   - history: active row minus one
    ///
    pub viewport: Viewport,

    /// The pin used for when the viewport scrolls. This is always pre-allocated
    /// so that scrolling doesn't have a failable memory allocation. This should
    /// never be access directly; use `viewport`.
    pub viewport_pin: PinPtr,

    /// The row offset from the top that the viewport pin is at. We
    /// store the offset from the top because it doesn't change while more
    /// data is printed to the terminal.
    ///
    /// This is null when it isn't calculated. It is calculated on demand
    /// when the viewportRowOffset function is called, because it is only
    /// required for certain operations such as rendering the scrollbar.
    ///
    /// In order to make this more efficient, in many places where the value
    /// would be invalidated, we update it in-place instead. This is key to
    /// keeping our performance decent in normal cases since recalculating
    /// this from scratch, depending on the size of the scrollback and position
    /// of the pin, can be very expensive.
    ///
    /// This is only valid if viewport is `pin`. Every other offset is
    /// self-evident or quick to calculate.
    pub viewport_pin_row_offset: Option<usize>,

    /// The current desired screen dimensions. I say "desired" because individual
    /// pages may still be a different size and not yet reflowed since we lazily
    /// reflow text.
    pub cols: CellCountInt,
    pub rows: CellCountInt,

    /// If this is nonzero then verifyIntegrity will do nothing. This is
    /// only used with runtime safety enabled.
    pub pause_integrity_checks: usize,
}

// SAFETY: a PageList exclusively owns every node, page and pin reachable
// through its raw pointers; it can be moved to another thread as a whole.
unsafe impl Send for PageList {}

/// The viewport location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Viewport {
    /// The viewport is pinned to the active area. By using a specific marker
    /// for this instead of tracking the row offset, we eliminate a number of
    /// memory writes making scrolling faster.
    Active,

    /// The viewport is pinned to the top of the screen, or the farthest
    /// back in the scrollback history.
    Top,

    /// The viewport is pinned to a tracked pin. The tracked pin is ALWAYS
    /// s.viewport_pin hence this has no value. We force that value to prevent
    /// allocations.
    Pin,
}

/// Calculates the initial capacity for a new page for a given column
/// count. This will attempt to fit within std_size at all times so we
/// can use our memory pool, but if cols is too big, this will return a
/// larger capacity.
///
/// The returned capacity is always guaranteed to layout properly (not
/// overflow). We are able to support capacities up to the maximum int
/// value of cols, so this will never overflow.
fn initial_capacity(cols: CellCountInt) -> Capacity {
    // This is an important invariant that ensures that this function
    // can never return an error. We verify here that our standard capacity
    // when increased to maximum possible columns can always support at
    // least one row in memory. (Zig checks this at comptime.)
    debug_assert!({
        let mut cap = STD_CAPACITY;
        cap.cols = CellCountInt::MAX;
        Page::layout(cap).total_size <= size::MAX_PAGE_SIZE
    });

    if let Ok(cap) = STD_CAPACITY.adjust(pagepkg::Adjustment { cols: Some(cols) }) {
        // If we can adjust our standard capacity, we fit within the
        // standard size and we're good!
        return cap;
    }

    // This code path means that our standard capacity can't even
    // accommodate our column count! The only solution is to increase
    // our capacity and go non-standard.
    let mut cap = STD_CAPACITY;
    cap.cols = cols;
    cap
}

/// Fail points for `init` (Zig: `init_tw.FailPoint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitFailPoint {
    InitMemoryPool,
    InitPages,
    ViewportPin,
    ViewportPinTrack,
}

impl InitFailPoint {
    pub const ALL: [InitFailPoint; 4] = [
        InitFailPoint::InitMemoryPool,
        InitFailPoint::InitPages,
        InitFailPoint::ViewportPin,
        InitFailPoint::ViewportPinTrack,
    ];
}

static INIT_TW: tripwire::Module<InitFailPoint, AllocError> = tripwire::Module::new();

/// Fail points for `initPages` (Zig: `initPages_tw.FailPoint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitPagesFailPoint {
    PageNode,
    PageBufStd,
    PageBufNonStd,
}

impl InitPagesFailPoint {
    pub const ALL: [InitPagesFailPoint; 3] = [
        InitPagesFailPoint::PageNode,
        InitPagesFailPoint::PageBufStd,
        InitPagesFailPoint::PageBufNonStd,
    ];
}

static INIT_PAGES_TW: tripwire::Module<InitPagesFailPoint, AllocError> = tripwire::Module::new();

/// Options for `init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    /// The initial active-area size. This can be resized with `resize`.
    pub cols: CellCountInt,
    pub rows: CellCountInt,

    /// The maximum number of bytes allocated for pages. The effective limit
    /// is raised when necessary to hold the active area. Null is unlimited.
    pub max_size: Option<usize>,

    /// The maximum number of scrollback rows, excluding the active
    /// area. Rows are physical (viewed) rows, so a wrapped row counts as
    /// multiple rows. Max line pruning only happens at page-boundaries
    /// (the minimum internal allocation size) so in practice the max lines
    /// is always slightly larger than configured.
    ///
    /// Null is unlimited.
    pub max_lines: Option<usize>,
}

/// Create the tracked pin set for a new PageList with the viewport pin
/// already tracked. The set is sized for exactly the viewport pin and the
/// cursor pin that every Screen tracks.
fn init_tracked_pins(alloc: Allocator, viewport_pin: PinPtr) -> Result<PinSet, AllocError> {
    alloc.check()?;
    let mut set = PinSet::new();
    set.try_reserve_exact(PIN_PREHEAT).map_err(|_| AllocError::OutOfMemory)?;
    set.insert(viewport_pin);
    Ok(set)
}

/// Allocate one page node for `init_pages`, releasing partial allocations
/// on failure.
fn init_one_page(
    pool: &mut MemoryPool,
    serial: u64,
    layout: pagepkg::Layout,
    pooled: bool,
) -> Result<(NodePtr, usize), AllocError> {
    let tw = &INIT_PAGES_TW;
    let page_alloc = pool.pages.allocator;

    tw.check(InitPagesFailPoint::PageNode)?;
    let node = pool.nodes.create().map_err(pool_err)?;

    let buf: (NonNull<u8>, usize) = if pooled {
        let r = tw
            .check(InitPagesFailPoint::PageBufStd)
            .and_then(|_| pool.pages.create());
        match r {
            Ok(item) => {
                // SAFETY: live pool item.
                terminal_mem::recommit(unsafe { pool_item(item) });
                (item, std_size())
            }
            Err(e) => {
                pool.nodes.destroy(node);
                return Err(e);
            }
        }
    } else {
        let r = tw
            .check(InitPagesFailPoint::PageBufNonStd)
            .and_then(|_| page_alloc.alloc_bytes(layout.total_size, PAGE_SIZE_MIN));
        match r {
            Ok(ptr) => (ptr, layout.total_size),
            Err(e) => {
                pool.nodes.destroy(node);
                return Err(e);
            }
        }
    };

    // In runtime safety modes we memset to zero, mirroring the Zig code
    // (whose allocators fill undefined memory with 0xAA). The OS page
    // allocator already returns zeroed pages.
    if cfg!(debug_assertions) {
        // SAFETY: `buf` is `buf.1` writable bytes.
        unsafe { core::ptr::write_bytes(buf.0.as_ptr(), 0, buf.1) };
    }

    // Initialize the first set of pages to contain our viewport so that
    // the top of the first page is always the active area.
    // SAFETY: zeroed, page-aligned memory of at least `layout.total_size`
    // bytes; `node` is an uninitialized pool item.
    unsafe {
        let mem = PageMemory::from_raw_parts(buf.0.as_ptr(), buf.1);
        node.as_ptr().write(Node {
            prev: None,
            next: None,
            data: NodeData::Resident(Page::init_buf(OffsetBuf::init(mem), layout)),
            serial,
            owned: if pooled { Owned::Pool } else { Owned::Heap },
        });
    }

    Ok((node, buf.1))
}

fn init_pages(
    pool: &mut MemoryPool,
    serial: &mut u64,
    cols: CellCountInt,
    rows: CellCountInt,
) -> Result<(List, usize), AllocError> {
    let mut page_list = List::new();
    let mut page_size: usize = 0;

    // Add pages as needed to create our initial viewport.
    let cap = initial_capacity(cols);
    let layout = Page::layout(cap);
    let pooled = layout.total_size <= std_size();

    // Guaranteed by comptime checks in initialCapacity but
    // redundant here for safety.
    assert!(layout.total_size <= size::MAX_PAGE_SIZE);

    let mut rem = rows;
    while rem > 0 {
        let (node, buf_len) = match init_one_page(pool, *serial, layout, pooled) {
            Ok(v) => v,
            Err(e) => {
                // If we have an error, we need to release the pages we created.
                release_pages(pool, page_list);
                return Err(e);
            }
        };

        // SAFETY: just initialized.
        unsafe {
            let n = nd(node);
            n.page().size.rows = rem.min(n.capacity().rows);
            rem -= n.rows();

            // Add the page to the list
            page_list.append(node);
        }
        page_size += buf_len;

        // Increment our serial
        *serial += 1;
    }

    assert!(page_list.first.is_some());

    Ok((page_list, page_size))
}

/// The integrity errors that `verify_integrity` can detect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityError {
    MaxLinesExceeded,
    PageSerialInvalid,
    TotalRowsMismatch,
    TrackedPinInvalid,
    ViewportPinOffsetMismatch,
    ViewportPinInsufficientRows,
}

impl fmt::Display for IntegrityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for IntegrityError {}

/// Release every page in the list during a teardown walk (deinit,
/// reset, or an errdefer unwinding a partially built list): heap-owned
/// pages go back to the page allocator, pool-owned pages back to the
/// page pool, and the nodes back to the node pool's free list.
fn release_pages(pool: &mut MemoryPool, list: List) {
    let page_alloc = pool.pages.allocator;
    let mut it = list.first;
    while let Some(node) = it {
        // SAFETY: nodes in the list are live until destroyed below.
        unsafe {
            let n = nd(node);
            it = n.next;
            let owned = n.owned;
            let page = *n.restore(RestoreMode::Discard);
            match owned {
                Owned::Pool => release_pool_page(pool, &page),
                Owned::Heap => page_alloc.free_bytes(
                    NonNull::new_unchecked(page.memory.ptr()),
                    page.memory.len(),
                    PAGE_SIZE_MIN,
                ),
            }
        }
        pool.nodes.destroy(node);
    }
}

/// Release a pool-owned page during a teardown walk. Unlike
/// destroyNodeExt, this does not zero the page: on native, the item goes
/// straight back to the page allocator, and zeroing it first would write
/// the whole page (and fault a decommitted mapping back in) only for it
/// to be unmapped.
fn release_pool_page(pool: &mut MemoryPool, page: &Page) {
    // SAFETY: pool-owned page memory always starts a pool item.
    let item = unsafe { NonNull::new_unchecked(page.memory.ptr()) };
    pool.pages.release(item);
}

/// Options for `clone` (Zig: `PageList.Clone`).
#[derive(Debug)]
pub struct Clone<'a> {
    /// The top and bottom (inclusive) points of the region to clone.
    /// The x coordinate is ignored; the full row is always cloned.
    pub top: point::Point,
    pub bot: Option<point::Point>,

    /// If this is non-null then cloning will attempt to remap the tracked
    /// pins into the new cloned area and will keep track of the old to
    /// new mapping in this map. If this is null, the cloned pagelist will
    /// not retain any previously tracked pins except those required for
    /// internal operations.
    ///
    /// Any pins not present in the map were not remapped.
    pub tracked_pins: Option<&'a mut TrackedPinsRemap>,
}

/// (Zig: `Clone.TrackedPinsRemap`.)
pub type TrackedPinsRemap = HashMap<PinPtr, PinPtr>;

/// Errors from `clone` (Zig: `Allocator.Error || Page.CloneFromError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneError {
    OutOfMemory,
    CloneFrom(pagepkg::CloneFromError),
}

impl From<AllocError> for CloneError {
    fn from(_: AllocError) -> Self {
        CloneError::OutOfMemory
    }
}

impl fmt::Display for CloneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for CloneError {}

/// Resize options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resize {
    /// The new cols/cells of the screen.
    pub cols: Option<CellCountInt>,
    pub rows: Option<CellCountInt>,

    /// Whether to reflow the text. If this is false then the text will
    /// be truncated if the new size is smaller than the old size.
    pub reflow: bool,

    /// Set this to the current cursor position in the active area. Some
    /// resize/reflow behavior depends on the cursor position.
    pub cursor: Option<ResizeCursor>,

    /// Whether the resize may pull rows out of scrollback back into the
    /// active area. If false, growing rows always appends blank rows at the
    /// bottom and a column reflow keeps the top of the active area on the
    /// same content, so a line that is fully in scrollback stays there.
    /// A wrapped line with at least one row still in the active area may
    /// still unwrap back into view.
    ///
    /// This should be false for ptys that keep their own screen buffer
    /// without scrollback (e.g. Windows ConPTY), since they can't pull
    /// rows back and would otherwise get out of sync with us.
    pub pull_scrollback: bool,
}

impl Default for Resize {
    fn default() -> Self {
        Resize {
            cols: None,
            rows: None,
            reflow: true,
            cursor: None,
            pull_scrollback: true,
        }
    }
}

/// (Zig: `Resize.Cursor`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeCursor {
    pub x: CellCountInt,
    pub y: CellCountInt,

    /// When set, this pin preserves right-side blank cells up to the cursor
    /// during reflow.
    pub pin: Option<PinPtr>,
}

impl ResizeCursor {
    pub fn new(x: CellCountInt, y: CellCountInt) -> ResizeCursor {
        ResizeCursor { x, y, pin: None }
    }
}

/// The state remembered for the cursor across a column reflow.
#[derive(Debug, Clone, Copy)]
struct PreservedCursor {
    tracked_pin: PinPtr,
    untrack: bool,
    remaining_rows: usize,
    wrapped_rows: usize,
}

/// Scroll options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// Scroll to the active area. This is also sometimes referred to as
    /// the "bottom" of the screen. This makes it so that the end of the
    /// screen is fully visible since the active area is the bottom
    /// rows/cols of the screen.
    Active,

    /// Scroll to the top of the screen, which is the farthest back in
    /// the scrollback history.
    Top,

    /// Scroll to the given absolute row from the top. A value of zero
    /// is the top row. This row will be the first visible row in the viewport.
    /// Scrolling into or below the active area will clamp to the active area.
    Row(usize),

    /// Scroll up (negative) or down (positive) by the given number of
    /// rows. This is clamped to the "top" and "active" top left.
    DeltaRow(isize),

    /// Jump forwards (positive) or backwards (negative) a set number of
    /// prompts. If the absolute value is greater than the number of prompts
    /// in either direction, jump to the furthest prompt in that direction.
    DeltaPrompt(isize),

    /// Scroll directly to a specific pin in the page. This will be set
    /// as the top left of the viewport (ignoring the pin x value).
    Pin(Pin),
}

/// Split errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitError {
    // Allocator OOM
    OutOfMemory,
    // Page can't be split further because it is already a single row.
    OutOfSpace,
}

impl From<AllocError> for SplitError {
    fn from(_: AllocError) -> Self {
        SplitError::OutOfMemory
    }
}

impl fmt::Display for SplitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for SplitError {}

/// This represents the state necessary to render a scrollbar for this
/// PageList. It has the total size, the offset, and the size of the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scrollbar {
    /// Total size of the scrollable area.
    pub total: usize,

    /// The offset into the total area that the viewport is at. This is
    /// guaranteed to be less than or equal to total. This includes the
    /// visible row.
    pub offset: usize,

    /// The length of the visible area. This is including the offset row.
    pub len: usize,
}

/// Sync with: ghostty_action_scrollbar_s (Zig: `Scrollbar.C`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarC {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
}

impl Scrollbar {
    /// A zero-sized scrollable region.
    pub const ZERO: Scrollbar = Scrollbar {
        total: 0,
        offset: 0,
        len: 0,
    };

    pub fn cval(self) -> ScrollbarC {
        ScrollbarC {
            total: self.total as u64,
            offset: self.offset as u64,
            len: self.len as u64,
        }
    }

    /// Comparison for scrollbars.
    pub fn eql(self, other: Scrollbar) -> bool {
        self.total == other.total && self.offset == other.offset && self.len == other.len
    }
}

/// Possible dimensions to increase capacity for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncreaseCapacity {
    Styles,
    GraphemeBytes,
    HyperlinkBytes,
    StringBytes,
}

impl IncreaseCapacity {
    /// Returns the capacity dimension that must be increased for the
    /// given row clone error to succeed on retry, or null if the page
    /// only needs to be rehashed at its current capacity.
    pub fn for_clone_error(err: pagepkg::CloneFromError) -> Option<IncreaseCapacity> {
        use pagepkg::CloneFromError as E;
        match err {
            // Rehash the sets
            E::StyleSetNeedsRehash | E::HyperlinkSetNeedsRehash => None,

            // Increase style memory
            E::StyleSetOutOfMemory => Some(IncreaseCapacity::Styles),

            // Increase string memory
            E::StringAllocOutOfMemory => Some(IncreaseCapacity::StringBytes),

            // Increase hyperlink memory
            E::HyperlinkSetOutOfMemory | E::HyperlinkMapOutOfMemory => Some(IncreaseCapacity::HyperlinkBytes),

            // Increase grapheme memory
            E::GraphemeMapOutOfMemory | E::GraphemeAllocOutOfMemory => Some(IncreaseCapacity::GraphemeBytes),
        }
    }

    /// Read this dimension of a capacity (as a u64).
    fn get(self, cap: &Capacity) -> u64 {
        match self {
            IncreaseCapacity::Styles => cap.styles as u64,
            IncreaseCapacity::GraphemeBytes => cap.grapheme_bytes as u64,
            IncreaseCapacity::HyperlinkBytes => cap.hyperlink_bytes as u64,
            IncreaseCapacity::StringBytes => cap.string_bytes as u64,
        }
    }

    /// Write this dimension of a capacity. `v` must fit the field type.
    fn set(self, cap: &mut Capacity, v: u64) {
        match self {
            IncreaseCapacity::Styles => cap.styles = v as size::StyleCountInt,
            IncreaseCapacity::GraphemeBytes => cap.grapheme_bytes = v as size::GraphemeBytesInt,
            IncreaseCapacity::HyperlinkBytes => cap.hyperlink_bytes = v as size::HyperlinkCountInt,
            IncreaseCapacity::StringBytes => cap.string_bytes = v as size::StringBytesInt,
        }
    }

    /// The maximum value of this dimension's integer type.
    fn max_int(self) -> u64 {
        match self {
            IncreaseCapacity::Styles => size::StyleCountInt::MAX as u64,
            IncreaseCapacity::GraphemeBytes => size::GraphemeBytesInt::MAX as u64,
            IncreaseCapacity::HyperlinkBytes => size::HyperlinkCountInt::MAX as u64,
            IncreaseCapacity::StringBytes => size::StringBytesInt::MAX as u64,
        }
    }
}

/// Errors from `increaseCapacity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncreaseCapacityError {
    // An actual system OOM trying to allocate memory.
    OutOfMemory,

    // The existing page is already at max capacity for the given
    // adjustment. The caller must create a new page, remove data from
    // the old page, etc. (up to the caller).
    OutOfSpace,
}

impl From<AllocError> for IncreaseCapacityError {
    fn from(_: AllocError) -> Self {
        IncreaseCapacityError::OutOfMemory
    }
}

impl fmt::Display for IncreaseCapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for IncreaseCapacityError {}

/// One PageList-pooled page which has not yet joined the live page sequence.
///
/// The destination is a raw pointer: the PageList must not move or be
/// dropped while the allocation exists. Dropping the allocation releases
/// an uncommitted page (Zig: `deinit`).
pub struct PageAllocation {
    destination: *mut PageList,
    node: Option<NodePtr>,
}

/// Where `finalize` puts a page (Zig: `PageAllocation.Location`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// Prepend the page to the start of the list (oldest history).
    Prepend,
}

/// Options for `finalize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FinalizeOptions {
    /// Compress the page after it is added, unless it is visible in the
    /// viewport.
    ///
    /// Use this when adding many pages of old history one at a time,
    /// such as when restoring a snapshot, so that the full history is
    /// never held uncompressed in memory. Compression is best effort.
    /// A page that does not compress well, or a platform that does not
    /// support compression, leaves the page uncompressed without an
    /// error. Accessing a compressed page later uncompresses it
    /// automatically.
    pub compress: bool,
}

/// Errors from `finalize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeError {
    InvalidPageDimensions,
    RowCountOverflow,
    PageSizeOverflow,
    MaxSizeExceeded,
    MaxLinesExceeded,
}

impl fmt::Display for FinalizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for FinalizeError {}

impl PageAllocation {
    /// The node of this allocation, if it has not been finalized or
    /// released.
    pub fn node(&self) -> Option<NodePtr> {
        self.node
    }

    /// Return the fresh page storage for the caller to populate.
    pub fn page(&mut self) -> &mut Page {
        // SAFETY: the detached node is owned by this allocation.
        unsafe { nd(self.node.unwrap()).page_assume_resident() }
    }

    /// Release an uncommitted page back to its PageList's pools.
    ///
    /// This is safe to call after `finalize` succeeds, so callers can defer
    /// it unconditionally.
    pub fn deinit(&mut self) {
        let Some(node) = self.node else { return };
        // SAFETY: the destination outlives this allocation (type contract).
        let dest = unsafe { &mut *self.destination };
        destroy_node_ext(&mut dest.pool, node, None);
        self.node = None;
    }

    /// Finalize this complete page and transfer its ownership to the PageList.
    /// The location determines where it goes into the PageList.
    ///
    /// Existing pages and tracked pins keep their identity. A pinned viewport
    /// keeps showing the same content while its cached absolute row offset
    /// moves down by the number of newly inserted rows.
    pub fn finalize(&mut self, location: Location, options: FinalizeOptions) -> Result<(), FinalizeError> {
        match location {
            Location::Prepend => self.prepend(options),
        }
    }

    fn prepend(&mut self, options: FinalizeOptions) -> Result<(), FinalizeError> {
        // SAFETY: the destination outlives this allocation (type contract).
        let destination = unsafe { &mut *self.destination };
        let node = self.node.unwrap();
        // SAFETY: the detached node is owned by this allocation.
        let n = unsafe { nd(node) };

        // Validate the populated page and all resulting accounting before
        // publishing the detached node into the live list.
        if n.cols() == 0 || n.rows() == 0 {
            return Err(FinalizeError::InvalidPageDimensions);
        }
        let total_rows = destination
            .total_rows
            .checked_add(n.rows() as usize)
            .ok_or(FinalizeError::RowCountOverflow)?;
        let node_size: usize = match n.owned {
            Owned::Pool => PagePool::item_size(),
            Owned::Heap => n.page_assume_resident().memory.len(),
        };
        let page_size = destination
            .page_size
            .checked_add(node_size)
            .ok_or(FinalizeError::PageSizeOverflow)?;

        // Restored history is exact data, so reject a page which cannot
        // coexist with the receiving PageList's configured limits.
        if page_size > destination.limits.max(LimitKey::Bytes) {
            return Err(FinalizeError::MaxSizeExceeded);
        }
        if total_rows - destination.rows as usize > destination.limits.max(LimitKey::Lines) {
            return Err(FinalizeError::MaxLinesExceeded);
        }

        // No fallible work remains. Publish the page and update every cached
        // quantity affected by inserting rows above the existing first page.
        // SAFETY: the node is detached and live.
        unsafe { destination.pages.prepend(node) };
        destination.page_size = page_size;
        destination.total_rows = total_rows;
        if destination.viewport == Viewport::Pin {
            if let Some(offset) = destination.viewport_pin_row_offset.as_mut() {
                *offset += n.rows() as usize;
            }
        }
        destination.page_compression.mark_activity();

        destination.assert_integrity();
        self.node = None;

        // A prepended page is above every existing page, so it is never part
        // of the active area. It is visible only when the viewport is at the
        // top of the scrollback, which makes it the first visible page. A
        // failed compression leaves the page uncompressed and is not an error.
        if options.compress
            && terminal_mem::can_reclaim(DecommitMode::Strict)
            && destination.get_top_left(point::Tag::Viewport).node != node
        {
            destination.compress_page(node);
        }

        Ok(())
    }
}

impl Drop for PageAllocation {
    fn drop(&mut self) {
        self.deinit();
    }
}

/// Options for createPage and createPageExt.
#[derive(Debug, Clone, Copy)]
struct CreatePage {
    /// The capacity to allocate the page with.
    cap: Capacity,

    /// Force the page backing memory to be an exact-size heap
    /// allocation even if it would fit within a standard-size pool
    /// item. This is used when compacting pages to their minimum
    /// size, since a pool item always retains a full std_size buffer
    /// regardless of the page layout.
    exact_size: bool,
}

impl CreatePage {
    fn cap(cap: Capacity) -> CreatePage {
        CreatePage { cap, exact_size: false }
    }
}

fn create_page_ext(
    pool: &mut MemoryPool,
    opts: CreatePage,
    serial: &mut u64,
    total_size: Option<&mut usize>,
) -> Result<NodePtr, AllocError> {
    let page = pool.nodes.create().map_err(pool_err)?;

    let layout = Page::layout(opts.cap);
    let pooled = !opts.exact_size && layout.total_size <= std_size();
    let page_alloc = pool.pages.allocator;

    // It would be better to encode this into the error handling
    // system but that is a big undertaking and we only have a few
    // centralized call sites so it is handled on its own currently.
    assert!(layout.total_size <= size::MAX_PAGE_SIZE);

    // Our page buffer comes from our standard memory pool if it
    // is within our standard size since this is what the pool
    // dispenses. Otherwise, we use the heap allocator to allocate.
    let buf: (NonNull<u8>, usize) = if pooled {
        match pool.pages.create() {
            Ok(item) => {
                // SAFETY: live pool item.
                terminal_mem::recommit(unsafe { pool_item(item) });
                (item, std_size())
            }
            Err(e) => {
                pool.nodes.destroy(page);
                return Err(e);
            }
        }
    } else {
        match page_alloc.alloc_bytes(layout.total_size, PAGE_SIZE_MIN) {
            Ok(ptr) => (ptr, layout.total_size),
            Err(e) => {
                pool.nodes.destroy(page);
                return Err(e);
            }
        }
    };

    // In runtime safety modes, Zig allocators fill with 0xAA so the
    // buffer is zeroed (mirrored here in debug builds).
    //
    // Otherwise, we rely on pool item buffers being zeroed: fresh items
    // come from the OS page allocator (zeroed pages), destroyNodeExt
    // zeroes buffers before returning them to the pool, and the pool
    // never writes into its items (see PagePool).
    if cfg!(debug_assertions) {
        // SAFETY: `buf` is `buf.1` writable bytes.
        unsafe { core::ptr::write_bytes(buf.0.as_ptr(), 0, buf.1) };
    }

    // SAFETY: zeroed, aligned memory of at least `layout.total_size` bytes
    // and an uninitialized node pool item.
    unsafe {
        let mem = PageMemory::from_raw_parts(buf.0.as_ptr(), buf.1);
        page.as_ptr().write(Node {
            prev: None,
            next: None,
            data: NodeData::Resident(Page::init_buf(OffsetBuf::init(mem), layout)),
            serial: *serial,
            owned: if pooled { Owned::Pool } else { Owned::Heap },
        });
        nd(page).page().size.rows = 0;
    }
    *serial += 1;

    if let Some(v) = total_size {
        // Accumulate page size now. We don't assert or check max size
        // because we may exceed it here temporarily as we are allocating
        // pages before destroy.
        *v += buf.1;
    }

    Ok(page)
}

/// Temporary output memory used while creating a compressed page.
///
/// Standard-sized output borrows a page-pool item so repeated compression can
/// reuse the same virtual mapping. Oversized pages use a temporary allocation
/// from the page allocator and release it immediately after compression.
///
/// A borrowed item goes back to the pool through zero-mode decommit, which
/// only has to clear the bytes the encoder wrote. Callers therefore pass the
/// dirty length to `deinit` rather than paying to clear the whole item.
enum CompressionScratch {
    Pooled(PagePoolItemPtr),
    Allocated(NonNull<u8>, usize),
}

impl CompressionScratch {
    fn init(pool: &mut MemoryPool, required: usize, raw_len: usize) -> Result<CompressionScratch, AllocError> {
        assert!(required <= raw_len);

        if required <= std_size() {
            let memory = pool.pages.create()?;
            // SAFETY: live pool item.
            terminal_mem::recommit(unsafe { pool_item(memory) });
            return Ok(CompressionScratch::Pooled(memory));
        }

        let page_alloc = pool.pages.allocator;
        Ok(CompressionScratch::Allocated(
            page_alloc.alloc_bytes(raw_len, PAGE_SIZE_MIN)?,
            raw_len,
        ))
    }

    fn bytes<'a>(&mut self) -> &'a mut [u8] {
        match *self {
            // SAFETY: live pool item.
            CompressionScratch::Pooled(memory) => unsafe { pool_item(memory) },
            // SAFETY: live allocation of `len` bytes.
            CompressionScratch::Allocated(memory, len) => unsafe {
                core::slice::from_raw_parts_mut(memory.as_ptr(), len)
            },
        }
    }

    fn deinit(self, pool: &mut MemoryPool, dirty_len: usize) {
        match self {
            CompressionScratch::Pooled(memory) => {
                // SAFETY: live pool item.
                terminal_mem::decommit(DecommitMode::Zero, unsafe { pool_item(memory) }, dirty_len);
                pool.pages.destroy(memory);
            }
            CompressionScratch::Allocated(memory, len) => {
                let page_alloc = pool.pages.allocator;
                // SAFETY: allocated in `init` with this size/alignment.
                unsafe { page_alloc.free_bytes(memory, len, PAGE_SIZE_MIN) };
            }
        }
    }
}

/// Flags of [`IncrementalCompressionState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IncrementalCompressionFlags {
    /// Set after any page is compressed during the current traversal.
    /// We always run incremental compression until we get a fully
    /// no-compression pass (the verification pass). This lets us
    /// recompress decompressed pages due to search, scrolling, etc.
    pub did_compress: bool,

    /// Set after a traversal first reaches the active boundary. When this
    /// verification traversal reaches the boundary, `did_compress`
    /// determines whether to restart once more or finish the pass.
    pub verifying: bool,
}

/// PageList-owned state for incremental compression.
///
/// The position is stored as a page serial rather than a node pointer so it
/// remains safe when PageList operations destroy, replace, or reuse nodes
/// between steps. It also records the first serial which was unallocated at
/// the prior step so new nodes before a valid marker restart safely. If the
/// exact serial no longer exists in the cold prefix, the next step also
/// restarts at the first page.
///
/// A completed pass is followed by another pass from the oldest cold page.
/// Compression becomes idle only when that verification pass compresses no
/// pages. This converges after pages are restored between incremental steps
/// without requiring consumers to maintain a separate attempt cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IncrementalCompressionState {
    pub flags: IncrementalCompressionFlags,

    /// Changes whenever PageList activity may affect compression work.
    /// Callers use this value (via Terminal.compressionActivity) to
    /// determine whether to recompress.
    ///
    /// The directionality of this doesn't matter. We overflow and
    /// wrap when maxxed. The comparison of this value to your saved
    /// value is all that matters. There is a possible edge case where you
    /// don't compress, a full wraparound happens, and you get the same value,
    /// but its so unlikely.
    ///
    /// This is 48-bits (stored in a u64 and wrapped at 2^48) so that
    /// 16-bits can be reserved for Terminal to add extra state.
    pub activity_serial: u64,

    /// Serial of the last page inspected by the traversal. This is
    /// intentionally an implementation detail and is reset by PageList
    /// operations which restart traversal progress.
    pub last_serial: Option<u64>,

    /// First node serial which had not been allocated at the prior step. A
    /// node at or above this value before the saved marker requires a restart.
    pub next_serial: u64,
}

impl IncrementalCompressionState {
    /// The mask for the 48-bit activity serial.
    pub const ACTIVITY_SERIAL_MASK: u64 = (1 << 48) - 1;

    /// Record activity without disturbing valid traversal progress.
    fn mark_activity(&mut self) {
        self.activity_serial = self.activity_serial.wrapping_add(1) & Self::ACTIVITY_SERIAL_MASK;
    }

    /// Discard traversal progress without changing the activity token.
    fn reset(&mut self) {
        let activity_serial = self.activity_serial;
        *self = IncrementalCompressionState {
            activity_serial,
            ..Default::default()
        };
    }
}

/// Result of one incremental compression step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncrementalCompressionResult {
    /// Strict retained-mapping reclamation is unavailable on this target.
    Unsupported,

    /// More cold pages or a verification pass remain after this invocation's
    /// candidate-bounded work.
    Pending,

    /// A complete verification pass compressed zero pages.
    Complete,
}

/// The compression mode for `compress`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressMode {
    Incremental,
    Drain,
    Full,
}

/// Iterate complete historical pages which do not intersect the viewport.
///
/// All boundaries come from PageList pins and Page metadata. Advancing this
/// iterator never restores a compressed page or reads its backing memory.
struct CompressionIterator {
    current: NodePtr,
    active: NodePtr,
    viewport_first: NodePtr,
    viewport_last: NodePtr,
}

impl CompressionIterator {
    fn init(list: &PageList) -> CompressionIterator {
        CompressionIterator {
            current: list.pages.first.unwrap(),
            active: list.get_top_left(point::Tag::Active).node,
            viewport_first: list.get_top_left(point::Tag::Viewport).node,
            viewport_last: list.get_bottom_right(point::Tag::Viewport).unwrap().node,
        }
    }

    fn next(&mut self) -> Option<NodePtr> {
        // SAFETY: the iterated nodes are live list nodes.
        unsafe {
            while self.current != self.active {
                // The viewport is a contiguous node range. Once traversal reaches
                // its first node, advance through the complete visible range and
                // resume at the next offscreen page.
                if self.current == self.viewport_first {
                    while self.current != self.active && self.current != self.viewport_last {
                        self.current = nd(self.current).next.unwrap();
                    }

                    if self.current == self.active {
                        return None;
                    }
                    self.current = nd(self.current).next.unwrap();
                    continue;
                }

                let node = self.current;
                self.current = nd(node).next.unwrap();
                return Some(node);
            }
        }

        None
    }

    fn done(&self) -> bool {
        self.current == self.active
    }
}

/// Bound candidate inspection independently from compression work. Skipping
/// an already-compressed page is cheap, but still counts toward this limit.
const INCREMENTAL_COMPRESSION_MAX_INSPECTED: usize = 8;

/// Fail points for `compressPage` (Zig: `compressPage_tw`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressPageFailPoint {
    Decommit,
}

/// The error of `compressPage_tw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecommitError {
    DecommitFailed,
}

/// Failure injection for the final reclamation step. The operating-system
/// failure path cannot otherwise be exercised by tests because terminal_mem
/// deliberately simulates successful decommit in test builds.
static COMPRESS_PAGE_TW: tripwire::Module<CompressPageFailPoint, DecommitError> = tripwire::Module::new();

/// Destroy the memory of the given node and return it to the pool. The
/// node is assumed to already be removed from any linked list.
fn destroy_node_ext(pool: &mut MemoryPool, node: NodePtr, total_size: Option<&mut usize>) {
    // SAFETY: the node is live until destroyed at the end.
    let (owned, page) = unsafe {
        let n = nd(node);
        (n.owned, *n.restore(RestoreMode::Discard))
    };

    // Update our accounting for page size. This must mirror what was
    // added at creation time: a pool-owned page always accounts for a
    // full pool item even if its layout is smaller, while a heap-owned
    // page accounts for its exact memory length.
    if let Some(v) = total_size {
        *v -= match owned {
            Owned::Pool => PagePool::item_size(),
            Owned::Heap => page.memory.len(),
        };
    }

    match owned {
        Owned::Pool => {
            assert!(page.memory.len() <= std_size());

            // Reset the memory to zero (and decommit it, where
            // supported) so it can be reused.
            // SAFETY: pool-owned page memory starts a live pool item.
            let item = unsafe { NonNull::new_unchecked(page.memory.ptr()) };
            terminal_mem::decommit(DecommitMode::Zero, unsafe { pool_item(item) }, page.memory.len());
            pool.pages.destroy(item);
        }

        Owned::Heap => {
            let page_alloc = pool.pages.allocator;
            // SAFETY: heap pages are exact allocations from the page allocator.
            unsafe {
                page_alloc.free_bytes(
                    NonNull::new_unchecked(page.memory.ptr()),
                    page.memory.len(),
                    PAGE_SIZE_MIN,
                )
            };
        }
    }

    pool.nodes.destroy(node);
}

/// Direction that iterators can move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    LeftUp,
    RightDown,
}

/// The subset of cells returned by [`Pin::cells`] (Zig: `Pin.CellSubset`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellSubset {
    All,
    Left,
    Right,
}

impl PageList {
    /// Initialize the page. The top of the first page in the list is always the
    /// top of the active area of the screen (important knowledge for quickly
    /// setting up cursors in Screen).
    ///
    /// `max_size` is the maximum number of bytes that will be allocated for
    /// pages. If this is smaller than the bytes required to show the viewport
    /// then max_size will be ignored and the viewport will be shown, but no
    /// scrollback will be created. max_size is always rounded down to the nearest
    /// terminal page size (not virtual memory page), otherwise we would always
    /// slightly exceed max_size in the limits.
    ///
    /// `max_lines` is the maximum number of physical rows retained as scrollback,
    /// excluding the active area. It is a page-granular heuristic: at least one
    /// standard page worth of rows is permitted and only complete historical
    /// pages are removed.
    ///
    /// If either limit is null then that dimension has no defined limit and the
    /// screen will grow forever.
    pub fn init(opts: Options) -> Result<PageList, AllocError> {
        Self::init_alloc(Allocator::GLOBAL, opts)
    }

    /// `init` with an explicit general-purpose allocator (Zig: `init(alloc, opts)`).
    pub fn init_alloc(alloc: Allocator, opts: Options) -> Result<PageList, AllocError> {
        let tw = &INIT_TW;
        let cols = opts.cols;
        let rows = opts.rows;

        // The screen starts with a single page that is the entire viewport,
        // and we'll split it thereafter if it gets too large and add more as
        // necessary.
        tw.check(InitFailPoint::InitMemoryPool)?;
        let mut pool = MemoryPool::init(alloc, page_allocator(alloc), PAGE_PREHEAT)?;

        tw.check(InitFailPoint::InitPages)?;
        let mut page_serial: u64 = 0;
        let (page_list, page_size) = init_pages(&mut pool, &mut page_serial, cols, rows)?;

        let mut limits = Limits::init(cols, rows);
        limits.set(LimitKey::Bytes, opts.max_size);
        limits.set(LimitKey::Lines, opts.max_lines);

        // We always track our viewport pin to ensure this is never an allocation
        let viewport_pin = match tw
            .check(InitFailPoint::ViewportPin)
            .and_then(|_| pool.pins.create().map_err(pool_err))
        {
            Ok(p) => p,
            Err(e) => {
                release_pages(&mut pool, page_list);
                return Err(e);
            }
        };
        // SAFETY: fresh pin pool item.
        unsafe { viewport_pin.as_ptr().write(Pin::new(page_list.first.unwrap())) };

        let tracked_pins = match tw
            .check(InitFailPoint::ViewportPinTrack)
            .and_then(|_| init_tracked_pins(pool.alloc, viewport_pin))
        {
            Ok(v) => v,
            Err(e) => {
                pool.pins.destroy(viewport_pin);
                release_pages(&mut pool, page_list);
                return Err(e);
            }
        };

        let result = PageList {
            cols,
            rows,
            pool,
            pages: page_list,
            page_serial,
            page_serial_epoch: 0,
            page_size,
            page_compression: IncrementalCompressionState::default(),
            recycle_node: None,
            limits,
            total_rows: rows as usize,
            tracked_pins,
            viewport: Viewport::Active,
            viewport_pin,
            viewport_pin_row_offset: None,
            pause_integrity_checks: 0,
        };
        result.assert_integrity();
        Ok(result)
    }

    /// Assert that the PageList is in a valid state. This is a no-op in
    /// release builds.
    #[inline]
    #[track_caller]
    pub fn assert_integrity(&self) {
        if !build_options::SLOW_RUNTIME_SAFETY {
            return;
        }

        if let Err(err) = self.verify_integrity() {
            log::error!(target: LOG_TARGET, "PageList integrity check failed: {}", err);
            panic!("PageList integrity check failed: {err}");
        }
    }

    /// Pause or resume integrity checks. This is useful when you're doing
    /// a multi-step operation that temporarily leaves the PageList in an
    /// inconsistent state.
    #[inline]
    pub fn pause_integrity_checks(&mut self, pause: bool) {
        if !build_options::SLOW_RUNTIME_SAFETY {
            return;
        }
        if pause {
            self.pause_integrity_checks += 1;
        } else {
            self.pause_integrity_checks -= 1;
        }
    }

    /// Verify the integrity of the PageList. This is expensive and should
    /// only be called in debug/test builds.
    pub fn verify_integrity(&self) -> Result<(), IntegrityError> {
        if !build_options::SLOW_RUNTIME_SAFETY {
            return Ok(());
        }
        if self.pause_integrity_checks > 0 {
            return Ok(());
        }

        // SAFETY: integrity checks only read live list nodes and pins.
        unsafe {
            // Our viewport pin should never be garbage
            assert!(!pinr(self.viewport_pin).garbage);

            // Grab our total rows
            let mut actual_total: usize = 0;
            {
                let mut node_ = self.pages.first;
                while let Some(node) = node_ {
                    let n = nd(node);
                    actual_total += n.rows() as usize;
                    node_ = n.next;

                    // Every live node must belong to the current validity epoch.
                    if n.serial < self.page_serial_epoch {
                        log::warn!(
                            target: LOG_TARGET,
                            "PageList integrity violation: page serial predates epoch serial={} epoch={}",
                            n.serial,
                            self.page_serial_epoch
                        );
                        return Err(IntegrityError::PageSerialInvalid);
                    }
                }
            }

            // Verify that our cached total_rows matches the actual row count
            if actual_total != self.total_rows {
                log::warn!(
                    target: LOG_TARGET,
                    "PageList integrity violation: total_rows mismatch cached={} actual={}",
                    self.total_rows,
                    actual_total
                );
                return Err(IntegrityError::TotalRowsMismatch);
            }

            // A line limit may only be exceeded when the oldest page also contains
            // active rows. Complete historical pages are always eligible for pruning.
            if self.total_rows > self.rows as usize {
                let history_rows = self.total_rows - self.rows as usize;
                if history_rows > self.limits.max(LimitKey::Lines)
                    && self.pages.first.unwrap() != self.get_top_left(point::Tag::Active).node
                {
                    log::warn!(
                        target: LOG_TARGET,
                        "PageList integrity violation: max lines exceeded history={} max={}",
                        history_rows,
                        self.limits.max(LimitKey::Lines)
                    );
                    return Err(IntegrityError::MaxLinesExceeded);
                }
            }

            // Verify that all our tracked pins point to valid pages.
            for &p in self.tracked_pins.iter() {
                if !self.pin_is_valid(*pinr(p)) {
                    return Err(IntegrityError::TrackedPinInvalid);
                }
            }

            if self.viewport == Viewport::Pin {
                let vp = *pinr(self.viewport_pin);

                // Verify that our viewport pin row offset is correct.
                let actual_offset: usize = 'offset: {
                    let mut offset: usize = 0;
                    let mut node = self.pages.last;
                    while let Some(n) = node {
                        offset += nd(n).rows() as usize;
                        if n == vp.node {
                            offset -= vp.y as usize;
                            break 'offset self.total_rows - offset;
                        }
                        node = nd(n).prev;
                    }

                    log::warn!(target: LOG_TARGET, "PageList integrity violation: viewport pin not in list");
                    return Err(IntegrityError::ViewportPinOffsetMismatch);
                };

                if let Some(cached_offset) = self.viewport_pin_row_offset {
                    if cached_offset != actual_offset {
                        log::warn!(
                            target: LOG_TARGET,
                            "PageList integrity violation: viewport pin offset mismatch cached={} actual={}",
                            cached_offset,
                            actual_offset
                        );
                        return Err(IntegrityError::ViewportPinOffsetMismatch);
                    }
                }

                // Ensure our viewport has enough rows.
                let rows = self.total_rows - actual_offset;
                if rows < self.rows as usize {
                    log::warn!(
                        target: LOG_TARGET,
                        "PageList integrity violation: viewport pin rows too small rows={} needed={}",
                        rows,
                        self.rows
                    );
                    return Err(IntegrityError::ViewportPinInsufficientRows);
                }
            }
        }

        Ok(())
    }

    /// Reset the PageList back to an empty state. This is similar to
    /// deinit and reinit but it importantly preserves the pointer
    /// stability of tracked pins (they're moved to the top-left since
    /// all contents are cleared).
    ///
    /// This can't fail because we always retain at least enough allocated
    /// memory to fit the active area.
    pub fn reset(&mut self) {
        // Reset discards all scrollback, so there is nothing left to compress.
        self.page_compression.reset();

        // Begin a new whole-list validity epoch before rebuilding from the pools.
        // Every old reference now has a serial below the epoch and can be rejected
        // in O(1), even if the node pool later reuses its pointer address.
        self.page_serial_epoch = self.page_serial;

        // We need enough pages/nodes to keep our active area. This should
        // never fail since we by definition have allocated a page already
        // that fits our size but I'm not confident to make that assertion.
        let cap = initial_capacity(self.cols);
        assert!(cap.rows > 0);

        // The number of pages we need is the number of rows in the active
        // area divided by the row capacity of a page.
        let page_count = (self.rows as usize).div_ceil(cap.rows as usize);

        // Before resetting our pools we need to release our pages: heap-owned
        // pages go back to the page allocator and pool-owned pages back to
        // the page pool.
        release_pages(&mut self.pool, self.pages);

        // Reset our pools to free as much memory as possible while retaining
        // the capacity for at least the minimum number of pages we need.
        //
        // Retained page pool items are zero (see PagePool), so there is
        // nothing to scrub before initPages reuses them.
        self.pool
            .pages
            .reset(ResetMode::RetainWithLimit(page_count * PagePool::item_size()));
        self.pool
            .nodes
            .reset(ResetMode::RetainWithLimit(page_count * NodePool::ITEM_SIZE));

        // Initialize our pages. This should not be able to fail since
        // we retained the capacity for the minimum number of pages we need.
        let (pages, page_size) = init_pages(&mut self.pool, &mut self.page_serial, self.cols, self.rows)
            .unwrap_or_else(|_| panic!("initPages failed"));
        self.pages = pages;
        self.page_size = page_size;

        // Our total rows always goes back to the default
        self.total_rows = self.rows as usize;

        // Update all our tracked pins to point to our first page top-left
        // and mark them as garbage, because it got mangled in a way where
        // semantically it really doesn't make sense.
        // SAFETY: tracked pins are live.
        unsafe {
            let first = self.pages.first.unwrap();
            for &p in self.tracked_pins.iter() {
                let p = pinr(p);
                p.node = first;
                p.x = 0;
                p.y = 0;
                p.garbage = true;
            }

            // Our viewport pin is never garbage
            pinr(self.viewport_pin).garbage = false;
        }

        // Move our viewport back to the active area since everything is gone.
        self.viewport = Viewport::Active;

        self.assert_integrity();
    }

    /// Clone this pagelist from the top to bottom (inclusive).
    ///
    /// The viewport is always moved to the active area.
    ///
    /// The cloned pagelist must contain at least enough rows for the active
    /// area. If the region specified has less rows than the active area then
    /// rows will be added to the bottom of the region to make up the difference.
    pub fn clone(&self, opts: Clone<'_>) -> Result<PageList, CloneError> {
        self.clone_alloc(Allocator::GLOBAL, opts)
    }

    /// `clone` with an explicit general-purpose allocator.
    pub fn clone_alloc(&self, alloc: Allocator, mut opts: Clone<'_>) -> Result<PageList, CloneError> {
        let mut it = self.page_iterator(Direction::RightDown, opts.top, opts.bot);

        // First, count our pages so our preheat is exactly what we need.
        let page_count: usize = {
            let mut it_copy = it;
            let mut count: usize = 0;
            while it_copy.next().is_some() {
                count += 1;
            }
            count
        };

        // Setup our pool
        let mut pool = MemoryPool::init(alloc, page_allocator(alloc), page_count)?;

        // Create our viewport. In a clone, the viewport always goes
        // to the top.
        let viewport_pin = pool.pins.create().map_err(pool_err)?;
        // The real value is set below once the first page exists.
        // SAFETY: fresh pin pool item.
        unsafe { viewport_pin.as_ptr().write(Pin::new(NonNull::dangling())) };
        let tracked_pins = match init_tracked_pins(pool.alloc, viewport_pin) {
            Ok(v) => v,
            Err(e) => {
                pool.pins.destroy(viewport_pin);
                return Err(e.into());
            }
        };

        // From here on, `result` owns everything and its Drop releases the
        // partially built list on error (Zig's errdefers). Integrity checks
        // are paused until the list is complete.
        let mut result = PageList {
            pool,
            pages: List::new(),
            page_serial: 0,
            page_serial_epoch: 0,
            page_size: 0,
            page_compression: IncrementalCompressionState::default(),
            recycle_node: None,
            limits: self.limits,
            cols: self.cols,
            rows: self.rows,
            total_rows: 0,
            tracked_pins,
            viewport: Viewport::Active,
            viewport_pin,
            viewport_pin_row_offset: None,
            pause_integrity_checks: 1,
        };

        // Copy our pages
        // SAFETY: source chunks are live nodes of `self`; destination nodes
        // are live nodes of `result`.
        unsafe {
            while let Some(chunk) = it.next() {
                // Clone the page. We have to use createPageExt here because
                // we don't know if the source page has a standard size.
                let node = create_page_ext(
                    &mut result.pool,
                    CreatePage::cap(nd(chunk.node).capacity()),
                    &mut result.page_serial,
                    Some(&mut result.page_size),
                )?;

                // Add the page to the list immediately so that the errdefer
                // above releases it if cloning fails.
                result.pages.append(node);

                let dst_page: *mut Page = nd(node).page();
                let src_page: *mut Page = nd(chunk.node).page();
                assert!(nd(node).capacity().rows >= chunk.end - chunk.start);
                (*dst_page).size.rows = chunk.end - chunk.start;
                (*dst_page).size.cols = nd(chunk.node).cols();
                let r = (*dst_page).clone_from(src_page, chunk.start as usize, chunk.end as usize);
                (*dst_page).assert_integrity();
                r.map_err(CloneError::CloneFrom)?;

                (*dst_page).dirty = (*src_page).dirty;

                result.total_rows += nd(node).rows() as usize;

                // Remap our tracked pins by changing the page and
                // offsetting the Y position based on the chunk start.
                if let Some(remap) = opts.tracked_pins.as_deref_mut() {
                    for &p in self.tracked_pins.iter() {
                        let pv = *pinr(p);
                        // We're only interested in pins that were within the chunk.
                        if pv.node != chunk.node || pv.y < chunk.start || pv.y >= chunk.end {
                            continue;
                        }
                        if result.tracked_pins.len() == result.tracked_pins.capacity() {
                            result.pool.alloc.check()?;
                        }
                        result
                            .tracked_pins
                            .try_reserve(1)
                            .map_err(|_| CloneError::OutOfMemory)?;
                        let new_p = result.pool.pins.create().map_err(pool_err)?;
                        let mut np = pv;
                        np.node = node;
                        np.y -= chunk.start;
                        new_p.as_ptr().write(np);
                        result.tracked_pins.insert(new_p);
                        remap.insert(p, new_p);
                    }
                }
            }

            // Initialize our viewport pin to point to the first cloned page
            // so it points to valid memory.
            *pinr(viewport_pin) = Pin::new(result.pages.first.unwrap());
        }
        result.pause_integrity_checks = 0;

        // We always need to have enough rows for our viewport because this is
        // a pagelist invariant that other code relies on.
        if result.total_rows < self.rows as usize {
            let len = self.rows as usize - result.total_rows;
            for _ in 0..len {
                result.grow()?;

                // Clear the row. This is not very fast but in reality right
                // now we rarely clone less than the active area and if we do
                // the area is by definition very small.
                // SAFETY: the last node is live.
                unsafe {
                    let last = result.pages.last.unwrap();
                    let page = nd(last).page();
                    let row = &mut *page.rows.ptr(page.memory).add(nd(last).rows() as usize - 1);
                    page.clear_cells(row, 0, result.cols as usize);
                }
            }

            // Update our total rows to be our row size.
            result.total_rows = result.rows as usize;
        }

        // A clone can copy more history than its inherited line limit.
        let limits = result.limits;
        limits.enforce(&mut result, LimitKey::Lines);

        result.assert_integrity();
        Ok(result)
    }

    /// Resize
    /// TODO: docs
    pub fn resize(&mut self, opts: Resize) -> Result<(), AllocError> {
        let r = self.resize_(opts);
        self.assert_integrity();
        r
    }

    fn resize_(&mut self, opts: Resize) -> Result<(), AllocError> {
        // Resizing forces all nodes to be decompressed today so we need to
        // reschedule compression.
        // TODO(mitchellh): Deferred reflow on non-viewport/non-active pages.
        self.page_compression.reset();
        self.page_compression.mark_activity();

        if cfg!(debug_assertions) {
            // Resize does not work with 0 values, this should be protected
            // upstream
            if let Some(v) = opts.cols {
                assert!(v > 0);
            }
            if let Some(v) = opts.rows {
                assert!(v > 0);
            }
        }

        // Resizing (especially with reflow) can cause our row offset to
        // become invalid. Rather than do something fancy like we do other
        // places and try to update it in place, we just invalidate it because
        // its too easy to get the logic wrong in here.
        self.viewport_pin_row_offset = None;

        if !opts.reflow {
            self.resize_without_reflow(opts)?;
            // Shrinking the active row count turns former active rows into
            // scrollback even without reflow, which can cross the line limit.
            let limits = self.limits;
            limits.enforce(self, LimitKey::Lines);
            return Ok(());
        }

        // Recalculate our minimum limits. This allows grow to work properly when
        // increasing beyond the explicit limits to fit the active area.
        let old_limits = self.limits;
        self.limits
            .resize(opts.cols.unwrap_or(self.cols), opts.rows.unwrap_or(self.rows));

        // On reflow, the main thing that causes reflow is column changes. If
        // only rows change, reflow is impossible. So we change our behavior based
        // on the change of columns.
        let cols = opts.cols.unwrap_or(self.cols);
        let r = match cols.cmp(&self.cols) {
            core::cmp::Ordering::Equal => self.resize_without_reflow(opts),

            core::cmp::Ordering::Greater => {
                // We grow rows after cols so that we can do our unwrapping/reflow
                // before we do a no-reflow grow.
                self.resize_cols(cols, opts)
                    .and_then(|_| self.resize_without_reflow(opts))
            }

            core::cmp::Ordering::Less => {
                // We first change our row count so that we have the proper amount
                // we can use when shrinking our cols.
                let mut copy = opts;
                copy.cols = Some(self.cols);
                self.resize_without_reflow(copy)
                    .and_then(|_| self.resize_cols(cols, opts))
            }
        };
        if let Err(e) = r {
            self.limits = old_limits;
            return Err(e);
        }

        // Various resize operations can change our total row count such
        // that our viewport pin is now in the active area and has insufficient
        // space. We need to check for this case and fix it up.
        if self.viewport == Viewport::Pin {
            // SAFETY: the viewport pin is live.
            if self.pin_is_active(unsafe { *pinr(self.viewport_pin) }) {
                self.viewport = Viewport::Active;
            }
        }

        // Column reflow can change the physical history row count, and a row
        // resize can move the active boundary. Both may expose whole old pages
        // that are now eligible for line-limit pruning.
        let limits = self.limits;
        limits.enforce(self, LimitKey::Lines);
        Ok(())
    }

    /// Resize the pagelist with reflow by adding or removing columns.
    fn resize_cols(&mut self, cols: CellCountInt, opts: Resize) -> Result<(), AllocError> {
        assert!(cols != self.cols);
        let cursor = opts.cursor;

        // The active area is always the last `rows` rows, so a reflow that
        // changes the number of rows our text needs slides the active area
        // over the content. If we aren't allowed to pull scrollback then we
        // track the top of the active area so we can restore it afterwards.
        let active_top: Option<PinPtr> = if !opts.pull_scrollback {
            Some(self.track_pin(self.get_top_left(point::Tag::Active))?)
        } else {
            None
        };

        // If we have a cursor position (x,y), then we try under any col resizing
        // to keep the same number remaining active rows beneath it. This is a
        // very special case if you can imagine clearing the screen (i.e.
        // scrollClear), having an empty active area, and then resizing to less
        // cols then we don't want the active area to "jump" to the bottom and
        // pull down scrollback.
        let preserved_cursor = match self.resize_cols_preserve_cursor(cursor) {
            Ok(v) => v,
            Err(e) => {
                if let Some(p) = active_top {
                    self.untrack_pin(p);
                }
                return Err(e);
            }
        };

        let r = self.resize_cols_reflow(cols, active_top, preserved_cursor);

        if let Some(c) = preserved_cursor {
            if c.untrack {
                self.untrack_pin(c.tracked_pin);
            }
        }
        if let Some(p) = active_top {
            self.untrack_pin(p);
        }

        r
    }

    fn resize_cols_preserve_cursor(&mut self, cursor: Option<ResizeCursor>) -> Result<Option<PreservedCursor>, AllocError> {
        let Some(c) = cursor else { return Ok(None) };

        // SAFETY: the cursor pin (if any) is a tracked pin.
        let p = if let Some(cursor_pin) = c.pin {
            unsafe { *pinr(cursor_pin) }
        } else {
            match self.pin(point::Point::Active(point::Coordinate {
                x: c.x,
                y: c.y as u32,
            })) {
                Some(p) => p,
                None => return Ok(None),
            }
        };

        let active_pin = self.pin(point::Point::Active(point::Coordinate::default()));

        // We count how many wraps the cursor had before it to begin with
        // so that we can offset any additional wraps to avoid pushing the
        // original row contents in to the scrollback.
        let wrapped: usize = 'wrapped: {
            let mut wrapped: usize = 0;

            // If shrinking rows (in the .lt branch of resize, rows shrink
            // before we get here) pushed the cursor pin above the new active
            // area, there are no rows to count and iterating .left_up toward
            // the active-area top would be an invalid (reversed) range. The
            // preserved-cursor growth below already no-ops for a cursor that
            // isn't in the active area, so we just count zero here.
            if let Some(ap) = active_pin {
                if p.before(ap) {
                    break 'wrapped 0;
                }
            }

            let mut row_it = p.row_iterator(Direction::LeftUp, active_pin);
            while let Some(next) = row_it.next() {
                // SAFETY: valid pin from the iterator.
                let row = unsafe { &*next.row_and_cell().row };
                if row.wrap_continuation() {
                    wrapped += 1;
                }
            }

            wrapped
        };

        let tracked_pin = match c.pin {
            Some(p) => p,
            None => self.track_pin(p)?,
        };

        Ok(Some(PreservedCursor {
            tracked_pin,
            untrack: c.pin.is_none(),
            remaining_rows: (self.rows as usize).saturating_sub(c.y as usize + 1),
            wrapped_rows: wrapped,
        }))
    }

    fn resize_cols_reflow(
        &mut self,
        cols: CellCountInt,
        active_top: Option<PinPtr>,
        preserved_cursor: Option<PreservedCursor>,
    ) -> Result<(), AllocError> {
        // Update our cols. We have to do this early because grow() that we
        // may call below relies on this to calculate the proper page size, but
        // after preserved_cursor so that the cursor pin can resolve coordinates in
        // the old active coordinate space.
        self.cols = cols;

        // Create the first node that contains our reflow.
        // SAFETY: the first node is live.
        let first_rewritten_node = unsafe {
            let page = nd(self.pages.first.unwrap()).page();
            let cap = match page.capacity.adjust(pagepkg::Adjustment { cols: Some(cols) }) {
                Ok(c) => c,
                Err(_) => {
                    // We verify all maxed out page layouts work.
                    let mut cap = page.capacity;
                    cap.cols = cols;

                    // We're growing columns so we can only get less rows so use
                    // the lesser of our capacity and size so we minimize wasted
                    // rows.
                    cap.rows = page.size.rows.min(cap.rows);
                    cap
                }
            };

            let node = self.create_page(CreatePage::cap(cap))?;
            nd(node).page().size.rows = 1;
            node
        };

        // We need to grab our rowIterator now before we rewrite our
        // linked list below.
        let mut it = self.row_iterator(
            Direction::RightDown,
            point::Point::Screen(point::Coordinate::default()),
            None,
        );

        // Set our new page as the only page. This orphans the existing pages
        // in the list, but that's fine since we're gonna delete them anyway.
        self.pages.first = Some(first_rewritten_node);
        self.pages.last = Some(first_rewritten_node);

        // Reflow all our rows.
        let reflow = (|| -> Result<(), AllocError> {
            let mut reflow_cursor = ReflowCursor::init(first_rewritten_node);
            while let Some(row) = it.next() {
                reflow_cursor.reflow_row(self, row, preserved_cursor.map(|c| c.tracked_pin))?;

                // Once we're done reflowing a page, we're done with it, so
                // make it available for reuse (or destroy it). Making it
                // immediately available frees memory and makes it more
                // likely in memory constrained environments that the next
                // reflow will work.
                // SAFETY: the source node is live until destroyed here.
                unsafe {
                    let n = nd(row.node);
                    if row.y == n.rows() - 1 {
                        if self.recycle_node.is_some() || n.owned != Owned::Pool || n.storage() != Storage::Resident {
                            self.destroy_node(row.node);
                        } else {
                            self.recycle_node = Some(row.node);
                        }
                    }
                }
            }

            // At the end of the reflow, setup our total row cache
            self.total_rows = reflow_cursor.total_rows;
            Ok(())
        })();

        if let Err(e) = reflow {
            // If an error occurs, we're in a pretty disastrous broken state,
            // but we should still try to clean up our leaked memory. Free
            // any of the remaining orphaned pages from before. If we reflowed
            // successfully this will be null.
            let mut node_ = it.chunk.map(|c| c.node);
            while let Some(node) = node_ {
                // SAFETY: orphaned source nodes are live until destroyed.
                node_ = unsafe { nd(node).next };
                self.destroy_node(node);
            }
        }

        // Reflowed source pages are stashed for reuse as destination
        // pages (see recycle_node). Whether we succeed or fail, a stashed
        // node must not outlive the reflow.
        if let Some(node) = self.recycle_node.take() {
            self.destroy_node(node);
        }

        reflow?;

        // If our total rows is less than our active rows, we need to grow.
        // This can happen if you're growing columns such that enough active
        // rows unwrap that we no longer have enough.
        {
            let mut node_it = self.pages.first;
            let mut total: usize = 0;
            let mut enough = false;
            while let Some(node) = node_it {
                // SAFETY: live list node.
                let n = unsafe { nd(node) };
                total += n.rows() as usize;
                if total >= self.rows as usize {
                    enough = true;
                    break;
                }
                node_it = n.next;
            }
            if !enough {
                for _ in total..self.rows as usize {
                    self.grow()?;
                }
            }
        }

        // Reflow can unwrap enough rows that a history viewport pin lands in the
        // active area before we do any preserved-cursor growth below. Switch back
        // to the active viewport now so intermediate grow() integrity checks stay
        // valid.
        if self.viewport == Viewport::Pin {
            // SAFETY: live viewport pin.
            if self.pin_is_active(unsafe { *pinr(self.viewport_pin) }) {
                self.viewport = Viewport::Active;
            }
        }

        // If we can't pull scrollback then pad the bottom with blank rows until
        // the old top of the active area is back at the top. If the reflow
        // instead pushed it into scrollback (the text needs more rows than
        // we have) then there is nothing to do. This subsumes the preserved
        // cursor logic below since that also only exists to avoid a pull.
        if let Some(p) = active_top {
            // SAFETY: tracked pin.
            if let Some(pt) = self.point_from_pin(point::Tag::Active, unsafe { *pinr(p) }) {
                for _ in 0..pt.coord().y {
                    self.grow()?;
                }
            }
            return Ok(());
        }

        // See preserved_cursor setup for why.
        if let Some(c) = preserved_cursor {
            // SAFETY: tracked pin.
            let tracked = unsafe { *pinr(c.tracked_pin) };
            if let Some(active_pt) = self.point_from_pin(point::Tag::Active, tracked) {
                let active_pin = self.pin(point::Point::Active(point::Coordinate::default()));

                // We need to determine how many rows we wrapped from the original
                // and subtract that from the remaining rows we expect because if
                // we wrap down we don't want to push our original row contents into
                // the scrollback.
                let wrapped = {
                    let mut wrapped: usize = 0;

                    let mut row_it = tracked.row_iterator(Direction::LeftUp, active_pin);
                    while let Some(next) = row_it.next() {
                        // SAFETY: valid pin from the iterator.
                        let row = unsafe { &*next.row_and_cell().row };
                        if row.wrap_continuation() {
                            wrapped += 1;
                        }
                    }

                    wrapped
                };

                let current = (self.rows as usize).saturating_sub(active_pt.coord().y as usize + 1);

                let mut req_rows = c.remaining_rows;
                req_rows = req_rows.saturating_sub(wrapped.saturating_sub(c.wrapped_rows));
                req_rows = req_rows.saturating_sub(current);

                while req_rows > 0 {
                    self.grow()?;
                    req_rows -= 1;
                }
            }
        }

        Ok(())
    }

    fn resize_without_reflow(&mut self, opts: Resize) -> Result<(), AllocError> {
        // We only set the new minimums if we're not reflowing. If we are
        // reflowing, then the outer resize call handles this for us.
        let old_limits = self.limits;
        if !opts.reflow {
            self.limits
                .resize(opts.cols.unwrap_or(self.cols), opts.rows.unwrap_or(self.rows));
        }
        let r = self.resize_without_reflow_(opts);
        if r.is_err() {
            self.limits = old_limits;
        }
        r
    }

    fn resize_without_reflow_(&mut self, opts: Resize) -> Result<(), AllocError> {
        // Important! We have to do cols first because cols may cause us to
        // destroy pages if we're increasing cols which will free up page_size
        // so that when we call grow() in the row mods, we won't prune.
        if let Some(cols) = opts.cols {
            // Any column change without reflow should not result in row counts
            // changing.
            let old_total_rows = self.total_rows;

            match cols.cmp(&self.cols) {
                core::cmp::Ordering::Equal => {}

                // Making our columns smaller. We always have space for this
                // in existing pages so we need to go through the pages,
                // resize the columns, and clear any cells that are beyond
                // the new size.
                core::cmp::Ordering::Less => {
                    let mut it = self.page_iterator(
                        Direction::RightDown,
                        point::Point::Screen(point::Coordinate::default()),
                        None,
                    );
                    while let Some(chunk) = it.next() {
                        // SAFETY: live node; rows/cells are within the page.
                        unsafe {
                            let page = nd(chunk.node).page();
                            let rows = page.rows.ptr(page.memory);
                            for i in 0..page.size.rows as usize {
                                let row = &mut *rows.add(i);
                                // If the cut splits a wide char, clear its head too.
                                let cells = row.cells().ptr(page.memory);
                                let start = if (*cells.add(cols as usize - 1)).wide() == pagepkg::Wide::Wide {
                                    cols - 1
                                } else {
                                    cols
                                };
                                page.clear_cells(row, start as usize, self.cols as usize);
                            }

                            page.size.cols = cols;
                            page.assert_integrity();
                        }
                    }

                    // Update all our tracked pins. If they have an X
                    // beyond the edge, clamp it.
                    for &p in self.tracked_pins.iter() {
                        // SAFETY: tracked pin.
                        let p = unsafe { pinr(p) };
                        if p.x >= cols {
                            p.x = cols - 1;
                        }
                    }

                    self.cols = cols;
                }

                // Make our columns larger. This is a bit more complicated because
                // pages may not have the capacity for this. If they don't have
                // the capacity we need to allocate a new page and copy the data.
                core::cmp::Ordering::Greater => {
                    // See the comment in the while loop when setting self.cols
                    let old_cols = self.cols;

                    let mut it = self.page_iterator(
                        Direction::RightDown,
                        point::Point::Screen(point::Coordinate::default()),
                        None,
                    );
                    while let Some(chunk) = it.next() {
                        // We need to restore our old cols after we resize because
                        // we have an assertion on this and we want to be able to
                        // call this method multiple times.
                        self.cols = old_cols;
                        self.resize_without_reflow_grow_cols(cols, chunk)?;
                    }

                    self.cols = cols;
                }
            }

            debug_assert_eq!(self.total_rows, old_total_rows);
        }

        if let Some(rows) = opts.rows {
            match rows.cmp(&self.rows) {
                core::cmp::Ordering::Equal => {}

                // Making rows smaller, we simply change our rows value. Changing
                // the row size doesn't affect anything else since max size and
                // so on are all byte-based.
                core::cmp::Ordering::Less => {
                    // If our rows are shrinking, we prefer to trim trailing
                    // blank lines from the active area instead of creating
                    // history if we can.
                    //
                    // This matches macOS Terminal.app behavior. I chose to match that
                    // behavior because it seemed fine in an ocean of differing behavior
                    // between terminal apps. I'm completely open to changing it as long
                    // as resize behavior isn't regressed in a user-hostile way.
                    let trimmed = self.trim_trailing_blank_rows(self.rows - rows);

                    // Account for our trimmed rows in the total row cache
                    self.total_rows -= trimmed as usize;

                    // If we didn't trim enough, just modify our row count and this
                    // will create additional history.
                    self.rows = rows;
                }

                // Making rows larger we adjust our row count, and then grow
                // to the row count.
                core::cmp::Ordering::Greater => 'gt: {
                    // If our rows increased and our cursor is NOT at the bottom,
                    // we want to try to preserve the y value of the old cursor.
                    // In other words, we don't want to "pull down" scrollback.
                    // This is purely a UX feature.
                    //
                    // If we're not allowed to pull scrollback at all then we
                    // always do this regardless of the cursor.
                    let pull = 'pull: {
                        if !opts.pull_scrollback {
                            break 'pull false;
                        }
                        let Some(cursor) = opts.cursor else { break 'pull true };
                        cursor.y >= self.rows - 1
                    };
                    if !pull {
                        // We just grow our rows and we're done. Cursor does
                        // NOT change for this since we're not pulling down
                        // scrollback.
                        let delta = rows - self.rows;
                        self.rows = rows;
                        for _ in 0..delta {
                            self.grow()?;
                        }
                        break 'gt;
                    }

                    // This must be set BEFORE any calls to grow() so that
                    // grow() doesn't prune pages that we need for the active
                    // area.
                    self.rows = rows;

                    // Cursor is at the bottom or we don't care about cursors.
                    // In this case, if we have enough rows in our pages, we
                    // just update our rows and we're done. This effectively
                    // "pulls down" scrollback.
                    //
                    // This traversal intentionally reads only node metadata. A
                    // compressed history page pulled into the active area remains
                    // compressed until a renderer or other content consumer goes
                    // through `Node.page`, which restores it transparently.
                    //
                    // If we don't have enough scrollback, we add the difference,
                    // to the active area.
                    let mut count: usize = 0;
                    let mut page = self.pages.first;
                    let mut enough = false;
                    while let Some(p) = page {
                        // SAFETY: live list node.
                        let n = unsafe { nd(p) };
                        count += n.rows() as usize;
                        if count >= rows as usize {
                            enough = true;
                            break;
                        }
                        page = n.next;
                    }
                    if !enough {
                        assert!(count < rows as usize);
                        for _ in count..rows as usize {
                            self.grow()?;
                        }
                    }

                    // Make sure that the viewport pin isn't below the active
                    // area, since that will lead to all sorts of problems.
                    if self.viewport == Viewport::Pin {
                        // SAFETY: live viewport pin.
                        if self.pin_is_active(unsafe { *pinr(self.viewport_pin) }) {
                            self.viewport = Viewport::Active;
                        }
                    }
                }
            }

            if build_options::SLOW_RUNTIME_SAFETY {
                // We never have less rows than our active screen has.
                assert!(self.total_rows() >= self.rows as usize);
            }
        }

        Ok(())
    }

    fn resize_without_reflow_grow_cols(&mut self, cols: CellCountInt, chunk: Chunk) -> Result<(), AllocError> {
        assert!(cols > self.cols);
        // SAFETY: the chunk node is live; all page/row accesses below stay
        // within live pages.
        unsafe {
            let page: *mut Page = nd(chunk.node).page();

            // Update our col count
            let old_cols = self.cols;
            self.cols = cols;

            // Unlikely fast path: we have capacity in the page. This
            // is only true if we resized to less cols earlier.
            if (*page).capacity.cols >= cols {
                // If any row has a spacer head at the old last column, it will
                // be invalid at the new (wider) size. Fall through to the slow
                // path which handles spacer heads correctly via cloneRowFrom.
                let rows = (*page).rows.ptr((*page).memory);
                let mut fast = true;
                for i in 0..(*page).size.rows as usize {
                    let cells = &*(*page).get_cells(&*rows.add(i));
                    if cells[old_cols as usize - 1].wide() == pagepkg::Wide::SpacerHead {
                        fast = false;
                        break;
                    }
                }

                if fast {
                    (*page).size.cols = cols;
                    return Ok(());
                }
            }

            // Likely slow path: we don't have capacity, so we need
            // to allocate a page, and copy the old data into it.

            // Try to fit our new column size into our existing page capacity.
            // If that doesn't work then use a non-standard page with the
            // given columns.
            let cap = match (*page).capacity.adjust(pagepkg::Adjustment { cols: Some(cols) }) {
                Ok(c) => c,
                Err(_) => {
                    // We verify all maxed out page layouts don't overflow,
                    let mut cap = (*page).capacity;
                    cap.cols = cols;

                    // We're growing columns so we can only get less rows so use
                    // the lesser of our capacity and size so we minimize wasted
                    // rows.
                    cap.rows = (*page).size.rows.min(cap.rows);
                    cap
                }
            };

            let prev = nd(chunk.node).prev;

            // Keeps track of all our copied rows. Assertions at the end is that
            // we copied exactly our page size.
            let mut copied: CellCountInt = 0;

            // This function has an unfortunate side effect in that it causes memory
            // fragmentation on rows if the columns are increasing in a way that
            // shrinks capacity rows. If we have pages that don't divide evenly then
            // we end up creating a final page that is not using its full capacity.
            // If this chunk isn't the last chunk in the page list, then we've created
            // a page where we'll never reclaim that capacity. This makes our max size
            // calculation incorrect since we'll throw away data even though we have
            // excess capacity. To avoid this, we try to fill our previous page
            // first if it has capacity.
            //
            // This can fail for many reasons (can't fit styles/graphemes, etc.) so
            // if it fails then we give up and drop back into creating new pages.
            if let Some(prev_node) = prev {
                'prev: {
                    let prev_page: *mut Page = nd(prev_node).page();

                    // We only want scenarios where we have excess capacity.
                    if (*prev_page).size.rows >= (*prev_page).capacity.rows {
                        break 'prev;
                    }

                    // We can copy as much as we can to fill the capacity or our
                    // current page size.
                    let len = ((*prev_page).capacity.rows - (*prev_page).size.rows).min((*page).size.rows);

                    let src_rows = (*page).rows.ptr((*page).memory);
                    let dst_rows = (*prev_page)
                        .rows
                        .ptr((*prev_page).memory)
                        .add((*prev_page).size.rows as usize);
                    for i in 0..len as usize {
                        (*prev_page).size.rows += 1;
                        copied += 1;
                        if (*prev_page)
                            .clone_row_from(page, &mut *dst_rows.add(i), &*src_rows.add(i))
                            .is_err()
                        {
                            // If an error happens, we undo our row copy and break out
                            // into creating a new page.
                            (*prev_page).size.rows -= 1;
                            copied -= 1;
                            break 'prev;
                        }
                    }

                    assert!(copied == len);
                    assert!((*prev_page).size.rows <= (*prev_page).capacity.rows);

                    // Remap any tracked pins that pointed to rows we just copied to prev.
                    for &p in self.tracked_pins.iter() {
                        let p = pinr(p);
                        if p.node != chunk.node || p.y >= len {
                            continue;
                        }
                        p.node = prev_node;
                        p.y += (*prev_page).size.rows - len;
                    }
                }
            }

            // If we have an error, we clear the rows we just added to our prev page.
            let prev_copied = copied;

            // We need to loop because our col growth may force us
            // to split pages.
            while copied < (*page).size.rows {
                let new_node = match self.create_page(CreatePage::cap(cap)) {
                    Ok(n) => n,
                    Err(e) => {
                        // We delete any of the nodes we added.
                        let mut it = nd(chunk.node).prev;
                        while let Some(node) = it {
                            if Some(node) == prev {
                                break;
                            }
                            it = nd(node).prev;
                            self.pages.remove(node);
                            self.destroy_node(node);
                        }

                        // Clear the rows we added to our prev page.
                        if prev_copied > 0 {
                            let prev_page = nd(prev.unwrap()).page();
                            let prev_size = prev_page.size.rows - prev_copied;
                            let prev_rows = prev_page.rows.ptr(prev_page.memory);
                            for i in prev_size as usize..prev_page.size.rows as usize {
                                prev_page.reset_row(&mut *prev_rows.add(i));
                            }
                            prev_page.size.rows = prev_size;
                        }

                        self.cols = old_cols;
                        return Err(e);
                    }
                };
                let new_page: *mut Page = nd(new_node).page();

                // The length we can copy into the new page is at most the number
                // of rows in our cap. But if we can finish our source page we use that.
                let len = cap.rows.min((*page).size.rows - copied);

                // Perform the copy
                let y_start = copied;
                let src_rows = (*page).rows.ptr((*page).memory).add(y_start as usize);
                let dst_rows = (*new_page).rows.ptr((*new_page).memory);
                for i in 0..len as usize {
                    (*new_page).size.rows += 1;
                    match (*new_page).clone_row_from(page, &mut *dst_rows.add(i), &*src_rows.add(i)) {
                        Ok(()) => copied += 1,
                        Err(err) => {
                            // I don't THINK this should be possible, because while our
                            // row count may diminish due to the adjustment, our
                            // prior capacity should have been sufficient to hold all the
                            // managed memory.
                            log::warn!(
                                target: LOG_TARGET,
                                "unexpected cloneRowFrom failure during resizeWithoutReflowGrowCols: {:?}",
                                err
                            );

                            // We can actually safely handle this though by exiting
                            // this loop early and cutting our copy short.
                            (*new_page).size.rows -= 1;
                            break;
                        }
                    }
                }
                let y_end = copied;
                (*new_page).assert_integrity();

                // Insert our new page
                self.pages.insert_before(chunk.node, new_node);

                // Update our tracked pins that pointed to this previous page.
                for &p in self.tracked_pins.iter() {
                    let p = pinr(p);
                    if p.node != chunk.node || p.y < y_start || p.y >= y_end {
                        continue;
                    }
                    p.node = new_node;
                    p.y -= y_start;
                }
            }
            assert!(copied == (*page).size.rows);

            // Remove the old page.
            // Deallocate the old page.
            self.pages.remove(chunk.node);
            self.destroy_node(chunk.node);
        }

        Ok(())
    }

    /// Returns the number of trailing blank lines, not to exceed max. Max
    /// is used to limit our traversal in the case of large scrollback.
    #[allow(dead_code)]
    fn trailing_blank_lines(&self, max: CellCountInt) -> CellCountInt {
        let mut count: CellCountInt = 0;

        // Go through our pages backwards since we're counting trailing blanks.
        let mut it = self.pages.last;
        // SAFETY: live list nodes.
        unsafe {
            while let Some(node) = it {
                let n = nd(node);
                let page: *mut Page = n.page();
                let page = &*page;
                let len = n.rows() as usize;
                let rows = page.rows.ptr(page.memory);
                for i in 0..len {
                    let rev_i = len - i - 1;
                    let cells = core::slice::from_raw_parts(
                        (*rows.add(rev_i)).cells().ptr(page.memory),
                        n.cols() as usize,
                    );

                    // If the row has any text then we're done.
                    if pagepkg::Cell::has_text_any(cells) {
                        return count;
                    }

                    // Inc count, if we're beyond max then we're done.
                    count += 1;
                    if count >= max {
                        return count;
                    }
                }
                it = n.prev;
            }
        }

        count
    }

    /// Trims up to max trailing blank rows from the pagelist and returns the
    /// number of rows trimmed. A blank row is any row with no text (but may
    /// have styling).
    ///
    /// IMPORTANT: This function does NOT update `total_rows`. It returns the
    /// number of rows trimmed, and the caller is responsible for decrementing
    /// `total_rows` by this amount.
    fn trim_trailing_blank_rows(&mut self, max: CellCountInt) -> CellCountInt {
        let mut trimmed: CellCountInt = 0;
        let mut invalidated_node: Option<NodePtr> = None;
        let bl_pin = self.get_bottom_right(point::Tag::Screen).unwrap();
        let mut it = bl_pin.row_iterator(Direction::LeftUp, None);
        while let Some(row_pin) = it.next() {
            // SAFETY: valid row pins from the iterator; the iterator has
            // already advanced past a node before we erase it.
            unsafe {
                let cells = &*row_pin.cells(CellSubset::All);

                // If the row has any text then we're done.
                if pagepkg::Cell::has_text_any(cells) {
                    return trimmed;
                }

                // If our tracked pins are in this row then we cannot trim it
                // because it implies some sort of importance. If we trimmed this
                // we'd invalidate this pin, as well.
                for &p in self.tracked_pins.iter() {
                    let p = pinr(p);
                    if p.node != row_pin.node || p.y != row_pin.y {
                        continue;
                    }
                    return trimmed;
                }

                // No text, we can trim this row. Because it has
                // no text we can also be sure it has no styling
                // so we don't need to worry about memory.
                if nd(row_pin.node).rows() > 1 && invalidated_node != Some(row_pin.node) {
                    // Shrinking a retained page changes its valid row-coordinate range.
                    self.invalidate_node_layout(row_pin.node);
                    invalidated_node = Some(row_pin.node);
                }

                // The row has no text but can still carry metadata (e.g. a
                // blank prompt continuation line) and background-colored
                // cells. The retired storage is re-exposed by the grow()
                // fast path without any clearing, so it must be left in the
                // default state.
                let page = nd(row_pin.node).page();
                page.reset_row(&mut *row_pin.row_and_cell().row);

                page.size.rows -= 1;
                if page.size.rows == 0 {
                    self.erase_page(row_pin.node);
                } else {
                    page.assert_integrity();
                }
            }

            trimmed += 1;
            if trimmed >= max {
                return trimmed;
            }
        }

        trimmed
    }

    /// Scroll the viewport. This will never create new scrollback, allocate
    /// pages, etc. This can only be used to move the viewport within the
    /// previously allocated pages.
    pub fn scroll(&mut self, behavior: Scroll) {
        // Special case no-scrollback mode to never allow scrolling.
        if self.limits.bytes.explicit == 0 {
            self.viewport = Viewport::Active;
            self.assert_integrity();
            return;
        }

        self.scroll_(behavior);

        // Moving the viewport changes which historical pages are visible. Restart
        // traversal so pages which leave the viewport are reconsidered after the
        // renderer's idle delay. False positives from clamped scrolling are cheap.
        self.page_compression.reset();
        self.page_compression.mark_activity();

        self.assert_integrity();
    }

    fn scroll_(&mut self, behavior: Scroll) {
        // SAFETY: the viewport pin and list nodes are live.
        unsafe {
            match behavior {
                Scroll::Active => self.viewport = Viewport::Active,
                Scroll::Top => self.viewport = Viewport::Top,
                Scroll::Pin(p) => {
                    if self.pin_is_active(p) {
                        self.viewport = Viewport::Active;
                        return;
                    } else if self.pin_is_top(p) {
                        self.viewport = Viewport::Top;
                        return;
                    }

                    *pinr(self.viewport_pin) = p;
                    self.viewport = Viewport::Pin;
                    self.viewport_pin_row_offset = None; // invalidate cache
                }
                Scroll::Row(n) => 'row: {
                    // If we're at the top, pin the top.
                    if n == 0 {
                        self.viewport = Viewport::Top;
                        break 'row;
                    }

                    // If we're below the top of the active area, pin the active area.
                    if n >= self.total_rows - self.rows as usize {
                        self.viewport = Viewport::Active;
                        break 'row;
                    }

                    // See if there are any other faster paths we can take.
                    if self.viewport == Viewport::Pin {
                        if let Some(v) = self.viewport_pin_row_offset {
                            // If we have a pin and we already calculated a row offset,
                            // then we can efficiently calculate the delta and move
                            // that much from that pin.
                            let delta: isize = n as isize - v as isize;
                            self.scroll_(Scroll::DeltaRow(delta));
                            return;
                        }
                    }

                    // We have an accurate row offset so store it to prevent
                    // calculating this again.
                    self.viewport_pin_row_offset = Some(n);
                    self.viewport = Viewport::Pin;

                    // Slow path, we've just got to traverse the linked list and
                    // get to our row. As a slight speedup, let's pick the traversal
                    // that's likely faster based on our absolute row and total rows.
                    let midpoint = self.total_rows / 2;
                    if n < midpoint {
                        // Iterate forward from the first node.
                        let mut node_it = self.pages.first;
                        let mut rem: usize = n;
                        while let Some(node) = node_it {
                            let rows = nd(node).rows() as usize;
                            if rem < rows {
                                let Ok(y) = CellCountInt::try_from(rem) else {
                                    self.viewport = Viewport::Active;
                                    break 'row;
                                };
                                *pinr(self.viewport_pin) = Pin {
                                    node,
                                    y,
                                    ..Pin::new(node)
                                };
                                break 'row;
                            }

                            rem -= rows;
                            node_it = nd(node).next;
                        }
                    } else {
                        // Iterate backwards from the last node.
                        let mut node_it = self.pages.last;
                        let mut rem: usize = self.total_rows - n;
                        while let Some(node) = node_it {
                            let rows = nd(node).rows() as usize;
                            if rem <= rows {
                                let Ok(y) = CellCountInt::try_from(rows - rem) else {
                                    self.viewport = Viewport::Active;
                                    break 'row;
                                };
                                *pinr(self.viewport_pin) = Pin {
                                    node,
                                    y,
                                    ..Pin::new(node)
                                };
                                break 'row;
                            }

                            rem -= rows;
                            node_it = nd(node).prev;
                        }
                    }

                    // If we reached here, then we couldn't find the offset.
                    // This feels impossible? Just clamp to active, screw it lol.
                    self.viewport = Viewport::Active;
                }
                Scroll::DeltaPrompt(n) => self.scroll_prompt(n),
                Scroll::DeltaRow(n) => 'delta_row: {
                    let amount: usize = n.unsigned_abs();

                    match self.viewport {
                        // If we're at the top and we're scrolling backwards,
                        // we don't have to do anything, because there's nowhere to go.
                        Viewport::Top => {
                            if n <= 0 {
                                break 'delta_row;
                            }
                        }

                        // If we're at active and we're scrolling forwards, we don't
                        // have to do anything because it'll result in staying in
                        // the active.
                        Viewport::Active => {
                            if n >= 0 {
                                break 'delta_row;
                            }
                        }

                        // If we're already a pin type, then we can fast-path our
                        // delta by simply moving the pin. This has the added benefit
                        // that we can update our row offset cache efficiently, too.
                        Viewport::Pin => match n.cmp(&0) {
                            core::cmp::Ordering::Equal => break 'delta_row,

                            core::cmp::Ordering::Less => match pinr(self.viewport_pin).up_overflow(amount) {
                                PinOverflow::Offset(new_pin) => {
                                    *pinr(self.viewport_pin) = new_pin;
                                    if let Some(v) = self.viewport_pin_row_offset.as_mut() {
                                        *v -= amount;
                                    }
                                    break 'delta_row;
                                }

                                // If we overflow up we're at the top.
                                PinOverflow::Overflow { .. } => {
                                    self.viewport = Viewport::Top;
                                    break 'delta_row;
                                }
                            },

                            core::cmp::Ordering::Greater => match pinr(self.viewport_pin).down_overflow(amount) {
                                // If we offset its a valid pin but we still have to
                                // check if we're in the active area.
                                PinOverflow::Offset(new_pin) => {
                                    if self.pin_is_active(new_pin) {
                                        self.viewport = Viewport::Active;
                                    } else {
                                        *pinr(self.viewport_pin) = new_pin;
                                        if let Some(v) = self.viewport_pin_row_offset.as_mut() {
                                            *v += amount;
                                        }
                                    }
                                    break 'delta_row;
                                }

                                // If we overflow down we're at active.
                                PinOverflow::Overflow { .. } => {
                                    self.viewport = Viewport::Active;
                                    break 'delta_row;
                                }
                            },
                        },
                    }

                    // Slow path: we have to calculate the new pin by moving
                    // from our viewport.
                    let top = self.get_top_left(point::Tag::Viewport);
                    let p: Pin = if n < 0 {
                        match top.up_overflow(amount) {
                            PinOverflow::Offset(v) => v,
                            PinOverflow::Overflow { end, .. } => end,
                        }
                    } else {
                        match top.down_overflow(amount) {
                            PinOverflow::Offset(v) => v,
                            PinOverflow::Overflow { end, .. } => end,
                        }
                    };

                    // If we are still within the active area, then we pin the
                    // viewport to active. This isn't EXACTLY the same behavior as
                    // other scrolling because normally when you scroll the viewport
                    // is pinned to _that row_ even if new scrollback is created.
                    // But in a terminal when you get to the bottom and back into the
                    // active area, you usually expect that the viewport will now
                    // follow the active area.
                    if self.pin_is_active(p) {
                        self.viewport = Viewport::Active;
                        return;
                    }

                    // If we're at the top, then just set the top. This is a lot
                    // more efficient everywhere. We must check this after the
                    // active check above because we prefer active if they overlap.
                    if self.pin_is_top(p) {
                        self.viewport = Viewport::Top;
                        return;
                    }

                    // Pin is not active so we need to track it.
                    *pinr(self.viewport_pin) = p;
                    self.viewport = Viewport::Pin;
                    self.viewport_pin_row_offset = None; // invalidate cache
                }
            }
        }
    }

    /// Jump the viewport forwards (positive) or backwards (negative) a set number of
    /// prompts (delta).
    fn scroll_prompt(&mut self, delta: isize) {
        // If we aren't jumping any prompts then we don't need to do anything.
        if delta == 0 {
            return;
        }
        let delta_start: usize = delta.unsigned_abs();
        let mut delta_rem: usize = delta_start;

        // We start at the row before or after our viewport depending on the
        // delta so that we don't land back on our current viewport.
        let start_pin = 'start: {
            let tl = self.get_top_left(point::Tag::Viewport);

            // If we're moving up we can just move the viewport up because
            // promptIterator handles jumpting to the start of prompts.
            if delta <= 0 {
                match tl.up(1) {
                    Some(p) => break 'start p,
                    None => return,
                }
            }

            // If we're moving down and we're presently at some kind of
            // prompt, we need to skip all the continuation lines because
            // promptIterator can't know if we're cutoff or continuing.
            let Some(mut adjusted) = tl.down(1) else { return };
            // SAFETY: valid pins.
            unsafe {
                if (*tl.row_and_cell().row).semantic_prompt() != pagepkg::SemanticPrompt::None {
                    while (*adjusted.row_and_cell().row).semantic_prompt() == pagepkg::SemanticPrompt::PromptContinuation {
                        match adjusted.down(1) {
                            Some(p) => adjusted = p,
                            None => break,
                        }
                    }
                }
            }

            adjusted
        };

        // Go through prompts delta times
        let mut it = start_pin.prompt_iterator(
            if delta > 0 {
                Direction::RightDown
            } else {
                Direction::LeftUp
            },
            None,
        );
        let mut prompt_pin: Option<Pin> = None;
        while let Some(next) = it.next() {
            prompt_pin = Some(next);
            delta_rem -= 1;
            if delta_rem == 0 {
                break;
            }
        }

        // If we found a prompt, we move to it. If the prompt is in the active
        // area we keep our viewport as active because we can't scroll DOWN
        // into the active area. Otherwise, we scroll up to the pin.
        if let Some(p) = prompt_pin {
            if self.pin_is_active(p) {
                self.viewport = Viewport::Active;
            } else {
                // SAFETY: live viewport pin.
                unsafe { *pinr(self.viewport_pin) = p };
                self.viewport = Viewport::Pin;
                self.viewport_pin_row_offset = None; // invalidate cache
            }
        }
    }

    /// Clear the screen by scrolling written contents up into the scrollback.
    /// This will not update the viewport.
    pub fn scroll_clear(&mut self) -> Result<(), AllocError> {
        // Go through the active area backwards to find the first non-empty
        // row. We use this to determine how many rows to scroll up.
        // SAFETY: live list nodes and rows within them.
        let non_empty: usize = unsafe {
            'non_empty: {
                let mut page = self.pages.last.unwrap();
                let mut n: usize = 0;
                loop {
                    let current_page = nd(page).page();
                    let rows = current_page.rows.ptr(current_page.memory);
                    let page_rows = nd(page).rows() as usize;
                    for i in 0..page_rows {
                        let rev_i = page_rows - i - 1;
                        let row = *rows.add(rev_i);
                        let cells = core::slice::from_raw_parts(row.cells().ptr(current_page.memory), self.cols as usize);
                        for cell in cells {
                            if !cell.is_empty() {
                                break 'non_empty self.rows as usize - n;
                            }
                        }

                        n += 1;
                        if n > self.rows as usize {
                            break 'non_empty 0;
                        }
                    }

                    match nd(page).prev {
                        Some(p) => page = p,
                        None => break 'non_empty 0,
                    }
                }
            }
        };

        // Scroll
        let r = (|| {
            for _ in 0..non_empty {
                self.grow()?;
            }
            Ok(())
        })();
        self.assert_integrity();
        r
    }

    /// Give a live node a new generation before changing its coordinate layout in
    /// place.
    ///
    /// This must be called when a node remains at the same address and stays in the
    /// list, but a mutation changes which logical row a `(node, y)` coordinate
    /// identifies or whether that coordinate is still in range. Examples include
    /// rotating rows after an erase, truncating a page's row range, reinitializing
    /// the sole remaining page, or shortening the source page of a split. Without
    /// a new generation, a cached pointer, serial, and coordinate could still pass
    /// `nodeIsValid` while referring to a different row than it originally did.
    /// Screen and Terminal fast paths which manipulate Page rows directly must use
    /// this because they bypass PageList's own row-mutation helpers.
    ///
    /// The caller must pass a node which is currently live in this PageList. Call
    /// this at the point the operation commits to changing the layout and before
    /// the first such change. When an operation is intended to be atomic, finish
    /// its failable preparation first so a failed operation does not needlessly
    /// invalidate references. One call per affected node is sufficient when an
    /// operation performs several layout changes without exposing intermediate
    /// references.
    ///
    /// This helper is only needed for in-place changes. Removing or replacing a
    /// node already invalidates old references through the live-list check in
    /// `nodeIsValid`, and newly allocated or reused nodes receive a fresh serial as
    /// part of their initialization. Ordinary cell or style changes which preserve
    /// the meaning of page coordinates do not require a new generation solely for
    /// that reason.
    ///
    /// The only state changed here is `node.serial` and the next-generation counter
    /// `page_serial`. Consequently, every previously captured pointer-plus-serial
    /// pair for this node becomes invalid while new references can capture the new
    /// generation. This does not advance `page_serial_epoch`: only `reset` starts a
    /// new whole-list validity epoch.
    ///
    /// This also does not mutate the page, update tracked pins or viewport state,
    /// adjust row or memory accounting, mark cells dirty, or notify incremental
    /// compression. The surrounding operation remains responsible for all such
    /// bookkeeping required by its layout change.
    pub fn invalidate_node_layout(&mut self, node: NodePtr) {
        // SAFETY: caller passes a live node.
        unsafe { nd(node).serial = self.page_serial };
        self.page_serial += 1;
    }

    /// Compact a page to use the minimum required memory for the contents
    /// it stores. Returns the new node pointer if compaction occurred, or null
    /// if the page was already compact or compaction would not provide any
    /// savings.
    ///
    /// The compacted page is always an exact-size heap allocation, never
    /// a pool item, since a pool item always retains a full std_size
    /// buffer regardless of the page layout. Note that this means that
    /// when compacting a pool-owned node, the freed pool item is returned
    /// to the pool free list, so the memory savings are only fully
    /// realized once the pool itself is reset or freed.
    ///
    /// If this returns OOM, the PageList is left unchanged and no dangling
    /// memory references exist. It is safe to ignore the error and continue using
    /// the uncompacted page.
    pub fn compact(&mut self, node: NodePtr) -> Result<Option<NodePtr>, AllocError> {
        let r = self.compact_(node);
        self.assert_integrity();
        r
    }

    fn compact_(&mut self, node: NodePtr) -> Result<Option<NodePtr>, AllocError> {
        // SAFETY: caller passes a live node.
        unsafe {
            let page: *mut Page = nd(node).page();

            // We should never have empty rows in our pagelist anyways...
            assert!((*page).size.rows > 0);

            // Compute the minimum capacity required for this page's content
            let req_cap = (*page).exact_row_capacity(0, (*page).size.rows as usize);
            let new_size = Page::layout(req_cap).total_size;

            // The memory this node currently retains. A pool-owned node always
            // retains a full pool item no matter its layout size.
            let old_size: usize = match nd(node).owned {
                Owned::Pool => PagePool::item_size(),
                Owned::Heap => (*page).memory.len(),
            };
            if new_size >= old_size {
                return Ok(None);
            }

            // Create the new smaller page
            let new_node = self.create_page(CreatePage {
                cap: req_cap,
                exact_size: true,
            })?;
            let new_page: *mut Page = nd(new_node).page();
            (*new_page).size = (*page).size;
            (*new_page).dirty = (*page).dirty;
            if let Err(err) = (*new_page).clone_from(page, 0, (*page).size.rows as usize) {
                // cloneFrom should not fail when compacting since req_cap is
                // computed to exactly fit the source content and our expectation
                // of exactRowCapacity ensures it can fit all the requested
                // data.
                log::error!(target: LOG_TARGET, "compact clone failed err={:?}", err);

                // In this case, let's gracefully degrade by pretending we
                // didn't need to compact.
                self.destroy_node(new_node);
                return Ok(None);
            }

            // Fix up all tracked pins to point to the new page
            for &p in self.tracked_pins.iter() {
                let p = pinr(p);
                if p.node != node {
                    continue;
                }
                p.node = new_node;
            }

            // Insert the new page and destroy the old one
            self.pages.insert_before(node, new_node);
            self.pages.remove(node);
            self.destroy_node(node);
            self.page_compression.mark_activity();

            (*new_page).assert_integrity();
            Ok(Some(new_node))
        }
    }

    /// Split the given node in the PageList at the given pin.
    ///
    /// The row at the pin and after will be moved into a new page with
    /// the same capacity as the original page. Alternatively, you can "split
    /// above" by splitting the row following the desired split row.
    ///
    /// Since the split happens below the pin, the pin remains valid.
    pub fn split(&mut self, p: Pin) -> Result<(), SplitError> {
        if build_options::SLOW_RUNTIME_SAFETY {
            assert!(self.pin_is_valid(p));
        }

        // Ran into a bug that I can only explain via aliasing. If a tracked
        // pin is passed in, its possible Zig will alias the memory and then
        // when we modify it later it updates our p here. Copying the node
        // fixes this.
        let original_node = p.node;
        // SAFETY: the pin is valid.
        unsafe {
            let page: *mut Page = nd(original_node).page();

            // A page that is already 1 row can't be split. In the future we can
            // theoretically maybe split by soft-wrapping multiple pages but that
            // seems crazy and the rest of our PageList can't handle heterogeneously
            // sized pages today.
            if (*page).size.rows <= 1 {
                return Err(SplitError::OutOfSpace);
            }

            // Splitting at row 0 is a no-op since there's nothing before the split point.
            if p.y == 0 {
                return Ok(());
            }

            // Create a new node with the same capacity of managed memory.
            let target = match self.create_page(CreatePage::cap((*page).capacity)) {
                Ok(t) => t,
                Err(e) => {
                    self.assert_integrity();
                    return Err(e.into());
                }
            };

            // Determine how many rows we're copying
            let y_start = p.y;
            let y_end = (*page).size.rows;
            nd(target).page().size.rows = y_end - y_start;
            assert!(nd(target).rows() <= nd(target).capacity().rows);

            // Copy our old data. This should NOT fail because we have the
            // capacity of the old page which already fits the data we requested.
            if let Err(err) = nd(target)
                .page()
                .clone_from(page, y_start as usize, y_end as usize)
            {
                log::error!(target: LOG_TARGET, "error cloning rows for split err={:?}", err);

                // Rather than crash, we return an OutOfSpace to show that
                // we couldn't split and let our callers gracefully handle it.
                // Realistically though... this should not happen.
                self.destroy_node(target);
                self.assert_integrity();
                return Err(SplitError::OutOfSpace);
            }

            // From this point forward there is no going back. We have no
            // error handling. It is possible but we haven't written it.

            // Failable split work is complete; shortening the source changes its row range.
            self.invalidate_node_layout(original_node);
            self.page_compression.mark_activity();

            // Move any tracked pins from the copied rows
            for &tracked in self.tracked_pins.iter() {
                let tracked = pinr(tracked);
                if nd(tracked.node).page() as *mut Page != page || tracked.y < p.y {
                    continue;
                }

                tracked.node = target;
                tracked.y -= p.y;
                // p.x remains the same since we're copying the row as-is
            }

            // Reset our rows. They are retired into unused page capacity,
            // which the grow() fast path re-exposes without any clearing, so
            // they must be left in the default state.
            let rows = (*page).rows.ptr((*page).memory);
            for y in y_start as usize..y_end as usize {
                (*page).reset_row(&mut *rows.add(y));
            }
            (*page).size.rows -= y_end - y_start;

            self.pages.insert_after(original_node, target);
        }

        self.assert_integrity();
        Ok(())
    }

    /// Return the scrollbar state for this PageList.
    ///
    /// This is amortized O(1): the total is maintained incrementally and
    /// the viewport offset is cached. The first call after the viewport
    /// moves to an arbitrary pin (e.g. scrolling to a selection) may cost
    /// O(pages) to compute the offset, after which it is cached again.
    /// See viewportRowOffset for more details.
    pub fn scrollbar(&mut self) -> Scrollbar {
        // If we have no scrollback, special case no scrollbar.
        // We need to do this because the way PageList works is that
        // it always has SOME extra space (due to the way we allocate by page).
        // So even with no scrollback we have some growth. It is architecturally
        // much simpler to just hide that for no-scrollback cases.
        if self.limits.bytes.explicit == 0 {
            return Scrollbar {
                total: self.rows as usize,
                offset: 0,
                len: self.rows as usize,
            };
        }

        Scrollbar {
            total: self.total_rows,
            offset: self.viewport_row_offset(),
            len: self.rows as usize, // Length is always rows
        }
    }

    /// Returns the offset of the current viewport from the top of the
    /// screen.
    ///
    /// This is potentially expensive to calculate because if the viewport
    /// is a pin and the pin is near the beginning of the scrollback, we
    /// will traverse a lot of linked list nodes.
    ///
    /// The result is cached so repeated calls are cheap.
    fn viewport_row_offset(&mut self) -> usize {
        match self.viewport {
            Viewport::Top => 0,
            Viewport::Active => self.total_rows - self.rows as usize,
            Viewport::Pin => {
                // Return cached value if available
                if let Some(cached) = self.viewport_pin_row_offset {
                    self.assert_integrity();
                    return cached;
                }

                // Traverse from the end and count rows until we reach the
                // viewport pin. We count backwards because most of the time
                // a user is scrolling near the active area.
                // SAFETY: live nodes and viewport pin.
                let top_offset: usize = unsafe {
                    let vp = *pinr(self.viewport_pin);
                    let mut offset: usize = 0;
                    let mut node = self.pages.last;
                    let mut found = None;
                    while let Some(n) = node {
                        offset += nd(n).rows() as usize;
                        if n == vp.node {
                            assert!(nd(n).rows() > vp.y);
                            offset -= vp.y as usize;
                            found = Some(self.total_rows - offset);
                            break;
                        }
                        node = nd(n).prev;
                    }

                    // Invalid pins are not possible.
                    found.unwrap()
                };

                // The offset is from the bottom and our cached value and this
                // function returns from the top, so we need to invert it.
                self.viewport_pin_row_offset = Some(top_offset);

                // We assert integrity on this code path because it verifies
                // that the cached value is correct.
                self.assert_integrity();
                top_offset
            }
        }
    }

    /// This fixes up the viewport data when rows are removed from the
    /// PageList. This will update a viewport to `active` if row removal
    /// puts the viewport into the active area, to `top` if the viewport
    /// is now at row 0, and updates any row offset caches as necessary.
    ///
    /// This is unit tested transitively through other tests such as
    /// eraseRows.
    fn fixup_viewport(&mut self, removed: usize) {
        // Page removal can mark every pin on the removed page as garbage. The
        // viewport pin is an internal navigation anchor that is always remapped,
        // so it remains valid after the removal.
        // SAFETY: live viewport pin.
        unsafe { pinr(self.viewport_pin).garbage = false };

        match self.viewport {
            Viewport::Active => {}

            // For pin, we check if our pin is now in the active area and if so
            // we move our viewport back to the active area.
            Viewport::Pin => {
                // SAFETY: live viewport pin.
                if self.pin_is_active(unsafe { *pinr(self.viewport_pin) }) {
                    self.viewport = Viewport::Active;
                } else if let Some(v) = self.viewport_pin_row_offset {
                    // If we have a cached row offset, we need to update it
                    // to account for the erased rows.
                    if v < removed {
                        self.viewport = Viewport::Top;
                    } else {
                        self.viewport_pin_row_offset = Some(v - removed);
                    }
                }
            }

            // For top, we move back to active if our erasing moved our
            // top page into the active area.
            Viewport::Top => {
                if self.pin_is_active(Pin::new(self.pages.first.unwrap())) {
                    self.viewport = Viewport::Active;
                }
            }
        }
    }

    /// Change the maximum logical page allocation at runtime. Null removes the
    /// explicit byte limit and zero disables scrollback.
    ///
    /// Lowering the limit immediately removes eligible complete historical pages.
    /// The effective limit may still be raised to fit the active area, and a page
    /// which overlaps the active area is never split solely to satisfy this limit.
    pub fn set_max_bytes(&mut self, max: Option<usize>) {
        self.limits.set(LimitKey::Bytes, max);
        let limits = self.limits;
        limits.enforce(self, LimitKey::Bytes);
        if self.limits.bytes.explicit == 0 {
            self.viewport = Viewport::Active;
        }
        self.assert_integrity();
    }

    /// Change the maximum number of physical scrollback rows at runtime. Null
    /// removes the explicit line limit.
    ///
    /// Lowering the limit immediately removes eligible complete historical pages.
    /// The effective limit always permits at least one standard page of history,
    /// and a page which overlaps the active area is never split for enforcement.
    pub fn set_max_lines(&mut self, max: Option<usize>) {
        self.limits.set(LimitKey::Lines, max);
        let limits = self.limits;
        limits.enforce(self, LimitKey::Lines);
        self.assert_integrity();
    }

    /// Grow the active area by exactly one row.
    ///
    /// This may allocate, but also may not if our current page has more
    /// capacity we can use. This will prune scrollback if necessary to
    /// adhere to max_size and max_lines.
    ///
    /// This returns the newly allocated page node if there is one.
    pub fn grow(&mut self) -> Result<Option<NodePtr>, AllocError> {
        let r = self.grow_();
        self.assert_integrity();
        r
    }

    fn grow_(&mut self) -> Result<Option<NodePtr>, AllocError> {
        // Growing can move a complete page behind the active boundary.
        self.page_compression.mark_activity();

        let last = self.pages.last.unwrap();
        // SAFETY: live list nodes; pins are live.
        unsafe {
            if nd(last).capacity().rows > nd(last).rows() {
                // Fast path: we have capacity in the last page. The exposed
                // row requires no clearing work here: rows in unused page
                // capacity are always in the default zero state, either
                // because the page memory was never used (pool buffers are
                // zeroed) or because whatever retired the row reset it (see
                // Page.resetRow).
                let page = nd(last).page();
                page.size.rows += 1;
                page.assert_integrity();

                // Increase our total rows by one
                self.total_rows += 1;

                // Growing inside the last page moves the active boundary without
                // allocating; that alone can make the first page wholly historical.
                let limits = self.limits;
                limits.enforce(self, LimitKey::Lines);
                return Ok(None);
            }

            // Slower path: we have no space, we need to allocate a new page.

            // Get the layout first so our failable work is done early.
            // We'll need this for both paths.
            let cap = initial_capacity(self.cols);

            // If allocation would exceed our max size, we prune the first page.
            // We don't need to reallocate because we can simply reuse that first
            // page.
            //
            // We only take this path if we have more than one page since pruning
            // reuses the popped page. It is possible to have a single page and
            // exceed the max size if that page was adjusted to be larger after
            // initial allocation.
            if self.pages.first.is_some()
                && self.pages.first != self.pages.last
                && self.page_size + PagePool::item_size() > self.limits.max(LimitKey::Bytes)
            {
                'prune: {
                    let first = self.pages.pop_first().unwrap();
                    assert!(first != last);

                    // Decrease our total row count from the pruned page
                    self.total_rows -= nd(first).rows() as usize;

                    // If our total row count is now less than our required
                    // rows then we can't prune. The "+ 1" is because we'll add one
                    // more row below.
                    if self.total_rows + 1 < self.rows as usize {
                        self.pages.prepend(first);
                        assert!(self.pages.first == Some(first));
                        self.total_rows += nd(first).rows() as usize;
                        break 'prune;
                    }

                    // If we have a pin viewport cache then we need to update it.
                    if self.viewport == Viewport::Pin {
                        if let Some(v) = self.viewport_pin_row_offset {
                            // If our offset is less than the number of rows in the
                            // pruned page, then we are now at the top.
                            if v < nd(first).rows() as usize {
                                self.viewport = Viewport::Top;
                            } else {
                                // Otherwise, our viewport pin is below what we pruned
                                // so we just decrement our offset.
                                self.viewport_pin_row_offset = Some(v - nd(first).rows() as usize);
                            }
                        }
                    }

                    // Update any tracked pins that point to this page to point to the
                    // new first page to the top-left, and mark them as garbage.
                    let new_first = self.pages.first.unwrap();
                    for &p in self.tracked_pins.iter() {
                        let p = pinr(p);
                        if p.node != first {
                            continue;
                        }
                        p.node = new_first;
                        p.y = 0;
                        p.x = 0;
                        p.garbage = true;
                    }
                    pinr(self.viewport_pin).garbage = false;

                    match nd(first).owned {
                        // Pool-owned pages are reused below.
                        Owned::Pool => {}

                        // Heap-owned pages can't be reused because they may be
                        // any size (larger or smaller than a standard page), so
                        // just destroy them.
                        Owned::Heap => {
                            self.destroy_node(first);
                            break 'prune;
                        }
                    }

                    // Reset our memory
                    let buf = nd(first).restore(RestoreMode::Discard).memory;
                    core::ptr::write_bytes(buf.ptr(), 0, buf.len());
                    assert!(buf.len() <= std_size());

                    // Initialize our new page and reinsert it as the last
                    nd(first).data = NodeData::Resident(Page::init_buf(OffsetBuf::init(buf), Page::layout(cap)));
                    let page = nd(first).page();
                    page.size.rows = 1;
                    self.pages.insert_after(last, first);
                    self.total_rows += 1;

                    // Reusing the node gives it a fresh generation. Do not begin a new
                    // page_serial_epoch here: generations are not monotonic in list order,
                    // so older live successors may have lower generations. The epoch only
                    // advances when reset invalidates the entire list.
                    nd(first).serial = self.page_serial;
                    self.page_serial += 1;

                    // In this case we do NOT need to update page_size because
                    // we're reusing an existing page so nothing has changed.

                    page.assert_integrity();

                    // Byte-limit recycling may leave history above the independent line
                    // limit, so enforce it after the recycled page becomes the new tail.
                    let limits = self.limits;
                    limits.enforce(self, LimitKey::Lines);
                    return Ok(Some(first));
                }
            }

            // We need to allocate a new memory buffer.
            let next_node = self.create_page(CreatePage::cap(cap))?;
            // we don't errdefer this because we've added it to the linked
            // list and its fine to have dangling unused pages.
            self.pages.append(next_node);
            let page = nd(next_node).page();
            page.size.rows = 1;

            // We should never be more than our max size here because we've
            // verified the case above.
            page.assert_integrity();

            // Record the increased row count
            self.total_rows += 1;

            // Appending a page can cross the line limit and can make the oldest
            // active-boundary page wholly historical.
            let limits = self.limits;
            limits.enforce(self, LimitKey::Lines);
            Ok(Some(next_node))
        }
    }

    /// Increase the capacity of the given page node in the given direction.
    /// This will always allocate a new node and remove the old node, so the
    /// existing node pointer will be invalid after this call. The newly created
    /// node on success is returned.
    ///
    /// The increase amount is at the control of the PageList implementation,
    /// but is guaranteed to always increase by at least one unit in the
    /// given dimension. Practically, we'll always increase by much more
    /// (we currently double every time) but callers shouldn't depend on that.
    /// The only guarantee is some amount of growth.
    ///
    /// Adjustment can be null if you want to recreate, reclone the page
    /// with the same capacity. This is a special case used for rehashing since
    /// the logic is otherwise the same. In this case, OutOfMemory is the
    /// only possible error.
    pub fn increase_capacity(
        &mut self,
        node: NodePtr,
        adjustment: Option<IncreaseCapacity>,
    ) -> Result<NodePtr, IncreaseCapacityError> {
        let r = self.increase_capacity_(node, adjustment);
        self.assert_integrity();
        r
    }

    fn increase_capacity_(
        &mut self,
        node: NodePtr,
        adjustment: Option<IncreaseCapacity>,
    ) -> Result<NodePtr, IncreaseCapacityError> {
        // SAFETY: caller passes a live node.
        unsafe {
            let page: *mut Page = nd(node).page();

            // Apply our adjustment
            let mut cap = (*page).capacity;
            if let Some(tag) = adjustment {
                let max_int = tag.max_int();
                let old = tag.get(&cap);

                let new: u64 = 'new: {
                    // A dimension can be zero for pages with exact
                    // capacities (see compact). Doubling zero stays zero,
                    // which would break our guarantee that we always
                    // increase by at least one unit and turn caller retry
                    // loops into infinite loops. Jump straight to the
                    // standard default instead: it is what all standard
                    // pages start with, so retrying callers are guaranteed
                    // enough room for their pending allocation.
                    if old == 0 {
                        let default = Capacity::new(0, 0);
                        break 'new tag.get(&default);
                    }

                    // We use checked math to prevent overflow. If there is
                    // an overflow it means we're out of space in this
                    // dimension, since pages can take up to their maxInt
                    // capacity in any category.
                    let doubled = old * 2;
                    if doubled <= max_int {
                        break 'new doubled;
                    }

                    // Our final doubling would overflow since maxInt is
                    // 2^N - 1 for an unsignged int of N bits. So, if we overflow
                    // and we haven't used all the bits, use all the bits.
                    if old < max_int {
                        break 'new max_int;
                    }
                    return Err(IncreaseCapacityError::OutOfSpace);
                };
                tag.set(&mut cap, new);

                // If our capacity exceeds the maximum page size, treat it
                // as an OutOfSpace because things like page splitting will
                // help.
                let layout = Page::layout(cap);
                if layout.total_size > size::MAX_PAGE_SIZE {
                    return Err(IncreaseCapacityError::OutOfSpace);
                }

                // Doubling alone has a really bad behavior in that if you're
                // in a pathological scenario with only one dimension of cap
                // increase, you have to pay repeat double costs. Each time you
                // double we have to reclone the entire page. As the page gets
                // bigger this gets more expensive.
                //
                // Instead, for some dimensions, we do something else: we look
                // at the current utilization, and project that to every row
                // in the page capacity (if it fits). We just assume that your
                // future workload will look the one you're currently filling.
                // If we're wrong, its some wasted capacity but future growth
                // still works in other dimensions.
                //
                // This only applies to the current page being grown. I previously
                // tried a high-water-mark style solution to preallocate pages,
                // which does work really well, but its not clear when you reset
                // the mark.
                'project: {
                    // The dimensions we project must measure their current
                    // usage in the same units as the capacity field.
                    let used: u64 = match tag {
                        IncreaseCapacity::GraphemeBytes => (*page).grapheme_alloc.used_bytes((*page).memory) as u64,

                        // Living item count. Note the capacity field is a
                        // requested item count that Layout.init rounds in
                        // both directions (table to the next power of two,
                        // items to the load factor of that), so this is an
                        // approximation in capacity units; the headroom
                        // below absorbs the error and an undershoot only
                        // costs one more (non-ladder) growth event.
                        IncreaseCapacity::Styles => (*page).styles.count() as u64,

                        _ => break 'project,
                    };
                    if used == 0 || (*page).size.rows == 0 {
                        break 'project;
                    }

                    // Full-page need at current density, plus 25% headroom
                    // for chunk rounding and fragmentation.
                    let density = used * cap.rows as u64 / (*page).size.rows as u64;
                    let projected_raw = density + density / 4;

                    // Bound the jump to 32× the pre-growth capacity. Arbitrary
                    // choice until we can show otherwise.
                    let projected = projected_raw.min(old * 32).min(max_int);
                    if projected <= tag.get(&cap) {
                        break 'project;
                    }

                    // Only take the projection if the resulting page still fits.
                    let mut proj_cap = cap;
                    tag.set(&mut proj_cap, projected);
                    if Page::layout(proj_cap).total_size > size::MAX_PAGE_SIZE {
                        break 'project;
                    }
                    cap = proj_cap;
                }
            }

            log::debug!(target: LOG_TARGET, "adjusting page capacity={:?}", cap);

            // Create our new page and clone the old page into it.
            let new_node = self.create_page(CreatePage::cap(cap))?;
            let new_page: *mut Page = nd(new_node).page();
            assert!((*new_page).capacity.rows >= (*page).capacity.rows);
            assert!((*new_page).capacity.cols >= (*page).capacity.cols);
            (*new_page).size.rows = (*page).size.rows;
            (*new_page).size.cols = (*page).size.cols;
            if let Err(err) = (*new_page).clone_from(page, 0, (*page).size.rows as usize) {
                // cloneFrom only errors if there isn't capacity for the data
                // from the source page but we're only increasing capacity so
                // this should never be possible. If it happens, we should crash
                // because we're in no man's land and can't safely recover.
                log::error!(target: LOG_TARGET, "increaseCapacity clone failed err={:?}", err);
                panic!("unexpected clone failure");
            }

            // Preserve page-level dirty flag (cloneFrom only copies row data)
            (*new_page).dirty = (*page).dirty;

            // Must not fail after this because the operations we do after this
            // can't be recovered.

            // Fix up all our tracked pins to point to the new page.
            for &p in self.tracked_pins.iter() {
                let p = pinr(p);
                if p.node != node {
                    continue;
                }
                p.node = new_node;
            }

            // Insert this page and destroy the old page
            self.pages.insert_before(node, new_node);
            self.pages.remove(node);
            self.destroy_node(node);
            self.page_compression.mark_activity();

            (*new_page).assert_integrity();
            Ok(new_node)
        }
    }

    /// Allocate a new page using the PageList's memory pools.
    ///
    /// The page is detached: it doesn't contribute to the memory limits or
    /// row counts or anything in the PageList. The caller must call `finalize`
    /// to add it to the PageList at the appropriate place, or `deinit` (drop)
    /// to throw it away.
    pub fn allocate_page(&mut self, capacity: Capacity) -> Result<PageAllocation, AllocError> {
        let node = create_page_ext(&mut self.pool, CreatePage::cap(capacity), &mut self.page_serial, None)?;
        Ok(PageAllocation {
            destination: self as *mut PageList,
            node: Some(node),
        })
    }

    /// Create a new page node. This does not add it to the list and this
    /// does not do any memory size accounting with max_size/page_size.
    #[inline]
    fn create_page(&mut self, opts: CreatePage) -> Result<NodePtr, AllocError> {
        // If we have a node available for recycling (only during reflow,
        // see recycle_node), reuse it directly rather than going through
        // the memory pool.
        if let Some(node) = self.recycle_node {
            'recycle: {
                // SAFETY: the recycle node is live (removed from the list).
                unsafe {
                    // Only a standard pool-owned resident node can be rebuilt
                    // in place for a standard-size layout.
                    if opts.exact_size {
                        break 'recycle;
                    }
                    if nd(node).owned != Owned::Pool {
                        break 'recycle;
                    }
                    if nd(node).storage() != Storage::Resident {
                        break 'recycle;
                    }
                    let layout = Page::layout(opts.cap);
                    if layout.total_size > std_size() {
                        break 'recycle;
                    }

                    self.recycle_node = None;

                    // The pool guarantees that buffers it hands out are zeroed.
                    // A pool-owned page dirties only its Page.memory prefix of
                    // the underlying standard-size item, so zeroing that prefix
                    // re-establishes the guarantee (this mirrors destroyNodeExt,
                    // minus the decommit).
                    let page = *nd(node).page_assume_resident();
                    let item = PageMemory::from_raw_parts(page.memory.ptr(), std_size());
                    core::ptr::write_bytes(page.memory.ptr(), 0, page.memory.len());

                    // Accounting: a pool-owned node always accounts for a full
                    // pool item in page_size, so destroying the node and
                    // creating a new pooled one is a net zero.

                    node.as_ptr().write(Node {
                        prev: None,
                        next: None,
                        data: NodeData::Resident(Page::init_buf(OffsetBuf::init(item), layout)),
                        serial: self.page_serial,
                        owned: Owned::Pool,
                    });
                    nd(node).page().size.rows = 0;
                    self.page_serial += 1;
                    return Ok(node);
                }
            }
        }

        create_page_ext(&mut self.pool, opts, &mut self.page_serial, Some(&mut self.page_size))
    }

    /// Compress eligible nodes, saving a significant amount of memory.
    ///
    /// Eligible nodes are complete pages before the active boundary which do not
    /// intersect the viewport. The boundary page is excluded because it may
    /// contain both scrollback and active rows; visible pages remain resident for
    /// immediate redraw and scrolling.
    ///
    /// Compression requires a system that supports reclaiming physical memory for
    /// virtual allocations while retaining their address ranges.
    ///
    /// Compression is SLOW (relatively), so incremental compression during idle
    /// periods is recommended. Incremental mode performs one bounded step and its
    /// result specifies whether to continue immediately. Drain mode performs
    /// incremental steps until the pass and its verification pass finish. Full
    /// mode visits every currently eligible node once without using incremental
    /// state.
    ///
    /// PageList tracks mutations which require a later incremental pass in its
    /// compression state. On supported targets, full compression returns
    /// `complete`, indicating that it has no continuation to schedule rather than
    /// that every page was compressed.
    pub fn compress(&mut self, mode: CompressMode) -> IncrementalCompressionResult {
        match mode {
            CompressMode::Incremental => self.compress_incremental(),
            CompressMode::Drain => loop {
                match self.compress_incremental() {
                    IncrementalCompressionResult::Pending => continue,
                    IncrementalCompressionResult::Complete => break IncrementalCompressionResult::Complete,
                    IncrementalCompressionResult::Unsupported => break IncrementalCompressionResult::Unsupported,
                }
            },
            CompressMode::Full => {
                // Match incremental mode's unsupported result. Full compression
                // has no useful work to perform without strict reclamation.
                if !terminal_mem::can_reclaim(DecommitMode::Strict) {
                    self.page_compression.reset();
                    return IncrementalCompressionResult::Unsupported;
                }

                self.compress_full();

                // Full compression has no continuation. Discard any partial
                // incremental cursor so later activity starts at the oldest page.
                self.page_compression.reset();

                IncrementalCompressionResult::Complete
            }
        }
    }

    /// Perform one candidate-bounded incremental cold-history compression step.
    fn compress_incremental(&mut self) -> IncrementalCompressionResult {
        // If we can't reclaim virtual memory, compression is unsupported.
        if !terminal_mem::can_reclaim(DecommitMode::Strict) {
            self.page_compression.reset();
            return IncrementalCompressionResult::Unsupported;
        }

        // Find the node following the exact continuation marker within the cold
        // prefix. A missing marker means the list changed between steps, so begin
        // again at the current first page. This lookup does not touch page memory.
        let mut it = CompressionIterator::init(self);
        if let Some(last_serial) = self.page_compression.last_serial {
            let mut found = false;
            while let Some(node) = it.next() {
                // SAFETY: live list node.
                let serial = unsafe { nd(node).serial };

                // A newly allocated or replacement node appeared before the
                // marker. Restart immediately so that node cannot be skipped.
                if serial >= self.page_compression.next_serial {
                    break;
                }

                // Not a match? Keep looking
                if serial != last_serial {
                    continue;
                }

                // Match! The iterator already points to the next offscreen node.
                found = true;
                break;
            }

            if !found {
                // Not found or otherwise invalid. Reset
                self.page_compression.last_serial = None;
                it = CompressionIterator::init(self);
            }
        }

        // Keep track of our next_serial
        self.page_compression.next_serial = self.page_serial;

        // We cap the number of pages we look at to do our best to
        // time-bound the incremental compression.
        let mut inspected_pages: usize = 0;
        while inspected_pages < INCREMENTAL_COMPRESSION_MAX_INSPECTED {
            let Some(node) = it.next() else { break };

            // SAFETY: live list node.
            self.page_compression.last_serial = Some(unsafe { nd(node).serial });
            inspected_pages += 1;

            // If this page is already compressed, ignore it.
            // SAFETY: live list node.
            if unsafe { nd(node).is_compressed() } {
                continue;
            }

            // Compression is substantially more expensive even if it fails.
            // So we just try it.
            if self.compress_page(node) {
                self.page_compression.flags.did_compress = true;
            }
            break;
        }

        // If we didn't reach our active node, then we still have work to do.
        if !it.done() {
            return IncrementalCompressionResult::Pending;
        }

        // We reached our active node. So we're done, except that we always
        // do one pass after the first success so we can recompress nodes that
        // were possibly decompressed (e.g. by search, inspector, whatever).
        let state = &mut self.page_compression;
        if !state.flags.verifying || state.flags.did_compress {
            let activity_serial = state.activity_serial;
            *state = IncrementalCompressionState {
                flags: IncrementalCompressionFlags {
                    verifying: true,
                    did_compress: false,
                },
                activity_serial,
                ..Default::default()
            };
            return IncrementalCompressionResult::Pending;
        }

        // Leave the state fresh while idle so later activity naturally begins at
        // the oldest cold page, including pages restored after this pass.
        state.reset();
        IncrementalCompressionResult::Complete
    }

    /// Compress every fully historical resident page which is currently cold.
    fn compress_full(&mut self) {
        let mut it = CompressionIterator::init(self);
        while let Some(node) = it.next() {
            // Don't restore an already-compressed page just to recompress it.
            // SAFETY: live list node.
            if unsafe { nd(node).is_compressed() } {
                continue;
            }

            // Failure leaves this node resident and unchanged.
            self.compress_page(node);
        }
    }

    /// Attempt to compress one resident page while retaining its raw mapping.
    ///
    /// Compression is opportunistic: every failure leaves the page resident and
    /// usable. Candidate selection and retry policy belong to `compress`; this
    /// primitive only performs one state transition.
    pub(crate) fn compress_page(&mut self, node: NodePtr) -> bool {
        // SAFETY: caller passes a live node.
        let n = unsafe { nd(node) };

        // Recompression requires first restoring the raw page and is a policy
        // decision, so this primitive only accepts resident nodes.
        if n.is_compressed() {
            return false;
        }

        let page: *mut Page = n.page();
        // SAFETY: resident page.
        let memory_len = unsafe { (*page).memory.len() };

        // The scratch size is capped just below the representation's break-even
        // point. Codec limits and pages too small to cover the compressed-state
        // overhead simply make this page ineligible for compression.
        let required = match compression::Page::required_scratch(memory_len) {
            Ok(v) => v,
            Err(_) => return false,
        };
        if required == 0 {
            return false;
        }

        // Build the compressed candidate without changing the node. This scope is
        // intentional: releasing the borrowed or temporary scratch happens before
        // we attempt to discard the source mapping below. Only the candidate's
        // exact-sized encoded allocation survives the scope.
        let candidate = {
            let mut scratch = match CompressionScratch::init(&mut self.pool, required, memory_len) {
                Ok(s) => s,
                Err(_) => return false,
            };

            // The encoder writes at most `required` bytes and reports exactly
            // how many on success. Track that so returning the scratch only
            // clears the prefix it dirtied instead of the whole item.
            let mut dirty_len: usize = required;

            let mut table: compression::lz4::HashTable = [0; 4096];
            let bytes = scratch.bytes();
            // SAFETY: resident page.
            let result = compression::Page::init(self.pool.alloc, unsafe { &*page }, &mut bytes[..required], &mut table);
            let result = match result {
                Ok(r) => r,
                Err(_) => {
                    scratch.deinit(&mut self.pool, dirty_len);
                    return false;
                }
            };
            if let Some(compressed) = &result {
                dirty_len = compressed.encoded.len();
            }
            scratch.deinit(&mut self.pool, dirty_len);
            result
        };

        // Null means compression crossed the break-even point. The node and its
        // resident mapping are still untouched in this case.
        let Some(mut compressed) = candidate else { return false };

        // Strict decommit is the final fallible step. It either discards the whole
        // raw mapping or leaves it untouched, so failure can safely free the
        // candidate and preserve the resident node exactly as it was.
        let decommit_allowed = COMPRESS_PAGE_TW.check(CompressPageFailPoint::Decommit).is_ok();
        if !decommit_allowed
            || !terminal_mem::decommit(
                DecommitMode::Strict,
                // SAFETY: the page mapping is owned by this node.
                unsafe { compressed.page.memory.as_mut_slice() },
                compressed.page.memory.len(),
            )
        {
            compressed.deinit();
            return false;
        }

        // Publish the new state only after both the encoded allocation and the
        // retained-mapping decommit have succeeded.
        n.data = NodeData::Compressed(compressed);
        true
    }

    /// Destroy the memory of the given node in the PageList linked list
    /// and return it to the pool. The node is assumed to already be removed
    /// from the linked list.
    ///
    /// IMPORTANT: This function does NOT update `total_rows`. The caller is
    /// responsible for accounting for the removed rows. This function only
    /// updates `page_size` (byte accounting), not row accounting.
    fn destroy_node(&mut self, node: NodePtr) {
        destroy_node_ext(&mut self.pool, node, Some(&mut self.page_size));
    }

    /// Clone the given source row into the row at `dst_y` of the given
    /// node's page, increasing the node's capacity as necessary to fit the
    /// source row's managed memory (styles, hyperlinks, etc.).
    ///
    /// Since increasing capacity replaces the node in the page list, the
    /// (possibly replaced) node is returned and the caller must use it in
    /// place of the old node. The source must NOT be on the given node
    /// since the node's page memory may be freed on capacity increase.
    fn clone_row_grow_capacity(&mut self, node: NodePtr, dst_y: usize, src_page: *mut Page, src_row: *const Row) -> NodePtr {
        // SAFETY: live nodes; the source page is a different live page.
        unsafe {
            assert!(src_page != nd(node).page() as *mut Page);

            let mut current = node;
            loop {
                let cur_page = nd(current).page();
                let cur_rows = cur_page.rows.ptr(cur_page.memory);
                if let Err(err) = cur_page.clone_row_from(src_page, &mut *cur_rows.add(dst_y), &*src_row) {
                    // Adjust our page capacity to make room for what we
                    // didn't have space for.
                    current = match self.increase_capacity(current, IncreaseCapacity::for_clone_error(err)) {
                        Ok(n) => n,
                        // We can't gracefully recover from either of these
                        // here: our callers have already rotated rows, so
                        // returning an error would leave the page list
                        // half-mutated (and corrupt), so a crash is better.
                        Err(IncreaseCapacityError::OutOfMemory) => panic!("increaseCapacity system allocator OOM"),
                        Err(IncreaseCapacityError::OutOfSpace) => panic!("increaseCapacity OutOfSpace"),
                    };

                    // Retry the row copy with the increased capacity.
                    continue;
                }

                return current;
            }
        }
    }

    /// Fast-path function to erase exactly 1 row. Erasing means that the row
    /// is completely REMOVED, not just cleared. All rows following the removed
    /// row will be shifted up by 1 to fill the empty space.
    ///
    /// Unlike eraseRows, eraseRow does not change the size of any pages. The
    /// caller is responsible for adjusting the row count of the final page if
    /// that behavior is required.
    pub fn erase_row(&mut self, pt: point::Point) {
        let pn = self.pin(pt).unwrap();

        // SAFETY: live nodes, rows and pins.
        unsafe {
            let mut node = pn.node;
            let mut page: *mut Page = nd(node).page();
            let mut rows = (*page).rows.ptr((*page).memory);

            // Erasing history may restore compressed pages. Mark unconditionally
            // because incrementing the activity token is cheaper than locating the
            // active boundary.
            self.page_compression.mark_activity();

            // In order to move the following rows up we rotate the rows array by 1.
            // The rotate operation turns e.g. [ 0 1 2 3 ] in to [ 1 2 3 0 ], which
            // works perfectly to move all of our elements where they belong.
            // Rotating rows changes which logical row cached coordinates identify.
            self.invalidate_node_layout(node);
            fastmem::rotate_once(core::slice::from_raw_parts_mut(
                rows.add(pn.y as usize),
                nd(node).rows() as usize - pn.y as usize,
            ));

            // We adjust the tracked pins in this page, moving up any that were below
            // the removed row.
            for &p in self.tracked_pins.iter() {
                let p = pinr(p);
                if p.node == node && p.y > pn.y {
                    p.y -= 1;
                }
            }

            // If we have a pinned viewport, we need to adjust for active area.
            self.fixup_viewport(1);

            // Mark the whole page as dirty.
            //
            // Technically we only need to mark rows from the erased row to the end
            // of the page as dirty, but that's slower and this is a hot function.
            (*page).dirty = true;

            // We iterate through all of the following pages in order to move their
            // rows up by 1 as well.
            while let Some(next) = nd(node).next {
                let next_page: *mut Page = nd(next).page();
                let next_rows = (*next_page).rows.ptr((*next_page).memory);

                // We take the top row of the page and clone it in to the bottom
                // row of the previous page, which gets rid of the top row that was
                // rotated down in the previous page, and accounts for the row in
                // this page that will be rotated down as well.
                //
                //  rotate -> clone --> rotate -> result
                //    0 -.      1         1         1
                //    1  |      2         2         2
                //    2  |      3         3         3
                //    3 <'      0 <.      4         4
                //   ---       --- |     ---       ---  <- page boundary
                //    4         4 -'      4 -.      5
                //    5         5         5  |      6
                //    6         6         6  |      7
                //    7         7         7 <'      4
                //
                // The copy may replace the destination node in order to
                // increase its capacity. We can discard the replacement
                // because we advance to the next node below, and the
                // replacement is already linked in its place (so e.g. the
                // `node.prev` access in the pin fixups below is correct).
                let _ = self.clone_row_grow_capacity(node, nd(node).rows() as usize - 1, next_page, next_rows);

                node = next;
                page = next_page;
                rows = next_rows;

                // Rotating this page moves every cached row coordinate up by one.
                self.invalidate_node_layout(node);
                fastmem::rotate_once(core::slice::from_raw_parts_mut(rows, nd(node).rows() as usize));

                // Mark the whole page as dirty.
                (*page).dirty = true;

                // Our tracked pins for this page need to be updated.
                // If the pin is in row 0 that means the corresponding row has
                // been moved to the previous page. Otherwise, move it up by 1.
                for &p in self.tracked_pins.iter() {
                    let p = pinr(p);
                    if p.node != node {
                        continue;
                    }
                    if p.y == 0 {
                        p.node = nd(node).prev.unwrap();
                        p.y = nd(p.node).rows() - 1;
                        continue;
                    }
                    p.y -= 1;
                }
            }

            // Reset the final row which was rotated from the top of the page.
            // A full reset (not just clearing cells) so no metadata from the
            // erased row is retained by the new blank row.
            (*page).reset_row(&mut *rows.add(nd(node).rows() as usize - 1));
        }

        self.assert_integrity();
    }

    /// A variant of eraseRow that shifts only a bounded number of following
    /// rows up, filling the space they leave behind with blank rows.
    ///
    /// `limit` is exclusive of the erased row. A limit of 1 will erase the target
    /// row and shift the row below in to its position, leaving a blank row below.
    pub fn erase_row_bounded(&mut self, pt: point::Point, limit: usize) {
        self.erase_row_bounded_(pt, limit);
        self.assert_integrity();
    }

    fn erase_row_bounded_(&mut self, pt: point::Point, limit: usize) {
        // This function has a lot of repeated code in it because it is a hot path.
        //
        // To get a better idea of what's happening, read eraseRow first for more
        // in-depth explanatory comments. To avoid repetition, the only comments for
        // this function are for where it differs from eraseRow.

        let pn = self.pin(pt).unwrap();

        // SAFETY: live nodes, rows and pins.
        unsafe {
            let mut node: NodePtr = pn.node;
            let mut page: *mut Page = nd(node).page();
            let mut rows = (*page).rows.ptr((*page).memory);

            // Erasing history may restore compressed pages. Mark unconditionally
            // because incrementing the activity token is cheaper than locating the
            // active boundary.
            self.page_compression.mark_activity();

            // If the row limit is less than the remaining rows before the end of the
            // page, then we clear the row, rotate it to the end of the boundary limit
            // and update our pins.
            if nd(node).rows() as usize - pn.y as usize > limit {
                // Rotating this bounded region changes its cached row coordinates.
                self.invalidate_node_layout(node);
                (*page).reset_row(&mut *rows.add(pn.y as usize));
                fastmem::rotate_once(core::slice::from_raw_parts_mut(rows.add(pn.y as usize), limit + 1));

                // Mark the whole page as dirty.
                //
                // Technically we only need to mark from the erased row to the
                // limit but this is a hot function, so we want to minimize work.
                (*page).dirty = true;

                // If our viewport is a pin and our pin is within the erased
                // region we need to maybe shift our cache up. We do this here instead
                // of in the pin loop below because its unlikely to be true and we
                // don't want to run the conditional N times.
                if self.viewport == Viewport::Pin {
                    if let Some(v) = self.viewport_pin_row_offset.as_mut() {
                        let p = *pinr(self.viewport_pin);
                        if !(p.node != node
                            || (p.y as usize) < pn.y as usize
                            || p.y as usize > pn.y as usize + limit
                            || p.y == 0)
                        {
                            *v -= 1;
                        }
                    }
                }

                // Update pins in the shifted region.
                for &p in self.tracked_pins.iter() {
                    let p = pinr(p);
                    if p.node == node && p.y >= pn.y && p.y as usize <= pn.y as usize + limit {
                        if p.y == 0 {
                            p.x = 0;
                        } else {
                            p.y -= 1;
                        }
                    }
                }

                return;
            }

            // Rotating this suffix changes which logical row its coordinates identify.
            self.invalidate_node_layout(node);
            fastmem::rotate_once(core::slice::from_raw_parts_mut(
                rows.add(pn.y as usize),
                nd(node).rows() as usize - pn.y as usize,
            ));

            // Mark the whole page as dirty.
            //
            // Technically we only need to mark rows from the erased row to the end
            // of the page as dirty, but that's slower and this is a hot function.
            (*page).dirty = true;

            // We need to keep track of how many rows we've shifted so that we can
            // determine at what point we need to do a partial shift on subsequent
            // pages.
            let mut shifted: usize = nd(node).rows() as usize - pn.y as usize;

            // Update tracked pins.
            {
                // See the other places we do something similar in this function
                // for a detailed explanation.
                if self.viewport == Viewport::Pin {
                    if let Some(v) = self.viewport_pin_row_offset.as_mut() {
                        let p = *pinr(self.viewport_pin);
                        if !(p.node != node || p.y < pn.y || p.y == 0) {
                            *v -= 1;
                        }
                    }
                }

                for &p in self.tracked_pins.iter() {
                    let p = pinr(p);
                    if p.node == node && p.y >= pn.y {
                        if p.y == 0 {
                            p.x = 0;
                        } else {
                            p.y -= 1;
                        }
                    }
                }
            }

            while let Some(next) = nd(node).next {
                let next_page: *mut Page = nd(next).page();
                let next_rows = (*next_page).rows.ptr((*next_page).memory);

                // The copy may replace the destination node in order to
                // increase its capacity. We can discard the replacement
                // because we advance to the next node below, and the
                // replacement is already linked in its place (so e.g. the
                // `node.prev` access in the pin fixups below is correct).
                let _ = self.clone_row_grow_capacity(node, nd(node).rows() as usize - 1, next_page, next_rows);

                node = next;
                page = next_page;
                rows = next_rows;

                // We check to see if this page contains enough rows to satisfy the
                // specified limit, accounting for rows we've already shifted in prior
                // pages.
                //
                // The logic here is very similar to the one before the loop.
                let shifted_limit = limit - shifted;
                if nd(node).rows() as usize > shifted_limit {
                    // Rotating this bounded prefix changes its cached row coordinates.
                    self.invalidate_node_layout(node);
                    (*page).reset_row(&mut *rows);
                    fastmem::rotate_once(core::slice::from_raw_parts_mut(rows, shifted_limit + 1));

                    // Mark the whole page as dirty.
                    //
                    // Technically we only need to mark from the erased row to the
                    // limit but this is a hot function, so we want to minimize work.
                    (*page).dirty = true;

                    // See the other places we do something similar in this function
                    // for a detailed explanation.
                    if self.viewport == Viewport::Pin {
                        if let Some(v) = self.viewport_pin_row_offset.as_mut() {
                            let p = *pinr(self.viewport_pin);
                            if !(p.node != node || p.y as usize > shifted_limit) {
                                *v -= 1;
                            }
                        }
                    }

                    // Update pins in the shifted region.
                    for &p in self.tracked_pins.iter() {
                        let p = pinr(p);
                        if p.node != node || p.y as usize > shifted_limit {
                            continue;
                        }
                        if p.y == 0 {
                            p.node = nd(node).prev.unwrap();
                            p.y = nd(p.node).rows() - 1;
                            continue;
                        }
                        p.y -= 1;
                    }

                    return;
                }

                // Rotating the whole page moves every cached row coordinate up by one.
                self.invalidate_node_layout(node);
                fastmem::rotate_once(core::slice::from_raw_parts_mut(rows, nd(node).rows() as usize));

                // Mark the whole page as dirty.
                (*page).dirty = true;

                // Account for the rows shifted in this node.
                shifted += nd(node).rows() as usize;

                // See the other places we do something similar in this function
                // for a detailed explanation.
                if self.viewport == Viewport::Pin {
                    if let Some(v) = self.viewport_pin_row_offset.as_mut() {
                        let p = *pinr(self.viewport_pin);
                        if p.node == node {
                            *v -= 1;
                        }
                    }
                }

                // Update tracked pins.
                for &p in self.tracked_pins.iter() {
                    let p = pinr(p);
                    if p.node != node {
                        continue;
                    }
                    if p.y == 0 {
                        p.node = nd(node).prev.unwrap();
                        p.y = nd(p.node).rows() - 1;
                        continue;
                    }
                    p.y -= 1;
                }
            }

            // We reached the end of the page list before the limit, so we reset
            // the final row since it was rotated down from the top of this page.
            (*page).reset_row(&mut *rows.add(nd(node).rows() as usize - 1));
        }
    }

    /// Erase all history rows, optionally up to a bottom-left bound.
    /// This always starts from the beginning of the history area.
    pub fn erase_history(&mut self, bl_pt: Option<point::Point>) {
        self.erase_rows(point::Point::History(point::Coordinate::default()), bl_pt);
    }

    /// Erase active area rows, from the top of the active area to the
    /// given row (inclusive).
    pub fn erase_active(&mut self, y: CellCountInt) {
        assert!(y < self.rows);
        self.erase_rows(
            point::Point::Active(point::Coordinate::default()),
            Some(point::Point::Active(point::Coordinate { x: 0, y: y as u32 })),
        );
    }

    /// Erase rows from tl_pt to bl_pt (inclusive), physically removing
    /// them rather than just clearing their contents. If a point falls
    /// in the middle of a page, remaining rows in that page are shifted
    /// and the page becomes underutilized (size < capacity).
    ///
    /// Callers must ensure that the erased range only removes pages from
    /// the front or back of the linked list, never the middle. The pin and row
    /// accounting in this operation is only defined for those boundary ranges.
    /// Use the public eraseHistory/eraseActive wrappers which enforce this.
    fn erase_rows(&mut self, tl_pt: point::Point, bl_pt: Option<point::Point>) {
        self.erase_rows_(tl_pt, bl_pt);
        self.assert_integrity();
    }

    fn erase_rows_(&mut self, tl_pt: point::Point, bl_pt: Option<point::Point>) {
        self.page_compression.mark_activity();

        // The count of rows that was erased.
        let mut erased: usize = 0;

        // Erased history rows are gone for good, so mark the pins that pointed
        // to them as garbage.
        let discard = matches!(tl_pt, point::Point::History(_));

        // A pageIterator iterates one page at a time from the back forward.
        // "back" here is in terms of scrollback, but actually the front of the
        // linked list.
        let mut it = self.page_iterator(Direction::RightDown, tl_pt, bl_pt);
        // SAFETY: live chunk nodes; the iterator has already advanced past a
        // node before it is erased.
        unsafe {
            while let Some(chunk) = it.next() {
                // If the chunk is a full page, deinit thit page and remove it from
                // the linked list.
                if chunk.full_page() {
                    // A rare special case is that we're deleting everything
                    // in our linked list. erasePage requires at least one other
                    // page so to handle this we reinit this page, set it to zero
                    // size which will let us grow our active area back.
                    if nd(chunk.node).next.is_none() && nd(chunk.node).prev.is_none() {
                        // Reinitializing the sole page invalidates every coordinate in it.
                        self.invalidate_node_layout(chunk.node);
                        let page = nd(chunk.node).page();
                        erased += page.size.rows as usize;
                        page.reinit();
                        page.size.rows = 0;
                        break;
                    }

                    erased += nd(chunk.node).rows() as usize;
                    if discard {
                        for &p in self.tracked_pins.iter() {
                            let p = pinr(p);
                            if p.node == chunk.node {
                                p.garbage = true;
                            }
                        }
                    }
                    self.erase_page(chunk.node);
                    continue;
                }

                // Moving the retained suffix changes every captured row coordinate.
                self.invalidate_node_layout(chunk.node);

                // We are modifying our chunk so make sure it is in a good state.
                let page = nd(chunk.node).page();

                // The chunk is not a full page so we need to move the rows.
                // This is a cheap operation because we're just moving cell offsets,
                // not the actual cell contents.
                assert!(chunk.start == 0);
                let rows = page.rows.ptr(page.memory);
                let scroll_amount = nd(chunk.node).rows() as usize - chunk.end as usize;
                for i in 0..scroll_amount {
                    let src = rows.add(i + chunk.end as usize);
                    let dst = rows.add(i);
                    let old_dst = *dst;
                    *dst = *src;
                    *src = old_dst;

                    // Mark the moved row as dirty.
                    (*dst).set_dirty(true);
                }

                // Reset our remaining rows that we didn't shift or swapped.
                // These are retired into unused page capacity, which the
                // grow() fast path re-exposes without any clearing, so they
                // must be left in the default state.
                for i in scroll_amount..nd(chunk.node).rows() as usize {
                    page.reset_row(&mut *rows.add(i));
                }

                // Update any tracked pins to shift their y. If it was in the erased
                // row then we move it to the top of this page.
                for &p in self.tracked_pins.iter() {
                    let p = pinr(p);
                    if p.node != chunk.node {
                        continue;
                    }
                    if p.y >= chunk.end {
                        p.y -= chunk.end;
                    } else {
                        p.y = 0;
                        p.x = 0;
                        if discard {
                            p.garbage = true;
                        }
                    }
                }

                // Our new size is the amount we scrolled
                page.size.rows = scroll_amount as CellCountInt;
                erased += chunk.end as usize;
                page.assert_integrity();
            }
        }

        // Update our total row count
        self.total_rows -= erased;

        // If we deleted active, we need to regrow because one of our invariants
        // is that we always have full active space.
        if matches!(tl_pt, point::Point::Active(_)) {
            for _ in 0..erased {
                if let Err(err) = self.grow() {
                    // If this fails its a pretty big issue actually... but I don't
                    // want to turn this function into an error-returning function
                    // because erasing active is so rare and even if it happens failing
                    // is even more rare...
                    log::error!(target: LOG_TARGET, "failed to regrow active area after erase err={:?}", err);
                    return;
                }
            }
        }

        // If we have a pinned viewport, we need to adjust for active area.
        self.fixup_viewport(erased);
    }

    /// Erase a single page, freeing all its resources. The page must be
    /// at the front or back of the linked list (not the middle) and must
    /// NOT be the final page in the entire list (i.e. must not make the
    /// list empty).
    ///
    /// IMPORTANT: This function does NOT update `total_rows`. The caller is
    /// responsible for accounting for the removed rows before or after calling
    /// this function.
    fn erase_page(&mut self, node: NodePtr) {
        // SAFETY: caller passes a live node; tracked pins are live.
        unsafe {
            let n = nd(node);

            // Must not be the final page.
            assert!(n.next.is_some() || n.prev.is_some());

            // We only support erasing from the front or back, never the middle. The
            // public erase operations maintain this contract by construction.
            assert!(n.prev.is_none() || n.next.is_none());

            // Update any tracked pins to move to the previous or next page.
            let target = n.prev.or(n.next).unwrap();
            for &p in self.tracked_pins.iter() {
                let p = pinr(p);
                if p.node != node {
                    continue;
                }
                p.node = target;
                p.y = 0;
                p.x = 0;

                // This doesn't get marked garbage because the tracked pin
                // movement is sensical.
            }

            // Remove the page from the linked list
            self.pages.remove(node);
        }
        self.destroy_node(node);
    }

    /// Returns the pin for the given point. The pin is NOT tracked so it
    /// is only valid as long as the pagelist isn't modified.
    ///
    /// This will return null if the point is out of bounds. The caller
    /// should clamp the point to the bounds of the coordinate space if
    /// necessary.
    pub fn pin(&self, pt: point::Point) -> Option<Pin> {
        // getTopLeft is much more expensive than checking the cols bounds
        // so we do this first.
        let x = pt.coord().x;
        if x >= self.cols {
            return None;
        }

        // Grab the top left and move to the point.
        let mut p = self.get_top_left(pt.tag()).down(pt.coord().y as usize)?;
        // Incomplete reflow can leave a page narrower than the desired width.
        // Never manufacture an out-of-bounds pin for that page.
        // SAFETY: valid pin.
        if x >= unsafe { nd(p.node).cols() } {
            return None;
        }
        p.x = x;
        Some(p)
    }

    /// Convert the given pin to a tracked pin. A tracked pin will always be
    /// automatically updated as the pagelist is modified. If the point the
    /// pin points to is removed completely, the tracked pin will be updated
    /// to the top-left of the screen.
    pub fn track_pin(&mut self, p: Pin) -> Result<PinPtr, AllocError> {
        if build_options::SLOW_RUNTIME_SAFETY {
            assert!(self.pin_is_valid(p));
        }

        // Add it to the tracked list (reserve first so insertion can't fail)
        if self.tracked_pins.len() == self.tracked_pins.capacity() {
            self.pool.alloc.check()?;
        }
        self.tracked_pins
            .try_reserve(1)
            .map_err(|_| AllocError::OutOfMemory)?;

        // Create our tracked pin
        let tracked = self.pool.pins.create().map_err(pool_err)?;
        // SAFETY: fresh pin pool item.
        unsafe { tracked.as_ptr().write(p) };

        let inserted = self.tracked_pins.insert(tracked);
        debug_assert!(inserted);

        Ok(tracked)
    }

    /// Untrack a previously tracked pin. This will deallocate the pin.
    pub fn untrack_pin(&mut self, p: PinPtr) {
        assert!(p != self.viewport_pin);
        if self.tracked_pins.swap_remove(&p) {
            self.pool.pins.destroy(p);
        }
    }

    pub fn count_tracked_pins(&self) -> usize {
        self.tracked_pins.len()
    }

    /// Returns the tracked pins for this pagelist. The slice is owned by the
    /// pagelist and is only valid until the pagelist is modified.
    pub fn tracked_pins(&self) -> &indexmap::set::Slice<PinPtr> {
        self.tracked_pins.as_slice()
    }

    /// Checks if a pin is valid for this pagelist. This is a very slow and
    /// expensive operation since we traverse the entire linked list in the
    /// worst case. Only for runtime safety/debug.
    pub fn pin_is_valid(&self, p: Pin) -> bool {
        let mut it = self.pages.first;
        while let Some(node) = it {
            // SAFETY: live list node.
            let n = unsafe { nd(node) };
            if node != p.node {
                it = n.next;
                continue;
            }
            return p.y < n.rows() && p.x < n.cols();
        }

        false
    }

    /// Returns whether a node pointer and serial still identify the same page in
    /// this list. The node pointer is only compared and is safe even if the node
    /// has been destroyed or reused since the serial was captured.
    pub fn node_is_valid(&self, target: NodePtr, serial: u64) -> bool {
        // Reset invalidates the whole prior epoch, so reject those generations
        // without scanning the live list.
        if serial < self.page_serial_epoch {
            return false;
        }

        let mut it = self.pages.first;
        while let Some(node) = it {
            // SAFETY: live list node.
            let n = unsafe { nd(node) };
            if node == target {
                return n.serial == serial;
            }
            it = n.next;
        }

        false
    }

    /// Returns the viewport for the given pin, preferring to pin to
    /// "active" if the pin is within the active area.
    fn pin_is_active(&self, p: Pin) -> bool {
        // If the pin is in the active page, then we can quickly determine
        // if we're beyond the end.
        let active = self.get_top_left(point::Tag::Active);
        if p.node == active.node {
            return p.y >= active.y;
        }

        // SAFETY: live list nodes.
        let mut node_ = unsafe { nd(active.node).next };
        while let Some(node) = node_ {
            // This loop is pretty fast because the active area is
            // never that large so this is at most one, two nodes for
            // reasonable terminals (including very large real world
            // ones).

            // A node forward in the active area is our node, so we're
            // definitely in the active area.
            if node == p.node {
                return true;
            }
            node_ = unsafe { nd(node).next };
        }

        false
    }

    /// Returns true if the pin is at the top of the scrollback area.
    fn pin_is_top(&self, p: Pin) -> bool {
        p.y == 0 && p.node == self.pages.first.unwrap()
    }

    /// Convert a pin to a point in the given context. If the pin can't fit
    /// within the given tag (i.e. its in the history but you requested active),
    /// then this will return null.
    ///
    /// Note that this can be a very expensive operation depending on the tag and
    /// the location of the pin. This works by traversing the linked list of pages
    /// in the tagged region.
    ///
    /// Therefore, this is recommended only very rarely.
    pub fn point_from_pin(&self, tag: point::Tag, p: Pin) -> Option<point::Point> {
        let tl = self.get_top_left(tag);

        // Count our first page which is special because it may be partial.
        let mut coord = point::Coordinate { x: p.x, y: 0 };
        if p.node == tl.node {
            // If our top-left is after our y then we're outside the range.
            if tl.y > p.y {
                return None;
            }
            coord.y = (p.y - tl.y) as u32;
        } else {
            // SAFETY: live list nodes.
            unsafe {
                coord.y = coord.y.checked_add((nd(tl.node).rows() - tl.y) as u32)?;
                let mut node_ = nd(tl.node).next;
                let mut found = false;
                while let Some(node) = node_ {
                    if node == p.node {
                        coord.y = coord.y.checked_add(p.y as u32)?;
                        found = true;
                        break;
                    }

                    coord.y = coord.y.checked_add(nd(node).rows() as u32)?;
                    node_ = nd(node).next;
                }

                // We never saw our node, meaning we're outside the range.
                if !found {
                    return None;
                }
            }
        }

        Some(match tag {
            point::Tag::Active => point::Point::Active(coord),
            point::Tag::Viewport => point::Point::Viewport(coord),
            point::Tag::Screen => point::Point::Screen(coord),
            point::Tag::History => point::Point::History(coord),
        })
    }

    /// Get the cell at the given point, or null if the cell does not
    /// exist or is out of bounds.
    ///
    /// Warning: this is slow and should not be used in performance critical paths
    pub fn get_cell(&self, pt: point::Point) -> Option<Cell> {
        let pt_pin = self.pin(pt)?;
        // SAFETY: valid pin.
        let rac = unsafe { nd(pt_pin.node).page().get_row_and_cell(pt_pin.x as usize, pt_pin.y as usize) };
        Some(Cell {
            node: pt_pin.node,
            row: rac.row,
            cell: rac.cell,
            row_idx: pt_pin.y,
            col_idx: pt_pin.x,
        })
    }

    /// Log a debug diagram of the page list to the provided writer.
    ///
    /// EXAMPLE:
    ///
    /// ```text
    ///      +-----+ = PAGE 0
    ///  ... |     |
    ///   50 | foo |
    ///  ... |     |
    ///     +--------+ ACTIVE
    ///  124 |     | | 0
    ///  125 |Text | | 1
    ///      :  ^  : : = PIN 0
    ///  126 |Wrap…  | 2
    ///      +-----+ :
    ///      +-----+ : = PAGE 1
    ///    0 …ed   | | 3
    ///    1 | etc.| | 4
    ///      +-----+ :
    ///     +--------+
    /// ```
    ///
    /// Too many tracked pins in one row (Zig:
    /// `error.TooManyTrackedPinsInRow`) is reported as `fmt::Error`.
    pub fn diagram(&self, writer: &mut dyn fmt::Write) -> fmt::Result {
        fn write_n(writer: &mut dyn fmt::Write, c: char, n: usize) -> fmt::Result {
            for _ in 0..n {
                writer.write_char(c)?;
            }
            Ok(())
        }

        let active_pin = self.get_top_left(point::Tag::Active);

        let mut active = false;
        let mut active_index: usize = 0;

        let mut page_index: usize = 0;
        let mut cols: usize = 0;

        let mut it = self.page_iterator(
            Direction::RightDown,
            point::Point::Screen(point::Coordinate::default()),
            None,
        );
        while let Some(chunk) = it.next() {
            // SAFETY: live chunk node.
            let n = unsafe { nd(chunk.node) };
            cols = n.cols() as usize;

            // Whether we've just skipped some number of rows and drawn
            // an ellipsis row (this is reset when a row is not skipped).
            let mut skipped = false;

            for y in 0..n.rows() as usize {
                // Active header
                if !active && chunk.node == active_pin.node && active_pin.y as usize == y {
                    active = true;
                    writer.write_str("     +-")?;
                    write_n(writer, '-', cols)?;
                    writer.write_str("--+ ACTIVE")?;
                    writer.write_char('\n')?;
                }

                // Page header
                if y == 0 {
                    writer.write_str("      +")?;
                    write_n(writer, '-', cols)?;
                    writer.write_char('+')?;
                    if active {
                        writer.write_str(" :")?;
                    }
                    write!(writer, " = PAGE {}", page_index)?;
                    writer.write_char('\n')?;
                }

                // Row contents
                {
                    let page = n.page();
                    // SAFETY: y is within the page.
                    let row = unsafe { &*page.get_row(y) };
                    let cells = unsafe { &(&*page.get_cells(row))[..cols] };

                    let row_has_content = cells.iter().any(|cell| cell.has_text());

                    // We don't want to print this row's contents
                    // unless it has text or is in the active area.
                    if !active && !row_has_content {
                        // If we haven't, draw an ellipsis row.
                        if !skipped {
                            writer.write_str("  ... :")?;
                            write_n(writer, ' ', cols)?;
                            writer.write_char(':')?;
                            if active {
                                writer.write_str(" :")?;
                            }
                            writer.write_char('\n')?;
                        }
                        skipped = true;
                        continue;
                    }

                    skipped = false;

                    // Left pad row number to 5 wide
                    let y_digits = if y == 0 { 0 } else { y.ilog10() as usize };
                    write_n(writer, ' ', 4 - y_digits)?;
                    write!(writer, "{} ", y)?;

                    // Left edge or wrap continuation marker
                    writer.write_str(if row.wrap_continuation() { "…" } else { "|" })?;

                    // Row text
                    if row_has_content {
                        for cell in cells {
                            // Skip spacer tails, since wide cells are, well, wide.
                            if cell.wide() == pagepkg::Wide::SpacerTail {
                                continue;
                            }

                            // Write non-printing bytes as base36, for convenience.
                            if cell.codepoint() < ' ' as u32 {
                                writer.write_char(
                                    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"[cell.codepoint() as usize] as char,
                                )?;
                                continue;
                            }
                            writer.write_char(char::from_u32(cell.codepoint()).unwrap_or('\u{FFFD}'))?;
                            if cell.has_grapheme() {
                                let grapheme = page.lookup_grapheme(cell).unwrap();
                                for &cp in grapheme {
                                    writer.write_char(char::from_u32(cp).unwrap_or('\u{FFFD}'))?;
                                }
                            }
                        }
                    } else {
                        write_n(writer, ' ', cols)?;
                    }

                    // Right edge or wrap marker
                    writer.write_str(if row.wrap() { "…" } else { "|" })?;
                    if active {
                        write!(writer, " | {}", active_index)?;
                        active_index += 1;
                    }

                    writer.write_char('\n')?;
                }

                // Tracked pin marker(s)
                'pins: {
                    // If we have more than 16 tracked pins in a row, oh well,
                    // don't wanna bother making this function allocating.
                    let mut pin_buf: [Pin; 16] = [Pin::new(chunk.node); 16];
                    let mut pin_count: usize = 0;
                    for &p in self.tracked_pins.iter() {
                        // SAFETY: tracked pin.
                        let p = unsafe { *pinr(p) };
                        if p.node != chunk.node {
                            continue;
                        }
                        if p.y as usize != y {
                            continue;
                        }
                        pin_buf[pin_count] = p;
                        pin_count += 1;
                        if pin_count >= pin_buf.len() {
                            return Err(fmt::Error);
                        }
                    }

                    if pin_count == 0 {
                        break 'pins;
                    }

                    let pins = &mut pin_buf[..pin_count];
                    pins.sort_by_key(|p| p.x);

                    writer.write_str("      :")?;
                    let mut x: usize = 0;

                    for p in pins.iter() {
                        if x > p.x as usize {
                            continue;
                        }
                        write_n(writer, ' ', p.x as usize - x)?;
                        writer.write_char('^')?;
                        x = p.x as usize + 1;
                    }

                    write_n(writer, ' ', cols - x)?;
                    writer.write_char(':')?;

                    if active {
                        writer.write_str(" :")?;
                    }

                    write!(writer, " = PIN{}", if pin_count > 1 { "S" } else { "" })?;

                    let x = pins[0].x;
                    for (i, p) in pins.iter().enumerate() {
                        if p.x != x {
                            writer.write_char(',')?;
                        }
                        write!(writer, " {}", i)?;
                    }

                    writer.write_char('\n')?;
                }
            }

            // Page footer
            {
                writer.write_str("      +")?;
                write_n(writer, '-', cols)?;
                writer.write_char('+')?;
                if active {
                    writer.write_str(" :")?;
                }
                writer.write_char('\n')?;
            }

            page_index += 1;
        }

        // Active footer
        {
            writer.write_str("     +-")?;
            write_n(writer, '-', cols)?;
            writer.write_str("--+")?;
            writer.write_char('\n')?;
        }

        Ok(())
    }

    /// Returns the boundaries of the given semantic content type for
    /// the prompt at the given pin. The pin row MUST be the first row
    /// of a prompt, otherwise the results may be nonsense.
    ///
    /// To get prompt pins, use promptIterator. Warning that if there are
    /// no semantic prompts ever present, promptIterator will iterate the
    /// entire PageList. Downstream callers should keep track of a flag if
    /// they've ever seen semantic prompt operations to prevent this performance
    /// case.
    ///
    /// Note that some semantic content type such as "input" is usually
    /// nested within prompt boundaries, so the returned boundaries may include
    /// prompt text.
    pub fn highlight_semantic_content(
        &self,
        at: Pin,
        content: pagepkg::SemanticContent,
    ) -> Option<highlight::Untracked> {
        // Performance note: we can do this more efficiently in a single
        // forward-pass. Semantic content operations aren't usually fast path
        // but if someone wants to optimize them someday that's great.

        let end: Pin = 'end: {
            // Safety assertion, our starting point should be a prompt row.
            // so the first returned prompt should be ourselves.
            let mut it = at.prompt_iterator(Direction::RightDown, None);
            assert!(it.next().unwrap().y == at.y);

            // Our end is the end of the line just before the next prompt
            // line, which should exist since we verified we have at least
            // two prompts here.
            if let Some(next) = it.next() {
                if let Some(mut prev) = next.up(1) {
                    // SAFETY: valid pin.
                    prev.x = unsafe { nd(prev.node).cols() } - 1;
                    break 'end prev;
                }
            }

            // Didn't find any further prompt so the end of our zone is
            // the end of the screen.
            self.get_bottom_right(point::Tag::Screen).unwrap()
        };

        // SAFETY: cell pointers from valid pins.
        let semantic = |p: Pin| unsafe { (*p.row_and_cell().cell).semantic_content() };
        let has_text = |p: Pin| unsafe { (*p.row_and_cell().cell).has_text() };

        match content {
            // For the prompt, we select all the way up to command output.
            // We include all the input lines, too.
            pagepkg::SemanticContent::Prompt => {
                let mut result = highlight::Untracked {
                    start: at.left(at.x as usize),
                    end: at,
                };

                let mut it = at.cell_iterator(Direction::RightDown, Some(end));
                while let Some(p) = it.next() {
                    match semantic(p) {
                        pagepkg::SemanticContent::Prompt | pagepkg::SemanticContent::Input => result.end = p,
                        pagepkg::SemanticContent::Output => break,
                    }
                }

                Some(result)
            }

            // For input, we include the start of the input to the end of
            // the input, which may include all the prompts in the middle, too.
            pagepkg::SemanticContent::Input => {
                let mut result: Option<highlight::Untracked> = None;

                // Find the start
                let mut it = at.cell_iterator(Direction::RightDown, Some(end));
                while let Some(p) = it.next() {
                    match semantic(p) {
                        pagepkg::SemanticContent::Prompt => {}
                        pagepkg::SemanticContent::Input => {
                            result = Some(highlight::Untracked { start: p, end: p });
                            break;
                        }
                        pagepkg::SemanticContent::Output => return None,
                    }
                }

                // No input found
                let mut result = result?;

                // Find the end
                while let Some(p) = it.next() {
                    match semantic(p) {
                        // Prompts can be nested in our input for continuation
                        pagepkg::SemanticContent::Prompt => {}

                        // Output means we're done
                        pagepkg::SemanticContent::Output => break,

                        pagepkg::SemanticContent::Input => result.end = p,
                    }
                }

                Some(result)
            }

            pagepkg::SemanticContent::Output => {
                let mut result: Option<highlight::Untracked> = None;

                // Find the start
                let mut it = at.cell_iterator(Direction::RightDown, Some(end));
                while let Some(p) = it.next() {
                    match semantic(p) {
                        pagepkg::SemanticContent::Prompt | pagepkg::SemanticContent::Input => {}
                        pagepkg::SemanticContent::Output => {
                            // Skip empty cells - they default to .output but aren't real output
                            if !has_text(p) {
                                continue;
                            }
                            result = Some(highlight::Untracked { start: p, end: p });
                            break;
                        }
                    }
                }

                // No output found
                let mut result = result?;

                // Find the end
                while let Some(p) = it.next() {
                    match semantic(p) {
                        pagepkg::SemanticContent::Prompt | pagepkg::SemanticContent::Input => break,
                        pagepkg::SemanticContent::Output => {
                            // Only extend to cells with actual text
                            if has_text(p) {
                                result.end = p;
                            }
                        }
                    }
                }

                Some(result)
            }
        }
    }

    pub fn prompt_iterator(
        &self,
        direction: Direction,
        tl_pt: point::Point,
        bl_pt: Option<point::Point>,
    ) -> PromptIterator {
        let tl_pin = self.pin(tl_pt).unwrap();
        let bl_pin = match bl_pt {
            Some(pt) => self.pin(pt).unwrap(),
            None => match self.get_bottom_right(tl_pt.tag()) {
                Some(p) => p,
                None => return PromptIterator::EMPTY,
            },
        };

        match direction {
            Direction::RightDown => tl_pin.prompt_iterator(Direction::RightDown, Some(bl_pin)),
            Direction::LeftUp => bl_pin.prompt_iterator(Direction::LeftUp, Some(tl_pin)),
        }
    }

    pub fn cell_iterator(
        &self,
        direction: Direction,
        tl_pt: point::Point,
        bl_pt: Option<point::Point>,
    ) -> CellIterator {
        let tl_pin = self.pin(tl_pt).unwrap();
        let bl_pin = match bl_pt {
            Some(pt) => self.pin(pt).unwrap(),
            None => match self.get_bottom_right(tl_pt.tag()) {
                Some(p) => p,
                None => return CellIterator::default(),
            },
        };

        match direction {
            Direction::RightDown => tl_pin.cell_iterator(Direction::RightDown, Some(bl_pin)),
            Direction::LeftUp => bl_pin.cell_iterator(Direction::LeftUp, Some(tl_pin)),
        }
    }

    /// Create an iterator that can be used to iterate all the rows in
    /// a region of the screen from the given top-left. The tag of the
    /// top-left point will also determine the end of the iteration,
    /// so convert from one reference point to another to change the
    /// iteration bounds.
    pub fn row_iterator(
        &self,
        direction: Direction,
        tl_pt: point::Point,
        bl_pt: Option<point::Point>,
    ) -> RowIterator {
        let tl_pin = self.pin(tl_pt).unwrap();
        let bl_pin = match bl_pt {
            Some(pt) => self.pin(pt).unwrap(),
            None => match self.get_bottom_right(tl_pt.tag()) {
                Some(p) => p,
                None => return RowIterator::default(),
            },
        };

        match direction {
            Direction::RightDown => tl_pin.row_iterator(Direction::RightDown, Some(bl_pin)),
            Direction::LeftUp => bl_pin.row_iterator(Direction::LeftUp, Some(tl_pin)),
        }
    }

    /// Return an iterator that iterates through the rows in the tagged area
    /// of the point. The iterator returns row "chunks", which are the largest
    /// contiguous set of rows in a single backing page for a given portion of
    /// the point region.
    ///
    /// This is a more efficient way to iterate through the data in a region,
    /// since you can do simple pointer math and so on.
    ///
    /// If bl_pt is non-null, iteration will stop at the bottom left point
    /// (inclusive). If bl_pt is null, the entire region specified by the point
    /// tag will be iterated over. tl_pt and bl_pt must be the same tag, and
    /// bl_pt must be greater than or equal to tl_pt.
    ///
    /// If direction is left_up, iteration will go from bl_pt to tl_pt. If
    /// direction is right_down, iteration will go from tl_pt to bl_pt.
    /// Both inclusive.
    pub fn page_iterator(
        &self,
        direction: Direction,
        tl_pt: point::Point,
        bl_pt: Option<point::Point>,
    ) -> PageIterator {
        let tl_pin = self.pin(tl_pt).unwrap();
        let bl_pin = match bl_pt {
            Some(pt) => self.pin(pt).unwrap(),
            None => match self.get_bottom_right(tl_pt.tag()) {
                Some(p) => p,
                None => {
                    return PageIterator {
                        row: None,
                        ..Default::default()
                    }
                }
            },
        };

        if build_options::SLOW_RUNTIME_SAFETY {
            assert!(tl_pin.eql(bl_pin) || tl_pin.before(bl_pin));
        }

        match direction {
            Direction::RightDown => tl_pin.page_iterator(Direction::RightDown, Some(bl_pin)),
            Direction::LeftUp => bl_pin.page_iterator(Direction::LeftUp, Some(tl_pin)),
        }
    }

    /// Get the top-left of the screen for the given tag.
    pub fn get_top_left(&self, tag: point::Tag) -> Pin {
        match tag {
            // The full screen or history is always just the first page.
            point::Tag::Screen | point::Tag::History => Pin::new(self.pages.first.unwrap()),

            point::Tag::Viewport => match self.viewport {
                Viewport::Active => self.get_top_left(point::Tag::Active),
                Viewport::Top => self.get_top_left(point::Tag::Screen),
                // SAFETY: live viewport pin.
                Viewport::Pin => unsafe { *pinr(self.viewport_pin) },
            },

            // The active area is calculated backwards from the last page.
            // This makes getting the active top left slower but makes scrolling
            // much faster because we don't need to update the top left. Under
            // heavy load this makes a measurable difference.
            point::Tag::Active => {
                let mut rem = self.rows;
                let mut it = self.pages.last;
                while let Some(node) = it {
                    // SAFETY: live list node.
                    let n = unsafe { nd(node) };
                    if rem <= n.rows() {
                        return Pin {
                            node,
                            y: n.rows() - rem,
                            ..Pin::new(node)
                        };
                    }

                    rem -= n.rows();
                    it = n.prev;
                }

                unreachable!("assertion: we always have enough rows for active");
            }
        }
    }

    /// Returns the bottom right of the screen for the given tag. This can
    /// return null because it is possible that a tag is not in the screen
    /// (e.g. history does not yet exist).
    pub fn get_bottom_right(&self, tag: point::Tag) -> Option<Pin> {
        // SAFETY: live list nodes.
        unsafe {
            match tag {
                point::Tag::Screen | point::Tag::Active => {
                    let node = self.pages.last.unwrap();
                    Some(Pin {
                        node,
                        y: nd(node).rows() - 1,
                        x: nd(node).cols() - 1,
                        garbage: false,
                    })
                }

                point::Tag::Viewport => {
                    let mut br = self.get_top_left(point::Tag::Viewport);
                    br = br.down(self.rows as usize - 1).unwrap();
                    br.x = nd(br.node).cols() - 1;
                    Some(br)
                }

                point::Tag::History => {
                    let mut br = self.get_top_left(point::Tag::Active);
                    br = br.up(1)?;
                    br.x = nd(br.node).cols() - 1;
                    Some(br)
                }
            }
        }
    }

    /// The total rows in the screen. This is the actual row count currently
    /// and not a capacity or maximum.
    ///
    /// This is very slow, it traverses the full list of pages to count the
    /// rows, so it is not pub. This is only used for testing/debugging.
    fn total_rows(&self) -> usize {
        let mut rows: usize = 0;
        let mut node_ = self.pages.first;
        while let Some(node) = node_ {
            // SAFETY: live list node.
            let n = unsafe { nd(node) };
            rows += n.rows() as usize;
            node_ = n.next;
        }

        rows
    }

    /// The total number of pages in this list. This should only be used
    /// for tests since it is O(N) over the list of pages.
    pub fn total_pages(&self) -> usize {
        let mut pages: usize = 0;
        let mut node_ = self.pages.first;
        while let Some(node) = node_ {
            pages += 1;
            // SAFETY: live list node.
            node_ = unsafe { nd(node).next };
        }

        pages
    }

    /// Return a metadata-only snapshot of page backing storage.
    ///
    /// This never restores compressed pages. It is intended for diagnostics and
    /// other infrequent reporting because it traverses the complete page list.
    pub fn memory_stats(&self) -> MemoryStats {
        let mut result = MemoryStats::default();
        let mut current = self.pages.first;
        while let Some(node) = current {
            // SAFETY: live list node.
            let n = unsafe { nd(node) };
            let raw_len = n.metadata().memory.len();
            let backing_len = match n.owned {
                Owned::Pool => PagePool::item_size(),
                Owned::Heap => raw_len,
            };
            assert!(backing_len >= raw_len);
            result.raw_bytes += raw_len;

            match &n.data {
                NodeData::Resident(_) => {
                    result.resident_pages += 1;
                    result.resident_raw_bytes += raw_len;
                    result.resident_backing_bytes += backing_len;
                }

                NodeData::Compressed(compressed) => {
                    result.compressed_pages += 1;
                    result.decommitted_raw_bytes += raw_len;
                    // Strict reclamation covers only Page.memory. A standard pool
                    // item can have an unused tail which remains resident.
                    result.resident_backing_bytes += backing_len - raw_len;
                    result.encoded_bytes += compressed.encoded.len();
                }
            }
            current = n.next;
        }

        result
    }

    /// Return the memory held by this page list, summarized for callers that
    /// budget memory across many terminals. See `memoryStats` for a more
    /// detailed breakdown.
    ///
    /// This never restores a compressed page. It does visit every page, so its
    /// cost grows with the scrollback. Call it periodically rather than after
    /// every write.
    pub fn memory_usage(&self) -> MemoryUsage {
        let stats = self.memory_stats();
        MemoryUsage {
            pages: stats.resident_pages + stats.compressed_pages,
            virtual_bytes: self.page_size + self.pool.pages.free_count() * PagePool::item_size(),
            resident_bytes: stats.estimated_resident_bytes(),
            compressed_pages: stats.compressed_pages,
            compressed_bytes: stats.encoded_bytes,
        }
    }

    /// Grow the number of rows available in the page list by n.
    /// This is only used for testing so it isn't optimized in any way.
    #[allow(dead_code)]
    fn grow_rows(&mut self, n: usize) -> Result<(), AllocError> {
        for _ in 0..n {
            self.grow()?;
        }
        Ok(())
    }

    /// Clear all dirty bits on all pages. This is not efficient since it
    /// traverses the entire list of pages. This is used for testing/debugging.
    pub fn clear_dirty(&mut self) {
        let mut page = self.pages.first;
        while let Some(p) = page {
            // SAFETY: live list node and its rows.
            unsafe {
                let current_page = nd(p).page();
                current_page.dirty = false;
                let rows = current_page.rows.ptr(current_page.memory);
                for i in 0..nd(p).rows() as usize {
                    (*rows.add(i)).set_dirty(false);
                }
                page = nd(p).next;
            }
        }
    }

    /// Returns true if the point is dirty, used for testing.
    pub fn is_dirty(&self, pt: point::Point) -> bool {
        self.get_cell(pt).unwrap().is_dirty()
    }

    /// Mark a point as dirty, used for testing.
    #[allow(dead_code)]
    fn mark_dirty(&mut self, pt: point::Point) {
        self.pin(pt).unwrap().mark_dirty();
    }
}

impl Drop for PageList {
    /// Deinit the pagelist, freeing all page memory and the memory pool.
    fn drop(&mut self) {
        // Verify integrity before cleanup
        if !std::thread::panicking() {
            self.assert_integrity();
        }

        // Return every tracked pin to the pin pool (Zig frees them with the
        // pool's arena).
        for p in self.tracked_pins.drain(..) {
            self.pool.pins.destroy(p);
        }

        // Release every page and node back to the pools, then free the pools.
        release_pages(&mut self.pool, self.pages);
        self.pages = List::new();
    }
}

impl fmt::Debug for PageList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageList")
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("total_rows", &self.total_rows)
            .field("page_size", &self.page_size)
            .field("viewport", &self.viewport)
            .finish_non_exhaustive()
    }
}

// We use a cursor to track where we are in the src/dst. This is very
// similar to Screen.Cursor, so see that for docs on individual fields.
// We don't use a Screen because we don't need all the same data and we
// do our best to optimize having direct access to the page memory.
struct ReflowCursor {
    x: CellCountInt,
    y: CellCountInt,
    pending_wrap: bool,
    node: NodePtr,
    page: *mut Page,
    page_row: *mut Row,
    page_cell: *mut pagepkg::Cell,
    new_rows: usize,

    /// This is the final row count of the reflowed pages.
    total_rows: usize,

    /// Memoizes the most recent source-to-destination style id
    /// mapping. Styled cells come in long runs sharing the same style
    /// so this lets writeCell bump the destination ref count directly
    /// instead of performing a set lookup for every styled cell.
    ///
    /// The destination id is only valid for the current destination
    /// page, which is why this lives on the cursor: every destination
    /// page change goes through init() which resets this.
    style_cache: StyleCache,

    /// Memoizes the capacity adjustment for new destination pages
    /// (see reflowRow). It only depends on the source page so this is
    /// keyed by the source page pointer, which reflow visits
    /// sequentially and never revisits.
    cap_memo: Option<(*const Page, Capacity)>,
}

#[derive(Clone, Copy)]
struct StyleCache {
    src_page: *const Page,
    src_id: stylepkg::Id,
    dst_id: stylepkg::Id,
}

impl StyleCache {
    const INVALID: StyleCache = StyleCache {
        src_page: core::ptr::null(),
        src_id: stylepkg::DEFAULT_ID,
        dst_id: stylepkg::DEFAULT_ID,
    };
}

/// The result of `ReflowCursor::write_cell`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteCellResult {
    Success,
    Repeat,
    SkipNext,
}

/// The group length for the vectorized bulk run scan below: the
/// SIMD lane count where the target supports it, otherwise a
/// plain unrolled group like other cell scans use (e.g. the
/// render state scans).
const BULK_GROUP_LEN: usize = match simd::lanes::<u64>() {
    Some(v) => v,
    None => 8,
};

/// Masked compare helper covering every cell field that a run
/// must share to be copied by copyRun: given that the first cell
/// of a run passed the full bulkCopyable predicate, equality on
/// these fields implies the same for every subsequent cell, with
/// the same style.
///
/// Note this is slightly stricter than bulkCopyable (e.g. a
/// bg-color cell won't extend an unstyled text run even though it
/// is copyable): that only splits the copy into multiple runs,
/// which is still correct.
type BulkRunMask = pagepkg::Mask<
    pagepkg::Cell,
    {
        field_mask(&[
            cell_fields::CONTENT_TAG,
            cell_fields::STYLE_ID,
            cell_fields::WIDE,
            cell_fields::HYPERLINK,
        ])
    },
    BULK_GROUP_LEN,
>;

/// Masked compare helper for detecting the Kitty virtual
/// placeholder codepoint in text cells. Placeholders take the
/// slow path (they must set a row flag), so they terminate a run.
type PlaceholderMask = pagepkg::Mask<pagepkg::Cell, { field_mask(&[cell_fields::CONTENT_CODEPOINT_DATA]) }, BULK_GROUP_LEN>;

impl ReflowCursor {
    fn init(node: NodePtr) -> ReflowCursor {
        // SAFETY: live destination node.
        unsafe {
            let page: *mut Page = nd(node).page();
            let rows = (*page).rows.ptr((*page).memory);
            ReflowCursor {
                x: 0,
                y: 0,
                pending_wrap: false,
                node,
                page,
                page_row: rows,
                page_cell: (*rows).cells().ptr((*page).memory),
                new_rows: 0,

                // Initially whatever size our input node is.
                total_rows: nd(node).rows() as usize,

                style_cache: StyleCache::INVALID,
                cap_memo: None,
            }
        }
    }

    /// Reflow the provided row in to this cursor.
    fn reflow_row(&mut self, list: &mut PageList, row: Pin, cursor_pin: Option<PinPtr>) -> Result<(), AllocError> {
        // SAFETY: the source row is a valid pin into a live source node; the
        // destination pointers are into live destination nodes.
        unsafe {
            let src_page: *mut Page = nd(row.node).page();
            let src_row: *mut Row = row.row_and_cell().row;
            let src_y = row.y;
            let cells: &[pagepkg::Cell] = core::slice::from_raw_parts(
                (*src_row).cells().ptr((*src_page).memory),
                (*src_page).size.cols as usize,
            );

            // Calculate the columns in this row. First up we trim non-semantic
            // rightmost blanks.
            let mut cols_len: usize = (*src_page).size.cols as usize;
            if !(*src_row).wrap() {
                while cols_len > 0 {
                    if !cells[cols_len - 1].is_empty() {
                        break;
                    }
                    cols_len -= 1;
                }

                // If the row has a semantic prompt then the blank row is meaningful
                // so we just consider pretend the first cell of the row isn't empty.
                if cols_len == 0 && (*src_row).semantic_prompt() != pagepkg::SemanticPrompt::None {
                    cols_len = 1;
                }
            }

            // Handle tracked pin adjustments. We also note whether any
            // tracked pin is on this row at all so that the per-cell loop
            // below can skip pin scans entirely for the overwhelmingly
            // common case of a row with no pins. Note we compare nodes
            // rather than pages since a node owns exactly one page; this
            // is cheaper and avoids `page()` restoring unrelated
            // compressed nodes purely for a comparison.
            let mut row_has_pins = false;
            {
                // Deferred line breaks and pending wrap reset the destination column.
                let dst_x = if self.new_rows > 0 || self.pending_wrap { 0 } else { self.x };

                for &pp in list.tracked_pins.iter() {
                    let p = pinr(pp);
                    if p.node != row.node || p.y != src_y {
                        continue;
                    }

                    // This row has pins
                    row_has_pins = true;

                    if cursor_pin == Some(pp) {
                        continue;
                    }

                    // If this pin is in the blanks on the right and past the end
                    // of the dst col width then we move it to the end of the dst
                    // col width instead.
                    if p.x as usize >= cols_len {
                        p.x = p.x.min((*self.page).size.cols - 1 - dst_x);
                    }

                    // We increase our col len to at least include this pin.
                    // This ensures that blank rows with pins are processed,
                    // so that the pins can be properly remapped.
                    cols_len = cols_len.max(p.x as usize + 1);
                }
            }

            // If the cursor is after blanks on the right, those cells are still
            // before the next write and must reflow with it.
            if let Some(p) = cursor_pin {
                let p = pinr(p);
                if p.node == row.node && p.y == src_y {
                    cols_len = cols_len.max(p.x as usize + 1);
                }
            }

            // Defer processing of blank rows so that blank rows
            // at the end of the page list are never written.
            if cols_len == 0 {
                // If this blank row was a wrap continuation somehow
                // then we won't need to write it since it should be
                // a part of the previously written row.
                if !(*src_row).wrap_continuation() {
                    self.new_rows += 1;
                }
                return Ok(());
            }

            // Inherit increased styles or grapheme bytes from the src page
            // we're reflowing from for new pages.
            //
            // This only depends on the source page, which we process row
            // by row, so memoize it: computing the adjustment requires a
            // full page layout calculation which is much too expensive to
            // do for every row.
            let cap: Capacity = match self.cap_memo {
                Some((memo_page, memo_cap)) if memo_page == src_page as *const Page => memo_cap,
                // Source page changed, fall through to recompute.
                _ => self.compute_and_memoize_cap(src_page),
            };

            // Our row isn't blank, write any new rows we deferred.
            while self.new_rows > 0 {
                self.cursor_scroll_or_new_page(list, cap)?;
                self.new_rows -= 1;
            }

            self.copy_row_metadata(&*src_row);

            let mut x: usize = 0;
            while x < cols_len {
                if self.pending_wrap {
                    (*self.page_row).set_wrap(true);
                    self.cursor_scroll_or_new_page(list, cap)?;
                    self.copy_row_metadata(&*src_row);
                    (*self.page_row).set_wrap_continuation(true);
                }

                // Fast path: bulk-copy a run of simple cells directly
                // into the destination row. The vast majority of cells
                // are narrow cells or complete wide pairs, have no
                // managed memory (graphemes, hyperlinks), and share a
                // single style in long runs, so this avoids the
                // per-cell state machine below for most of the work.
                // Rows with tracked pins take the slow path so pin
                // remapping behaves identically.
                if !row_has_pins {
                    let max_run = (cols_len - x).min((*self.page).size.cols as usize - self.x as usize);
                    let window = &cells[x..x + max_run];
                    let run = bulk_run_length(window);
                    if run > 0 && self.copy_run(&window[..run], src_page) {
                        x += run;
                        continue;
                    }
                }

                // Move any tracked pins from the source.
                if row_has_pins {
                    for &p in list.tracked_pins.iter() {
                        let p = pinr(p);
                        if p.node != row.node || p.y != src_y || p.x as usize != x {
                            continue;
                        }

                        p.node = self.node;
                        p.x = self.x;
                        p.y = self.y;
                    }
                }

                match self.write_cell(list, &cells[x], src_page) {
                    // Wrote the cell, move to the next.
                    Ok(WriteCellResult::Success) => x += 1,

                    // Wrote the cell but request to skip the next so skip it.
                    // This is used for things like spacers.
                    Ok(WriteCellResult::SkipNext) => {
                        // Remap any tracked pins at the skipped position (x+1)
                        // since we won't process that cell in the loop.
                        if row_has_pins {
                            for &p in list.tracked_pins.iter() {
                                let p = pinr(p);
                                if p.node != row.node || p.y != src_y || p.x as usize != x + 1 {
                                    continue;
                                }

                                p.node = self.node;
                                p.x = self.x;
                                p.y = self.y;
                            }
                        }

                        x += 2;
                    }

                    // Didn't write the cell, repeat writing this same cell.
                    Ok(WriteCellResult::Repeat) => {}

                    // System out of memory, we can't fix this.
                    Err(IncreaseCapacityError::OutOfMemory) => return Err(AllocError::OutOfMemory),

                    // We reached the capacity of a single page and can't
                    // add any more of some type of managed memory. When this
                    // happens we split out the current row we're working on
                    // into a new page and continue from there.
                    Err(IncreaseCapacityError::OutOfSpace) => {
                        if self.y == 0 {
                            // If we're already on the first-row, we can't split
                            // any further, so we just ignore bad cells and take
                            // corrupted (but valid) cell contents.
                            log::warn!(
                                target: LOG_TARGET,
                                "reflowRow OutOfSpace on first row, discarding cell managed memory"
                            );
                            x += 1;
                            self.cursor_forward();
                        } else {
                            // Move our last row to a new page.
                            self.move_last_row_to_new_page(list, cap)?;

                            // Do NOT increment x so that we retry writing
                            // the same existing cell.
                        }
                    }
                }
            }

            // If the source row isn't wrapped then we should scroll afterwards.
            if !(*src_row).wrap() {
                self.new_rows += 1;
            }
        }

        Ok(())
    }

    /// Compute and memoize the new-page capacity for the given source
    /// page. See the call site in reflowRow for details.
    fn compute_and_memoize_cap(&mut self, src_page: *const Page) -> Capacity {
        // SAFETY: live source and destination pages.
        let cap = unsafe {
            match (*src_page).capacity.adjust(pagepkg::Adjustment {
                cols: Some((*self.page).size.cols),
            }) {
                Ok(c) => c,
                Err(_) => {
                    let mut cap = (*src_page).capacity;
                    cap.cols = (*self.page).size.cols;
                    // We're already a non-standard page. We don't want to
                    // inherit a massive set of rows, so cap it at our std size.
                    cap.rows = (*src_page).size.rows.min(STD_CAPACITY.rows);
                    cap
                }
            }
        };

        self.cap_memo = Some((src_page, cap));
        cap
    }

    /// Copy a run of bulk-copyable cells (see bulkCopyable) sharing
    /// one style into the destination row at the current position,
    /// then advance the cursor. The run must fit in the remaining
    /// columns of the destination row.
    ///
    /// Returns false without any state change if the style could not
    /// be mapped into the destination page without a capacity
    /// change; the caller should fall back to writeCell which
    /// handles growing capacity.
    fn copy_run(&mut self, src_cells: &[pagepkg::Cell], src_page: *const Page) -> bool {
        // SAFETY: live source and destination pages.
        unsafe {
            let page = &mut *self.page;
            debug_assert!(!self.pending_wrap);
            debug_assert!(!src_cells.is_empty());
            debug_assert!(src_cells.len() <= (page.size.cols - self.x) as usize);

            let style_id = src_cells[0].style_id();
            let n: u16 = src_cells.len() as u16;

            // Resolve the destination style id for this run and take one
            // reference per cell. This mirrors the per-cell style logic
            // in writeCell, including the memoization (see StyleCache).
            let dst_style_id: stylepkg::Id = if style_id == stylepkg::DEFAULT_ID {
                stylepkg::DEFAULT_ID
            } else if self.style_cache.src_page == src_page && self.style_cache.src_id == style_id {
                let id = self.style_cache.dst_id;
                page.styles.use_multiple(page.memory, id, n);
                id
            } else {
                let style = *(*src_page).styles.get((*src_page).memory, style_id);

                // Any error here (set full or needs rehash) is handled
                // by falling back to the slow path, which grows capacity.
                // No state has been modified yet at this point.
                let id = match page.styles.add_with_id(page.memory, style, style_id) {
                    Ok(v) => v.unwrap_or(style_id),
                    Err(_) => return false,
                };

                // addWithId took one reference, take the rest.
                if n > 1 {
                    page.styles.use_multiple(page.memory, id, n - 1);
                }

                self.style_cache = StyleCache {
                    src_page,
                    src_id: style_id,
                    dst_id: id,
                };

                id
            };

            // Copy the raw cell contents.
            core::ptr::copy_nonoverlapping(src_cells.as_ptr(), self.page_cell, src_cells.len());
            let dst_cells = core::slice::from_raw_parts_mut(self.page_cell, src_cells.len());

            // If the style resolved to a different id in the destination
            // page then rewrite the copied cells to point at it.
            if dst_style_id != style_id {
                for cell in dst_cells.iter_mut() {
                    cell.set_style_id(dst_style_id);
                }
            }
            if dst_style_id != stylepkg::DEFAULT_ID {
                (*self.page_row).set_styled(true);
            }

            // Advance the cursor, matching what repeated cursorForward
            // calls after each cell write would have done.
            let cols = page.size.cols;
            if self.x + n == cols {
                self.x = cols - 1;
                self.page_cell = self.page_cell.add(n as usize - 1);
                self.pending_wrap = true;
            } else {
                self.x += n;
                self.page_cell = self.page_cell.add(n as usize);
            }

            true
        }
    }

    /// Write a cell. On error, this will not unwrite the cell but
    /// the cell may be incomplete (but valid). For example, if the source
    /// cell is styled and we failed to allocate space for styles, the
    /// written cell may not be styled but it is valid.
    ///
    /// The key failure to recognize for callers is when we can't increase
    /// capacity in our destination page. In this case, the caller may want
    /// to split the page at this row, rewrite the row into a new page
    /// and continue from there.
    ///
    /// But this function guarantees the terminal/page will be in a
    /// coherent state even on error.
    fn write_cell(
        &mut self,
        list: &mut PageList,
        cell: &pagepkg::Cell,
        src_page: *const Page,
    ) -> Result<WriteCellResult, IncreaseCapacityError> {
        // SAFETY: live source and destination pages; `cell` lives in the
        // source page memory.
        unsafe {
            // Initialize self.page_cell with basic, unmanaged memory contents.
            {
                // This must not fail because we want to make sure we atomically
                // setup our page cell to be valid.

                // Copy cell contents.
                match cell.content_tag() {
                    pagepkg::ContentTag::Codepoint | pagepkg::ContentTag::CodepointGrapheme => match cell.wide() {
                        pagepkg::Wide::Narrow => *self.page_cell = *cell,

                        pagepkg::Wide::Wide => {
                            if (*self.page).size.cols > 1 {
                                if self.x == (*self.page).size.cols - 1 {
                                    // If there's a wide character in the last column of
                                    // the reflowed page then we need to insert a spacer
                                    // head and wrap before handling it.
                                    let mut head = pagepkg::Cell::default();
                                    head.set_content_tag(pagepkg::ContentTag::Codepoint);
                                    head.set_wide(pagepkg::Wide::SpacerHead);
                                    *self.page_cell = head;

                                    // Move to the next row (this sets pending wrap
                                    // which will cause us to wrap on the next
                                    // iteration).
                                    self.cursor_forward();

                                    // Decrement the source position so that when we
                                    // loop we'll process this source cell again,
                                    // since we can't copy it into a spacer head.
                                    return Ok(WriteCellResult::Repeat);
                                } else {
                                    *self.page_cell = *cell;
                                }
                            } else {
                                // Edge case, when resizing to 1 column, wide
                                // characters are just destroyed and replaced
                                // with empty narrow cells.
                                (*self.page_cell).set_content_raw(0);
                                (*self.page_cell).set_wide(pagepkg::Wide::Narrow);
                                self.cursor_forward();

                                // Skip spacer tail so it doesn't cause a wrap.
                                return Ok(WriteCellResult::SkipNext);
                            }
                        }

                        pagepkg::Wide::SpacerTail => {
                            if (*self.page).size.cols > 1 {
                                *self.page_cell = *cell;
                            } else {
                                // Edge case, when resizing to 1 column, wide
                                // characters are just destroyed and replaced
                                // with empty narrow cells, so we should just
                                // discard any spacer tails.
                                return Ok(WriteCellResult::Success);
                            }
                        }

                        pagepkg::Wide::SpacerHead => {
                            // Spacer heads should be ignored. If we need a
                            // spacer head in our reflowed page, it is added
                            // when processing the wide cell it belongs to.
                            return Ok(WriteCellResult::Success);
                        }
                    },

                    pagepkg::ContentTag::BgColorPalette | pagepkg::ContentTag::BgColorRgb => {
                        // These are guaranteed to have no style or grapheme
                        // data associated with them so we can fast path them.
                        *self.page_cell = *cell;
                        self.cursor_forward();
                        return Ok(WriteCellResult::Success);
                    }
                }

                // These will create issues by trying to clone managed memory that
                // isn't set if the current dst row needs to be moved to a new page.
                // They'll be fixed once we do properly copy the relevant memory.
                (*self.page_cell).set_content_tag(pagepkg::ContentTag::Codepoint);
                (*self.page_cell).set_hyperlink(false);
                (*self.page_cell).set_style_id(stylepkg::DEFAULT_ID);

                if build_options::KITTY_GRAPHICS {
                    // Copy Kitty virtual placeholder status
                    if cell.codepoint() == graphics_unicode::PLACEHOLDER {
                        (*self.page_row).set_kitty_virtual_placeholder(true);
                    }
                }
            }

            // From this point on we're moving on to failable, managed memory.
            // If we reach an error, we do the minimal cleanup necessary to
            // not leave dangling memory but otherwise we gracefully degrade
            // into some functional but not strictly correct cell.

            // Copy grapheme data.
            if cell.content_tag() == pagepkg::ContentTag::CodepointGrapheme {
                // Copy the graphemes
                let cps: &[u32] = (*src_page).lookup_grapheme(cell).unwrap();

                // If our page can't support an additional cell
                // with graphemes then we increase capacity.
                if (*self.page).grapheme_count() >= (*self.page).grapheme_capacity() {
                    self.increase_capacity(list, Some(IncreaseCapacity::GraphemeBytes))?;
                }

                // Attempt to allocate the space that would be required
                // for these graphemes, and if it's not available, then
                // increase capacity. Keep trying until we succeed.
                loop {
                    let page = &mut *self.page;
                    match page.grapheme_alloc.alloc::<u32>(page.memory, cps.len()) {
                        Ok(slice) => {
                            page.grapheme_alloc.free(page.memory, slice);
                            break;
                        }
                        Err(_) => {
                            // Grow our capacity until we can fit the extra bytes.
                            self.increase_capacity(list, Some(IncreaseCapacity::GraphemeBytes))?;
                        }
                    }
                }

                if let Err(err) = (*self.page).set_graphemes(&mut *self.page_row, &mut *self.page_cell, cps) {
                    // This shouldn't fail since we made sure we have space
                    // above. There is no reasonable behavior we can take here
                    // so we have a warn level log. This is ALMOST non-recoverable,
                    // though we choose to recover by corrupting the cell
                    // to a non-grapheme codepoint.
                    log::error!(target: LOG_TARGET, "setGraphemes failed after capacity increase err={:?}", err);
                    if cfg!(debug_assertions) {
                        // Force a crash with safe builds.
                        unreachable!();
                    }

                    // Unsafe builds we throw away grapheme data!
                    (*self.page_cell).set_content_tag(pagepkg::ContentTag::Codepoint);
                    (*self.page_cell).set_content_codepoint(0xFFFD);
                }
            }

            // Copy hyperlink data.
            if cell.hyperlink() {
                'hyperlink: {
                    let src_id = (*src_page).lookup_hyperlink(cell).unwrap();
                    let src_link: hyperlink::PageEntry = *(*src_page).hyperlink_set.get((*src_page).memory, src_id);

                    // If our page can't support an additional cell
                    // with a hyperlink then we increase capacity.
                    if (*self.page).hyperlink_count() >= (*self.page).hyperlink_capacity() {
                        self.increase_capacity(list, Some(IncreaseCapacity::HyperlinkBytes))?;
                    }

                    // Ensure that the string alloc has sufficient capacity
                    // to dupe the link (and the ID if it's not implicit).
                    // Grow our capacity until the hyperlink fits.
                    while !self.hyperlink_strings_fit(&src_link) {
                        self.increase_capacity(list, Some(IncreaseCapacity::StringBytes))?;
                    }

                    let dst_link = match src_link.dupe(&*src_page, &mut *self.page) {
                        Ok(l) => l,
                        Err(err) => {
                            // This shouldn't fail since we did a capacity
                            // check above.
                            log::error!(target: LOG_TARGET, "link dupe failed with capacity check err={:?}", err);
                            if cfg!(debug_assertions) {
                                // Force a crash with safe builds.
                                unreachable!();
                            }

                            break 'hyperlink;
                        }
                    };

                    let dst_id = {
                        let page = &mut *self.page;
                        let ctx = hyperlink::SetContext::new(self.page);
                        match page.hyperlink_set.add_with_id_context(page.memory, dst_link, src_id, &ctx) {
                            Ok(v) => v.unwrap_or(src_id),
                            Err(err) => {
                                // Always free our original link in case the increaseCap
                                // call fails so we aren't leaking memory.
                                dst_link.free(self.page);

                                // If the add failed then either the set needs to grow
                                // or it needs to be rehashed. Either one of those can
                                // be accomplished by increasing capacity, either with
                                // no actual change or with an increased hyperlink cap.
                                self.increase_capacity(
                                    list,
                                    match err {
                                        crate::terminal::ref_counted_set::AddError::OutOfMemory => {
                                            Some(IncreaseCapacity::HyperlinkBytes)
                                        }
                                        crate::terminal::ref_counted_set::AddError::NeedsRehash => None,
                                    },
                                )?;

                                // The increaseCapacity call above swapped self.page
                                // for a new page, so the string capacity check done
                                // before the first dupe no longer applies. Re-establish
                                // it against the current page before duping again.
                                while !self.hyperlink_strings_fit(&src_link) {
                                    self.increase_capacity(list, Some(IncreaseCapacity::StringBytes))?;
                                }

                                // We need to recreate the link into the new page.
                                let dst_link2 = match src_link.dupe(&*src_page, &mut *self.page) {
                                    Ok(l) => l,
                                    Err(err2) => {
                                        // This shouldn't fail since we did a capacity
                                        // check above.
                                        log::error!(
                                            target: LOG_TARGET,
                                            "link dupe failed with capacity check err={:?}",
                                            err2
                                        );
                                        if cfg!(debug_assertions) {
                                            // Force a crash with safe builds.
                                            unreachable!();
                                        }

                                        break 'hyperlink;
                                    }
                                };

                                // We assume this one will succeed. We dupe the link
                                // again, and don't have to worry about the other one
                                // because increasing the capacity naturally clears up
                                // any managed memory not associated with a cell yet.
                                let page = &mut *self.page;
                                let ctx = hyperlink::SetContext::new(self.page);
                                match page.hyperlink_set.add_with_id_context(page.memory, dst_link2, src_id, &ctx) {
                                    Ok(v) => v.unwrap_or(src_id),
                                    Err(err2) => {
                                        // This shouldn't happen since we increased capacity
                                        // above so we handle it like the other similar
                                        // cases and log it, crash in safe builds, and
                                        // remove the hyperlink in unsafe builds.
                                        log::error!(
                                            target: LOG_TARGET,
                                            "addWithIdContext failed after capacity increase err={:?}",
                                            err2
                                        );
                                        if cfg!(debug_assertions) {
                                            // Force a crash with safe builds.
                                            unreachable!();
                                        }

                                        dst_link2.free(self.page);
                                        break 'hyperlink;
                                    }
                                }
                            }
                        }
                    };

                    // We expect this to succeed due to the hyperlinkCapacity
                    // check we did before. If it doesn't succeed let's
                    // log it, crash (in safe builds), and clear our state.
                    if let Err(err) = (*self.page).set_hyperlink(&mut *self.page_row, &mut *self.page_cell, dst_id) {
                        log::error!(target: LOG_TARGET, "setHyperlink failed after capacity increase err={:?}", err);
                        if cfg!(debug_assertions) {
                            // Force a crash with safe builds.
                            unreachable!();
                        }

                        // Unsafe builds we throw away hyperlink data!
                        let page = &mut *self.page;
                        page.hyperlink_set.release(page.memory, dst_id);
                        (*self.page_cell).set_hyperlink(false);
                        break 'hyperlink;
                    }
                }
            }

            // Copy style data.
            if cell.has_styling() {
                'style: {
                    // Fast path: styled cells come in long runs sharing the
                    // same style. If this source style was just mapped into
                    // the current destination page, bump the ref count
                    // directly and skip the set lookup. The destination id is
                    // guaranteed alive because a previously written cell in
                    // this page holds a reference, and the cache is reset
                    // whenever the destination page changes (see init).
                    if self.style_cache.src_page == src_page && self.style_cache.src_id == cell.style_id() {
                        let id = self.style_cache.dst_id;
                        let page = &mut *self.page;
                        page.styles.use_(page.memory, id);
                        (*self.page_row).set_styled(true);
                        (*self.page_cell).set_style_id(id);
                        break 'style;
                    }

                    let style = *(*src_page).styles.get((*src_page).memory, cell.style_id());

                    let page = &mut *self.page;
                    let id = match page.styles.add_with_id(page.memory, style, cell.style_id()) {
                        Ok(v) => v.unwrap_or(cell.style_id()),
                        Err(err) => {
                            // If the add failed then either the set needs to grow
                            // or it needs to be rehashed. Either one of those can
                            // be accomplished by increasing capacity, either with
                            // no actual change or with an increased style cap.
                            self.increase_capacity(
                                list,
                                match err {
                                    crate::terminal::ref_counted_set::AddError::OutOfMemory => {
                                        Some(IncreaseCapacity::Styles)
                                    }
                                    crate::terminal::ref_counted_set::AddError::NeedsRehash => None,
                                },
                            )?;

                            // We assume this one will succeed.
                            let page = &mut *self.page;
                            match page.styles.add_with_id(page.memory, style, cell.style_id()) {
                                Ok(v) => v.unwrap_or(cell.style_id()),
                                Err(err2) => {
                                    // Should not fail since we just modified capacity
                                    // above. Log it, crash in safe builds, clear style
                                    // in unsafe builds.
                                    log::error!(
                                        target: LOG_TARGET,
                                        "addWithId failed after capacity increase err={:?}",
                                        err2
                                    );
                                    if cfg!(debug_assertions) {
                                        // Force a crash with safe builds.
                                        unreachable!();
                                    }

                                    (*self.page_cell).set_style_id(stylepkg::DEFAULT_ID);
                                    break 'style;
                                }
                            }
                        }
                    };

                    // Update our style cache with the latest style set so runs
                    // of cells with the same style are faster to write.
                    self.style_cache = StyleCache {
                        src_page,
                        src_id: cell.style_id(),
                        dst_id: id,
                    };

                    (*self.page_row).set_styled(true);
                    (*self.page_cell).set_style_id(id);
                }
            }

            self.cursor_forward();
            Ok(WriteCellResult::Success)
        }
    }

    /// Create a new page in the provided list with the provided
    /// capacity then clone the row currently being worked on to
    /// it and delete it from the old page. Places cursor in the
    /// same position it was in in the old row in the new one.
    ///
    /// Asserts that the cursor is on the final row of the page.
    ///
    /// Expects that the provided capacity is sufficient to copy
    /// the row.
    ///
    /// If this is the only row in the page, the page is removed
    /// from the list after cloning the row.
    fn move_last_row_to_new_page(&mut self, list: &mut PageList, cap: Capacity) -> Result<(), AllocError> {
        // SAFETY: live destination nodes and pins.
        unsafe {
            assert!(self.y == (*self.page).size.rows - 1);
            assert!(!self.pending_wrap);

            let old_node = self.node;
            let old_page = self.page;
            let old_row = self.page_row;
            let old_x = self.x;

            // Our total row count never changes, because we're removing one
            // row from the last page and moving it into a new page.
            let old_total_rows = self.total_rows;

            let r = self.cursor_new_page(list, cap);
            self.total_rows = old_total_rows;
            r?;
            assert!(self.node != old_node);
            assert!(self.y == 0);

            // We have no cleanup for our old state from here on out. No failures!

            // Restore the x position of the cursor.
            self.cursor_absolute(old_x, 0);

            // Copy our old data. This should NOT fail because we have the
            // capacity of the old page which already fits the data we requested.
            if let Err(err) = (*self.page).clone_row_from(old_page, &mut *self.page_row, &*old_row) {
                log::error!(target: LOG_TARGET, "error cloning single row for moveLastRowToNewPage err={:?}", err);
                panic!("unexpected copy row failure");
            }

            // Move any tracked pins from that last row into this new node.
            for &p in list.tracked_pins.iter() {
                let p = pinr(p);
                if nd(p.node).page() as *mut Page != old_page || p.y != (*old_page).size.rows - 1 {
                    continue;
                }

                p.node = self.node;
                p.y = self.y;
                // p.x remains the same since we're copying the row as-is
            }

            // Reset the row on the old page and truncate it. The retired
            // storage must be left in the default state (see resetRow).
            (*old_page).reset_row(&mut *old_row);
            (*old_page).size.rows -= 1;

            // If that was the last row in that page
            // then we should remove it from the list.
            if (*old_page).size.rows == 0 {
                list.pages.remove(old_node);
                list.destroy_node(old_node);
            }

            self.total_rows = old_total_rows;
        }

        Ok(())
    }

    /// Increase the capacity of the current page.
    fn increase_capacity(
        &mut self,
        list: &mut PageList,
        adjustment: Option<IncreaseCapacity>,
    ) -> Result<(), IncreaseCapacityError> {
        let old_x = self.x;
        let old_y = self.y;
        let old_total_rows = self.total_rows;

        let node = {
            // Pause integrity checks because the total row count won't
            // be correct during a reflow.
            list.pause_integrity_checks(true);
            let r = list.increase_capacity(self.node, adjustment);
            list.pause_integrity_checks(false);
            r?
        };
        // We must not fail after this, we've modified our self.node
        // and we need to fix it up.

        *self = ReflowCursor::init(node);
        self.cursor_absolute(old_x, old_y);
        self.total_rows = old_total_rows;
        Ok(())
    }

    /// True if the string allocator of the current page can fit the
    /// allocations that duping the given source hyperlink performs.
    /// The test allocations are freed before returning.
    ///
    /// This must mirror the exact allocation pattern of
    /// hyperlink.PageEntry.dupe: the URI and the explicit ID (if any)
    /// are allocated separately. The string allocator rounds every
    /// allocation up to its chunk size and requires each allocation to
    /// be contiguous, so a single combined allocation of the total byte
    /// length can succeed where the two separate allocations performed
    /// by dupe would fail.
    fn hyperlink_strings_fit(&self, src_link: &hyperlink::PageEntry) -> bool {
        // SAFETY: live destination page.
        unsafe {
            let page = &mut *self.page;
            let uri_buf = match page.string_alloc.alloc::<u8>(page.memory, src_link.uri.len) {
                Ok(b) => b,
                Err(_) => return false,
            };

            let ok = match src_link.id {
                hyperlink::PageEntryId::Implicit(_) => true,
                hyperlink::PageEntryId::Explicit(v) => match page.string_alloc.alloc::<u8>(page.memory, v.len) {
                    Ok(id_buf) => {
                        page.string_alloc.free(page.memory, id_buf);
                        true
                    }
                    Err(_) => false,
                },
            };

            page.string_alloc.free(page.memory, uri_buf);
            ok
        }
    }

    /// True if this cursor is at the bottom of the page by capacity,
    /// i.e. we can't scroll anymore.
    fn bottom(&self) -> bool {
        // SAFETY: live destination page.
        self.y == unsafe { (*self.page).capacity.rows } - 1
    }

    fn cursor_forward(&mut self) {
        // SAFETY: live destination page; the cell stays within the row.
        unsafe {
            if self.x == (*self.page).size.cols - 1 {
                self.pending_wrap = true;
            } else {
                self.page_cell = self.page_cell.add(1);
                self.x += 1;
            }
        }
    }

    /// Create a new row and move the cursor down.
    ///
    /// Asserts that the cursor is on the bottom row of the
    /// page and that there is capacity to add a new one.
    fn cursor_scroll(&mut self) {
        // SAFETY: live destination page; the next row is within capacity.
        unsafe {
            // Scrolling requires that we're on the bottom of our page.
            // We also assert that we have capacity because reflow always
            // works within the capacity of the page.
            assert!(self.y == (*self.page).size.rows - 1);
            assert!((*self.page).size.rows < (*self.page).capacity.rows);

            // Increase our page size
            (*self.page).size.rows += 1;

            // With the increased page size, safely move down a row.
            let row = self.page_row.add(1);
            self.page_row = row;
            self.page_cell = (*row).cells().ptr((*self.page).memory);
            self.pending_wrap = false;
            self.x = 0;
            self.y += 1;
        }
    }

    /// Create a new page in the provided list with the provided
    /// capacity and one row and move the cursor in to it at 0,0
    fn cursor_new_page(&mut self, list: &mut PageList, cap: Capacity) -> Result<(), AllocError> {
        // Remember our new row count so we can restore it
        // after reinitializing our cursor on the new page.
        let new_rows = self.new_rows;

        let node = list.create_page(CreatePage::cap(cap))?;
        // SAFETY: fresh live node; the cursor node is in the list.
        unsafe {
            nd(node).page().size.rows = 1;
            list.pages.insert_after(self.node, node);
        }

        *self = ReflowCursor::init(node);
        self.new_rows = new_rows;
        Ok(())
    }

    /// Performs `cursorScroll` or `cursorNewPage` as necessary
    /// depending on if the cursor is currently at the bottom.
    fn cursor_scroll_or_new_page(&mut self, list: &mut PageList, cap: Capacity) -> Result<(), AllocError> {
        // The functions below may overwrite self so we need to cache
        // our total rows. We add one because no matter what when this
        // returns we'll have one more row added.
        let new_total_rows: usize = self.total_rows + 1;

        let r = if self.bottom() {
            self.cursor_new_page(list, cap)
        } else {
            self.cursor_scroll();
            Ok(())
        };
        self.total_rows = new_total_rows;
        r
    }

    fn cursor_absolute(&mut self, x: CellCountInt, y: CellCountInt) {
        // SAFETY: live destination page; x/y are within it.
        unsafe {
            assert!(x < (*self.page).size.cols);
            assert!(y < (*self.page).size.rows);

            let row: *mut Row = match y.cmp(&self.y) {
                core::cmp::Ordering::Equal => self.page_row,
                core::cmp::Ordering::Less => self.page_row.sub((self.y - y) as usize),
                core::cmp::Ordering::Greater => self.page_row.add((y - self.y) as usize),
            };
            self.page_row = row;
            self.page_cell = (*row).cells().ptr((*self.page).memory).add(x as usize);
            self.pending_wrap = false;
            self.x = x;
            self.y = y;
        }
    }

    #[allow(dead_code)]
    fn count_trailing_empty_cells(&self) -> usize {
        // SAFETY: live destination page and row.
        unsafe {
            // If the row is wrapped, all empty cells are meaningful.
            if (*self.page_row).wrap() {
                return 0;
            }

            let cells = self.page_cell;
            let len: usize = ((*self.page).size.cols - self.x) as usize;
            for i in 0..len {
                let rev_i = len - i - 1;
                if !(*cells.add(rev_i)).is_empty() {
                    return i;
                }
            }

            // If the row has a semantic prompt then the blank row is meaningful
            // so we always return all but one so that the row is drawn.
            if (*self.page_row).semantic_prompt() != pagepkg::SemanticPrompt::None {
                return len - 1;
            }

            len
        }
    }

    fn copy_row_metadata(&mut self, other: &Row) {
        // SAFETY: live destination row.
        unsafe { (*self.page_row).set_semantic_prompt(other.semantic_prompt()) };
    }
}

/// True if this cell can be copied verbatim as part of a bulk
/// run: a narrow plain-text or bg-color cell with no managed
/// memory (graphemes, hyperlinks) and no special reflow handling
/// (wide characters, spacers, Kitty virtual placeholders). For
/// these cells writeCell reduces to a copy of the raw cell plus
/// a style ref count adjustment.
#[inline]
fn bulk_copyable(cell: pagepkg::Cell) -> bool {
    match cell.content_tag() {
        pagepkg::ContentTag::Codepoint => {
            if cell.wide() != pagepkg::Wide::Narrow {
                return false;
            }
            if cell.hyperlink() {
                return false;
            }
            if build_options::KITTY_GRAPHICS {
                // Placeholders must set a row flag, so they take
                // the slow path.
                if cell.content_codepoint() == graphics_unicode::PLACEHOLDER {
                    return false;
                }
            }
            true
        }

        // Grapheme data must be cloned cell-by-cell.
        pagepkg::ContentTag::CodepointGrapheme => false,

        // These are guaranteed to have no style or grapheme data
        // (see writeCell) so they are pure copies. The style
        // check is defensive so that a bg cell can never join or
        // extend a styled run.
        pagepkg::ContentTag::BgColorPalette | pagepkg::ContentTag::BgColorRgb => {
            cell.style_id() == stylepkg::DEFAULT_ID
        }
    }
}

/// The length of the prefix of cells that can be copied at once
/// with copyRun: bulk-copyable cells sharing a single style. The
/// scan uses masked compares of the raw cell bits (see
/// BulkRunMask), which is significantly cheaper than the
/// field-wise predicate for this hot loop.
fn bulk_run_length(cells: &[pagepkg::Cell]) -> usize {
    if cells.is_empty() {
        return 0;
    }
    let first = cells[0];

    // A run may start on a bulk-copyable narrow cell or on a wide pair.
    let pair_start =
        first.content_tag() == pagepkg::ContentTag::Codepoint && first.wide() == pagepkg::Wide::Wide && !first.hyperlink();
    if !pair_start && !bulk_copyable(first) {
        return 0;
    }

    // Patterns for each cell shape admitted to the run.
    let mut proto = first;
    proto.set_wide(pagepkg::Wide::Narrow);
    let narrow_pattern = BulkRunMask::pattern(proto);
    proto.set_wide(pagepkg::Wide::Wide);
    let wide_pattern = BulkRunMask::pattern(proto);
    proto.set_wide(pagepkg::Wide::SpacerTail);
    let tail_pattern = BulkRunMask::pattern(proto);

    // Only text cells can contain a placeholder; for bg color
    // tags the content bits are a color, so we skip the check for
    // those (the tag is part of BulkRunMask, making tags uniform
    // per run).
    let check_placeholder = build_options::KITTY_GRAPHICS && first.content_tag() == pagepkg::ContentTag::Codepoint;
    let placeholder_pattern = if build_options::KITTY_GRAPHICS {
        PlaceholderMask::pattern(pagepkg::Cell::init(graphics_unicode::PLACEHOLDER))
    } else {
        0
    };

    let mut len: usize = 0;
    'outer: loop {
        // Vectorized scan: check whole groups of narrow cells at
        // once. If a group fully matches, the run extends by the
        // whole group; otherwise fall through to the scalar loop
        // below, which finds the exact end of the run within it
        // or continues through a wide pair.
        while cells.len() - len >= BULK_GROUP_LEN {
            if !BulkRunMask::eql(cells, len, narrow_pattern) {
                break;
            }
            if check_placeholder && PlaceholderMask::eql_any(cells, len, placeholder_pattern) {
                break;
            }
            len += BULK_GROUP_LEN;
        }

        while len < cells.len() {
            let cell = cells[len];
            if BulkRunMask::eql_scalar(cell, narrow_pattern) {
                if check_placeholder && PlaceholderMask::eql_scalar(cell, placeholder_pattern) {
                    break 'outer;
                }
                len += 1;
                continue;
            }

            // Wide pairs are consumed atomically so the run
            // (or window) end can never split a pair.
            if len + 1 < cells.len()
                && BulkRunMask::eql_scalar(cell, wide_pattern)
                && BulkRunMask::eql_scalar(cells[len + 1], tail_pattern)
            {
                len += 2;
                // Re-enter the vectorized loop: narrow groups
                // commonly follow a stretch of pairs.
                continue 'outer;
            }

            break 'outer;
        }

        break;
    }

    len
}

// ---------------------------------------------------------------------------
// Iterators
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptIterator {
    /// The pin that we are currently at. Also the starting pin when
    /// initializing.
    pub current: Option<Pin>,

    /// The pin to end at or null if we end when we can't traverse
    /// anymore.
    pub limit: Option<Pin>,

    /// The direction to do the traversal.
    pub direction: Direction,
}

impl PromptIterator {
    pub const EMPTY: PromptIterator = PromptIterator {
        current: None,
        limit: None,
        direction: Direction::LeftUp,
    };

    pub fn next_right_down(&mut self) -> Option<Pin> {
        // Start at our current pin. If we have no current it means
        // we reached the end and we're done.
        let start: Pin = self.current?;

        // SAFETY: pins produced by up/down are valid.
        let semantic = |p: Pin| unsafe { (*p.row_and_cell().row).semantic_prompt() };

        // We need to traverse downwards and look for prompts.
        let mut current: Option<Pin> = Some(start);
        while let Some(p) = current {
            // Check our limit.
            let at_limit = self.limit.is_some_and(|limit| limit.eql(p));

            match semantic(p) {
                // This row isn't a prompt. Keep looking.
                pagepkg::SemanticPrompt::None => {
                    if at_limit {
                        break;
                    }
                }

                // This is a prompt line or continuation line. In either
                // case we consider the first line the prompt, and then
                // skip over any remaining prompt lines. This handles the
                // case where scrollback pruned the prompt.
                pagepkg::SemanticPrompt::Prompt | pagepkg::SemanticPrompt::PromptContinuation => {
                    // If we're at our limit just return this prompt.
                    if at_limit {
                        self.current = None;
                        return Some(p.left(p.x as usize));
                    }

                    // Skip over any continuation lines that follow this prompt,
                    // up to our limit.
                    let mut end_pin = p;
                    while let Some(next_pin) = end_pin.down(1) {
                        match semantic(next_pin) {
                            pagepkg::SemanticPrompt::PromptContinuation => {
                                if let Some(limit) = self.limit {
                                    if limit.eql(next_pin) {
                                        break;
                                    }
                                }
                            }

                            pagepkg::SemanticPrompt::Prompt | pagepkg::SemanticPrompt::None => {
                                self.current = Some(next_pin);
                                return Some(p.left(p.x as usize));
                            }
                        }
                        end_pin = next_pin;
                    }

                    self.current = None;
                    return Some(p.left(p.x as usize));
                }
            }

            current = p.down(1);
        }

        self.current = None;
        None
    }

    pub fn next_left_up(&mut self) -> Option<Pin> {
        // Start at our current pin. If we have no current it means
        // we reached the end and we're done.
        let start: Pin = self.current?;

        // SAFETY: pins produced by up/down are valid.
        let semantic = |p: Pin| unsafe { (*p.row_and_cell().row).semantic_prompt() };

        // We need to traverse upwards and look for prompts.
        let mut current: Option<Pin> = Some(start);
        while let Some(p) = current {
            // Check our limit.
            let at_limit = self.limit.is_some_and(|limit| limit.eql(p));

            match semantic(p) {
                // This row isn't a prompt. Keep looking.
                pagepkg::SemanticPrompt::None => {
                    if at_limit {
                        break;
                    }
                }

                // This is a prompt line.
                pagepkg::SemanticPrompt::Prompt => {
                    self.current = if at_limit { None } else { p.up(1) };
                    return Some(p.left(p.x as usize));
                }

                // If this is a prompt continuation, then we continue
                // looking for the start of the prompt OR a non-prompt
                // line, whichever is first. The non-prompt line is to handle
                // poorly behaved programs or scrollback that's been cut-off.
                pagepkg::SemanticPrompt::PromptContinuation => {
                    // If we're at our limit just return this continuation as prompt.
                    if at_limit {
                        self.current = None;
                        return Some(p.left(p.x as usize));
                    }

                    let mut end_pin = p;
                    while let Some(prior) = end_pin.up(1) {
                        if let Some(limit) = self.limit {
                            if limit.eql(prior) {
                                break;
                            }
                        }

                        match semantic(prior) {
                            // No prompt. That means our last pin is good!
                            pagepkg::SemanticPrompt::None => {
                                self.current = Some(prior);
                                return Some(end_pin.left(end_pin.x as usize));
                            }

                            // Prompt continuation, keep looking.
                            pagepkg::SemanticPrompt::PromptContinuation => {}

                            // Prompt! Found it!
                            pagepkg::SemanticPrompt::Prompt => {
                                self.current = prior.up(1);
                                return Some(prior.left(prior.x as usize));
                            }
                        }
                        end_pin = prior;
                    }

                    // No prior rows, trimmed scrollback probably.
                    self.current = None;
                    return Some(p.left(p.x as usize));
                }
            }

            current = p.up(1);
        }

        self.current = None;
        None
    }
}

impl Iterator for PromptIterator {
    type Item = Pin;

    /// Return the next pin that represents the first row in a prompt.
    /// From here, you can find the prompt input, command output, etc.
    fn next(&mut self) -> Option<Pin> {
        match self.direction {
            Direction::LeftUp => self.next_left_up(),
            Direction::RightDown => self.next_right_down(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CellIterator {
    pub row_it: RowIterator,
    pub cell: Option<Pin>,
}

impl Iterator for CellIterator {
    type Item = Pin;

    fn next(&mut self) -> Option<Pin> {
        let cell = self.cell?;

        // SAFETY: valid pins from the row iterator.
        match self.row_it.page_it.direction {
            Direction::RightDown => {
                if cell.x + 1 < unsafe { nd(cell.node).cols() } {
                    // We still have cells in this row, increase x.
                    let mut copy = cell;
                    copy.x += 1;
                    self.cell = Some(copy);
                } else {
                    // We need to move to the next row.
                    self.cell = self.row_it.next();
                }
            }

            Direction::LeftUp => {
                if cell.x > 0 {
                    // We still have cells in this row, decrease x.
                    let mut copy = cell;
                    copy.x -= 1;
                    self.cell = Some(copy);
                } else {
                    // We need to move to the previous row and last col
                    self.cell = self.row_it.next().map(|next_cell| {
                        let mut copy = next_cell;
                        copy.x = unsafe { nd(next_cell.node).cols() } - 1;
                        copy
                    });
                }
            }
        }

        Some(cell)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RowIterator {
    pub page_it: PageIterator,
    pub chunk: Option<Chunk>,
    pub offset: CellCountInt,
}

impl Iterator for RowIterator {
    type Item = Pin;

    fn next(&mut self) -> Option<Pin> {
        let chunk = self.chunk?;
        let row = Pin {
            y: self.offset,
            ..Pin::new(chunk.node)
        };

        match self.page_it.direction {
            Direction::RightDown => {
                // Increase our offset in the chunk
                self.offset += 1;

                // If we are beyond the chunk end, we need to move to the next chunk.
                if self.offset >= chunk.end {
                    self.chunk = self.page_it.next();
                    if let Some(c) = self.chunk {
                        self.offset = c.start;
                    }
                }
            }

            Direction::LeftUp => {
                // If we are at the start of the chunk, we need to move to the
                // previous chunk.
                if self.offset == 0 {
                    self.chunk = self.page_it.next();
                    if let Some(c) = self.chunk {
                        self.offset = c.end - 1;
                    }
                } else {
                    // If we're at the start of the chunk and its a non-zero
                    // offset then we've reached a limit.
                    if self.offset == chunk.start {
                        self.chunk = None;
                    } else {
                        self.offset -= 1;
                    }
                }
            }
        }

        Some(row)
    }
}

/// The limit of a [`PageIterator`] (Zig: `PageIterator.Limit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageIteratorLimit {
    #[default]
    None,
    Count(usize),
    Row(Pin),
}

#[derive(Debug, Clone, Copy)]
pub struct PageIterator {
    pub row: Option<Pin>,
    pub limit: PageIteratorLimit,
    pub direction: Direction,
}

impl Default for PageIterator {
    fn default() -> Self {
        PageIterator {
            row: None,
            limit: PageIteratorLimit::None,
            direction: Direction::RightDown,
        }
    }
}

impl PageIterator {
    fn next_down(&mut self) -> Option<Chunk> {
        // Get our current row location
        let row = self.row?;
        // SAFETY: the iterator's rows are valid pins.
        let n = unsafe { nd(row.node) };

        match &mut self.limit {
            PageIteratorLimit::None => {
                // If we have no limit, then we consume this entire page. Our
                // next row is the next page.
                self.row = n.next.map(Pin::new);

                Some(Chunk {
                    node: row.node,
                    start: row.y,
                    end: n.rows(),
                })
            }

            PageIteratorLimit::Count(limit) => {
                assert!(*limit > 0); // should be handled already
                let available: usize = (n.rows() - row.y) as usize;
                let len = available.min(*limit);
                *limit -= len;
                self.row = if *limit > 0 { row.down(len) } else { None };

                Some(Chunk {
                    node: row.node,
                    start: row.y,
                    end: (row.y as usize + len) as CellCountInt,
                })
            }

            PageIteratorLimit::Row(limit_row) => {
                // If this is not the same page as our limit then we
                // can consume the entire page.
                if limit_row.node != row.node {
                    self.row = n.next.map(Pin::new);

                    return Some(Chunk {
                        node: row.node,
                        start: row.y,
                        end: n.rows(),
                    });
                }

                // If this is the same page then we only consume up to
                // the limit row.
                let limit_y = limit_row.y;
                self.row = None;
                if row.y > limit_y {
                    return None;
                }
                Some(Chunk {
                    node: row.node,
                    start: row.y,
                    end: limit_y + 1,
                })
            }
        }
    }

    fn next_up(&mut self) -> Option<Chunk> {
        // Get our current row location
        let row = self.row?;
        // SAFETY: the iterator's rows are valid pins.
        let n = unsafe { nd(row.node) };

        let prev_row = |n: &Node| {
            n.prev.map(|next_page| Pin {
                // SAFETY: live list node.
                y: unsafe { nd(next_page).rows() } - 1,
                ..Pin::new(next_page)
            })
        };

        match &mut self.limit {
            PageIteratorLimit::None => {
                // If we have no limit, then we consume this entire page. Our
                // next row is the next page.
                self.row = prev_row(n);

                Some(Chunk {
                    node: row.node,
                    start: 0,
                    end: row.y + 1,
                })
            }

            PageIteratorLimit::Count(limit) => {
                assert!(*limit > 0); // should be handled already
                let available: usize = row.y as usize + 1;
                let len = available.min(*limit);
                *limit -= len;
                self.row = if *limit > 0 { row.up(len) } else { None };

                Some(Chunk {
                    node: row.node,
                    start: (available - len) as CellCountInt,
                    end: available as CellCountInt,
                })
            }

            PageIteratorLimit::Row(limit_row) => {
                // If this is not the same page as our limit then we
                // can consume the entire page.
                if limit_row.node != row.node {
                    self.row = prev_row(n);

                    return Some(Chunk {
                        node: row.node,
                        start: 0,
                        end: row.y + 1,
                    });
                }

                // If this is the same page then we only consume up to
                // the limit row.
                let limit_y = limit_row.y;
                self.row = None;
                if row.y < limit_y {
                    return None;
                }
                Some(Chunk {
                    node: row.node,
                    start: limit_y,
                    end: row.y + 1,
                })
            }
        }
    }
}

impl Iterator for PageIterator {
    type Item = Chunk;

    fn next(&mut self) -> Option<Chunk> {
        match self.direction {
            Direction::LeftUp => self.next_up(),
            Direction::RightDown => self.next_down(),
        }
    }
}

/// A chunk of rows in one page (Zig: `PageIterator.Chunk`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub node: NodePtr,

    /// Start y index (inclusive) of this chunk in the page.
    pub start: CellCountInt,

    /// End y index (exclusive) of this chunk in the page.
    pub end: CellCountInt,
}

impl Chunk {
    /// The rows of this chunk (Zig: `[]Row`).
    pub fn rows(&self) -> *mut [Row] {
        // SAFETY: live chunk node.
        unsafe {
            let page = nd(self.node).page();
            let rows_ptr = page.rows.ptr(page.memory);
            core::ptr::slice_from_raw_parts_mut(rows_ptr.add(self.start as usize), (self.end - self.start) as usize)
        }
    }

    /// Returns true if this chunk represents every row in the page.
    pub fn full_page(&self) -> bool {
        // SAFETY: live chunk node.
        self.start == 0 && self.end == unsafe { nd(self.node).rows() }
    }

    /// Returns true if this chunk overlaps with the given other chunk
    /// in any way.
    pub fn overlaps(&self, other: Chunk) -> bool {
        if self.node != other.node {
            return false;
        }
        if self.end <= other.start {
            return false;
        }
        if self.start >= other.end {
            return false;
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Memory accounting
// ---------------------------------------------------------------------------

/// Snapshot of the storage used by page nodes in this list.
///
/// The raw byte counts describe page backing mappings only. They exclude
/// nodes, allocator metadata, unused preheated pool items, and the small
/// representation values stored in each node. A compressed page retains its
/// raw mapping as virtual address space, but its bytes are counted as
/// decommitted because strict reclamation succeeded before the state was
/// published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryStats {
    /// Pages whose raw backing mappings are resident.
    pub resident_pages: usize,

    /// Pages represented by encoded storage and a decommitted raw mapping.
    pub compressed_pages: usize,

    /// Logical bytes in every raw page mapping.
    pub raw_bytes: usize,

    /// Raw mapping bytes which remain resident.
    pub resident_raw_bytes: usize,

    /// Raw mapping bytes discarded for compressed pages.
    pub decommitted_raw_bytes: usize,

    /// Raw allocation bytes which remain physically resident.
    ///
    /// This can exceed `resident_raw_bytes` because a pool-owned page uses
    /// only part of a standard pool item. Compressing such a page decommits
    /// its initialized range, but the unused tail of the item stays resident.
    pub resident_backing_bytes: usize,

    /// Exact encoded allocations retained for compressed pages.
    pub encoded_bytes: usize,
}

impl MemoryStats {
    /// Estimate resident page backing storage after compression.
    pub fn estimated_resident_bytes(self) -> usize {
        self.resident_backing_bytes + self.encoded_bytes
    }

    /// Estimate physical bytes avoided by compressed page backing storage.
    pub fn estimated_savings(self) -> usize {
        self.decommitted_raw_bytes.saturating_sub(self.encoded_bytes)
    }
}

/// The memory held by a page list. Returned by `memoryUsage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryUsage {
    /// Number of pages in the list, including compressed pages.
    pub pages: usize,

    /// Bytes of address space reserved for page memory. This counts every
    /// page in the list at its full allocated size, whether it is resident
    /// or compressed, plus the unused items held by the page pool. Unused
    /// pool items have had their physical memory released, so they add to
    /// this figure but never to `resident_bytes`.
    pub virtual_bytes: usize,

    /// Bytes of physical memory used by pages. A resident page counts its
    /// full allocated size. A compressed page counts its encoded data plus
    /// any unused tail of its pool item, which compression doesn't release.
    /// This is the same as `MemoryStats.estimatedResidentBytes`.
    pub resident_bytes: usize,

    /// Number of pages stored compressed.
    pub compressed_pages: usize,

    /// Total size of the encoded data of compressed pages. This is already
    /// part of `resident_bytes`.
    pub compressed_bytes: usize,
}

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// Runtime-configurable byte and line limits for a PageList.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub bytes: Limit,
    pub lines: Limit,
}

/// The limit keys (Zig: `Limits.Key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKey {
    Bytes,
    Lines,
}

/// (Zig: `Limits.Limit`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    /// Explicit is the specified maximum value for this limit
    /// by the user or maxInt otherwise.
    pub explicit: usize,

    /// Min is the minimum valid maximum for this entry, so if
    /// explicit is lower than this then min wins.
    pub min: usize,
}

impl Limit {
    fn new(min: usize) -> Limit {
        Limit {
            explicit: usize::MAX,
            min,
        }
    }
}

impl Limits {
    /// Return unlimited limits with minimums for the given PageList size.
    pub fn init(cols: CellCountInt, rows: CellCountInt) -> Limits {
        Limits {
            bytes: Limit::new(Self::min_max_size(cols, rows)),
            lines: Limit::new(Self::min_max_lines(cols)),
        }
    }

    /// Set an explicit limit. Null means unlimited. This does not enforce the
    /// new value automatically; the caller must still call `enforce`.
    pub fn set(&mut self, key: LimitKey, value: Option<usize>) {
        match key {
            LimitKey::Bytes => self.bytes.explicit = value.unwrap_or(usize::MAX),
            LimitKey::Lines => self.lines.explicit = value.unwrap_or(usize::MAX),
        }
    }

    /// Recalculate the effective minimums for a new PageList size.
    ///
    /// This must be called whenever either PageList dimension changes so the
    /// effective byte and line limits remain valid for the new size.
    pub fn resize(&mut self, cols: CellCountInt, rows: CellCountInt) {
        self.bytes.min = Self::min_max_size(cols, rows);
        self.lines.min = Self::min_max_lines(cols);
    }

    /// Return the effective maximum for a limit.
    pub fn max(&self, key: LimitKey) -> usize {
        match key {
            LimitKey::Bytes => self.bytes.explicit.max(self.bytes.min),
            LimitKey::Lines => self.lines.explicit.max(self.lines.min),
        }
    }

    /// Whether the given limit is currently exceeded. Both limits are
    /// heuristics: complete historical pages are the smallest unit that
    /// enforcement removes.
    pub fn exceeded(&self, pagelist: &PageList, limit: LimitKey) -> bool {
        match limit {
            LimitKey::Bytes => pagelist.page_size > self.max(LimitKey::Bytes),
            LimitKey::Lines => {
                pagelist.total_rows > pagelist.rows as usize
                    && pagelist.total_rows - pagelist.rows as usize > self.max(LimitKey::Lines)
            }
        }
    }

    /// Prune complete historical pages until the selected limit is satisfied
    /// or the oldest remaining page overlaps the active area.
    pub fn enforce(&self, pagelist: &mut PageList, key: LimitKey) {
        if !self.exceeded(pagelist, key) {
            return;
        }

        // A partially constructed clone can temporarily contain fewer rows than
        // its active area. It has no scrollback to prune.
        if pagelist.total_rows <= pagelist.rows as usize {
            return;
        }

        // Accumulate the row delta so viewport offsets are fixed up once
        // after all eligible pages have been removed.
        let mut removed: usize = 0;
        while self.exceeded(pagelist, key) {
            let first = pagelist.pages.first.unwrap();

            // The page containing the active top may also contain history. Keep
            // that boundary page whole even if its history exceeds the heuristic.
            if first == pagelist.get_top_left(point::Tag::Active).node {
                break;
            }

            if removed == 0 {
                pagelist.page_compression.mark_activity();
            }

            // SAFETY: live first node.
            let first_rows = unsafe { nd(first).rows() } as usize;

            // Automatic pruning invalidates the content represented by pins in
            // the removed page. erasePage remaps them to the next page below but
            // only the enforcing caller knows that their original content is gone,
            // so mark them as garbage here.
            for &p in pagelist.tracked_pins.iter() {
                // SAFETY: tracked pin.
                let p = unsafe { pinr(p) };
                if p.node == first {
                    p.garbage = true;
                }
            }

            // erasePage updates the list, pin targets, and byte accounting.
            // Row accounting belongs to the caller because erasePage is also used
            // by paths that already adjusted total_rows.
            pagelist.erase_page(first);
            pagelist.total_rows -= first_rows;
            removed += first_rows;
        }

        // Reconcile viewport mode and cached row offsets with the combined prefix
        // removal only after every page and pin points into the final list.
        if removed > 0 {
            pagelist.fixup_viewport(removed);
        }
    }

    /// Returns the minimum valid "max size" for a given number of rows and cols
    /// such that we can fit the active area AND at least two pages. Note we
    /// need the two pages for algorithms to work properly (such as grow) but
    /// we don't need to fit double the active area.
    ///
    /// This min size may not be totally correct in the case that a large
    /// number of other dimensions makes our row size in a page very small.
    /// But this gives us a nice fast heuristic for determining min/max size.
    /// Therefore, if the page size is violated you should always also verify
    /// that we have enough space for the active area.
    fn min_max_size(cols: CellCountInt, rows: CellCountInt) -> usize {
        // Get our capacity to fit our rows. If the cols are too big, it may
        // force less rows than we want meaning we need more than one page to
        // represent a viewport.
        let cap = initial_capacity(cols);

        // Calculate the number of standard sized pages we need to represent
        // an active area.
        let pages_exact = if cap.rows >= rows {
            1
        } else {
            (rows as usize).div_ceil(cap.rows as usize)
        };

        // We always need at least one page extra so that we
        // can fit partial pages to spread our active area across two pages.
        // Even for caps that can't fit all rows in a single page, we add one
        // because the most extra space we need at any given time is only
        // the partial amount of one page.
        let pages = pages_exact + 1;
        assert!(pages >= 2);

        PagePool::item_size() * pages
    }

    /// Returns the minimum line limit for a given column count. Line limits are
    /// page-granular, so we always permit at least one standard page worth of
    /// scrollback rows.
    fn min_max_lines(cols: CellCountInt) -> usize {
        initial_capacity(cols).rows as usize
    }
}

// ---------------------------------------------------------------------------
// Pin
// ---------------------------------------------------------------------------

/// Represents an exact x/y coordinate within the screen. This is called
/// a "pin" because it is a fixed point within the pagelist direct to
/// a specific page pointer and memory offset. The benefit is that this
/// point remains valid even through scrolling without any additional work.
///
/// A downside is that  the pin is only valid until the pagelist is modified
/// in a way that may invalidate page pointers or shuffle rows, such as resizing,
/// erasing rows, etc.
///
/// A pin can also be "tracked" which means that it will be updated as the
/// PageList is modified.
///
/// The PageList maintains a list of active pin references and keeps them
/// all up to date as the pagelist is modified. This isn't cheap so callers
/// should limit the number of active pins as much as possible.
///
/// Port note: the methods below dereference `node`; calling them on an
/// invalid pin is undefined behavior (see the module notes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pin {
    pub node: NodePtr,
    pub y: CellCountInt,
    pub x: CellCountInt,

    /// This is flipped to true for tracked pins that were tracking
    /// a page that got pruned for any reason and where the tracked pin
    /// couldn't be moved to a sensical location. Users of the tracked
    /// pin could use this data and make their own determination of
    /// semantics.
    pub garbage: bool,
}

// SAFETY: a pin is plain data; dereferencing it is governed by the
// PageList's exclusive-access contract.
unsafe impl Send for Pin {}

/// The result of [`Pin::down_overflow`]/[`Pin::up_overflow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOverflow {
    Offset(Pin),
    Overflow { end: Pin, remaining: usize },
}

impl Pin {
    /// A pin at the top-left of the given node (Zig: `.{ .node = node }`).
    #[inline]
    pub const fn new(node: NodePtr) -> Pin {
        Pin {
            node,
            y: 0,
            x: 0,
            garbage: false,
        }
    }

    /// The node of this pin (Zig: `pin.node.*`).
    ///
    /// # Safety
    ///
    /// The pin must be valid.
    #[inline]
    pub unsafe fn node_mut<'a>(&self) -> &'a mut Node {
        unsafe { nd(self.node) }
    }

    #[inline]
    pub fn row_and_cell(&self) -> pagepkg::RowAndCell {
        // SAFETY: valid pin (type contract).
        unsafe { nd(self.node).page().get_row_and_cell(self.x as usize, self.y as usize) }
    }

    /// Returns the cells for the row that this pin is on. The subset determines
    /// what subset of the cells are returned. The "left/right" subsets are
    /// inclusive of the x coordinate of the pin.
    #[inline]
    pub fn cells(&self, subset: CellSubset) -> *mut [pagepkg::Cell] {
        // SAFETY: valid pin (type contract).
        unsafe {
            let page = nd(self.node).page();
            let rac = page.get_row_and_cell(self.x as usize, self.y as usize);
            let all = page.get_cells(&*rac.row);
            let ptr = all as *mut pagepkg::Cell;
            match subset {
                CellSubset::All => all,
                CellSubset::Left => core::ptr::slice_from_raw_parts_mut(ptr, self.x as usize + 1),
                CellSubset::Right => {
                    core::ptr::slice_from_raw_parts_mut(ptr.add(self.x as usize), all.len() - self.x as usize)
                }
            }
        }
    }

    /// Returns the grapheme codepoints for the given cell. These are only
    /// the EXTRA codepoints and not the first codepoint.
    #[inline]
    pub fn grapheme(&self, cell: &pagepkg::Cell) -> Option<&[u32]> {
        // SAFETY: valid pin (type contract).
        unsafe { nd(self.node).page().lookup_grapheme(cell) }
    }

    /// Returns the style for the given cell in this pin.
    #[inline]
    pub fn style(&self, cell: &pagepkg::Cell) -> stylepkg::Style {
        if cell.style_id() == stylepkg::DEFAULT_ID {
            return stylepkg::Style::default();
        }
        // SAFETY: valid pin (type contract).
        unsafe {
            let page = nd(self.node).page();
            *page.styles.get(page.memory, cell.style_id())
        }
    }

    /// Check if this pin is dirty.
    #[inline]
    pub fn is_dirty(&self) -> bool {
        // SAFETY: valid pin (type contract).
        unsafe {
            let page = nd(self.node).page();
            page.dirty || (*page.get_row_and_cell(self.x as usize, self.y as usize).row).dirty()
        }
    }

    /// Mark this pin location as dirty.
    #[inline]
    pub fn mark_dirty(&self) {
        // SAFETY: valid pin (type contract).
        unsafe { (*self.row_and_cell().row).set_dirty(true) };
    }

    /// Iterators. These are the same as PageList iterator funcs but operate
    /// on pins rather than points. This is MUCH more efficient than calling
    /// pointFromPin and building up the iterator from points.
    ///
    /// The limit pin is inclusive.
    #[inline]
    pub fn page_iterator(self, direction: Direction, limit: Option<Pin>) -> PageIterator {
        if build_options::SLOW_RUNTIME_SAFETY {
            if let Some(l) = limit {
                // Check the order according to the iteration direction.
                match direction {
                    Direction::RightDown => assert!(self.eql(l) || self.before(l)),
                    Direction::LeftUp => assert!(self.eql(l) || l.before(self)),
                }
            }
        }

        PageIterator {
            row: Some(self),
            limit: match limit {
                Some(p) => PageIteratorLimit::Row(p),
                None => PageIteratorLimit::None,
            },
            direction,
        }
    }

    #[inline]
    pub fn row_iterator(self, direction: Direction, limit: Option<Pin>) -> RowIterator {
        let mut page_it = self.page_iterator(direction, limit);
        let Some(chunk) = page_it.next() else {
            return RowIterator {
                page_it,
                chunk: None,
                offset: 0,
            };
        };
        RowIterator {
            page_it,
            chunk: Some(chunk),
            offset: match direction {
                Direction::RightDown => chunk.start,
                Direction::LeftUp => chunk.end - 1,
            },
        }
    }

    #[inline]
    pub fn cell_iterator(self, direction: Direction, limit: Option<Pin>) -> CellIterator {
        let mut row_it = self.row_iterator(direction, limit);
        let Some(mut cell) = row_it.next() else {
            return CellIterator { row_it, cell: None };
        };
        cell.x = self.x;
        CellIterator {
            row_it,
            cell: Some(cell),
        }
    }

    #[inline]
    pub fn prompt_iterator(self, direction: Direction, limit: Option<Pin>) -> PromptIterator {
        PromptIterator {
            current: Some(self),
            limit,
            direction,
        }
    }

    /// Returns true if this pin is between the top and bottom, inclusive.
    //
    // Note: this is primarily unit tested as part of the Kitty
    // graphics deletion code.
    pub fn is_between(&self, top: Pin, bottom: Pin) -> bool {
        // SAFETY: valid pins (type contract).
        unsafe {
            if build_options::SLOW_RUNTIME_SAFETY {
                if top.node == bottom.node {
                    // If top is bottom, must be ordered.
                    assert!(top.y <= bottom.y);
                    if top.y == bottom.y {
                        assert!(top.x <= bottom.x);
                    }
                } else {
                    // If top is not bottom, top must be before bottom.
                    let mut node_ = nd(top.node).next;
                    let mut found = false;
                    while let Some(node) = node_ {
                        if node == bottom.node {
                            found = true;
                            break;
                        }
                        node_ = nd(node).next;
                    }
                    assert!(found);
                }
            }

            if self.node == top.node {
                // If our pin is the top page and our y is less than the top y
                // then we can't possibly be between the top and bottom.
                if self.y < top.y {
                    return false;
                }

                // If our y is after the top y but we're on the same page
                // then we're between the top and bottom if our y is less
                // than or equal to the bottom y if its the same page. If the
                // bottom is another page then it means that the range is
                // at least the full top page and since we're the same page
                // we're in the range.
                if self.y > top.y {
                    return if self.node == bottom.node { self.y <= bottom.y } else { true };
                }

                // Otherwise our y is the same as the top y, so we need to
                // check the x coordinate.
                assert!(self.y == top.y);
                if self.x < top.x {
                    return false;
                }
            }
            if self.node == bottom.node {
                // Our page is the bottom page so we're between the top and
                // bottom if our y is less than the bottom y.
                if self.y > bottom.y {
                    return false;
                }
                if self.y < bottom.y {
                    return true;
                }

                // If our y is the same, then we're between if we're before
                // or equal to the bottom x.
                assert!(self.y == bottom.y);
                return self.x <= bottom.x;
            }

            // Our page isn't the top or bottom so we need to check if
            // our page is somewhere between the top and bottom.

            // Since our loop starts at top.page.next we need to check that
            // top != bottom because if they're the same then we can't possibly
            // be between them.
            if top.node == bottom.node {
                return false;
            }
            let mut node_ = nd(top.node).next;
            while let Some(node) = node_ {
                if node == bottom.node {
                    break;
                }
                if node == self.node {
                    return true;
                }
                node_ = nd(node).next;
            }

            false
        }
    }

    /// Returns true if self is before other. This is very expensive since
    /// it requires traversing the linked list of pages. This should not
    /// be called in performance critical paths.
    pub fn before(&self, other: Pin) -> bool {
        if self.node == other.node {
            if self.y < other.y {
                return true;
            }
            if self.y > other.y {
                return false;
            }
            return self.x < other.x;
        }

        // SAFETY: valid pins (type contract).
        let mut node_ = unsafe { nd(self.node).next };
        while let Some(node) = node_ {
            if node == other.node {
                return true;
            }
            node_ = unsafe { nd(node).next };
        }

        false
    }

    #[inline]
    pub fn eql(&self, other: Pin) -> bool {
        self.node == other.node && self.y == other.y && self.x == other.x
    }

    /// Move the pin left n columns. n must fit within the size.
    #[inline]
    pub fn left(&self, n: usize) -> Pin {
        assert!(n <= self.x as usize);
        let mut result = *self;
        result.x -= CellCountInt::try_from(n).unwrap_or(result.x);
        result
    }

    /// Move the pin right n columns. n must fit within the size.
    #[inline]
    pub fn right(&self, n: usize) -> Pin {
        // SAFETY: valid pin (type contract).
        assert!((self.x as usize + n) < unsafe { nd(self.node).cols() } as usize);
        let mut result = *self;
        result.x = result
            .x
            .saturating_add(CellCountInt::try_from(n).unwrap_or(CellCountInt::MAX));
        result
    }

    /// Move the pin left n columns, stopping at the start of the row.
    #[inline]
    pub fn left_clamp(&self, n: CellCountInt) -> Pin {
        let mut result = *self;
        result.x = result.x.saturating_sub(n);
        result
    }

    /// Move the pin right n columns, stopping at the end of the row.
    #[inline]
    pub fn right_clamp(&self, n: CellCountInt) -> Pin {
        let mut result = *self;
        // SAFETY: valid pin (type contract).
        result.x = self.x.saturating_add(n).min(unsafe { nd(self.node).cols() } - 1);
        result
    }

    /// Move the pin left n cells, wrapping to the previous row as needed.
    ///
    /// If the offset goes beyond the top of the screen, returns null.
    ///
    /// TODO: Unit tests.
    pub fn left_wrap(&self, n: usize) -> Option<Pin> {
        let mut result = *self;
        let mut remaining = n;
        while remaining > result.x as usize {
            remaining -= result.x as usize + 1;
            result = result.up(1)?;
            // Crossing a row boundary lands on that destination row's final
            // cell, whose width may differ from ours during reflow.
            // SAFETY: valid pin.
            result.x = unsafe { nd(result.node).cols() } - 1;
        }

        result.x -= remaining as CellCountInt;
        Some(result)
    }

    /// Move the pin right n cells, wrapping to the next row as needed.
    ///
    /// If the offset goes beyond the bottom of the screen, returns null.
    ///
    /// TODO: Unit tests.
    pub fn right_wrap(&self, n: usize) -> Option<Pin> {
        let mut result = *self;
        let mut remaining = n;
        loop {
            // SAFETY: valid pin.
            let row_remaining = unsafe { nd(result.node).cols() } - result.x - 1;
            if remaining <= row_remaining as usize {
                result.x += remaining as CellCountInt;
                return Some(result);
            }

            remaining -= row_remaining as usize + 1;
            result = result.down(1)?;
            result.x = 0;
        }
    }

    /// Move the pin down a certain number of rows, or return null if
    /// the pin goes beyond the end of the screen.
    #[inline]
    pub fn down(&self, n: usize) -> Option<Pin> {
        match self.down_overflow(n) {
            PinOverflow::Offset(v) => Some(v),
            PinOverflow::Overflow { .. } => None,
        }
    }

    /// Move the pin up a certain number of rows, or return null if
    /// the pin goes beyond the start of the screen.
    #[inline]
    pub fn up(&self, n: usize) -> Option<Pin> {
        match self.up_overflow(n) {
            PinOverflow::Offset(v) => Some(v),
            PinOverflow::Overflow { .. } => None,
        }
    }

    /// Move the offset down n rows. If the offset goes beyond the
    /// end of the screen, return the overflow amount.
    pub fn down_overflow(&self, n: usize) -> PinOverflow {
        // SAFETY: valid pin (type contract); list links are live.
        unsafe {
            // Index fits within this page
            let rows = (nd(self.node).rows() - (self.y + 1)) as usize;
            if n <= rows {
                return PinOverflow::Offset(Pin {
                    node: self.node,
                    y: CellCountInt::try_from(self.y as usize + n).unwrap_or(CellCountInt::MAX),
                    x: self.x,
                    garbage: false,
                });
            }

            // Need to traverse page links to find the page
            let mut node: NodePtr = self.node;
            let mut n_left: usize = n - rows;
            loop {
                node = match nd(node).next {
                    Some(next) => next,
                    None => {
                        return PinOverflow::Overflow {
                            end: Pin {
                                node,
                                y: nd(node).rows() - 1,
                                x: self.x.min(nd(node).cols() - 1),
                                garbage: false,
                            },
                            remaining: n_left,
                        }
                    }
                };
                if n_left <= nd(node).rows() as usize {
                    return PinOverflow::Offset(Pin {
                        node,
                        y: CellCountInt::try_from(n_left - 1).unwrap_or(CellCountInt::MAX),
                        x: self.x.min(nd(node).cols() - 1),
                        garbage: false,
                    });
                }
                n_left -= nd(node).rows() as usize;
            }
        }
    }

    /// Move the offset up n rows. If the offset goes beyond the
    /// start of the screen, return the overflow amount.
    pub fn up_overflow(&self, n: usize) -> PinOverflow {
        // SAFETY: valid pin (type contract); list links are live.
        unsafe {
            // Index fits within this page
            if n <= self.y as usize {
                return PinOverflow::Offset(Pin {
                    node: self.node,
                    y: CellCountInt::try_from(self.y as usize - n).unwrap_or(CellCountInt::MAX),
                    x: self.x,
                    garbage: false,
                });
            }

            // Need to traverse page links to find the page
            let mut node: NodePtr = self.node;
            let mut n_left: usize = n - self.y as usize;
            loop {
                node = match nd(node).prev {
                    Some(prev) => prev,
                    None => {
                        return PinOverflow::Overflow {
                            end: Pin {
                                node,
                                y: 0,
                                x: self.x.min(nd(node).cols() - 1),
                                garbage: false,
                            },
                            remaining: n_left,
                        }
                    }
                };
                if n_left <= nd(node).rows() as usize {
                    return PinOverflow::Offset(Pin {
                        node,
                        y: CellCountInt::try_from(nd(node).rows() as usize - n_left).unwrap_or(CellCountInt::MAX),
                        x: self.x.min(nd(node).cols() - 1),
                        garbage: false,
                    });
                }
                n_left -= nd(node).rows() as usize;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Build up a PageList manually from a set of Pages.
///
/// This data structure is transactional: dropping it releases every page
/// until `finish` is called. This keeps the ownership clear: a complete
/// PageList either owns all its pages or doesn't.
///
/// This was specifically built to help facilitate snapshot decoding
/// which transfers pages directly, but could be generally useful
/// for other purposes as well.
pub struct Builder {
    /// The pool (`None` once ownership moved into a finished PageList).
    pool: Option<MemoryPool>,
    pub pages: List,
    pub page_serial: u64,
    pub page_size: usize,
    pub options: Options,
    pub finished: bool,
}

// SAFETY: the builder exclusively owns its nodes.
unsafe impl Send for Builder {}

/// Errors from `Builder::finish`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishError {
    OutOfMemory,
    InvalidDimensions,
    InvalidPageDimensions,
    NoPages,
    InsufficientRows,
}

impl From<AllocError> for FinishError {
    fn from(_: AllocError) -> Self {
        FinishError::OutOfMemory
    }
}

impl fmt::Display for FinishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for FinishError {}

impl Builder {
    /// Initialize an empty builder. The options are the final state
    /// of the PageList and some validation is done on the finish call
    /// to ensure you built up a proper PageList according to those options.
    pub fn init(options: Options) -> Result<Builder, AllocError> {
        Self::init_alloc(Allocator::GLOBAL, options)
    }

    /// `init` with an explicit general-purpose allocator.
    pub fn init_alloc(alloc: Allocator, options: Options) -> Result<Builder, AllocError> {
        Ok(Builder {
            pool: Some(MemoryPool::init(alloc, page_allocator(alloc), PAGE_PREHEAT)?),
            pages: List::new(),
            page_serial: 0,
            page_size: 0,
            options,
            finished: false,
        })
    }

    /// Release all pages when restoration does not finish (Zig: `deinit`).
    ///
    /// This is safe to call after `finish` succeeds; Drop calls it.
    pub fn deinit(&mut self) {
        if self.finished {
            return;
        }
        let Some(pool) = self.pool.as_mut() else { return };

        // Free all our in-progress pages
        // SAFETY: the builder's nodes are live.
        while let Some(node) = unsafe { self.pages.pop_first() } {
            destroy_node_ext(pool, node, Some(&mut self.page_size));
        }

        // Free memory pool
        self.pool = None;
    }

    /// Allocate a new page into the PageList with the given capacity.
    ///
    /// The caller can then take this page and populate it. When `finish`
    /// is called, ownership is transferred to the resulting PageList.
    /// Until then, this Builder owns the page.
    pub fn allocate_page(&mut self, capacity: Capacity) -> Result<&mut Page, AllocError> {
        let pool = self.pool.as_mut().unwrap();
        let node = create_page_ext(pool, CreatePage::cap(capacity), &mut self.page_serial, Some(&mut self.page_size))?;
        // SAFETY: fresh live node.
        unsafe {
            self.pages.append(node);
            Ok(nd(node).page_assume_resident())
        }
    }

    /// Validate the decoded pages and transfer them into a live PageList.
    /// After this succeeds, `deinit` is a no-op because all resources have
    /// transferred to the PageList.
    pub fn finish(&mut self) -> Result<PageList, FinishError> {
        // These are basic validations but they're cheap to do and
        // we want to be careful we don't let corruption from untrusted
        // sources into our PageList which asserts this.
        if self.options.cols == 0 || self.options.rows == 0 {
            return Err(FinishError::InvalidDimensions);
        }
        if self.pages.first.is_none() {
            return Err(FinishError::NoPages);
        }

        // Manually count our total rows at this point which we'll
        // need for our PageList cache as well as a safety check.
        // SAFETY: the builder's nodes are live.
        let total_rows: usize = unsafe {
            let mut total_rows: usize = 0;
            let mut node = self.pages.first;
            while let Some(current) = node {
                let n = nd(current);
                if n.cols() == 0 || n.rows() == 0 {
                    return Err(FinishError::InvalidPageDimensions);
                }
                total_rows += n.rows() as usize;
                node = n.next;
            }
            if total_rows < self.options.rows as usize {
                return Err(FinishError::InsufficientRows);
            }
            total_rows
        };

        // Get our active pin
        // SAFETY: the builder's nodes are live.
        let active_top: Pin = unsafe {
            let mut rem = self.options.rows;
            let mut node = self.pages.last;
            loop {
                let current = node.unwrap();
                let n = nd(current);
                if rem <= n.rows() {
                    break Pin {
                        node: current,
                        y: n.rows() - rem,
                        ..Pin::new(current)
                    };
                }
                rem -= n.rows();
                node = n.prev;
            }
        };

        let pool = self.pool.as_mut().unwrap();

        // Set our viewport up to the active
        let viewport_pin = pool.pins.create().map_err(pool_err)?;
        // SAFETY: fresh pin pool item.
        unsafe { viewport_pin.as_ptr().write(active_top) };

        // Setup our one viewport tracked pin
        let tracked_pins = match init_tracked_pins(pool.alloc, viewport_pin) {
            Ok(v) => v,
            Err(e) => {
                pool.pins.destroy(viewport_pin);
                return Err(e.into());
            }
        };

        // Initialize limits
        let mut limits = Limits::init(self.options.cols, self.options.rows);
        limits.set(LimitKey::Bytes, self.options.max_size);
        limits.set(LimitKey::Lines, self.options.max_lines);

        let result = PageList {
            cols: self.options.cols,
            rows: self.options.rows,
            pool: self.pool.take().unwrap(),
            pages: self.pages,
            page_serial: self.page_serial,
            page_serial_epoch: 0,
            page_size: self.page_size,
            page_compression: IncrementalCompressionState::default(),
            recycle_node: None,
            limits,
            total_rows,
            tracked_pins,
            viewport: Viewport::Active,
            viewport_pin,
            viewport_pin_row_offset: None,
            pause_integrity_checks: 0,
        };
        result.assert_integrity();
        self.pages = List::new();
        self.finished = true;
        Ok(result)
    }

    /// The builder's memory pool (until `finish`).
    pub fn pool(&mut self) -> Option<&mut MemoryPool> {
        self.pool.as_mut()
    }
}

impl Drop for Builder {
    fn drop(&mut self) {
        self.deinit();
    }
}

// ---------------------------------------------------------------------------
// Cell
// ---------------------------------------------------------------------------

/// A cell located through `getCell`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub node: NodePtr,
    pub row: *mut Row,
    pub cell: *mut pagepkg::Cell,
    pub row_idx: CellCountInt,
    pub col_idx: CellCountInt,
}

impl Cell {
    /// Returns true if this cell is marked as dirty.
    ///
    /// This is not very performant this is primarily used for assertions
    /// and testing.
    pub fn is_dirty(&self) -> bool {
        // SAFETY: the cell's node/row are live (getCell contract).
        unsafe { nd(self.node).page().dirty || (*self.row).dirty() }
    }

    /// Get the cell style.
    ///
    /// Not meant for non-test usage since this is inefficient.
    pub fn style(&self) -> stylepkg::Style {
        // SAFETY: the cell's node/cell are live (getCell contract).
        unsafe {
            if (*self.cell).style_id() == stylepkg::DEFAULT_ID {
                return stylepkg::Style::default();
            }
            let page = nd(self.node).page();
            *page.styles.get(page.memory, (*self.cell).style_id())
        }
    }

    /// Gets the screen point for the given cell.
    ///
    /// This is REALLY expensive/slow so it isn't pub. This was built
    /// for debugging and tests. If you have a need for this outside of
    /// this file then consider a different approach and ask yourself very
    /// carefully if you really need this.
    pub fn screen_point(&self) -> point::Point {
        let mut y: u32 = self.row_idx as u32;
        let mut node_ = self.node;
        // SAFETY: live list nodes.
        while let Some(node) = unsafe { nd(node_).prev } {
            y += unsafe { nd(node).rows() } as u32;
            node_ = node;
        }

        point::Point::Screen(point::Coordinate { x: self.col_idx, y })
    }
}

// ---------------------------------------------------------------------------
// Test support
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod testing {
    //! Test helpers shared with sibling modules (compress, highlight).

    use super::*;
    use std::ops::{Deref, DerefMut};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Stand-in for Zig's `std.testing.FailingAllocator`: fails the
    /// allocation with index `fail_index` and every one after it.
    pub struct FailingAllocator {
        pub fail_index: AtomicUsize,
        pub alloc_index: AtomicUsize,
        pub allocations: AtomicUsize,
        pub deallocations: AtomicUsize,
        pub has_induced_failure: AtomicBool,
    }

    impl FailingAllocator {
        /// Create a leaked (static) failing allocator.
        pub fn leak(fail_index: usize) -> &'static FailingAllocator {
            Box::leak(Box::new(FailingAllocator {
                fail_index: AtomicUsize::new(fail_index),
                alloc_index: AtomicUsize::new(0),
                allocations: AtomicUsize::new(0),
                deallocations: AtomicUsize::new(0),
                has_induced_failure: AtomicBool::new(false),
            }))
        }

        pub fn allocator(&'static self) -> Allocator {
            Allocator::new(self)
        }

        pub fn has_induced_failure(&self) -> bool {
            self.has_induced_failure.load(Ordering::Relaxed)
        }

        pub fn alloc_index(&self) -> usize {
            self.alloc_index.load(Ordering::Relaxed)
        }

        pub fn set_fail_index(&self, v: usize) {
            self.fail_index.store(v, Ordering::Relaxed);
        }

        pub fn allocations(&self) -> usize {
            self.allocations.load(Ordering::Relaxed)
        }

        pub fn deallocations(&self) -> usize {
            self.deallocations.load(Ordering::Relaxed)
        }
    }

    // SAFETY: forwards to the system allocator.
    unsafe impl GlobalAlloc for FailingAllocator {
        unsafe fn alloc(&self, layout: AllocLayout) -> *mut u8 {
            let index = self.alloc_index.fetch_add(1, Ordering::Relaxed);
            if index >= self.fail_index.load(Ordering::Relaxed) {
                self.has_induced_failure.store(true, Ordering::Relaxed);
                return core::ptr::null_mut();
            }
            self.allocations.fetch_add(1, Ordering::Relaxed);
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: AllocLayout) {
            self.deallocations.fetch_add(1, Ordering::Relaxed);
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    /// A page that is deinit'ed on drop (Zig: `defer page.deinit()`).
    pub struct TestPage(pub Page);

    impl Deref for TestPage {
        type Target = Page;
        fn deref(&self) -> &Page {
            &self.0
        }
    }

    impl DerefMut for TestPage {
        fn deref_mut(&mut self) -> &mut Page {
            &mut self.0
        }
    }

    impl Drop for TestPage {
        fn drop(&mut self) {
            self.0.deinit();
        }
    }

    /// A page-aligned heap buffer (Zig: `alignedAlloc(u8, page_size_min, n)`).
    pub struct AlignedBuf {
        ptr: NonNull<u8>,
        len: usize,
    }

    impl AlignedBuf {
        pub fn new(len: usize) -> AlignedBuf {
            AlignedBuf {
                ptr: Allocator::GLOBAL.alloc_bytes(len, PAGE_SIZE_MIN).unwrap(),
                len,
            }
        }

        pub fn memory(&self) -> PageMemory {
            // SAFETY: live allocation of `len` bytes.
            unsafe { PageMemory::from_raw_parts(self.ptr.as_ptr(), self.len) }
        }
    }

    impl Drop for AlignedBuf {
        fn drop(&mut self) {
            // SAFETY: allocated in `new`.
            unsafe { Allocator::GLOBAL.free_bytes(self.ptr, self.len, PAGE_SIZE_MIN) };
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::bool_assert_comparison)]
    use super::testing::*;
    use super::*;
    use crate::terminal::compress::lz4::test_rng::Prng;
    #[allow(unused_imports)]
    use crate::terminal::page::{Cell as PCell, ContentTag, SemanticContent, SemanticPrompt, Wide};
    use crate::terminal::point::{Coordinate, Point, Tag};

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    fn init(cols: CellCountInt, rows: CellCountInt) -> PageList {
        PageList::init(Options {
            cols,
            rows,
            ..Default::default()
        })
        .unwrap()
    }

    fn init_opts(opts: Options) -> PageList {
        PageList::init(opts).unwrap()
    }

    /// Dereference a node (test-only; unbounded lifetime).
    fn n<'a>(p: NodePtr) -> &'a mut Node {
        unsafe { nd(p) }
    }

    /// The (restored) page of a node.
    fn pg<'a>(p: NodePtr) -> &'a mut Page {
        unsafe { nd(p).page() }
    }

    /// Dereference a tracked pin.
    fn tp<'a>(p: PinPtr) -> &'a mut Pin {
        unsafe { pinr(p) }
    }

    fn rac<'a>(page: &Page, x: usize, y: usize) -> (&'a mut Row, &'a mut PCell) {
        let r = page.get_row_and_cell(x, y);
        unsafe { (&mut *r.row, &mut *r.cell) }
    }

    fn cell_at<'a>(page: &Page, x: usize, y: usize) -> &'a mut PCell {
        rac(page, x, y).1
    }

    #[allow(dead_code)]
    fn row_at<'a>(page: &Page, y: usize) -> &'a mut Row {
        unsafe { &mut *page.get_row(y) }
    }

    #[allow(dead_code)]
    fn cells_of<'a>(page: &Page, row: &Row) -> &'a mut [PCell] {
        unsafe { &mut *page.get_cells(row) }
    }

    #[allow(dead_code)]
    fn pin_rac<'a>(p: Pin) -> (&'a mut Row, &'a mut PCell) {
        let r = p.row_and_cell();
        unsafe { (&mut *r.row, &mut *r.cell) }
    }

    #[allow(dead_code)]
    fn pin_cells<'a>(p: Pin, subset: CellSubset) -> &'a mut [PCell] {
        unsafe { &mut *p.cells(subset) }
    }

    fn mem<'a>(page: &Page) -> &'a mut [u8] {
        unsafe { page.memory.as_mut_slice() }
    }

    fn cp(c: char) -> PCell {
        PCell::init(c as u32)
    }

    #[allow(dead_code)]
    fn cpu(c: u32) -> PCell {
        PCell::init(c)
    }

    #[allow(dead_code)]
    fn wide_cell(c: char, wide: Wide) -> PCell {
        let mut cell = PCell::init(c as u32);
        cell.set_wide(wide);
        cell
    }

    fn coord(x: CellCountInt, y: u32) -> Coordinate {
        Coordinate { x, y }
    }

    fn pt_active(x: CellCountInt, y: u32) -> Point {
        Point::Active(coord(x, y))
    }

    fn pt_screen(x: CellCountInt, y: u32) -> Point {
        Point::Screen(coord(x, y))
    }

    #[allow(dead_code)]
    fn pt_viewport(x: CellCountInt, y: u32) -> Point {
        Point::Viewport(coord(x, y))
    }

    fn pt_history(x: CellCountInt, y: u32) -> Point {
        Point::History(coord(x, y))
    }

    fn pin_at(node: NodePtr, x: CellCountInt, y: CellCountInt) -> Pin {
        Pin { x, y, ..Pin::new(node) }
    }

    fn first(s: &PageList) -> NodePtr {
        s.pages.first.unwrap()
    }

    fn last(s: &PageList) -> NodePtr {
        s.pages.last.unwrap()
    }

    fn next(p: NodePtr) -> NodePtr {
        n(p).next.unwrap()
    }

    #[allow(dead_code)]
    fn prev(p: NodePtr) -> NodePtr {
        n(p).prev.unwrap()
    }

    fn encoded<'a>(p: NodePtr) -> &'a mut [u8] {
        match &n(p).data {
            NodeData::Compressed(c) => unsafe { &mut *c.encoded },
            NodeData::Resident(_) => panic!("node is resident"),
        }
    }

    fn clone_opts<'a>(top: Point, bot: Option<Point>) -> Clone<'a> {
        Clone {
            top,
            bot,
            tracked_pins: None,
        }
    }

    fn vp(s: &PageList) -> Pin {
        *tp(s.viewport_pin)
    }

    fn append_node(s: &mut PageList, node: NodePtr) {
        unsafe { s.pages.append(node) };
    }

    fn random_bytes(seed: u64, buf: &mut [u8]) {
        Prng::init(seed).bytes(buf);
    }

    fn mixed_width_pin_list_for_test() -> PageList {
        let mut result = init(2, 1);

        // This deliberately constructs a layout that normal PageList operations
        // do not expose yet. Keep integrity checks paused through deinit so the
        // fixture can exercise mixed-width traversal in isolation.
        result.pause_integrity_checks(true);

        for cols in [4, 3] {
            let node = result.create_page(CreatePage::cap(Capacity::new(cols, 1))).unwrap();
            pg(node).size.rows = 1;
            append_node(&mut result, node);
            result.total_rows += 1;
        }

        // Desired geometry is wider than the first and last stored pages.
        result.cols = 4;
        result
    }

    impl PageList {
        /// Grow a test PageList until it contains at least `count` complete history
        /// pages. The production cold-page boundary is intentionally reused here so
        /// tests do not duplicate the row-to-page arithmetic.
        fn grow_cold_pages_for_test(&mut self, count: usize) {
            loop {
                let active_node = self.get_top_left(Tag::Active).node;
                let mut cold_count: usize = 0;
                let mut current = self.pages.first;
                while let Some(node) = current {
                    if node == active_node {
                        break;
                    }
                    cold_count += 1;
                    current = n(node).next;
                }

                if cold_count >= count {
                    return;
                }
                self.grow().unwrap();
            }
        }

        /// Fill the current tail page to capacity without allocating a successor.
        /// Capturing the tail before the loop makes this stop at the allocation
        /// boundary needed by bounded-pruning tests.
        fn fill_last_page_for_test(&mut self) {
            let last = self.pages.last.unwrap();
            while n(last).rows() < n(last).capacity().rows {
                self.grow().unwrap();
            }
        }

        /// Verify every live page belongs to the current validity epoch, has an
        /// allocated generation below the next serial, and validates through the same
        /// pointer-plus-generation lookup used by external references.
        fn expect_live_page_serials_valid_for_test(&self) {
            let mut node = self.pages.first;
            while let Some(live) = node {
                assert!(n(live).serial >= self.page_serial_epoch);
                assert!(n(live).serial < self.page_serial);
                assert!(self.node_is_valid(live, n(live).serial));
                node = n(live).next;
            }
        }
    }

    /// The screen point of the top-left viewport cell.
    fn vps(s: &PageList) -> Point {
        s.get_cell(pt_viewport(0, 0)).unwrap().screen_point()
    }

    fn sb(total: usize, offset: usize, len: usize) -> Scrollbar {
        Scrollbar { total, offset, len }
    }

    /// Run grow `rows * pages` times, pausing integrity checks on the
    /// current tail page while it fills (Zig tests do this inline).
    fn grow_paused(s: &mut PageList, count: usize) {
        let mut cur_page = last(s);
        pg(cur_page).pause_integrity_checks(true);
        for _ in 0..count {
            if let Some(new_page) = s.grow().unwrap() {
                pg(cur_page).pause_integrity_checks(false);
                cur_page = new_page;
                pg(cur_page).pause_integrity_checks(true);
            }
        }
        pg(cur_page).pause_integrity_checks(false);
    }

    /// A page usable as fake node data for metadata-only tests.
    fn fake_nodes(count: usize, rows: CellCountInt) -> (TestPage, Vec<Node>) {
        let page = TestPage(Page::init(Capacity::new(1, 1)).unwrap());
        let mut data = page.0;
        data.size = pagepkg::Size { cols: 1, rows };
        let mut nodes: Vec<Node> = (0..count)
            .map(|i| Node {
                prev: None,
                next: None,
                data: NodeData::Resident(data),
                serial: i as u64,
                owned: Owned::Heap,
            })
            .collect();
        let base = nodes.as_mut_ptr();
        for i in 0..count {
            unsafe {
                let node = &mut *base.add(i);
                node.prev = if i > 0 { NonNull::new(base.add(i - 1)) } else { None };
                node.next = if i + 1 < count { NonNull::new(base.add(i + 1)) } else { None };
            }
        }
        (page, nodes)
    }

    #[test]
    fn page_list_builder_transfers_mixed_width_pages() {
        // Build two populated pages whose widths differ from each other and from
        // the final active-area width. The first page also includes one row of
        // incidental history above the three-row active area.
        let result = {
            let mut builder = Builder::init(Options {
                cols: 4,
                rows: 3,
                max_size: None,
                max_lines: None,
            })
            .unwrap();

            let first = builder.allocate_page(Capacity::new(2, 2)).unwrap();
            first.size.rows = 2;
            *cell_at(first, 0, 0) = cp('A');

            let second = builder.allocate_page(Capacity::new(4, 2)).unwrap();
            second.size.rows = 2;
            *cell_at(second, 0, 0) = cp('B');

            builder.finish().unwrap()
        };

        // Successful finish transfers ownership and initializes the PageList's
        // desired geometry, viewport, and required tracked viewport pin.
        assert_eq!(4, result.cols);
        assert_eq!(3, result.rows);
        assert_eq!(2, result.total_pages());
        assert_eq!(1, result.count_tracked_pins());
        assert_eq!(Viewport::Active, result.viewport);

        // Complete pages and their contents are preserved in insertion order,
        // including widths which have not yet been reflowed.
        let screen_top = result.get_top_left(Tag::Screen);
        assert_eq!(2, n(screen_top.node).cols());
        assert_eq!('A' as u32, cell_at(pg(screen_top.node), 0, 0).codepoint());

        // The active area is calculated backward from the newest page, so it
        // begins at row one of the oldest page and leaves row zero as history.
        let active_top = result.get_top_left(Tag::Active);
        assert_eq!(screen_top.node, active_top.node);
        assert_eq!(1, active_top.y);
        assert_eq!(4, n(next(active_top.node)).cols());
        assert_eq!('B' as u32, cell_at(pg(next(active_top.node)), 0, 0).codepoint());

        result.assert_integrity();
    }

    #[test]
    fn page_list_builder_validates_the_finished_list() {
        // The desired PageList geometry must describe a non-empty screen.
        {
            let mut builder = Builder::init(Options {
                cols: 0,
                rows: 1,
                ..Default::default()
            })
            .unwrap();
            assert_eq!(FinishError::InvalidDimensions, builder.finish().unwrap_err());
        }

        // A PageList cannot be finished without any backing pages.
        {
            let mut builder = Builder::init(Options {
                cols: 1,
                rows: 1,
                ..Default::default()
            })
            .unwrap();
            assert_eq!(FinishError::NoPages, builder.finish().unwrap_err());
        }

        // Allocated capacity alone is insufficient: callers must populate a
        // nonzero logical page size before transferring ownership.
        {
            let mut builder = Builder::init(Options {
                cols: 1,
                rows: 1,
                ..Default::default()
            })
            .unwrap();
            let page = builder.allocate_page(Capacity::new(1, 1)).unwrap();
            page.size.rows = 0;
            assert_eq!(FinishError::InvalidPageDimensions, builder.finish().unwrap_err());
        }

        // The populated pages must contain enough rows to cover the active area.
        {
            let mut builder = Builder::init(Options {
                cols: 1,
                rows: 2,
                ..Default::default()
            })
            .unwrap();
            let page = builder.allocate_page(Capacity::new(1, 1)).unwrap();
            page.size.rows = 1;
            assert_eq!(FinishError::InsufficientRows, builder.finish().unwrap_err());
        }
    }

    #[test]
    fn page_list_builder_finish_is_transactional_on_allocation_failure() {
        // Construct a valid builder so finish reaches its fallible bookkeeping
        // allocations after all page and geometry validation succeeds.
        let failing = FailingAllocator::leak(usize::MAX);
        let mut builder = Builder::init_alloc(
            failing.allocator(),
            Options {
                cols: 1,
                rows: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let page = builder.allocate_page(Capacity::new(1, 1)).unwrap();
        page.size.rows = 1;

        // The pools are preheated, so the next general allocation is the tracked
        // viewport pin map created by finish.
        failing.set_fail_index(failing.alloc_index());
        assert_eq!(FinishError::OutOfMemory, builder.finish().unwrap_err());
        assert!(failing.has_induced_failure());

        // Failed finish leaves page ownership with the builder so its normal
        // deinit path can release the still-linked page.
        assert!(builder.pages.first.is_some());
        assert_eq!(builder.pages.first, builder.pages.last);
    }

    #[test]
    fn page_list_page_allocation_finalizes_pages_and_preserves_live_state() {
        // Build an existing list with history, a two-row active area, an external
        // active pin, and a pinned viewport whose absolute offset is cached.
        let mut result = {
            let mut builder = Builder::init(Options {
                cols: 4,
                rows: 2,
                max_size: None,
                max_lines: None,
            })
            .unwrap();

            let first = builder.allocate_page(Capacity::new(3, 2)).unwrap();
            first.size.rows = 2;
            *cell_at(first, 0, 0) = cp('C');

            let second = builder.allocate_page(Capacity::new(4, 2)).unwrap();
            second.size.rows = 2;
            *cell_at(second, 0, 0) = cp('D');

            builder.finish().unwrap()
        };

        let old_first = first(&result);
        let old_last = last(&result);
        let active_top = result.get_top_left(Tag::Active);
        let tracked_active = result.track_pin(active_top).unwrap();
        result.scroll(Scroll::Row(1));
        assert_eq!(Viewport::Pin, result.viewport);
        assert_eq!(1, result.scrollbar().offset);

        // Prepend differently sized historical pages newest-first. Each page is
        // populated while detached and joins the live list only on success.
        {
            let mut allocation = result.allocate_page(Capacity::new(4, 1)).unwrap();
            let page = allocation.page();
            page.size.rows = 1;
            *cell_at(page, 0, 0) = cp('B');
            allocation.finalize(Location::Prepend, FinalizeOptions::default()).unwrap();
        }
        {
            let mut allocation = result.allocate_page(Capacity::new(2, 2)).unwrap();
            let page = allocation.page();
            page.size.rows = 2;
            *cell_at(page, 0, 0) = cp('A');
            allocation.finalize(Location::Prepend, FinalizeOptions::default()).unwrap();
        }

        // Repeated prepends reconstruct oldest-to-newest order without replacing
        // any existing nodes or tracked pins.
        assert_eq!(4, result.total_pages());
        assert_eq!(7, result.total_rows);
        assert_eq!(old_last, last(&result));
        assert_eq!(old_first, next(next(first(&result))));
        assert_eq!('A' as u32, cell_at(pg(first(&result)), 0, 0).codepoint());
        assert_eq!('B' as u32, cell_at(pg(next(first(&result))), 0, 0).codepoint());
        assert!(active_top.eql(result.get_top_left(Tag::Active)));
        assert!(active_top.eql(*tp(tracked_active)));

        // The viewport remains pinned to the same content, while its cached row
        // offset and the scrollbar total include the three new historical rows.
        assert_eq!(Viewport::Pin, result.viewport);
        assert_eq!(old_first, vp(&result).node);
        assert_eq!(1, vp(&result).y);
        let scrollbar_state = result.scrollbar();
        assert_eq!(7, scrollbar_state.total);
        assert_eq!(4, scrollbar_state.offset);
        assert_eq!(2, scrollbar_state.len);

        result.assert_integrity();
    }

    #[test]
    fn page_list_page_allocation_compresses_prepended_pages_unless_visible() {
        #[derive(Clone, Copy, PartialEq)]
        enum Vp {
            Active,
            Pin,
            Top,
        }
        struct Case {
            /// How to position the viewport before prepending.
            viewport: Vp,
            /// Whether the prepended page should end up compressed.
            compressed: bool,
        }
        let cases = [
            // The viewport shows the active area, far below the new page.
            Case {
                viewport: Vp::Active,
                compressed: true,
            },
            // The viewport is pinned to existing history, which is still below
            // the new page.
            Case {
                viewport: Vp::Pin,
                compressed: true,
            },
            // The viewport follows the top of the scrollback, so the new page
            // becomes the first visible page.
            Case {
                viewport: Vp::Top,
                compressed: false,
            },
        ];

        for case in cases {
            let mut s = init(80, 24);
            s.grow_cold_pages_for_test(1);
            match case.viewport {
                Vp::Active => {}
                Vp::Pin => s.scroll(Scroll::Row(1)),
                Vp::Top => s.scroll(Scroll::Top),
            }
            assert_eq!(
                match case.viewport {
                    Vp::Active => Viewport::Active,
                    Vp::Pin => Viewport::Pin,
                    Vp::Top => Viewport::Top,
                },
                s.viewport
            );

            // A full-size page so compression is worthwhile.
            let capacity = n(first(&s)).capacity();
            let mut allocation = s.allocate_page(capacity).unwrap();
            let page = allocation.page();
            page.size.rows = capacity.rows;
            *cell_at(page, 0, 0) = cp('X');
            allocation
                .finalize(Location::Prepend, FinalizeOptions { compress: true })
                .unwrap();
            drop(allocation);

            let node = first(&s);
            assert_eq!(case.compressed, n(node).is_compressed());
            assert_eq!(if case.compressed { 1 } else { 0 }, s.memory_stats().compressed_pages);

            // Reading the page uncompresses it with its contents intact.
            assert_eq!('X' as u32, cell_at(pg(node), 0, 0).codepoint());
            s.assert_integrity();
        }
    }

    #[test]
    fn page_list_page_allocation_stays_detached_until_finalize() {
        let mut result = init_opts(Options {
            cols: 1,
            rows: 1,
            max_size: None,
            max_lines: None,
        });

        let initial_first = first(&result);
        let initial_last = last(&result);
        let initial_total_rows = result.total_rows;
        let initial_page_size = result.page_size;

        // Allocating and populating a detached page does not alter any live list
        // links or accounting. Deinit returns it to the same PageList pools.
        let mut detached = result.allocate_page(Capacity::new(1, 1)).unwrap();
        detached.page().size.rows = 1;
        assert_eq!(initial_first, first(&result));
        assert_eq!(initial_last, last(&result));
        assert_eq!(initial_total_rows, result.total_rows);
        assert_eq!(initial_page_size, result.page_size);
        result.assert_integrity();
        detached.deinit();
        result.assert_integrity();

        // Invalid populated dimensions leave ownership with the allocation so it
        // can still be released normally.
        let mut invalid = result.allocate_page(Capacity::new(1, 1)).unwrap();
        assert_eq!(
            FinalizeError::InvalidPageDimensions,
            invalid
                .finalize(Location::Prepend, FinalizeOptions::default())
                .unwrap_err()
        );

        assert_eq!(initial_first, first(&result));
        assert_eq!(initial_last, last(&result));
        assert_eq!(initial_total_rows, result.total_rows);
        assert_eq!(initial_page_size, result.page_size);
        result.assert_integrity();
        drop(invalid);
    }

    #[test]
    fn page_list_page_allocation_rejects_limits_before_modifying_the_destination() {
        let mut result = init_opts(Options {
            cols: 1,
            rows: 1,
            max_size: Some(0),
            max_lines: None,
        });

        // The effective minimum permits one complete page beyond the active page.
        // Fill that allowance so the following allocation exceeds the byte limit.
        {
            let mut allocation = result.allocate_page(Capacity::new(1, 1)).unwrap();
            allocation.page().size.rows = 1;
            allocation.finalize(Location::Prepend, FinalizeOptions::default()).unwrap();
        }

        let before_first = first(&result);
        let before_total_rows = result.total_rows;
        let before_page_size = result.page_size;

        let mut allocation = result.allocate_page(Capacity::new(1, 1)).unwrap();
        allocation.page().size.rows = 1;
        assert_eq!(
            FinalizeError::MaxSizeExceeded,
            allocation
                .finalize(Location::Prepend, FinalizeOptions::default())
                .unwrap_err()
        );

        assert_eq!(before_first, first(&result));
        assert_eq!(before_total_rows, result.total_rows);
        assert_eq!(before_page_size, result.page_size);
        result.assert_integrity();
        drop(allocation);
    }

    #[test]
    fn page_list_page_allocation_allocation_failure_leaves_list_unchanged() {
        let failing = FailingAllocator::leak(usize::MAX);
        let mut result = PageList::init_alloc(
            failing.allocator(),
            Options {
                cols: 1,
                rows: 1,
                max_size: None,
                max_lines: None,
            },
        )
        .unwrap();

        let initial_first = first(&result);
        let initial_total_rows = result.total_rows;
        let initial_page_size = result.page_size;

        // Existing pool capacity is deliberately an implementation detail. Allow
        // preheated slots to succeed until allocation reaches node-pool growth.
        failing.set_fail_index(failing.alloc_index());
        let mut allocations: Vec<PageAllocation> = Vec::new();

        let mut failed = false;
        for _ in 0..64 {
            match result.allocate_page(Capacity::new(1, 1)) {
                Ok(a) => allocations.push(a),
                Err(err) => {
                    assert_eq!(AllocError::OutOfMemory, err);
                    failed = true;
                    break;
                }
            }
        }
        assert!(failed);
        assert!(failing.has_induced_failure());

        // Detached allocations and failed pool growth never publish into the live
        // list; the deferred cleanup returns every successful allocation.
        assert_eq!(initial_first, first(&result));
        assert_eq!(initial_total_rows, result.total_rows);
        assert_eq!(initial_page_size, result.page_size);
        result.assert_integrity();
        drop(allocations);
    }

    #[test]
    fn page_list_pin_row_movement_clamps_across_mixed_width_pages() {
        let s = mixed_width_pin_list_for_test();

        let first = first(&s);
        let second = next(first);
        let third = next(second);

        assert!(pin_at(third, 2, 0).eql(pin_at(second, 3, 0).down(1).unwrap()));
        assert!(pin_at(first, 1, 0).eql(pin_at(second, 3, 0).up(1).unwrap()));

        match pin_at(second, 3, 0).down_overflow(10) {
            PinOverflow::Offset(_) => panic!(),
            PinOverflow::Overflow { end, .. } => assert!(pin_at(third, 2, 0).eql(end)),
        }
        match pin_at(second, 3, 0).up_overflow(10) {
            PinOverflow::Offset(_) => panic!(),
            PinOverflow::Overflow { end, .. } => assert!(pin_at(first, 1, 0).eql(end)),
        }
    }

    #[test]
    fn page_list_pin_wrapping_crosses_mixed_width_pages() {
        let s = mixed_width_pin_list_for_test();

        let first = first(&s);
        let second = next(first);
        let third = next(second);

        assert!(pin_at(third, 2, 0).eql(pin_at(first, 1, 0).right_wrap(7).unwrap()));
        assert!(pin_at(second, 0, 0).eql(pin_at(third, 2, 0).left_wrap(6).unwrap()));
        assert!(Pin::new(first).left_wrap(1).is_none());
        assert!(pin_at(third, 2, 0).right_wrap(1).is_none());
    }

    #[test]
    fn page_list_pin_rejects_columns_beyond_mixed_width_page_bounds() {
        let s = mixed_width_pin_list_for_test();

        assert!(s.pin(pt_screen(1, 0)).is_some());
        assert!(s.pin(pt_screen(2, 0)).is_none());
        assert!(s.pin(pt_screen(3, 1)).is_some());
        assert!(s.pin(pt_screen(3, 2)).is_none());
    }

    #[test]
    fn page_list_pin_right_wrap_exact_row_multiple() {
        let s = init(10, 3);

        let start = s.pin(pt_active(5, 0)).unwrap();
        let wrapped = start.right_wrap(14).unwrap();
        let _ = wrapped.row_and_cell();

        assert_eq!(Some(pt_active(9, 1)), s.point_from_pin(Tag::Active, wrapped));
    }

    #[test]
    fn page_list_pin_left_wrap_exact_row_multiple() {
        let s = init(10, 3);

        let start = s.pin(pt_active(5, 2)).unwrap();
        let wrapped = start.left_wrap(15).unwrap();
        let _ = wrapped.row_and_cell();

        assert_eq!(Some(pt_active(0, 1)), s.point_from_pin(Tag::Active, wrapped));
    }

    #[test]
    fn page_list_pin_right_wrap_maximum_distance() {
        let s = init(1, 3);

        let start = s.pin(pt_active(0, 0)).unwrap();
        assert_eq!(None, start.right_wrap(usize::MAX));
    }

    #[test]
    fn page_list_pin_left_wrap_maximum_distance() {
        let s = init(1, 3);

        let start = s.pin(pt_active(0, 2)).unwrap();
        assert_eq!(None, start.left_wrap(usize::MAX));
    }

    #[test]
    fn page_list_incremental_compression_skips_visible_history() {
        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(3);

        let initial_activity = s.page_compression.activity_serial;
        assert!(initial_activity > 0);

        s.scroll(Scroll::Top);
        let top_activity = s.page_compression.activity_serial;
        assert!(top_activity != initial_activity);
        assert_eq!(IncrementalCompressionResult::Complete, s.compress(CompressMode::Drain));

        let first = first(&s);
        let second = next(first);
        assert_eq!(first, s.get_top_left(Tag::Viewport).node);
        assert_eq!(first, s.get_bottom_right(Tag::Viewport).unwrap().node);
        assert!(!n(first).is_compressed());

        let mut eligible = CompressionIterator::init(&s);
        let mut compressed_pages: usize = 0;
        while let Some(node) = eligible.next() {
            compressed_pages += 1;
            assert!(n(node).is_compressed());
        }
        assert!(compressed_pages > 0);

        // Move the viewport to the start of the second page. Rendering it restores
        // that page, while the first page which just left view becomes eligible.
        s.scroll(Scroll::Row(n(first).rows() as usize));
        assert_eq!(second, s.get_top_left(Tag::Viewport).node);
        let _ = pg(second);
        assert!(!n(second).is_compressed());
        let _ = s.compress(CompressMode::Drain);
        assert!(n(first).is_compressed());
        assert!(!n(second).is_compressed());

        // Returning to the active area makes every complete historical page
        // eligible again, including the page which was just visible.
        s.scroll(Scroll::Active);
        let _ = s.compress(CompressMode::Drain);
        assert!(n(second).is_compressed());
        assert!(!s.page_compression.flags.did_compress);
        assert!(!s.page_compression.flags.verifying);
        assert_eq!(None, s.page_compression.last_serial);
        assert_eq!(0, s.page_compression.next_serial);
    }

    #[test]
    fn page_list_owns_incremental_compression_state() {
        let mut s = init(80, 24);

        let state = IncrementalCompressionState {
            flags: IncrementalCompressionFlags {
                did_compress: true,
                verifying: true,
            },
            activity_serial: 42,
            last_serial: Some(42),
            next_serial: 43,
        };
        let with_activity = |a: u64| IncrementalCompressionState {
            activity_serial: a,
            ..Default::default()
        };

        s.page_compression = state;
        s.scroll(Scroll::Top);
        assert_eq!(with_activity(43), s.page_compression);

        // Every scroll restarts traversal, even if clamping leaves the viewport in
        // the same place. Missing an eligible page is worse than a no-op pass.
        s.page_compression = state;
        s.scroll(Scroll::Top);
        assert_eq!(with_activity(43), s.page_compression);

        s.page_compression = state;
        s.scroll(Scroll::Active);
        assert_eq!(with_activity(43), s.page_compression);

        s.page_compression = state;
        s.resize(Resize {
            cols: Some(80),
            rows: Some(24),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(with_activity(43), s.page_compression);

        s.page_compression = state;
        s.reset();
        assert_eq!(with_activity(42), s.page_compression);

        s.page_compression = state;
        assert_eq!(IncrementalCompressionResult::Complete, s.compress(CompressMode::Full));
        assert_eq!(with_activity(42), s.page_compression);

        s.page_compression = state;
        let cloned = s.clone(clone_opts(pt_active(0, 0), None)).unwrap();
        assert_eq!(IncrementalCompressionState::default(), cloned.page_compression);
    }

    #[test]
    fn page_list_replacements_preserve_compression_continuation_and_mark_activity() {
        let mut s = init(80, 24);

        let state = IncrementalCompressionState {
            flags: IncrementalCompressionFlags {
                verifying: true,
                did_compress: false,
            },
            activity_serial: 42,
            last_serial: Some(7),
            next_serial: 8,
        };
        let expected = IncrementalCompressionState {
            activity_serial: 43,
            ..state
        };

        s.page_compression = state;
        let replacement = s.increase_capacity(first(&s), None).unwrap();
        assert_eq!(expected, s.page_compression);

        s.page_compression = state;
        s.compact(replacement).unwrap().unwrap();
        assert_eq!(expected, s.page_compression);
    }

    #[test]
    fn page_list_incremental_compression_bounds_inspected_pages() {
        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(INCREMENTAL_COMPRESSION_MAX_INSPECTED + 1);

        // Precompress every candidate so the incremental pass exercises its
        // metadata-only skip budget without stopping at a resident attempt.
        let _ = s.compress(CompressMode::Full);
        s.page_compression.reset();
        s.page_compression.mark_activity();
        assert_eq!(INCREMENTAL_COMPRESSION_MAX_INSPECTED + 1, s.memory_stats().compressed_pages);

        let mut expected_last = first(&s);
        for _ in 1..INCREMENTAL_COMPRESSION_MAX_INSPECTED {
            expected_last = next(expected_last);
        }

        let first_r = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, first_r);
        assert_eq!(n(expected_last).serial, s.page_compression.last_serial.unwrap());

        let second = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, second);
        assert!(s.page_compression.flags.verifying);
        assert!(s.page_compression.last_serial.is_none());

        // The verification pass is bounded independently, too.
        assert_eq!(IncrementalCompressionResult::Pending, s.compress(CompressMode::Incremental));
        assert_eq!(IncrementalCompressionResult::Complete, s.compress(CompressMode::Incremental));
    }

    #[test]
    fn page_list_incremental_compression_advances_after_failure() {
        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(2);

        let first = first(&s);
        let second = next(first);
        random_bytes(0x494E_4352_5041_5353, mem(pg(first)));

        let failed = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, failed);
        assert!(!n(first).is_compressed());

        // The unsuccessful first page does not stall the pass. The next step
        // continues at the following serial and compresses that page.
        let continued = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, continued);
        assert!(n(second).is_compressed());
    }

    #[test]
    fn page_list_incremental_compression_advances_after_allocation_failure() {
        let failing = FailingAllocator::leak(usize::MAX);
        let mut s = PageList::init_alloc(
            failing.allocator(),
            Options {
                cols: 80,
                rows: 24,
                ..Default::default()
            },
        )
        .unwrap();
        s.grow_cold_pages_for_test(2);
        let first = first(&s);
        let second = next(first);

        // Pool preheating supplies compression scratch. Failing the allocator's
        // next request therefore rejects the exact encoded allocation while the
        // source page and pass remain valid.
        failing.set_fail_index(failing.alloc_index());
        let failed = s.compress(CompressMode::Incremental);
        assert!(failing.has_induced_failure());
        assert_eq!(IncrementalCompressionResult::Pending, failed);
        assert!(!n(first).is_compressed());

        // Allow allocations again. The pass must continue with the following page
        // rather than retrying the failed candidate.
        failing.set_fail_index(usize::MAX);
        let continued = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, continued);
        assert!(n(second).is_compressed());
    }

    #[test]
    fn page_list_incremental_compression_advances_after_decommit_failure() {
        let tw = &COMPRESS_PAGE_TW;

        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(2);

        tw.error_always(CompressPageFailPoint::Decommit, DecommitError::DecommitFailed);
        let failed = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, failed);
        assert!(!n(first(&s)).is_compressed());
        tw.end(tripwire::ResetMode::Reset).unwrap();

        // The failed candidate remains resident and the pass continues at the
        // next serial once reclamation is available again.
        let continued = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, continued);
        assert!(n(next(first(&s))).is_compressed());
        tw.end(tripwire::ResetMode::Reset).unwrap_or(());
    }

    #[test]
    fn page_list_incremental_compression_restarts_after_replacement() {
        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(1);

        let initial = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, initial);
        assert!(n(first(&s)).is_compressed());

        let old = first(&s);
        let old_serial = n(old).serial;
        let mut replacement = old;
        while pg(replacement).memory.len() <= std_size() {
            replacement = s
                .increase_capacity(replacement, Some(IncreaseCapacity::GraphemeBytes))
                .unwrap();
        }
        assert!(n(replacement).serial != old_serial);
        assert!(pg(replacement).memory.len() > std_size());
        assert!(!n(replacement).is_compressed());

        // The exact continuation serial disappeared with the old node. The pass
        // restarts at the first page and considers the oversized replacement.
        let restarted = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, restarted);
        assert!(n(replacement).is_compressed());
    }

    #[test]
    fn page_list_incremental_compression_restarts_after_reset() {
        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(1);

        let initial = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, initial);
        assert!(n(first(&s)).is_compressed());

        // Reset replaces every page and clears the PageList-owned traversal.
        s.reset();
        s.grow_cold_pages_for_test(1);
        let restarted = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, restarted);
        assert!(n(first(&s)).is_compressed());
    }

    #[test]
    fn page_list_incremental_compression_restarts_after_active_boundary_resize() {
        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(1);

        let initial = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, initial);
        assert!(n(first(&s)).is_compressed());

        let first = first(&s);
        let all_rows = s.total_rows as CellCountInt;
        s.resize(Resize {
            rows: Some(all_rows),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(first, s.get_top_left(Tag::Active).node);

        // Restore the page while it is active. Resize reset the traversal, and
        // active contents remain ineligible.
        let _ = pg(first);
        let active_r = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, active_r);
        assert_eq!(IncrementalCompressionResult::Complete, s.compress(CompressMode::Incremental));

        // Shrinking the active area makes the page fully historical again. The
        // resize reset the PageList-owned cursor, so the next step can reclaim it.
        s.resize(Resize {
            rows: Some(24),
            ..Default::default()
        })
        .unwrap();
        s.grow_cold_pages_for_test(1);
        assert_eq!(IncrementalCompressionResult::Pending, s.compress(CompressMode::Incremental));
        assert!(n(first).is_compressed());
    }

    #[test]
    fn page_list_incremental_compression_restarts_after_prune_reuse() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(2 * PagePool::item_size()),
            ..Default::default()
        });
        s.grow_cold_pages_for_test(1);

        let initial = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, initial);
        assert!(n(first(&s)).is_compressed());

        let reused = first(&s);
        let old_serial = n(reused).serial;
        while n(last(&s)).rows() < n(last(&s)).capacity().rows {
            s.grow().unwrap();
        }
        assert_eq!(Some(reused), s.grow().unwrap());
        assert!(n(reused).serial != old_serial);

        // Make the remaining old page fully historical. The continuation serial
        // disappeared when its node was recycled, so the pass safely restarts.
        s.grow_cold_pages_for_test(1);
        let _ = s.compress(CompressMode::Incremental);
        assert_eq!(1, s.memory_stats().compressed_pages);
    }

    #[test]
    fn page_list_bounded_pruning_after_partial_erase_preserves_live_serials() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(2 * PagePool::item_size()),
            ..Default::default()
        });

        while s.total_pages() < 2 {
            s.grow().unwrap();
        }
        let first = first(&s);
        let old_serial = n(first).serial;
        let old_rows = n(first).rows();

        s.erase_history(Some(pt_history(0, 0)));
        assert_eq!(first, s.pages.first.unwrap());
        assert_eq!(old_rows - 1, n(first).rows());
        assert!(!s.node_is_valid(first, old_serial));

        s.fill_last_page_for_test();
        s.grow().unwrap();
        s.expect_live_page_serials_valid_for_test();
    }

    #[test]
    fn page_list_partial_erase_restarts_compression_before_continuation() {
        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(INCREMENTAL_COMPRESSION_MAX_INSPECTED + 1);
        let _ = s.compress(CompressMode::Full);

        let first = first(&s);
        assert!(n(first).is_compressed());

        let mut marker = first;
        for _ in 1..INCREMENTAL_COMPRESSION_MAX_INSPECTED {
            marker = next(marker);
        }
        s.page_compression = IncrementalCompressionState {
            flags: IncrementalCompressionFlags {
                verifying: true,
                did_compress: false,
            },
            last_serial: Some(n(marker).serial),
            next_serial: s.page_serial,
            activity_serial: 0,
        };

        let activity = s.page_compression.activity_serial;
        s.erase_history(Some(pt_history(0, 0)));
        assert!(!n(first).is_compressed());
        assert!(activity != s.page_compression.activity_serial);

        // The changed generation is before the saved marker, so continuation must
        // restart and recompress it instead of reporting verification complete.
        assert_eq!(IncrementalCompressionResult::Pending, s.compress(CompressMode::Incremental));
        assert!(n(first).is_compressed());
    }

    #[test]
    fn page_list_bounded_pruning_after_split_invalidation_preserves_live_serials() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(2 * PagePool::item_size()),
            ..Default::default()
        });

        while s.total_pages() < 2 {
            s.grow().unwrap();
        }
        let first = first(&s);
        let old_serial = n(first).serial;
        let activity = s.page_compression.activity_serial;

        s.split(pin_at(first, 0, n(first).rows() / 2)).unwrap();
        assert!(!s.node_is_valid(first, old_serial));
        assert!(activity != s.page_compression.activity_serial);

        s.fill_last_page_for_test();
        s.grow().unwrap();
        s.expect_live_page_serials_valid_for_test();
    }

    #[test]
    fn page_list_repeated_bounded_pruning_after_split_preserves_live_serials() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(3 * PagePool::item_size()),
            ..Default::default()
        });

        let epoch = s.page_serial_epoch;
        while s.total_pages() < 3 {
            s.grow().unwrap();
        }
        let first = first(&s);
        s.split(pin_at(first, 0, n(first).rows() / 2)).unwrap();

        // The split target has a fresh serial but precedes older successor pages.
        // Prune both the old source and then that target while verifying ordinary
        // list mutation does not advance the whole-list validity epoch.
        for _ in 0..2 {
            while n(last(&s)).rows() < n(last(&s)).capacity().rows {
                s.grow().unwrap();
            }
            s.grow().unwrap();

            // Ordinary pruning invalidates one generation at a time through live
            // list validation; only reset may begin a new whole-list epoch.
            assert_eq!(epoch, s.page_serial_epoch);
            s.expect_live_page_serials_valid_for_test();
        }
    }

    #[test]
    fn page_list_bounded_pruning_after_front_replacement_preserves_live_serials() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(2 * PagePool::item_size()),
            ..Default::default()
        });

        while s.total_pages() < 2 {
            s.grow().unwrap();
        }
        let old = first(&s);
        let old_serial = n(old).serial;
        let replacement = s.increase_capacity(old, None).unwrap();
        assert!(replacement != old);
        assert!(!s.node_is_valid(old, old_serial));

        s.fill_last_page_for_test();
        s.grow().unwrap();

        s.expect_live_page_serials_valid_for_test();
    }

    #[test]
    fn page_list_bounded_pruning_after_middle_replacement_preserves_live_serials() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(3 * PagePool::item_size()),
            ..Default::default()
        });

        while s.total_pages() < 3 {
            s.grow().unwrap();
        }
        let old = next(first(&s));
        let old_serial = n(old).serial;
        let replacement = s.increase_capacity(old, None).unwrap();
        assert!(replacement != old);
        assert!(!s.node_is_valid(old, old_serial));

        // Prune the original first page and then the fresh middle replacement.
        for _ in 0..2 {
            s.fill_last_page_for_test();
            s.grow().unwrap();
            s.expect_live_page_serials_valid_for_test();
        }
    }

    #[test]
    fn page_list_incremental_compression_restarts_after_earlier_replacement() {
        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(3);

        let _ = s.compress(CompressMode::Incremental);
        let _ = s.compress(CompressMode::Incremental);
        assert!(n(first(&s)).is_compressed());
        assert!(n(next(first(&s))).is_compressed());

        // Replace a page before the still-valid continuation marker. The list's
        // allocation serial changes even though the marker itself remains, so the
        // next step must restart and inspect the replacement.
        let old_first = first(&s);
        let old_serial = n(old_first).serial;
        let replacement = s
            .increase_capacity(old_first, Some(IncreaseCapacity::GraphemeBytes))
            .unwrap();
        assert!(n(replacement).serial != old_serial);
        assert!(!n(replacement).is_compressed());

        let _ = s.compress(CompressMode::Incremental);
        assert!(n(replacement).is_compressed());
    }

    #[test]
    fn page_list_incremental_compression_keeps_progress_after_tail_growth() {
        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(INCREMENTAL_COMPRESSION_MAX_INSPECTED + 1);
        let _ = s.compress(CompressMode::Full);
        s.page_compression.reset();
        s.page_compression.mark_activity();

        let mut expected_last = first(&s);
        for _ in 1..INCREMENTAL_COMPRESSION_MAX_INSPECTED {
            expected_last = next(expected_last);
        }

        let first_r = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, first_r);
        assert_eq!(n(expected_last).serial, s.page_compression.last_serial.unwrap());

        // Allocate a new page at the active tail between steps. It is after the
        // continuation marker and must not restart progress through cold history.
        let next_serial = s.page_serial;
        while s.page_serial == next_serial {
            s.grow().unwrap();
        }
        let continued = s.compress(CompressMode::Incremental);
        assert_eq!(IncrementalCompressionResult::Pending, continued);
        assert!(s.page_compression.flags.verifying);
    }

    #[test]
    fn page_list_memory_usage() {
        let mut s = init(80, 24);

        // A fresh list holds the preheated pool items, one of which backs
        // the only page.
        let fresh = s.memory_usage();
        assert_eq!(s.total_pages(), fresh.pages);
        assert_eq!(PAGE_PREHEAT * PagePool::item_size(), fresh.virtual_bytes);
        assert_eq!(s.page_size, fresh.resident_bytes);
        assert!(fresh.resident_bytes <= fresh.virtual_bytes);
        assert_eq!(0, fresh.compressed_pages);
        assert_eq!(0, fresh.compressed_bytes);

        s.grow_cold_pages_for_test(2);
        let before = s.memory_usage();
        assert_eq!(s.total_pages(), before.pages);
        assert_eq!(
            s.page_size + s.pool.pages.free_count() * PagePool::item_size(),
            before.virtual_bytes
        );
        assert!(before.resident_bytes <= before.virtual_bytes);

        // Compression changes residency but not address space.
        let _ = s.compress(CompressMode::Full);
        let compressed = s.memory_usage();
        assert_eq!(before.pages, compressed.pages);
        assert_eq!(before.virtual_bytes, compressed.virtual_bytes);
        assert!(compressed.resident_bytes < before.resident_bytes);
        assert_eq!(2, compressed.compressed_pages);
        assert!(compressed.compressed_bytes > 0);
        assert_eq!(s.memory_stats().estimated_resident_bytes(), compressed.resident_bytes);

        // The query itself never restores a page.
        assert_eq!(compressed, s.memory_usage());

        // Reading a page restores it.
        let _ = pg(first(&s));
        let restored = s.memory_usage();
        assert_eq!(1, restored.compressed_pages);
        assert!(restored.resident_bytes > compressed.resident_bytes);
        assert_eq!(before.virtual_bytes, restored.virtual_bytes);

        // Destroyed pages go back to the pool: the address space is kept but
        // the memory is no longer resident.
        s.erase_rows(pt_history(0, 0), None);
        let erased = s.memory_usage();
        assert!(erased.pages < restored.pages);
        assert_eq!(before.virtual_bytes, erased.virtual_bytes);
        assert!(erased.resident_bytes < restored.resident_bytes);
    }

    #[test]
    fn page_list_memory_stats_do_not_restore_compressed_pages() {
        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(2);

        let before = s.memory_stats();
        assert_eq!(s.total_pages(), before.resident_pages);
        assert_eq!(0, before.compressed_pages);
        assert_eq!(s.page_size, before.raw_bytes);
        assert_eq!(before.raw_bytes, before.resident_raw_bytes);
        assert_eq!(0, before.decommitted_raw_bytes);
        assert_eq!(s.page_size, before.resident_backing_bytes);
        assert_eq!(0, before.encoded_bytes);
        assert_eq!(before.resident_backing_bytes, before.estimated_resident_bytes());
        assert_eq!(0, before.estimated_savings());

        let _ = s.compress(CompressMode::Full);
        let first = first(&s);
        assert_eq!(Storage::Compressed, n(first).storage());
        assert!(n(first).page_if_resident().is_none());

        let after = s.memory_stats();
        assert!(n(first).is_compressed());
        assert_eq!(s.total_pages(), after.resident_pages + after.compressed_pages);
        assert_eq!(2, after.compressed_pages);
        assert_eq!(s.page_size, after.raw_bytes);
        assert_eq!(after.raw_bytes, after.resident_raw_bytes + after.decommitted_raw_bytes);
        assert_eq!(
            after.resident_backing_bytes + after.encoded_bytes,
            after.estimated_resident_bytes()
        );
        assert_eq!(after.decommitted_raw_bytes - after.encoded_bytes, after.estimated_savings());

        let first_raw_len = n(first).metadata().memory.len();
        let first_encoded_len = encoded(first).len();
        let _ = pg(first);
        assert_eq!(Storage::Resident, n(first).storage());
        assert!(n(first).page_if_resident().is_some());

        let restored = s.memory_stats();
        assert_eq!(after.resident_pages + 1, restored.resident_pages);
        assert_eq!(after.compressed_pages - 1, restored.compressed_pages);
        assert_eq!(after.raw_bytes, restored.raw_bytes);
        assert_eq!(after.resident_raw_bytes + first_raw_len, restored.resident_raw_bytes);
        assert_eq!(after.decommitted_raw_bytes - first_raw_len, restored.decommitted_raw_bytes);
        assert_eq!(after.resident_backing_bytes + first_raw_len, restored.resident_backing_bytes);
        assert_eq!(after.encoded_bytes - first_encoded_len, restored.encoded_bytes);
    }

    #[test]
    fn page_list_preserved_page_keeps_compressed_storage() {
        let mut s = init(80, 24);

        let node = first(&s);
        let resident = pg(node);
        resident.dirty = true;
        *cell_at(resident, 3, 2) = cp('X');

        // Resident nodes can be borrowed without allocating an unnecessary copy.
        {
            let failing = FailingAllocator::leak(0);
            let mut preserved = n(node).page_preserving_state(failing.allocator()).unwrap();
            match preserved {
                PreservedPage::Borrowed(page_) => assert_eq!(resident as *const Page, page_),
                PreservedPage::Owned { .. } => panic!(),
            }
            assert!(!failing.has_induced_failure());
            preserved.deinit();
        }

        let expected = mem(resident).to_vec();
        let retained_ptr = resident.memory.ptr();

        assert!(s.compress_page(node));
        let stats = s.memory_stats();
        let expected_encoded = encoded(node).to_vec();

        // Test decommit simulates physical reclamation by clearing the retained
        // mapping. A preserved page must decode elsewhere rather than restoring
        // it.
        assert!(mem(n(node).metadata()).iter().all(|&b| b == 0));

        // Preserved-page allocation is opportunistic for callers. Failure leaves
        // the node and its compressed representation untouched.
        let failing = FailingAllocator::leak(0);
        assert_eq!(
            AllocError::OutOfMemory,
            n(node).page_preserving_state(failing.allocator()).unwrap_err()
        );
        assert_eq!(Storage::Compressed, n(node).storage());
        assert_eq!(stats, s.memory_stats());

        let mut preserved = n(node).page_preserving_state(Allocator::GLOBAL).unwrap();
        match preserved {
            PreservedPage::Borrowed(_) => panic!(),
            PreservedPage::Owned { .. } => {}
        }
        let page_ = preserved.page();

        assert!(page_.memory.ptr() != retained_ptr);
        assert_eq!(&expected[..], mem(page_));
        assert!(page_.dirty);
        assert_eq!('X' as u32, cell_at(page_, 3, 2).content_codepoint());

        // The node still owns the same compressed representation, and neither its
        // storage accounting nor its discarded raw mapping changed while cloning.
        assert_eq!(Storage::Compressed, n(node).storage());
        assert_eq!(stats, s.memory_stats());
        assert_eq!(retained_ptr, n(node).metadata().memory.ptr());
        assert!(mem(n(node).metadata()).iter().all(|&b| b == 0));
        assert_eq!(&expected_encoded[..], encoded(node));
        preserved.deinit();
    }

    #[test]
    fn page_list_memory_stats_include_unused_pool_backing() {
        let mut s = init(80, 24);

        // Pool allocation ownership is based on the requested layout fitting in a
        // standard item. The Page itself exposes only the initialized prefix.
        let node = s.create_page(CreatePage::cap(Capacity::new(1, 1))).unwrap();
        assert_eq!(Owned::Pool, n(node).owned);
        assert!(pg(node).memory.len() < PagePool::item_size());
        pg(node).size.rows = 1;
        append_node(&mut s, node);
        s.total_rows += 1;

        let raw_len = n(node).metadata().memory.len();
        let before = s.memory_stats();
        assert!(before.raw_bytes < s.page_size);
        assert_eq!(before.raw_bytes, before.resident_raw_bytes);
        assert_eq!(s.page_size, before.resident_backing_bytes);
        assert_eq!(s.page_size, before.estimated_resident_bytes());

        assert!(s.compress_page(node));
        let encoded_len = encoded(node).len();
        let compressed = s.memory_stats();
        assert_eq!(before.raw_bytes, compressed.raw_bytes);
        assert_eq!(before.resident_raw_bytes - raw_len, compressed.resident_raw_bytes);
        assert_eq!(raw_len, compressed.decommitted_raw_bytes);
        assert_eq!(before.resident_backing_bytes - raw_len, compressed.resident_backing_bytes);
        assert_eq!(encoded_len, compressed.encoded_bytes);
        assert_eq!(
            before.estimated_resident_bytes() - raw_len + encoded_len,
            compressed.estimated_resident_bytes()
        );
    }

    #[test]
    fn page_list_does_not_compress_the_mixed_history_and_active_page() {
        let mut s = init(80, 24);

        // One additional row creates history, but the history and all active rows
        // still share the first page. The active boundary therefore has a
        // historical prefix and must remain resident as one indivisible mapping.
        s.grow().unwrap();
        let active = s.get_top_left(Tag::Active);
        assert_eq!(first(&s), active.node);
        assert!(active.y > 0);

        let _ = s.compress(CompressMode::Full);
        assert!(!n(first(&s)).is_compressed());
    }

    #[test]
    fn page_list_compresses_only_complete_cold_history_pages() {
        // More active rows than one page at these dimensions can hold ensures the
        // active area spans multiple nodes when the pass chooses its boundary.
        let active_rows = initial_capacity(80).rows + 1;
        let mut s = init(80, active_rows);
        s.grow_cold_pages_for_test(2);

        // Move the active top into the boundary page so it has both a historical
        // prefix and active rows while the active area still spans later pages.
        s.grow().unwrap();

        let active = s.get_top_left(Tag::Active);
        let active_node = active.node;
        assert!(active.y > 0);
        assert!(active_node != last(&s));

        let mut expected_compressed: usize = 0;
        let mut expected_raw_bytes: usize = 0;
        let mut current = s.pages.first;
        while let Some(node) = current {
            if node == active_node {
                break;
            }
            expected_compressed += 1;
            expected_raw_bytes += n(node).metadata().memory.len();
            current = n(node).next;
        }
        assert_eq!(2, expected_compressed);

        let first = first(&s);
        *cell_at(pg(first), 0, 0) = cp('X');
        let expected = mem(pg(first)).to_vec();
        let first_memory = pg(first).memory.ptr();
        let page_size = s.page_size;

        let _ = s.compress(CompressMode::Full);
        let memory = s.memory_stats();
        assert_eq!(expected_compressed, memory.compressed_pages);
        assert_eq!(expected_raw_bytes, memory.decommitted_raw_bytes);
        assert!(memory.encoded_bytes < memory.decommitted_raw_bytes);
        assert_eq!(page_size, s.page_size);

        current = s.pages.first;
        let mut actual_encoded_bytes: usize = 0;
        while let Some(node) = current {
            if node == active_node {
                break;
            }
            assert!(n(node).is_compressed());
            actual_encoded_bytes += encoded(node).len();
            current = n(node).next;
        }
        assert_eq!(actual_encoded_bytes, memory.encoded_bytes);
        current = Some(active_node);
        while let Some(node) = current {
            assert!(!n(node).is_compressed());
            current = n(node).next;
        }

        // Restoring the oldest page preserves both the mapping identity and all
        // of its bytes even though the pass discarded its physical pages.
        assert_eq!(first_memory, n(first).metadata().memory.ptr());
        assert_eq!(&expected[..], mem(pg(first)));
        assert_eq!(first_memory, pg(first).memory.ptr());
    }

    #[test]
    fn page_list_lazily_restores_compressed_history_made_active_by_resize() {
        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(1);

        let first = first(&s);
        *cell_at(pg(first), 0, 0) = cp('X');
        let memory_ptr = pg(first).memory.ptr();
        let memory_len = pg(first).memory.len();
        let page_size = s.page_size;

        let _ = s.compress(CompressMode::Full);
        assert!(n(first).is_compressed());

        // Pull all scrollback into the active area by making the viewport as tall
        // as the complete screen. A row-only resize needs only page metadata, so
        // the newly active page can remain compressed until its contents are used.
        let all_rows = s.total_rows as CellCountInt;
        s.resize(Resize {
            rows: Some(all_rows),
            ..Default::default()
        })
        .unwrap();
        let active = s.get_top_left(Tag::Active);
        assert_eq!(first, active.node);
        assert_eq!(0, active.y);
        assert!(n(first).is_compressed());
        assert_eq!(page_size, s.page_size);

        // The compression pass must not reconsider the node now that it is active.
        // Content access follows the normal page boundary, which recommits and
        // restores the retained mapping before returning the cell.
        let _ = s.compress(CompressMode::Full);
        assert!(n(first).is_compressed());
        let cell = s.get_cell(pt_active(0, 0)).unwrap();
        assert_eq!('X' as u32, unsafe { (*cell.cell).content_codepoint() });
        assert!(!n(first).is_compressed());
        assert_eq!(memory_ptr, pg(first).memory.ptr());
        assert_eq!(memory_len, pg(first).memory.len());
        assert_eq!(page_size, s.page_size);
    }

    #[test]
    fn page_list_full_and_incremental_compression_skip_a_spanning_viewport() {
        let mut full = init(80, 24);
        full.grow_cold_pages_for_test(3);

        let mut incremental = init(80, 24);
        incremental.grow_cold_pages_for_test(3);

        // Start near the end of the first page so the viewport intersects both
        // the first and second historical page mappings.
        let first = first(&full);
        let overlap_rows: usize = full.rows as usize / 2;
        let viewport_row: usize = n(first).rows() as usize - overlap_rows;
        full.scroll(Scroll::Row(viewport_row));
        incremental.scroll(Scroll::Row(viewport_row));
        assert!(full.get_top_left(Tag::Viewport).node != full.get_bottom_right(Tag::Viewport).unwrap().node);
        let second = next(first);
        assert_eq!(first, full.get_top_left(Tag::Viewport).node);
        assert_eq!(second, full.get_bottom_right(Tag::Viewport).unwrap().node);

        let _ = full.compress(CompressMode::Full);
        let _ = incremental.compress(CompressMode::Drain);
        assert_eq!(full.memory_stats(), incremental.memory_stats());
        assert!(!n(first).is_compressed());
        assert!(!n(second).is_compressed());

        let full_active = full.get_top_left(Tag::Active).node;
        let incremental_active = incremental.get_top_left(Tag::Active).node;
        let mut full_node = first;
        let mut incremental_node = incremental.pages.first.unwrap();
        while full_node != full_active {
            assert_eq!(n(full_node).is_compressed(), n(incremental_node).is_compressed());

            full_node = next(full_node);
            incremental_node = next(incremental_node);
        }
        assert_eq!(incremental_active, incremental_node);

        let mut eligible = CompressionIterator::init(&full);
        let mut compressed_pages: usize = 0;
        while let Some(node) = eligible.next() {
            compressed_pages += 1;
            assert!(n(node).is_compressed());
        }
        assert!(compressed_pages > 0);
    }

    #[test]
    fn page_list_cold_compression_continues_after_an_incompressible_page() {
        let mut s = init(80, 24);
        s.grow_cold_pages_for_test(2);

        let first = first(&s);
        let second = next(first);
        random_bytes(0x434F_4C44_5041_4745, mem(pg(first)));

        let page_size = s.page_size;
        let _ = s.compress(CompressMode::Full);
        let memory = s.memory_stats();
        assert_eq!(1, memory.compressed_pages);
        assert!(!n(first).is_compressed());
        assert!(n(second).is_compressed());
        assert_eq!(n(second).metadata().memory.len(), memory.decommitted_raw_bytes);
        assert!(memory.encoded_bytes < memory.decommitted_raw_bytes);
        assert_eq!(page_size, s.page_size);

        // Failed resident candidates are deliberately retried on later passes,
        // while the successful page remains compressed and is skipped.
        let _ = s.compress(CompressMode::Full);
        assert_eq!(memory, s.memory_stats());
    }

    #[test]
    fn page_list_compression_restores_through_page_access() {
        let mut s = init(80, 24);

        let node = first(&s);
        let page = pg(node);
        page.dirty = true;
        *cell_at(page, 3, 2) = cp('X');

        let expected = mem(page).to_vec();
        let memory_ptr = page.memory.ptr();
        let memory_len = page.memory.len();
        let page_size = s.page_size;

        assert!(s.compress_page(node));
        assert!(n(node).is_compressed());
        assert_eq!(24, n(node).rows());
        assert_eq!(80, n(node).cols());
        assert_eq!(memory_ptr, n(node).metadata().memory.ptr());
        assert_eq!(memory_len, n(node).metadata().memory.len());
        assert_eq!(page_size, s.page_size);

        // Pin access restores the page without changing its retained mapping.
        let page_pin = pin_at(node, 3, 2);
        assert_eq!('X' as u32, pin_rac(page_pin).1.content_codepoint());
        assert!(!n(node).is_compressed());
        assert_eq!(memory_ptr, pg(node).memory.ptr());
        assert_eq!(&expected[..], mem(pg(node)));
        assert!(pg(node).dirty);

        // Recompressing exercises reuse of the page-pool scratch item. Page
        // iterator chunks also restore before exposing row memory.
        assert!(s.compress_page(node));
        let mut page_it = Pin::new(node).page_iterator(Direction::RightDown, None);
        let chunk = page_it.next().unwrap();
        assert_eq!(n(node).rows() as usize, chunk.rows().len());
        assert!(!n(node).is_compressed());
        assert_eq!(&expected[..], mem(pg(node)));

        // Read-only PageList operations restore through the same boundary.
        assert!(s.compress_page(node));
        let cloned = s.clone(clone_opts(pt_screen(0, 0), None)).unwrap();
        assert!(!n(node).is_compressed());
        assert_eq!('X' as u32, cell_at(pg(first(&cloned)), 3, 2).content_codepoint());
    }

    #[test]
    fn page_list_compression_uses_temporary_scratch_for_oversized_pages() {
        let mut s = init(80, 24);

        let mut node = first(&s);
        while pg(node).memory.len() <= std_size() {
            node = s.increase_capacity(node, Some(IncreaseCapacity::GraphemeBytes)).unwrap();
        }

        let expected = mem(pg(node)).to_vec();
        let memory_ptr = pg(node).memory.ptr();
        let memory_len = pg(node).memory.len();
        let page_size = s.page_size;

        assert!(s.compress_page(node));
        assert!(n(node).is_compressed());
        assert_eq!(page_size, s.page_size);
        assert_eq!(memory_ptr, n(node).metadata().memory.ptr());
        assert_eq!(memory_len, n(node).metadata().memory.len());

        assert_eq!(&expected[..], mem(pg(node)));
        assert!(!n(node).is_compressed());
        assert_eq!(memory_ptr, pg(node).memory.ptr());
    }

    #[test]
    fn page_list_compression_leaves_incompressible_pages_resident() {
        let mut s = init(80, 24);

        let node = first(&s);
        let original = mem(pg(node)).to_vec();

        random_bytes(0x5041_4745_4C49_5354, mem(pg(node)));
        let page_size = s.page_size;

        assert!(!s.compress_page(node));
        assert!(!n(node).is_compressed());
        assert_eq!(page_size, s.page_size);

        mem(pg(node)).copy_from_slice(&original);
    }

    #[test]
    fn page_list_reset_discards_malformed_compressed_data() {
        let mut s = init(80, 24);

        let node = first(&s);
        assert!(s.compress_page(node));
        encoded(node).fill(0xFF);

        s.reset();
        assert!(!n(first(&s)).is_compressed());
        assert_eq!(1, s.total_pages());
    }

    #[test]
    fn page_list_deinit_discards_malformed_compressed_data() {
        let mut s = init(80, 24);
        let node = first(&s);
        assert!(s.compress_page(node));
        encoded(node).fill(0xFF);

        drop(s);
    }

    #[test]
    fn page_list_prune_reuses_malformed_compressed_page_memory() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(2 * PagePool::item_size()),
            ..Default::default()
        });

        // Allocate the second page so the first one can be pruned and reused.
        while s.pages.first == s.pages.last {
            s.grow().unwrap();
        }
        let first = first(&s);
        assert!(s.compress_page(first));
        encoded(first).fill(0xFF);

        let mut reused = false;
        let growth_limit = n(last(&s)).capacity().rows as usize + 1;
        for _ in 0..growth_limit {
            if let Some(new_node) = s.grow().unwrap() {
                if new_node == first {
                    reused = true;
                    break;
                }
            }
        }

        assert!(reused);
        assert_eq!(first, last(&s));
        assert!(!n(first).is_compressed());
        assert_eq!(1, n(first).rows());
        pg(first).assert_integrity();
    }

    #[test]
    fn page_list() {
        let mut s = init(80, 24);
        assert!(s.viewport == Viewport::Active);
        assert!(s.pages.first.is_some());
        assert_eq!(s.rows as usize, s.total_rows());

        // Initial total rows should be our row count
        assert_eq!(s.rows as usize, s.total_rows);

        // Our viewport pin must be defined. It isn't used until the
        // viewport is a pin but it prevents undefined access on clone.
        assert!(vp(&s).node == first(&s));

        // Active area should be the top
        assert_eq!(pin_at(first(&s), 0, 0), s.get_top_left(Tag::Active));

        // Scrollbar should be where we expect it
        assert_eq!(
            Scrollbar {
                total: s.rows as usize,
                offset: 0,
                len: s.rows as usize,
            },
            s.scrollbar()
        );
    }

    #[test]
    fn page_list_init_error() {
        // Test every failure point in `init` and ensure that we don't
        // leak memory (the pools assert this on drop) since we're exiting early.
        for tag in InitFailPoint::ALL {
            let tw = &INIT_TW;
            tw.error_always(tag, AllocError::OutOfMemory);
            assert_eq!(
                AllocError::OutOfMemory,
                PageList::init(Options {
                    cols: 80,
                    rows: 24,
                    ..Default::default()
                })
                .unwrap_err()
            );
            tw.end(tripwire::ResetMode::Reset).unwrap();
        }

        // init calls initPages transitively, so let's check that if
        // any failures happen in initPages, we also don't leak memory.
        for tag in InitPagesFailPoint::ALL {
            let tw = &INIT_PAGES_TW;
            tw.error_always(tag, AllocError::OutOfMemory);

            let cols: CellCountInt = if tag == InitPagesFailPoint::PageBufStd {
                80
            } else {
                STD_CAPACITY.max_cols().unwrap() + 1
            };
            assert_eq!(
                AllocError::OutOfMemory,
                PageList::init(Options {
                    cols,
                    rows: 24,
                    ..Default::default()
                })
                .unwrap_err()
            );
            tw.end(tripwire::ResetMode::Reset).unwrap();
        }

        // Try non-standard pages since they don't go in our pool.
        for tag in [InitPagesFailPoint::PageBufNonStd] {
            let tw = &INIT_PAGES_TW;
            tw.error_after(tag, AllocError::OutOfMemory, 1);
            assert_eq!(
                AllocError::OutOfMemory,
                PageList::init(Options {
                    cols: STD_CAPACITY.max_cols().unwrap() + 1,
                    rows: STD_CAPACITY.rows + 1,
                    ..Default::default()
                })
                .unwrap_err()
            );
            tw.end(tripwire::ResetMode::Reset).unwrap();
        }
    }

    #[test]
    fn page_list_init_rows_across_two_pages() {
        // Find a cap that makes it so that rows don't fit on one page.
        let rows = 100;
        let cap = {
            let mut cap = STD_CAPACITY.adjust(pagepkg::Adjustment { cols: Some(50) }).unwrap();
            while cap.rows >= rows {
                cap = STD_CAPACITY
                    .adjust(pagepkg::Adjustment {
                        cols: Some(cap.cols + 50),
                    })
                    .unwrap();
            }
            cap
        };

        // Init
        let mut s = init(cap.cols, rows);
        assert!(s.viewport == Viewport::Active);
        assert!(s.pages.first.is_some());
        assert_eq!(s.rows as usize, s.total_rows());

        // Initial total rows should be our row count
        assert_eq!(s.rows as usize, s.total_rows);

        // Scrollbar should be where we expect it
        assert_eq!(sb(s.rows as usize, 0, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_init_more_than_max_cols() {
        // Initialize with more columns than we can fit in our standard
        // capacity. This is going to force us to go to a non-standard page
        // immediately.
        let mut s = init(STD_CAPACITY.max_cols().unwrap() + 1, 80);
        assert!(s.viewport == Viewport::Active);
        assert_eq!(s.rows as usize, s.total_rows());

        // We expect a single, non-standard page
        assert!(s.pages.first.is_some());
        assert!(pg(first(&s)).memory.len() > std_size());

        // Initial total rows should be our row count
        assert_eq!(s.rows as usize, s.total_rows);

        // Scrollbar should be where we expect it
        assert_eq!(sb(s.rows as usize, 0, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_point_from_pin_active_no_history() {
        let s = init(80, 24);

        assert_eq!(
            pt_active(0, 0),
            s.point_from_pin(Tag::Active, pin_at(first(&s), 0, 0)).unwrap()
        );
        assert_eq!(
            pt_active(4, 2),
            s.point_from_pin(Tag::Active, pin_at(first(&s), 4, 2)).unwrap()
        );
    }

    #[test]
    fn page_list_point_from_pin_active_with_history() {
        let mut s = init(80, 24);
        s.grow_rows(30).unwrap();

        assert_eq!(
            pt_active(2, 0),
            s.point_from_pin(Tag::Active, pin_at(first(&s), 2, 30)).unwrap()
        );

        // In history, invalid
        assert!(s.point_from_pin(Tag::Active, pin_at(first(&s), 2, 21)).is_none());
    }

    #[test]
    fn page_list_point_from_pin_active_from_prior_page() {
        let mut s = init(80, 24);
        // Grow so we take up at least 5 pages.
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_paused(&mut s, cap_rows * 5);

        assert_eq!(
            pt_active(2, 0),
            s.point_from_pin(Tag::Active, pin_at(last(&s), 2, 0)).unwrap()
        );

        // Prior page
        assert!(s.point_from_pin(Tag::Active, pin_at(first(&s), 0, 0)).is_none());
    }

    #[test]
    fn page_list_point_from_pin_traverse_pages() {
        let mut s = init(80, 24);

        // Grow so we take up at least 2 pages.
        let page_cap = pg(last(&s)).capacity.rows as usize;
        grow_paused(&mut s, page_cap * 2);

        {
            let pages = s.total_pages();
            let expected_y = page_cap * (pages - 2) + 5;

            assert_eq!(
                pt_screen(2, expected_y as u32),
                s.point_from_pin(Tag::Screen, pin_at(prev(last(&s)), 2, 5)).unwrap()
            );
        }

        // Prior page
        assert!(s.point_from_pin(Tag::Active, pin_at(first(&s), 0, 0)).is_none());
    }

    #[test]
    fn page_list_point_from_pin_rejects_overflowing_screen_coordinate() {
        // Use maximum-height metadata-only pages to model a valid scrollback just
        // beyond the u32 coordinate range without allocating their backing cells.
        let page_count = 65_539;
        let rows_per_page = CellCountInt::MAX;
        let (_page, mut nodes) = fake_nodes(page_count, rows_per_page);

        let mut s = init(1, 1);
        let real_pages = s.pages;
        let base = nodes.as_mut_ptr();
        s.pages = List {
            first: NonNull::new(base),
            last: NonNull::new(unsafe { base.add(page_count - 1) }),
        };

        let result = s.point_from_pin(Tag::Screen, Pin::new(NonNull::new(unsafe { base.add(page_count - 1) }).unwrap()));
        s.pages = real_pages;
        assert!(result.is_none());
    }

    #[test]
    fn page_list_active_after_grow() {
        let mut s = init(80, 24);
        assert_eq!(s.rows as usize, s.total_rows());

        s.grow_rows(10).unwrap();
        assert_eq!(s.rows as usize + 10, s.total_rows());

        // Make sure all points make sense
        assert_eq!(pt_screen(0, 10), s.get_cell(pt_viewport(0, 0)).unwrap().screen_point());
        assert_eq!(pt_screen(0, 0), s.get_cell(pt_screen(0, 0)).unwrap().screen_point());
        assert_eq!(pt_screen(0, 10), s.get_cell(pt_active(0, 0)).unwrap().screen_point());

        // Scrollbar should be in the active area
        assert_eq!(sb(s.total_rows(), 10, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_grow_allows_exceeding_max_size_for_active_area() {
        // Setup our initial page so that we fully take up one page.
        let cap = STD_CAPACITY.adjust(pagepkg::Adjustment { cols: Some(5) }).unwrap();
        let mut s = init_opts(Options {
            cols: 5,
            rows: cap.rows,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(s.rows as usize, s.total_rows());

        // Grow once because we guarantee at least two pages of
        // capacity so we want to get to that.
        s.grow().unwrap();
        let start_pages = s.total_pages();
        assert!(start_pages >= 2);

        // Surgically modify our pages so that they have a smaller size.
        {
            let mut it = s.pages.first;
            while let Some(page) = it {
                pg(page).size.rows = 1;
                pg(page).capacity.rows = 1;
                it = n(page).next;
            }

            // Avoid integrity check failures
            s.total_rows = s.total_rows();
        }

        // Grow our row and ensure we don't prune pages because we need
        // enough for the active area.
        s.grow().unwrap();
        assert_eq!(start_pages + 1, s.total_pages());
    }

    #[test]
    fn page_list_grow_prune_required_with_a_single_page() {
        // Need scrollback > 0 to have a scrollbar to test
        let mut s = init(80, 24);

        // This block is all test setup. There is nothing required about this
        // behavior during a refactor. This is setting up a scenario that is
        // possible to trigger a bug (#2280).
        {
            // Increase our capacity until our page is larger than the standard size.
            // This is important because it triggers a scenario where our calculated
            // minSize() which is supposed to accommodate 2 pages is no longer true.
            loop {
                let layout = Page::layout(n(first(&s)).capacity());
                if layout.total_size > std_size() {
                    break;
                }
                s.increase_capacity(first(&s), Some(IncreaseCapacity::GraphemeBytes))
                    .unwrap();
            }
            assert!(s.pages.first.is_some());
            assert!(s.pages.first == s.pages.last);
        }

        // Figure out the remaining number of rows. This is the amount that
        // can be added to the current page before we need to allocate a new
        // page.
        let rem = {
            let page = first(&s);
            n(page).capacity().rows - n(page).rows()
        };
        for _ in 0..rem {
            assert!(s.grow().unwrap().is_none());
        }

        // The next one we add will trigger a new page.
        let new = s.grow().unwrap();
        assert!(new.is_some());
        assert!(new != s.pages.first);

        // Scrollbar should be in the active area
        assert_eq!(
            sb(s.total_rows(), s.total_rows - s.rows as usize, s.rows as usize),
            s.scrollbar()
        );
    }

    #[test]
    fn page_list_scrollbar_with_max_size_0_after_grow() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(0),
            ..Default::default()
        });

        // Grow some rows (simulates normal terminal output)
        s.grow_rows(10).unwrap();

        let sbar = s.scrollbar();

        // With no scrollback (max_size = 0), total should equal rows
        assert_eq!(s.rows as usize, sbar.total);

        // With no scrollback, offset should be 0 (nowhere to scroll back to)
        assert_eq!(0, sbar.offset);
    }

    #[test]
    fn page_list_scroll_with_max_size_0_no_history() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(0),
            ..Default::default()
        });

        s.grow_rows(10).unwrap();

        // Remember initial viewport position
        let pt_before = vps(&s);

        // Try to scroll backwards into "history" - should be no-op
        s.scroll(Scroll::DeltaRow(-5));
        assert!(s.viewport == Viewport::Active);

        // Scroll to top - should also be no-op with no scrollback
        s.scroll(Scroll::Top);
        let pt_after = vps(&s);
        assert_eq!(pt_before, pt_after);
    }

    #[test]
    fn page_list_scroll_top() {
        let mut s = init(80, 24);
        s.grow_rows(10).unwrap();

        assert_eq!(pt_screen(0, 10), vps(&s));

        s.scroll(Scroll::Top);

        assert_eq!(pt_screen(0, 0), vps(&s));

        assert_eq!(sb(s.total_rows(), 0, s.rows as usize), s.scrollbar());

        s.grow_rows(10).unwrap();
        assert_eq!(pt_screen(0, 0), vps(&s));

        assert_eq!(sb(s.total_rows(), 0, s.rows as usize), s.scrollbar());

        s.scroll(Scroll::Active);
        assert_eq!(pt_screen(0, 20), vps(&s));

        assert_eq!(
            sb(s.total_rows(), s.total_rows - s.rows as usize, s.rows as usize),
            s.scrollbar()
        );
    }

    #[test]
    fn page_list_scroll_delta_row_back() {
        let mut s = init(80, 24);
        s.grow_rows(10).unwrap();

        assert_eq!(pt_screen(0, 10), vps(&s));

        s.scroll(Scroll::DeltaRow(-1));

        assert_eq!(
            sb(s.total_rows(), s.total_rows - s.rows as usize - 1, s.rows as usize),
            s.scrollbar()
        );

        assert_eq!(pt_screen(0, 9), vps(&s));

        s.grow_rows(10).unwrap();
        assert_eq!(pt_screen(0, 9), vps(&s));

        assert_eq!(
            sb(s.total_rows(), s.total_rows - s.rows as usize - 11, s.rows as usize),
            s.scrollbar()
        );

        s.scroll(Scroll::DeltaRow(-1));

        assert_eq!(
            sb(s.total_rows(), s.total_rows - s.rows as usize - 12, s.rows as usize),
            s.scrollbar()
        );
    }

    #[test]
    fn page_list_scroll_delta_row_back_overflow() {
        let mut s = init(80, 24);
        s.grow_rows(10).unwrap();

        assert_eq!(pt_screen(0, 10), vps(&s));

        s.scroll(Scroll::DeltaRow(-100));

        assert_eq!(pt_screen(0, 0), vps(&s));

        assert_eq!(sb(s.total_rows(), 0, s.rows as usize), s.scrollbar());

        s.grow_rows(10).unwrap();
        assert_eq!(pt_screen(0, 0), vps(&s));

        assert_eq!(sb(s.total_rows(), 0, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_scroll_minimum_row_delta() {
        let mut s = init(10, 3);

        // Create one row of history so scrolling all the way back has an
        // observable result.
        s.grow_rows(1).unwrap();
        s.scroll(Scroll::DeltaRow(isize::MIN));

        assert_eq!(Viewport::Top, s.viewport);
    }

    #[test]
    fn page_list_scroll_delta_row_forward() {
        let mut s = init(80, 24);
        s.grow_rows(10).unwrap();

        assert_eq!(pt_screen(0, 10), vps(&s));

        s.scroll(Scroll::Top);
        s.scroll(Scroll::DeltaRow(2));

        assert_eq!(sb(s.total_rows(), 2, s.rows as usize), s.scrollbar());

        assert_eq!(pt_screen(0, 2), vps(&s));

        s.grow_rows(10).unwrap();
        assert_eq!(pt_screen(0, 2), vps(&s));

        assert_eq!(sb(s.total_rows(), 2, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_scroll_delta_row_forward_into_active() {
        let mut s = init(80, 24);

        s.scroll(Scroll::DeltaRow(2));

        assert_eq!(pt_screen(0, 0), vps(&s));

        assert_eq!(
            sb(s.total_rows(), s.total_rows - s.rows as usize, s.rows as usize),
            s.scrollbar()
        );
    }

    #[test]
    fn page_list_scroll_delta_row_back_without_space_preserves_active() {
        let mut s = init(80, 24);
        s.scroll(Scroll::DeltaRow(-1));

        assert_eq!(pt_screen(0, 0), vps(&s));

        assert!(s.viewport == Viewport::Active);

        assert_eq!(
            sb(s.total_rows(), s.total_rows - s.rows as usize, s.rows as usize),
            s.scrollbar()
        );
    }

    #[test]
    fn page_list_scroll_to_pin() {
        let mut s = init(80, 24);
        s.grow_rows(10).unwrap();

        s.scroll(Scroll::Pin(s.pin(pt_screen(2, 4)).unwrap()));

        assert_eq!(sb(s.total_rows(), 4, s.rows as usize), s.scrollbar());

        assert_eq!(pt_screen(0, 4), vps(&s));

        s.scroll(Scroll::Pin(s.pin(pt_screen(2, 5)).unwrap()));

        assert_eq!(sb(s.total_rows(), 5, s.rows as usize), s.scrollbar());

        assert_eq!(pt_screen(0, 5), vps(&s));
    }

    #[test]
    fn page_list_scroll_to_pin_in_active() {
        let mut s = init(80, 24);
        s.grow_rows(10).unwrap();

        s.scroll(Scroll::Pin(s.pin(pt_screen(2, 30)).unwrap()));

        assert_eq!(
            sb(s.total_rows(), s.total_rows - s.rows as usize, s.rows as usize),
            s.scrollbar()
        );

        assert_eq!(pt_screen(0, 10), vps(&s));
    }

    #[test]
    fn page_list_scroll_to_pin_at_top() {
        let mut s = init(80, 24);
        s.grow_rows(10).unwrap();

        s.scroll(Scroll::Pin(s.pin(pt_screen(2, 0)).unwrap()));

        assert!(s.viewport == Viewport::Top);

        assert_eq!(sb(s.total_rows(), 0, s.rows as usize), s.scrollbar());

        assert_eq!(pt_screen(0, 0), vps(&s));
    }

    #[test]
    fn page_list_scroll_to_row_0() {
        let mut s = init(80, 24);
        s.grow_rows(10).unwrap();

        assert_eq!(pt_screen(0, 10), vps(&s));

        s.scroll(Scroll::Row(0));
        assert!(s.viewport == Viewport::Top);

        assert_eq!(pt_screen(0, 0), vps(&s));

        assert_eq!(sb(s.total_rows, 0, s.rows as usize), s.scrollbar());

        s.grow_rows(10).unwrap();
        assert_eq!(pt_screen(0, 0), vps(&s));

        assert_eq!(sb(s.total_rows, 0, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_scroll_to_row_in_scrollback() {
        let mut s = init(80, 24);
        s.grow_rows(20).unwrap();

        assert_eq!(pt_screen(0, 20), vps(&s));

        s.scroll(Scroll::Row(5));
        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, 5, s.rows as usize), s.scrollbar());

        assert_eq!(pt_screen(0, 5), vps(&s));

        s.grow_rows(10).unwrap();
        assert_eq!(pt_screen(0, 5), vps(&s));

        assert_eq!(sb(s.total_rows, 5, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_scroll_to_row_in_middle() {
        let mut s = init(80, 24);
        s.grow_rows(50).unwrap();

        let total = s.total_rows;
        let midpoint = total / 2;
        s.scroll(Scroll::Row(midpoint));

        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, midpoint, s.rows as usize), s.scrollbar());

        assert_eq!(pt_screen(0, midpoint as u32), vps(&s));

        s.grow_rows(10).unwrap();
        assert_eq!(pt_screen(0, midpoint as u32), vps(&s));

        assert_eq!(sb(s.total_rows, midpoint, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_scroll_to_row_at_active_boundary() {
        let mut s = init(80, 24);
        s.grow_rows(20).unwrap();

        let active_start = s.total_rows - s.rows as usize;

        s.scroll(Scroll::Row(active_start));

        assert!(s.viewport == Viewport::Active);

        assert_eq!(pt_screen(0, active_start as u32), vps(&s));

        assert_eq!(
            sb(s.total_rows, s.total_rows - s.rows as usize, s.rows as usize),
            s.scrollbar()
        );

        s.grow_rows(10).unwrap();

        assert!(s.viewport == Viewport::Active);

        assert_eq!(
            sb(s.total_rows, s.total_rows - s.rows as usize, s.rows as usize),
            s.scrollbar()
        );
    }

    #[test]
    fn page_list_scroll_to_row_beyond_active() {
        let mut s = init(80, 24);
        s.grow_rows(10).unwrap();

        s.scroll(Scroll::Row(1000));

        assert!(s.viewport == Viewport::Active);

        assert_eq!(pt_screen(0, 10), vps(&s));

        assert_eq!(
            sb(s.total_rows, s.total_rows - s.rows as usize, s.rows as usize),
            s.scrollbar()
        );
    }

    #[test]
    fn page_list_scroll_to_row_without_scrollback() {
        let mut s = init(80, 24);

        s.scroll(Scroll::Row(5));

        assert!(s.viewport == Viewport::Active);

        assert_eq!(pt_screen(0, 0), vps(&s));

        assert_eq!(
            sb(s.total_rows, s.total_rows - s.rows as usize, s.rows as usize),
            s.scrollbar()
        );
    }

    #[test]
    fn page_list_scroll_to_row_then_delta() {
        let mut s = init(80, 24);
        s.grow_rows(30).unwrap();

        s.scroll(Scroll::Row(10));

        assert!(s.viewport == Viewport::Pin);

        assert_eq!(pt_screen(0, 10), vps(&s));

        assert_eq!(sb(s.total_rows, 10, s.rows as usize), s.scrollbar());

        s.scroll(Scroll::DeltaRow(5));

        assert!(s.viewport == Viewport::Pin);

        assert_eq!(pt_screen(0, 15), vps(&s));

        assert_eq!(sb(s.total_rows, 15, s.rows as usize), s.scrollbar());

        s.scroll(Scroll::DeltaRow(-3));

        assert!(s.viewport == Viewport::Pin);

        assert_eq!(pt_screen(0, 12), vps(&s));

        assert_eq!(sb(s.total_rows, 12, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_scroll_to_row_with_cache_fast_path_down() {
        let mut s = init(80, 24);
        s.grow_rows(50).unwrap();

        s.scroll(Scroll::Row(10));

        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, 10, s.rows as usize), s.scrollbar());

        assert_eq!(pt_screen(0, 10), vps(&s));

        // Verify cache is populated
        assert!(s.viewport_pin_row_offset.is_some());
        assert_eq!(10, s.viewport_pin_row_offset.unwrap());

        // Now scroll to a different row - this should use the fast path
        s.scroll(Scroll::Row(20));

        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, 20, s.rows as usize), s.scrollbar());

        assert_eq!(pt_screen(0, 20), vps(&s));

        s.grow_rows(10).unwrap();
        assert_eq!(pt_screen(0, 20), vps(&s));

        assert_eq!(sb(s.total_rows, 20, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_scroll_to_row_with_cache_fast_path_up() {
        let mut s = init(80, 24);
        s.grow_rows(50).unwrap();

        s.scroll(Scroll::Row(30));

        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, 30, s.rows as usize), s.scrollbar());

        assert_eq!(pt_screen(0, 30), vps(&s));

        // Verify cache is populated
        assert!(s.viewport_pin_row_offset.is_some());
        assert_eq!(30, s.viewport_pin_row_offset.unwrap());

        // Now scroll up to a different row - this should use the fast path
        s.scroll(Scroll::Row(15));

        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, 15, s.rows as usize), s.scrollbar());

        assert_eq!(pt_screen(0, 15), vps(&s));

        s.grow_rows(10).unwrap();
        assert_eq!(pt_screen(0, 15), vps(&s));

        assert_eq!(sb(s.total_rows, 15, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_scroll_clear() {
        let mut s = init(80, 24);

        {
            let cell = s.get_cell(pt_active(0, 0)).unwrap();
            unsafe { *cell.cell = cp('A') };
        }
        {
            let cell = s.get_cell(pt_active(0, 1)).unwrap();
            unsafe { *cell.cell = cp('A') };
        }

        s.scroll_clear().unwrap();

        assert_eq!(pt_screen(0, 2), vps(&s));
    }

    #[test]
    fn page_list_jump_zero_prompts() {
        let mut s = init(5, 3);
        s.grow_rows(3).unwrap();
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        rac(page, 0, 1).0.set_semantic_prompt(SemanticPrompt::Prompt);
        rac(page, 0, 5).0.set_semantic_prompt(SemanticPrompt::Prompt);

        s.scroll(Scroll::DeltaPrompt(0));
        assert!(s.viewport == Viewport::Active);

        assert_eq!(
            sb(s.total_rows, s.total_rows - s.rows as usize, s.rows as usize),
            s.scrollbar()
        );
    }

    #[test]
    fn page_list_jump_minimum_prompt_delta() {
        let mut s = init(10, 3);

        s.scroll(Scroll::DeltaPrompt(isize::MIN));
        assert_eq!(Viewport::Active, s.viewport);
    }

    #[test]
    fn screen_jump_back_one_prompt() {
        let mut s = init(5, 3);
        s.grow_rows(3).unwrap();
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        rac(page, 0, 1).0.set_semantic_prompt(SemanticPrompt::Prompt);
        rac(page, 0, 5).0.set_semantic_prompt(SemanticPrompt::Prompt);

        // Jump back
        {
            s.scroll(Scroll::DeltaPrompt(-1));
            assert!(s.viewport == Viewport::Pin);
            assert_eq!(
                pt_screen(0, 1),
                s.point_from_pin(Tag::Screen, s.pin(pt_viewport(0, 0)).unwrap()).unwrap()
            );

            assert_eq!(sb(s.total_rows, 1, s.rows as usize), s.scrollbar());
        }
        {
            s.scroll(Scroll::DeltaPrompt(-1));
            assert!(s.viewport == Viewport::Pin);
            assert_eq!(
                pt_screen(0, 1),
                s.point_from_pin(Tag::Screen, s.pin(pt_viewport(0, 0)).unwrap()).unwrap()
            );

            assert_eq!(sb(s.total_rows, 1, s.rows as usize), s.scrollbar());
        }

        // Jump forward
        {
            s.scroll(Scroll::DeltaPrompt(1));
            assert!(s.viewport == Viewport::Active);
            assert_eq!(
                sb(s.total_rows, s.total_rows - s.rows as usize, s.rows as usize),
                s.scrollbar()
            );
        }
        {
            s.scroll(Scroll::DeltaPrompt(1));
            assert!(s.viewport == Viewport::Active);
            assert_eq!(
                sb(s.total_rows, s.total_rows - s.rows as usize, s.rows as usize),
                s.scrollbar()
            );
        }
    }

    #[test]
    fn screen_jump_forward_prompt_skips_multiline_continuation() {
        let mut s = init(5, 3);
        s.grow_rows(7).unwrap();

        // Multiline prompt on rows 1-3.
        pin_rac(s.pin(pt_screen(0, 1)).unwrap()).0.set_semantic_prompt(SemanticPrompt::Prompt);
        pin_rac(s.pin(pt_screen(0, 2)).unwrap())
            .0
            .set_semantic_prompt(SemanticPrompt::PromptContinuation);
        pin_rac(s.pin(pt_screen(0, 3)).unwrap())
            .0
            .set_semantic_prompt(SemanticPrompt::PromptContinuation);

        // Next prompt after command output.
        pin_rac(s.pin(pt_screen(0, 6)).unwrap()).0.set_semantic_prompt(SemanticPrompt::Prompt);

        // Starting at the first prompt line should jump to the next prompt,
        // not to continuation lines.
        s.scroll(Scroll::Row(1));
        s.scroll(Scroll::DeltaPrompt(1));
        assert!(s.viewport == Viewport::Pin);
        assert_eq!(
            pt_screen(0, 6),
            s.point_from_pin(Tag::Screen, s.pin(pt_viewport(0, 0)).unwrap()).unwrap()
        );

        // Starting in the middle of continuation lines should also jump to
        // the next prompt.
        s.scroll(Scroll::Row(2));
        s.scroll(Scroll::DeltaPrompt(1));
        assert!(s.viewport == Viewport::Pin);
        assert_eq!(
            pt_screen(0, 6),
            s.point_from_pin(Tag::Screen, s.pin(pt_viewport(0, 0)).unwrap()).unwrap()
        );
    }

    #[test]
    fn page_list_grow_fit_in_capacity() {
        let mut s = init(80, 24);

        // So we know we're using capacity to grow
        let last = pg(last(&s));
        assert!(last.size.rows < last.capacity.rows);

        // Grow
        assert!(s.grow().unwrap().is_none());
        assert_eq!(pt_screen(0, 1), s.get_cell(pt_active(0, 0)).unwrap().screen_point());
    }

    #[test]
    fn page_list_grow_allocate() {
        let mut s = init(80, 24);

        // Grow to capacity
        let last_node = last(&s);
        let last = pg(last_node);
        for _ in 0..(last.capacity.rows - last.size.rows) {
            assert!(s.grow().unwrap().is_none());
        }

        // Grow, should allocate
        let new = s.grow().unwrap().unwrap();
        assert!(s.pages.last.unwrap() == new);
        assert!(next(last_node) == new);
        {
            let cell = s.get_cell(pt_active(0, s.rows as u32 - 1)).unwrap();
            assert!(cell.node == new);
            assert_eq!(pt_screen(0, last.capacity.rows as u32), cell.screen_point());
        }
    }

    #[test]
    fn page_list_cell_screen_point_supports_long_scrollback() {
        // A modest number of full-size page nodes is enough to exceed the u16
        // row range without allocating any page backing memory. screenPoint only
        // reads the linked metadata while calculating the absolute coordinate.
        let page_count = 307;
        let rows_per_page = STD_CAPACITY.rows;
        let (_page, mut nodes) = fake_nodes(page_count, rows_per_page);

        let expected_y: u32 = (page_count as u32 - 1) * rows_per_page as u32;
        assert!(expected_y > CellCountInt::MAX as u32);

        let cell = Cell {
            node: NonNull::new(unsafe { nodes.as_mut_ptr().add(page_count - 1) }).unwrap(),
            row: core::ptr::null_mut(),
            cell: core::ptr::null_mut(),
            row_idx: 0,
            col_idx: 0,
        };
        assert_eq!(pt_screen(0, expected_y), cell.screen_point());
    }

    #[test]
    fn page_list_set_max_bytes_prunes_immediately_and_can_be_raised() {
        let cols: CellCountInt = 80;
        let page_rows: usize = initial_capacity(cols).rows as usize;

        let mut s = init_opts(Options {
            cols,
            rows: 1,
            max_size: None,
            ..Default::default()
        });

        // Build four complete pages of history followed by the active row.
        s.grow_rows(4 * page_rows).unwrap();
        assert_eq!(5, s.total_pages());

        let removed = first(&s);
        let retained = prev(last(&s));
        let removed_pin = s.track_pin(Pin::new(removed)).unwrap();
        let retained_pin = s.track_pin(Pin::new(retained)).unwrap();

        s.scroll(Scroll::Pin(*tp(retained_pin)));
        assert_eq!(3 * page_rows, s.scrollbar().offset);

        // The active-area minimum is two pages. Lowering below that immediately
        // removes all older complete historical pages.
        s.set_max_bytes(Some(PagePool::item_size()));
        assert_eq!(PagePool::item_size(), s.limits.bytes.explicit);
        assert_eq!(2 * PagePool::item_size(), s.limits.max(LimitKey::Bytes));
        assert_eq!(s.limits.max(LimitKey::Bytes), s.page_size);
        assert_eq!(2, s.total_pages());
        assert_eq!(page_rows, s.total_rows - s.rows as usize);
        assert_eq!(retained, first(&s));
        assert_eq!(retained, tp(removed_pin).node);
        assert!(tp(removed_pin).garbage);
        assert_eq!(retained, tp(retained_pin).node);
        assert!(!tp(retained_pin).garbage);
        assert_eq!(0, s.scrollbar().offset);

        // Raising the limit doesn't allocate or otherwise change retained data,
        // but subsequent growth can exceed the previous effective limit.
        let limited_size = s.page_size;
        let limited_rows = s.total_rows;
        s.set_max_bytes(Some(8 * PagePool::item_size()));
        assert_eq!(limited_size, s.page_size);
        assert_eq!(limited_rows, s.total_rows);
        s.grow_rows(2 * page_rows).unwrap();
        assert!(s.page_size > limited_size);

        // Null restores unlimited growth and likewise preserves current data.
        let raised_size = s.page_size;
        let raised_rows = s.total_rows;
        s.set_max_bytes(None);
        assert_eq!(usize::MAX, s.limits.bytes.explicit);
        assert_eq!(raised_size, s.page_size);
        assert_eq!(raised_rows, s.total_rows);
        s.grow_rows(5 * page_rows).unwrap();
        assert!(s.page_size > 8 * PagePool::item_size());

        s.untrack_pin(retained_pin);
        s.untrack_pin(removed_pin);
    }

    #[test]
    fn page_list_set_max_bytes_zero_preserves_active_boundary() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 1,
            max_size: None,
            ..Default::default()
        });

        // Make the sole page larger than the effective zero-byte limit. Its first
        // row will be history, but the same indivisible page also contains active.
        while s.page_size <= s.limits.bytes.min {
            s.increase_capacity(first(&s), Some(IncreaseCapacity::GraphemeBytes))
                .unwrap();
        }
        s.grow().unwrap();
        assert_eq!(1, s.total_pages());
        assert_eq!(first(&s), s.get_top_left(Tag::Active).node);
        assert!(s.get_top_left(Tag::Active).y > 0);

        s.scroll(Scroll::Top);
        assert!(s.viewport == Viewport::Top);

        s.set_max_bytes(Some(0));
        assert_eq!(0, s.limits.bytes.explicit);
        assert!(s.page_size > s.limits.max(LimitKey::Bytes));
        assert_eq!(1, s.total_pages());
        assert!(s.viewport == Viewport::Active);
        assert_eq!(sb(s.rows as usize, 0, s.rows as usize), s.scrollbar());

        // No-scrollback mode cannot be moved back into the retained boundary row.
        s.scroll(Scroll::Top);
        assert!(s.viewport == Viewport::Active);
    }

    #[test]
    fn page_list_set_max_lines_prunes_immediately_and_can_be_raised() {
        let cols: CellCountInt = 80;
        let page_rows: usize = initial_capacity(cols).rows as usize;
        let lowered_lines = page_rows + page_rows / 2;

        let mut s = init_opts(Options {
            cols,
            rows: 1,
            max_size: None,
            max_lines: None,
        });

        s.grow_rows(4 * page_rows).unwrap();
        assert_eq!(5, s.total_pages());

        let removed = first(&s);
        let retained = prev(last(&s));
        let removed_pin = s.track_pin(Pin::new(removed)).unwrap();
        let retained_pin = s.track_pin(Pin::new(retained)).unwrap();

        s.scroll(Scroll::Pin(*tp(retained_pin)));
        assert_eq!(3 * page_rows, s.scrollbar().offset);

        // Whole-page enforcement undershoots a non-page-aligned line limit.
        s.set_max_lines(Some(lowered_lines));
        assert_eq!(lowered_lines, s.limits.lines.explicit);
        assert_eq!(lowered_lines, s.limits.max(LimitKey::Lines));
        assert_eq!(page_rows, s.total_rows - s.rows as usize);
        assert_eq!(2, s.total_pages());
        assert_eq!(retained, first(&s));
        assert_eq!(retained, tp(removed_pin).node);
        assert!(tp(removed_pin).garbage);
        assert_eq!(retained, tp(retained_pin).node);
        assert!(!tp(retained_pin).garbage);
        assert_eq!(0, s.scrollbar().offset);

        let limited_size = s.page_size;
        let limited_rows = s.total_rows;
        s.set_max_lines(Some(4 * page_rows));
        assert_eq!(limited_size, s.page_size);
        assert_eq!(limited_rows, s.total_rows);
        s.grow_rows(2 * page_rows).unwrap();
        assert!(s.total_rows - s.rows as usize > lowered_lines);

        let raised_size = s.page_size;
        let raised_rows = s.total_rows;
        s.set_max_lines(None);
        assert_eq!(usize::MAX, s.limits.lines.explicit);
        assert_eq!(raised_size, s.page_size);
        assert_eq!(raised_rows, s.total_rows);
        s.grow_rows(3 * page_rows).unwrap();
        assert!(s.total_rows - s.rows as usize > 4 * page_rows);

        s.untrack_pin(retained_pin);
        s.untrack_pin(removed_pin);
    }

    #[test]
    fn page_list_set_max_limits_remain_independent() {
        let cols: CellCountInt = 80;
        let page_rows: usize = initial_capacity(cols).rows as usize;
        let byte_limit = 3 * PagePool::item_size();
        let line_limit = page_rows / 2;

        let mut s = init_opts(Options {
            cols,
            rows: 1,
            max_size: None,
            max_lines: None,
        });

        s.grow_rows(4 * page_rows).unwrap();

        // The byte setter leaves the line limit unlimited.
        s.set_max_bytes(Some(byte_limit));
        assert_eq!(byte_limit, s.limits.bytes.explicit);
        assert_eq!(usize::MAX, s.limits.lines.explicit);
        assert_eq!(3, s.total_pages());
        assert_eq!(2 * page_rows, s.total_rows - s.rows as usize);

        // The smaller runtime line limit prunes one more complete page without
        // changing the configured byte limit. Its effective value is raised to
        // the existing one-page minimum.
        s.set_max_lines(Some(line_limit));
        assert_eq!(byte_limit, s.limits.bytes.explicit);
        assert_eq!(line_limit, s.limits.lines.explicit);
        assert_eq!(page_rows, s.limits.max(LimitKey::Lines));
        assert_eq!(2, s.total_pages());
        assert_eq!(page_rows, s.total_rows - s.rows as usize);

        // Removing only the line limit leaves byte enforcement in effect.
        s.set_max_lines(None);
        s.grow_rows(3 * page_rows).unwrap();
        assert_eq!(byte_limit, s.page_size);
        assert_eq!(3, s.total_pages());
        assert!(s.total_rows - s.rows as usize > page_rows);
    }

    #[test]
    fn page_list_max_lines_uses_one_page_minimum() {
        let cols: CellCountInt = 80;
        let page_rows: usize = initial_capacity(cols).rows as usize;

        let mut s = init_opts(Options {
            cols,
            rows: 1,
            max_lines: Some(page_rows / 2),
            ..Default::default()
        });

        assert_eq!(page_rows, s.limits.max(LimitKey::Lines));

        // The requested limit is below one page, so a complete page of history
        // remains valid.
        s.grow_rows(page_rows).unwrap();
        assert_eq!(page_rows, s.total_rows - s.rows as usize);
        assert_eq!(2, s.total_pages());

        let first = first(&s);
        let old_page_size = s.page_size;

        // One more row puts us over the effective limit. The now-complete
        // historical page is removed rather than partially trimmed.
        s.grow().unwrap();
        assert_eq!(1, s.total_rows - s.rows as usize);
        assert_eq!(1, s.total_pages());
        assert!(s.pages.first.unwrap() != first);
        assert_eq!(old_page_size - PagePool::item_size(), s.page_size);
    }

    #[test]
    fn page_list_max_lines_does_not_round_larger_limits() {
        let cols: CellCountInt = 80;
        let page_rows: usize = initial_capacity(cols).rows as usize;
        let max_lines = page_rows + page_rows / 2;

        let mut s = init_opts(Options {
            cols,
            rows: 1,
            max_lines: Some(max_lines),
            ..Default::default()
        });

        assert_eq!(max_lines, s.limits.max(LimitKey::Lines));
        s.grow_rows(max_lines).unwrap();
        assert_eq!(max_lines, s.total_rows - s.rows as usize);

        let first = first(&s);
        let retained = next(first);
        let removed_pin = s.track_pin(Pin::new(first)).unwrap();
        let retained_pin = s.track_pin(Pin::new(retained)).unwrap();

        s.scroll(Scroll::Pin(*tp(retained_pin)));
        assert_eq!(page_rows, s.scrollbar().offset);

        let old_page_size = s.page_size;
        s.grow().unwrap();

        // Whole-page pruning undershoots the requested limit without rounding it.
        assert_eq!(max_lines + 1 - page_rows, s.total_rows - s.rows as usize);
        assert_eq!(retained, s.pages.first.unwrap());
        assert_eq!(retained, tp(removed_pin).node);
        assert!(tp(removed_pin).garbage);
        assert_eq!(retained, tp(retained_pin).node);
        assert!(!tp(retained_pin).garbage);
        assert_eq!(0, s.scrollbar().offset);
        assert_eq!(old_page_size - PagePool::item_size(), s.page_size);

        s.untrack_pin(retained_pin);
        s.untrack_pin(removed_pin);
    }

    #[test]
    fn page_list_max_lines_and_max_size_enforce_the_smaller_limit() {
        let cols: CellCountInt = 80;
        let page_rows: usize = initial_capacity(cols).rows as usize;

        // A line limit of one page keeps the logical allocation below a much
        // larger byte limit.
        {
            let mut s = init_opts(Options {
                cols,
                rows: 1,
                max_size: Some(8 * PagePool::item_size()),
                max_lines: Some(page_rows),
            });

            s.grow_rows(4 * page_rows).unwrap();
            assert!(s.total_rows - s.rows as usize <= s.limits.max(LimitKey::Lines));
            assert!(s.total_pages() <= 2);
            assert!(s.page_size < s.limits.max(LimitKey::Bytes));
        }

        // A two-page byte limit prunes before the larger line limit is reached.
        {
            let mut s = init_opts(Options {
                cols,
                rows: 1,
                max_size: Some(PagePool::item_size()),
                max_lines: Some(4 * page_rows),
            });

            s.grow_rows(2 * page_rows).unwrap();
            assert!(s.total_rows - (s.rows as usize) < s.limits.max(LimitKey::Lines));
            assert_eq!(s.limits.max(LimitKey::Bytes), s.page_size);
        }
    }

    #[test]
    fn page_list_max_lines_applies_to_resize_and_clone() {
        let cols: CellCountInt = 80;
        let page_rows: usize = initial_capacity(cols).rows as usize;

        let mut s = init_opts(Options {
            cols,
            rows: 2,
            max_lines: Some(page_rows),
            ..Default::default()
        });

        s.grow_rows(page_rows).unwrap();
        assert_eq!(page_rows, s.total_rows - s.rows as usize);

        // Prevent row shrinking from trimming the trailing active row instead of
        // turning it into history.
        let cell = s.get_cell(pt_active(0, 1)).unwrap();
        unsafe { *cell.cell = cp('A') };

        s.resize(Resize {
            rows: Some(1),
            reflow: false,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(1, s.total_rows - s.rows as usize);

        let new_cols: CellCountInt = cols + 1;
        s.resize(Resize {
            cols: Some(new_cols),
            reflow: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(Limits::min_max_lines(new_cols), s.limits.lines.min);

        // Exercise the same active-row shrink through the reflow path. Reflow
        // completes before the newly historical complete page is pruned.
        {
            let mut reflowed = init_opts(Options {
                cols,
                rows: 2,
                max_lines: Some(page_rows),
                ..Default::default()
            });

            reflowed.grow_rows(page_rows).unwrap();
            let active_cell = reflowed.get_cell(pt_active(0, 1)).unwrap();
            unsafe { *active_cell.cell = cp('A') };

            reflowed
                .resize(Resize {
                    cols: Some(new_cols),
                    rows: Some(1),
                    reflow: true,
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(Limits::min_max_lines(new_cols), reflowed.limits.lines.min);
            assert!(
                reflowed.total_rows - reflowed.rows as usize <= reflowed.limits.max(LimitKey::Lines)
                    || reflowed.pages.first.unwrap() == reflowed.get_top_left(Tag::Active).node
            );
        }

        let mut cloned = s.clone(clone_opts(pt_screen(0, 0), None)).unwrap();

        assert_eq!(s.limits, cloned.limits);

        let max = cloned.limits.max(LimitKey::Lines);
        cloned.grow_rows(2 * max).unwrap();
        assert!(
            cloned.total_rows - cloned.rows as usize <= cloned.limits.max(LimitKey::Lines)
                || cloned.pages.first.unwrap() == cloned.get_top_left(Tag::Active).node
        );
    }

    #[test]
    fn page_list_grow_prune_scrollback() {
        // Use std_size to limit scrollback so pruning is triggered.
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(std_size()),
            ..Default::default()
        });

        // Grow to capacity
        let page1_node = last(&s);
        let page1 = pg(page1_node);
        for _ in 0..(page1.capacity.rows - page1.size.rows) {
            assert!(s.grow().unwrap().is_none());
        }

        // Grow and allocate one more page. Then fill that page up.
        let page2_node = s.grow().unwrap().unwrap();
        let page2 = pg(page2_node);
        for _ in 0..(page2.capacity.rows - page2.size.rows) {
            assert!(s.grow().unwrap().is_none());
        }

        // Get our page size
        let old_page_size = s.page_size;

        // Create a tracked pin in the first page
        let p = s.track_pin(s.pin(pt_screen(0, 0)).unwrap()).unwrap();
        assert!(tp(p).node == first(&s));

        // Scroll back to create a pinned viewport (not active)
        let pin_y = (page1.capacity.rows / 2) as usize;
        s.scroll(Scroll::Pin(s.pin(pt_screen(0, pin_y as u32)).unwrap()));
        assert!(s.viewport == Viewport::Pin);

        // Get the scrollbar state to populate the cache
        let scrollbar_before = s.scrollbar();
        assert_eq!(pin_y, scrollbar_before.offset);

        // Next should create a new page, but it should reuse our first
        // page since we're at max size.
        let new = s.grow().unwrap().unwrap();
        assert!(last(&s) == new);
        assert_eq!(s.page_size, old_page_size);

        // Our first should now be page2 and our last should be page1
        assert_eq!(page2_node, first(&s));
        assert_eq!(page1_node, last(&s));

        // Our tracked pin should point to the top-left of the first page
        assert!(tp(p).node == first(&s));
        assert!(tp(p).x == 0);
        assert!(tp(p).y == 0);
        assert!(tp(p).garbage);

        // Verify the viewport offset cache was invalidated. After pruning,
        // the offset should have changed because we removed rows from
        // the beginning.
        {
            let scrollbar_after = s.scrollbar();
            let rows_pruned = pg(page1_node).capacity.rows as usize;
            let expected_offset = if pin_y >= rows_pruned { pin_y - rows_pruned } else { 0 };
            assert_eq!(expected_offset, scrollbar_after.offset);
        }

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_grow_prune_scrollback_with_viewport_pin_not_in_pruned_page() {
        // Use std_size to limit scrollback so pruning is triggered.
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(std_size()),
            ..Default::default()
        });

        // Grow to capacity of first page
        let page1_node = last(&s);
        let page1 = pg(page1_node);
        for _ in 0..(page1.capacity.rows - page1.size.rows) {
            assert!(s.grow().unwrap().is_none());
        }

        // Grow and allocate second page, then fill it up
        let page2_node = s.grow().unwrap().unwrap();
        let page2 = pg(page2_node);
        for _ in 0..(page2.capacity.rows - page2.size.rows) {
            assert!(s.grow().unwrap().is_none());
        }

        // Get our page size
        let old_page_size = s.page_size;

        // Scroll back to create a pinned viewport in page2 (NOT page1)
        // This is the key difference from the previous test - the viewport
        // pin is NOT in the page that will be pruned.
        let pin_y = page1.capacity.rows as usize + 5;
        s.scroll(Scroll::Pin(s.pin(pt_screen(0, pin_y as u32)).unwrap()));
        assert!(s.viewport == Viewport::Pin);
        assert!(vp(&s).node == page2_node);

        // Get the scrollbar state to populate the cache
        let scrollbar_before = s.scrollbar();
        assert_eq!(pin_y, scrollbar_before.offset);

        // Next grow will trigger pruning of the first page.
        // The viewport_pin.node is page2, not page1, so it won't be moved
        // by the pin update loop, but the cached offset still needs to be
        // invalidated because rows were removed from the beginning.
        let new = s.grow().unwrap().unwrap();
        assert!(last(&s) == new);
        assert_eq!(s.page_size, old_page_size);

        // Our first should now be page2 (page1 was pruned)
        assert_eq!(page2_node, first(&s));

        // The viewport pin should still be on page2, unchanged
        assert!(vp(&s).node == page2_node);

        // Verify the viewport offset cache was invalidated/updated.
        // After pruning, the offset should have decreased by the number
        // of rows that were pruned.
        let scrollbar_after = s.scrollbar();
        let rows_pruned = pg(page1_node).capacity.rows as usize;
        let expected_offset = pin_y - rows_pruned;
        assert_eq!(expected_offset, scrollbar_after.offset);
    }

    /// Grow `count` rows (Zig: `for (0..count) |_| if (try s.grow()) |new_page| cur_page = new_page;`).
    fn grow_n(s: &mut PageList, count: usize) {
        for _ in 0..count {
            s.grow().unwrap();
        }
    }

    #[test]
    fn page_list_erase_rows_invalidates_viewport_offset_cache() {
        let mut s = init(80, 24);

        // Grow so we take up several pages worth of history
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_n(&mut s, cap_rows * 3);

        // Scroll back to create a pinned viewport somewhere in the middle
        // of the scrollback
        let pin_y = cap_rows;
        s.scroll(Scroll::Pin(s.pin(pt_screen(0, pin_y as u32)).unwrap()));
        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, pin_y, s.rows as usize), s.scrollbar());

        // Erase some history rows BEFORE the viewport pin.
        // This removes rows from before our pin, which changes its absolute
        // offset from the top, but the cache is not invalidated.
        let rows_to_erase = cap_rows / 2;
        s.erase_history(Some(pt_history(0, rows_to_erase as u32 - 1)));

        assert_eq!(
            sb(s.total_rows, pin_y - rows_to_erase, s.rows as usize),
            s.scrollbar()
        );
    }

    #[test]
    fn page_list_erase_row_invalidates_viewport_offset_cache() {
        let mut s = init(80, 24);

        // Grow so we take up several pages worth of history
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_n(&mut s, cap_rows * 3);

        // Scroll back to create a pinned viewport somewhere in the middle
        // of the scrollback
        let pin_y = cap_rows;
        s.scroll(Scroll::Pin(s.pin(pt_screen(0, pin_y as u32)).unwrap()));
        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, pin_y, s.rows as usize), s.scrollbar());

        // Erase a single row from the history BEFORE the viewport pin.
        // This removes one row from before our pin, which changes its absolute
        // offset from the top by 1, but the cache is not invalidated.
        s.erase_row(pt_history(0, 0));

        assert_eq!(sb(s.total_rows, pin_y - 1, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_erase_row_bounded_invalidates_viewport_offset_cache() {
        let mut s = init(80, 24);

        // Grow so we take up several pages worth of history
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_n(&mut s, cap_rows * 3);

        // Scroll back to create a pinned viewport somewhere in the middle
        // of the scrollback
        let pin_y: usize = 4;
        s.scroll(Scroll::Pin(s.pin(pt_screen(0, pin_y as u32)).unwrap()));
        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, pin_y, s.rows as usize), s.scrollbar());

        // Erase a row from the history BEFORE the viewport pin with a bounded
        // shift. This removes one row from before our pin, which changes its
        // absolute offset from the top by 1, but the cache is not invalidated.
        s.erase_row_bounded(pt_history(0, 0), 10);

        // Verify the scrollbar reflects the change (offset decreased by 1)
        assert_eq!(sb(s.total_rows, pin_y - 1, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_row_erasure_renews_affected_page_generations() {
        let mut s = init(80, 24);
        while s.total_pages() < 2 {
            s.grow().unwrap();
        }

        let first = first(&s);
        let second = next(first);
        let mut first_serial = n(first).serial;
        let mut second_serial = n(second).serial;
        let mut activity = s.page_compression.activity_serial;

        s.erase_row(pt_history(0, 0));
        assert!(!s.node_is_valid(first, first_serial));
        assert!(!s.node_is_valid(second, second_serial));
        assert!(activity != s.page_compression.activity_serial);

        first_serial = n(first).serial;
        second_serial = n(second).serial;
        activity = s.page_compression.activity_serial;
        s.erase_row_bounded(pt_history(0, 0), n(first).rows() as usize + 1);
        assert!(!s.node_is_valid(first, first_serial));
        assert!(!s.node_is_valid(second, second_serial));
        assert!(activity != s.page_compression.activity_serial);
    }

    #[test]
    fn page_list_trailing_row_truncation_renews_page_generation() {
        let mut s = init(80, 24);

        let node = last(&s);
        let old_serial = n(node).serial;
        let trimmed = s.trim_trailing_blank_rows(1);
        s.total_rows -= trimmed as usize;
        assert_eq!(1, trimmed);
        assert!(!s.node_is_valid(node, old_serial));

        s.grow().unwrap();
        s.expect_live_page_serials_valid_for_test();
    }

    #[test]
    fn page_list_erase_row_bounded_multi_page_invalidates_viewport_offset_cache() {
        let mut s = init(80, 24);

        // Grow so we take up several pages worth of history
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_n(&mut s, cap_rows * 3);

        // Scroll back to create a pinned viewport somewhere in the middle
        // of the scrollback, after the first page
        let pin_y = cap_rows + 1;
        s.scroll(Scroll::Pin(s.pin(pt_screen(0, pin_y as u32)).unwrap()));
        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, pin_y, s.rows as usize), s.scrollbar());

        // Erase a row from the beginning of history with a limit that spans
        // across multiple pages. This ensures we hit the code path where
        // eraseRowBounded finds the limit boundary in a subsequent page.
        let limit = cap_rows + 10;
        s.erase_row_bounded(pt_history(0, 0), limit);

        // Verify the scrollbar reflects the change (offset decreased by 1)
        assert_eq!(sb(s.total_rows, pin_y - 1, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_erase_row_bounded_full_page_shift_invalidates_viewport_offset_cache() {
        let mut s = init(80, 24);

        // Grow so we take up several pages worth of history
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_n(&mut s, cap_rows * 4);

        // Scroll back to create a pinned viewport somewhere well beyond
        // the first two pages
        let pin_y = 5;
        s.scroll(Scroll::Pin(s.pin(pt_screen(0, pin_y as u32)).unwrap()));
        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, pin_y, s.rows as usize), s.scrollbar());

        // Erase a row from the beginning of history with a limit that is
        // larger than multiple full pages. This ensures we hit the code path
        // where eraseRowBounded continues looping through entire pages,
        // rotating all rows in each page until it reaches the limit or
        // runs out of pages.
        let limit = cap_rows * 2 + 10;
        s.erase_row_bounded(pt_history(0, 0), limit);

        // Verify the scrollbar reflects the change (offset decreased by 1)
        assert_eq!(sb(s.total_rows, pin_y - 1, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_erase_row_bounded_exhausts_pages_invalidates_viewport_offset_cache() {
        let mut s = init(80, 24);

        // Grow so we take up several pages worth of history
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_n(&mut s, cap_rows * 3);

        // Our total rows should include history
        let total_rows_before = s.total_rows();
        assert!(total_rows_before > s.rows as usize);

        // Scroll back to create a pinned viewport somewhere in the history,
        // well after the erase will complete
        let pin_y = cap_rows * 2 + 10;
        s.scroll(Scroll::Pin(s.pin(pt_screen(0, pin_y as u32)).unwrap()));
        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, pin_y, s.rows as usize), s.scrollbar());

        // Erase a row from the beginning of history with a limit that is
        // LARGER than all remaining pages combined. This ensures we exhaust
        // all pages in the while loop and reach the cleanup code after the loop.
        let limit = total_rows_before * 2;
        s.erase_row_bounded(pt_history(0, 0), limit);

        // Verify the scrollbar reflects the change (offset decreased by 1)
        assert_eq!(sb(s.total_rows, pin_y - 1, s.rows as usize), s.scrollbar());
    }

    /// Fill every active cell with codepoint `x + offset`.
    fn fill_x(s: &PageList, offset: u32) {
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(s));
        for y in 0..s.rows as usize {
            for x in 0..s.cols as usize {
                *cell_at(page, x, y) = cpu(x as u32 + offset);
            }
        }
    }

    fn check_x(s: &PageList, offset: u32) {
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(s));
        for y in 0..s.rows as usize {
            for x in 0..s.cols as usize {
                assert_eq!(x as u32 + offset, cell_at(page, x, y).content_codepoint());
            }
        }
    }

    #[test]
    fn page_list_increase_capacity_to_increase_styles() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });

        let original_styles_cap = n(first(&s)).capacity().styles;

        // Write all our data so we can assert its the same after
        fill_x(&s, 0);

        // Increase our styles
        s.increase_capacity(first(&s), Some(IncreaseCapacity::Styles)).unwrap();

        // Verify capacity doubled
        assert!(s.pages.first == s.pages.last);
        assert_eq!(original_styles_cap * 2, pg(first(&s)).capacity.styles);

        // Verify data preserved
        check_x(&s, 0);
    }

    #[test]
    fn page_list_increase_capacity_styles_projects_capacity_from_page_density() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });

        let original_cap = n(first(&s)).capacity().styles;

        // Write styled cells so the page has a measurable per-row style
        // density (unlike the plain doubling test above, which grows a
        // page with no styles in use).
        let mut bold = stylepkg::Style::default();
        bold.flags.bold = true;
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            for y in 0..s.rows as usize {
                for x in 0..s.cols as usize {
                    let (row, cell) = rac(page, x, y);
                    let style_id = unsafe { page.styles.add(page.memory, bold) }.unwrap();
                    row.set_styled(true);
                    let mut c = cpu(x as u32 + 1);
                    c.set_style_id(style_id);
                    *cell = c;
                }
            }
        }

        s.increase_capacity(first(&s), Some(IncreaseCapacity::Styles)).unwrap();

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            // The page uses only two active rows out of thousands of rows
            // of capacity, so the projected full-page need saturates the
            // 32x-per-event growth bound instead of merely doubling.
            assert_eq!(original_cap * 32, page.capacity.styles);

            // All cell content and styles are preserved by the growth.
            for y in 0..s.rows as usize {
                for x in 0..s.cols as usize {
                    let cell = cell_at(page, x, y);
                    assert_eq!(x as u32 + 1, cell.content_codepoint());
                    assert!(cell.style_id() != stylepkg::DEFAULT_ID);
                    assert!(bold.eql(unsafe { &*page.styles.get(page.memory, cell.style_id()) }));
                }
            }
        }
    }

    #[test]
    fn page_list_increase_capacity_to_increase_graphemes() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });

        let original_cap = n(first(&s)).capacity().grapheme_bytes;

        fill_x(&s, 0);

        s.increase_capacity(first(&s), Some(IncreaseCapacity::GraphemeBytes))
            .unwrap();

        assert!(s.pages.first == s.pages.last);
        assert_eq!(original_cap * 2, pg(first(&s)).capacity.grapheme_bytes);
        check_x(&s, 0);
    }

    #[test]
    fn page_list_increase_capacity_graphemes_projects_capacity_from_page_density() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });

        let original_cap = n(first(&s)).capacity().grapheme_bytes;

        // Write cells with grapheme data so the page has a measurable
        // per-row grapheme density (unlike the plain doubling test above,
        // which grows a page with no grapheme usage).
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            for y in 0..s.rows as usize {
                for x in 0..s.cols as usize {
                    let (row, cell) = rac(page, x, y);
                    *cell = cpu(x as u32 + 1);
                    page.append_grapheme(row, cell, 0x0301).unwrap();
                    page.append_grapheme(row, cell, 0x0302).unwrap();
                }
            }
        }

        s.increase_capacity(first(&s), Some(IncreaseCapacity::GraphemeBytes))
            .unwrap();

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            // The page uses only two active rows out of thousands of rows
            // of capacity, so the projected full-page need saturates the
            // 32x-per-event growth bound instead of merely doubling.
            assert_eq!(original_cap * 32, page.capacity.grapheme_bytes);

            // All cell and grapheme content is preserved by the growth.
            assert_eq!(s.rows as usize * s.cols as usize, page.grapheme_count());
            for y in 0..s.rows as usize {
                for x in 0..s.cols as usize {
                    let cell = cell_at(page, x, y);
                    assert_eq!(x as u32 + 1, cell.content_codepoint());
                    let cps = page.lookup_grapheme(cell).unwrap();
                    assert_eq!(2, cps.len());
                    assert_eq!(0x0301, cps[0]);
                    assert_eq!(0x0302, cps[1]);
                }
            }
        }
    }

    #[test]
    fn page_list_increase_capacity_to_increase_hyperlinks() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });

        let original_cap = n(first(&s)).capacity().hyperlink_bytes;

        fill_x(&s, 0);

        s.increase_capacity(first(&s), Some(IncreaseCapacity::HyperlinkBytes))
            .unwrap();

        assert!(s.pages.first == s.pages.last);
        assert_eq!(original_cap * 2, pg(first(&s)).capacity.hyperlink_bytes);
        check_x(&s, 0);
    }

    #[test]
    fn page_list_increase_capacity_to_increase_string_bytes() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });

        let original_cap = n(first(&s)).capacity().string_bytes;

        fill_x(&s, 0);

        s.increase_capacity(first(&s), Some(IncreaseCapacity::StringBytes))
            .unwrap();

        assert!(s.pages.first == s.pages.last);
        assert_eq!(original_cap * 2, pg(first(&s)).capacity.string_bytes);
        check_x(&s, 0);
    }

    #[test]
    fn page_list_increase_capacity_tracked_pins() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });

        // Create a tracked pin on the first page
        let tracked = s.track_pin(s.pin(pt_active(1, 1)).unwrap()).unwrap();

        let old_node = first(&s);
        assert_eq!(old_node, tp(tracked).node);

        // Increase capacity
        let new_node = s.increase_capacity(first(&s), Some(IncreaseCapacity::Styles)).unwrap();

        // Pin should now point to the new node
        assert_eq!(new_node, tp(tracked).node);
        assert_eq!(1, tp(tracked).x);
        assert_eq!(1, tp(tracked).y);

        s.untrack_pin(tracked);
    }

    #[test]
    fn page_list_increase_capacity_returns_out_of_space_at_max_capacity() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });

        // Keep increasing styles capacity until we get OutOfSpace
        let max_styles = size::StyleCountInt::MAX;
        loop {
            if let Err(err) = s.increase_capacity(first(&s), Some(IncreaseCapacity::Styles)) {
                // Before OutOfSpace, we should have reached maxInt
                assert_eq!(IncreaseCapacityError::OutOfSpace, err);
                assert_eq!(max_styles, n(first(&s)).capacity().styles);
                break;
            }
        }
    }

    #[test]
    fn page_list_increase_capacity_after_col_shrink() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });

        // Shrink columns
        s.resize(Resize {
            cols: Some(5),
            reflow: false,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(5, s.cols);

        {
            let page = pg(first(&s));
            assert_eq!(5, page.size.cols);
            assert!(page.capacity.cols >= 10);
        }

        // Increase capacity
        s.increase_capacity(first(&s), Some(IncreaseCapacity::Styles)).unwrap();

        {
            let page = pg(first(&s));
            // size.cols should still be 5, not reverted to capacity.cols
            assert_eq!(5, page.size.cols);
            assert_eq!(5, s.cols);
        }
    }

    #[test]
    fn page_list_increase_capacity_multi_page() {
        let mut s = init(80, 24);

        // Grow to create a second page
        let page1_node = last(&s);
        pg(page1_node).pause_integrity_checks(true);
        for _ in 0..(n(page1_node).capacity().rows - n(page1_node).rows()) {
            assert!(s.grow().unwrap().is_none());
        }
        pg(page1_node).pause_integrity_checks(false);
        assert!(s.grow().unwrap().is_some());

        // Now we have two pages
        assert!(s.pages.first != s.pages.last);
        let page2_node = last(&s);

        let page1_styles_cap = n(first(&s)).capacity().styles;
        let page2_styles_cap = n(page2_node).capacity().styles;

        // Increase capacity on the first page only
        s.increase_capacity(first(&s), Some(IncreaseCapacity::Styles)).unwrap();

        // First page capacity should be doubled
        assert_eq!(page1_styles_cap * 2, n(first(&s)).capacity().styles);

        // Second page should be unchanged
        assert_eq!(page2_styles_cap, n(last(&s)).capacity().styles);
    }

    #[test]
    fn page_list_increase_capacity_preserves_dirty_flag() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 4,
            max_size: Some(0),
            ..Default::default()
        });

        // Set page dirty flag and mark some rows as dirty
        let page = pg(first(&s));
        page.dirty = true;

        row_at(page, 0).set_dirty(true);
        row_at(page, 1).set_dirty(false);
        row_at(page, 2).set_dirty(true);
        row_at(page, 3).set_dirty(false);

        // Increase capacity
        let new_node = s.increase_capacity(first(&s), Some(IncreaseCapacity::Styles)).unwrap();

        // The page dirty flag should be preserved
        assert!(pg(new_node).dirty);

        // Row dirty flags should be preserved
        let np = pg(new_node);
        assert!(row_at(np, 0).dirty());
        assert!(!row_at(np, 1).dirty());
        assert!(row_at(np, 2).dirty());
        assert!(!row_at(np, 3).dirty());
    }

    /// Grow the last page to capacity (integrity paused) then allocate one more page.
    fn fill_then_grow_page(s: &mut PageList) {
        let page1_node = last(s);
        let page1 = pg(page1_node);
        pg(page1_node).pause_integrity_checks(true);
        for _ in 0..(page1.capacity.rows - page1.size.rows) {
            assert!(s.grow().unwrap().is_none());
        }
        pg(page1_node).pause_integrity_checks(false);
        assert!(s.grow().unwrap().is_some());
    }

    #[test]
    fn page_list_page_iterator_single_page() {
        let s = init(80, 24);

        // The viewport should be within a single page
        assert!(n(first(&s)).next.is_none());

        // Iterate the active area
        let mut it = s.page_iterator(Direction::RightDown, pt_active(0, 0), None);
        {
            let chunk = it.next().unwrap();
            assert!(chunk.node == first(&s));
            assert_eq!(0, chunk.start);
            assert_eq!(s.rows, chunk.end);
        }

        // Should only have one chunk
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_page_iterator_two_pages() {
        let mut s = init(80, 24);

        // Grow to capacity
        fill_then_grow_page(&mut s);

        // Iterate the active area
        let mut it = s.page_iterator(Direction::RightDown, pt_active(0, 0), None);
        {
            let chunk = it.next().unwrap();
            assert!(chunk.node == first(&s));
            let start = n(chunk.node).rows() - s.rows + 1;
            assert_eq!(start, chunk.start);
            assert_eq!(n(chunk.node).rows(), chunk.end);
        }
        {
            let chunk = it.next().unwrap();
            assert!(chunk.node == last(&s));
            assert_eq!(0, chunk.start);
            assert_eq!(1, chunk.end);
        }
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_page_iterator_history_two_pages() {
        let mut s = init(80, 24);

        // Grow to capacity
        fill_then_grow_page(&mut s);

        // Iterate the active area
        let mut it = s.page_iterator(Direction::RightDown, pt_history(0, 0), None);
        {
            let active_tl = s.get_top_left(Tag::Active);
            let chunk = it.next().unwrap();
            assert!(chunk.node == first(&s));
            assert_eq!(0, chunk.start);
            assert_eq!(active_tl.y, chunk.end);
        }
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_page_iterator_reverse_single_page() {
        let s = init(80, 24);

        // The viewport should be within a single page
        assert!(n(first(&s)).next.is_none());

        // Iterate the active area
        let mut it = s.page_iterator(Direction::LeftUp, pt_active(0, 0), None);
        {
            let chunk = it.next().unwrap();
            assert!(chunk.node == first(&s));
            assert_eq!(0, chunk.start);
            assert_eq!(s.rows, chunk.end);
        }

        // Should only have one chunk
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_page_iterator_reverse_two_pages() {
        let mut s = init(80, 24);

        // Grow to capacity
        fill_then_grow_page(&mut s);

        // Iterate the active area
        let mut it = s.page_iterator(Direction::LeftUp, pt_active(0, 0), None);
        let mut count: usize = 0;
        {
            let chunk = it.next().unwrap();
            assert!(chunk.node == last(&s));
            assert_eq!(0, chunk.start);
            assert_eq!(1, chunk.end);
            count += (chunk.end - chunk.start) as usize;
        }
        {
            let chunk = it.next().unwrap();
            assert!(chunk.node == first(&s));
            let start = n(chunk.node).rows() - s.rows + 1;
            assert_eq!(start, chunk.start);
            assert_eq!(n(chunk.node).rows(), chunk.end);
            count += (chunk.end - chunk.start) as usize;
        }
        assert!(it.next().is_none());
        assert_eq!(s.rows as usize, count);
    }

    #[test]
    fn page_list_page_iterator_reverse_history_two_pages() {
        let mut s = init(80, 24);

        // Grow to capacity
        fill_then_grow_page(&mut s);

        // Iterate the active area
        let mut it = s.page_iterator(Direction::LeftUp, pt_history(0, 0), None);
        {
            let active_tl = s.get_top_left(Tag::Active);
            let chunk = it.next().unwrap();
            assert!(chunk.node == first(&s));
            assert_eq!(0, chunk.start);
            assert_eq!(active_tl.y, chunk.end);
        }
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_page_iterator_reverse_count_includes_row_zero() {
        let s = init(2, 2);

        let mut it = PageIterator {
            row: Some(s.get_top_left(Tag::Screen)),
            limit: PageIteratorLimit::Count(1),
            direction: Direction::LeftUp,
        };
        let chunk = it.next().unwrap();
        assert_eq!(0, chunk.start);
        assert_eq!(1, chunk.end);
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_page_iterator_count_crosses_page_boundaries() {
        let mut s = init(80, 24);

        let first = first(&s);
        pg(first).pause_integrity_checks(true);
        while n(first).rows() < n(first).capacity().rows {
            s.grow().unwrap();
        }
        pg(first).pause_integrity_checks(false);
        let second = s.grow().unwrap().unwrap();

        let mut down = PageIterator {
            row: Some(pin_at(first, 0, n(first).rows() - 1)),
            limit: PageIteratorLimit::Count(2),
            direction: Direction::RightDown,
        };
        {
            let chunk = down.next().unwrap();
            assert_eq!(first, chunk.node);
            assert_eq!(n(first).rows() - 1, chunk.start);
            assert_eq!(n(first).rows(), chunk.end);
        }
        {
            let chunk = down.next().unwrap();
            assert_eq!(second, chunk.node);
            assert_eq!(0, chunk.start);
            assert_eq!(1, chunk.end);
        }
        assert!(down.next().is_none());

        let mut up = PageIterator {
            row: Some(Pin::new(second)),
            limit: PageIteratorLimit::Count(2),
            direction: Direction::LeftUp,
        };
        {
            let chunk = up.next().unwrap();
            assert_eq!(second, chunk.node);
            assert_eq!(0, chunk.start);
            assert_eq!(1, chunk.end);
        }
        {
            let chunk = up.next().unwrap();
            assert_eq!(first, chunk.node);
            assert_eq!(n(first).rows() - 1, chunk.start);
            assert_eq!(n(first).rows(), chunk.end);
        }
        assert!(up.next().is_none());
    }

    #[test]
    fn page_list_cell_iterator() {
        let s = init_opts(Options {
            cols: 2,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });
        fill_x(&s, 0);

        let mut it = s.cell_iterator(Direction::RightDown, pt_screen(0, 0), None);
        assert_eq!(pt_screen(0, 0), s.point_from_pin(Tag::Screen, it.next().unwrap()).unwrap());
        assert_eq!(pt_screen(1, 0), s.point_from_pin(Tag::Screen, it.next().unwrap()).unwrap());
        assert_eq!(pt_screen(0, 1), s.point_from_pin(Tag::Screen, it.next().unwrap()).unwrap());
        assert_eq!(pt_screen(1, 1), s.point_from_pin(Tag::Screen, it.next().unwrap()).unwrap());
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_cell_iterator_reverse() {
        let s = init_opts(Options {
            cols: 2,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });
        fill_x(&s, 0);

        let mut it = s.cell_iterator(Direction::LeftUp, pt_screen(0, 0), None);
        assert_eq!(pt_screen(1, 1), s.point_from_pin(Tag::Screen, it.next().unwrap()).unwrap());
        assert_eq!(pt_screen(0, 1), s.point_from_pin(Tag::Screen, it.next().unwrap()).unwrap());
        assert_eq!(pt_screen(1, 0), s.point_from_pin(Tag::Screen, it.next().unwrap()).unwrap());
        assert_eq!(pt_screen(0, 0), s.point_from_pin(Tag::Screen, it.next().unwrap()).unwrap());
        assert!(it.next().is_none());
    }

    fn prompt_list(cols: CellCountInt, rows: CellCountInt) -> PageList {
        let s = init_opts(Options {
            cols,
            rows,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        s
    }

    fn set_prompt(page: &Page, y: usize, v: SemanticPrompt) {
        row_at(page, y).set_semantic_prompt(v);
    }

    fn sem_cell(c: char, sem: SemanticContent) -> PCell {
        let mut cell = cp(c);
        cell.set_semantic_content(sem);
        cell
    }

    /// Write `c` with semantic content `sem` to cells `xs` of row `y`.
    fn sem_fill(page: &Page, y: usize, xs: core::ops::Range<usize>, c: char, sem: SemanticContent) {
        for x in xs {
            *cell_at(page, x, y) = sem_cell(c, sem);
        }
    }

    /// Only set the semantic content of cells `xs` of row `y`.
    fn sem_mark(page: &Page, y: usize, xs: core::ops::Range<usize>, sem: SemanticContent) {
        for x in xs {
            cell_at(page, x, y).set_semantic_content(sem);
        }
    }

    fn pfp(s: &PageList, p: Pin) -> Point {
        s.point_from_pin(Tag::Screen, p).unwrap()
    }

    #[test]
    fn page_list_prompt_iterator_left_up() {
        let s = prompt_list(2, 20);
        let page = pg(first(&s));
        // Normal prompt
        set_prompt(page, 3, SemanticPrompt::Prompt);
        // Continuation
        set_prompt(page, 6, SemanticPrompt::Prompt);
        set_prompt(page, 7, SemanticPrompt::PromptContinuation);
        set_prompt(page, 8, SemanticPrompt::PromptContinuation);
        // Broken continuation that has non-prompts in between
        set_prompt(page, 12, SemanticPrompt::PromptContinuation);

        let mut it = s.prompt_iterator(Direction::LeftUp, pt_screen(0, 0), None);
        assert_eq!(pt_screen(0, 12), pfp(&s, it.next().unwrap()));
        assert_eq!(pt_screen(0, 6), pfp(&s, it.next().unwrap()));
        assert_eq!(pt_screen(0, 3), pfp(&s, it.next().unwrap()));
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_prompt_iterator_right_down() {
        let s = prompt_list(2, 20);
        let page = pg(first(&s));
        // Normal prompt
        set_prompt(page, 3, SemanticPrompt::Prompt);
        // Continuation (prompt on row 6, continuation on rows 7-8)
        set_prompt(page, 6, SemanticPrompt::Prompt);
        set_prompt(page, 7, SemanticPrompt::PromptContinuation);
        set_prompt(page, 8, SemanticPrompt::PromptContinuation);
        // Broken continuation that has non-prompts in between (orphaned continuation at row 12)
        set_prompt(page, 12, SemanticPrompt::PromptContinuation);

        let mut it = s.prompt_iterator(Direction::RightDown, pt_screen(0, 0), None);
        assert_eq!(pt_screen(0, 3), pfp(&s, it.next().unwrap()));
        assert_eq!(pt_screen(0, 6), pfp(&s, it.next().unwrap()));
        assert_eq!(pt_screen(0, 12), pfp(&s, it.next().unwrap()));
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_prompt_iterator_right_down_continuation_at_start() {
        let s = prompt_list(2, 20);
        let page = pg(first(&s));

        // Prompt continuation at row 0 (no prior rows - simulates trimmed scrollback)
        set_prompt(page, 0, SemanticPrompt::PromptContinuation);
        set_prompt(page, 1, SemanticPrompt::PromptContinuation);
        // Normal prompt later
        set_prompt(page, 5, SemanticPrompt::Prompt);

        let mut it = s.prompt_iterator(Direction::RightDown, pt_screen(0, 0), None);
        // Should return the first continuation line since there's no prior prompt
        assert_eq!(pt_screen(0, 0), pfp(&s, it.next().unwrap()));
        assert_eq!(pt_screen(0, 5), pfp(&s, it.next().unwrap()));
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_prompt_iterator_right_down_with_prompt_before_continuation() {
        let s = prompt_list(2, 20);
        let page = pg(first(&s));

        // Prompt on row 2, continuation on rows 3-4
        // Starting iteration from row 3 should still find the prompt at row 2
        set_prompt(page, 2, SemanticPrompt::Prompt);
        set_prompt(page, 3, SemanticPrompt::PromptContinuation);
        set_prompt(page, 4, SemanticPrompt::PromptContinuation);

        // Start iteration from row 3 (middle of the continuation)
        // Since we start on a continuation line, we treat it as the prompt start
        // (handles case where scrollback pruned the actual prompt)
        let mut it = s.prompt_iterator(Direction::RightDown, pt_screen(0, 3), None);
        // Returns row 3 since that's the first prompt-related line we encounter
        assert_eq!(pt_screen(0, 3), pfp(&s, it.next().unwrap()));
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_prompt_iterator_right_down_limit_inclusive() {
        let s = prompt_list(2, 20);
        let page = pg(first(&s));

        // Prompt on row 5
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Iterate with limit at row 5 (the prompt row) - should include it
        let mut it = s.prompt_iterator(Direction::RightDown, pt_screen(0, 0), Some(pt_screen(0, 5)));
        assert_eq!(pt_screen(0, 5), pfp(&s, it.next().unwrap()));
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_prompt_iterator_left_up_limit_inclusive() {
        let s = prompt_list(2, 20);
        let page = pg(first(&s));

        // Prompt on row 5
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Iterate with limit at row 10 (the prompt row) - should include it
        // tl_pt is the limit (upper bound), bl_pt is the start point for left_up
        let mut it = s.prompt_iterator(Direction::LeftUp, pt_screen(0, 10), Some(pt_screen(0, 15)));
        assert_eq!(pt_screen(0, 10), pfp(&s, it.next().unwrap()));
        assert!(it.next().is_none());
    }

    fn hl_check(s: &PageList, at: Point, content: SemanticContent, start: Point, end: Point) {
        let hl = s.highlight_semantic_content(s.pin(at).unwrap(), content).unwrap();
        assert_eq!(start, pfp(s, hl.start));
        assert_eq!(end, pfp(s, hl.end));
    }

    #[test]
    fn page_list_highlight_semantic_content_prompt() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // Start the prompt for the first 5 cols
        sem_fill(page, 5, 0..5, 'A', SemanticContent::Prompt);
        // Next 3 let's make input
        sem_fill(page, 5, 5..8, 'B', SemanticContent::Input);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        hl_check(&s, pt_screen(2, 5), SemanticContent::Prompt, pt_screen(0, 5), pt_screen(7, 5));
    }

    #[test]
    fn page_list_highlight_semantic_content_prompt_with_output() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // First 3 cols are prompt
        sem_fill(page, 5, 0..3, '$', SemanticContent::Prompt);
        // Next 4 are input
        sem_fill(page, 5, 3..7, 'l', SemanticContent::Input);
        // Rest is output (shouldn't be included in prompt highlight)
        sem_fill(page, 5, 7..10, 'o', SemanticContent::Output);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting from prompt should include prompt and input, but stop at output
        hl_check(&s, pt_screen(0, 5), SemanticContent::Prompt, pt_screen(0, 5), pt_screen(6, 5));
    }

    #[test]
    fn page_list_highlight_semantic_content_prompt_multiline() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt starts on row 5
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // First row is all prompt
        sem_fill(page, 5, 0..10, '$', SemanticContent::Prompt);
        // Row 6 continues with input
        sem_fill(page, 6, 0..5, 'c', SemanticContent::Input);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting should span both rows
        hl_check(&s, pt_screen(2, 5), SemanticContent::Prompt, pt_screen(0, 5), pt_screen(4, 6));
    }

    #[test]
    fn page_list_highlight_semantic_content_prompt_only() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5 with only prompt content (no input)
        set_prompt(page, 5, SemanticPrompt::Prompt);
        sem_fill(page, 5, 0..5, '$', SemanticContent::Prompt);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting should only include the prompt cells
        hl_check(&s, pt_screen(0, 5), SemanticContent::Prompt, pt_screen(0, 5), pt_screen(4, 5));
    }

    #[test]
    fn page_list_highlight_semantic_content_prompt_to_end_of_screen() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Single prompt on row 15, no following prompt
        set_prompt(page, 15, SemanticPrompt::Prompt);
        sem_fill(page, 15, 0..3, '$', SemanticContent::Prompt);
        sem_fill(page, 15, 3..8, 'c', SemanticContent::Input);

        // Highlighting should include prompt and input up to column 7
        hl_check(&s, pt_screen(0, 15), SemanticContent::Prompt, pt_screen(0, 15), pt_screen(7, 15));
    }

    #[test]
    fn page_list_highlight_semantic_content_input_basic() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // First 3 cols are prompt
        sem_fill(page, 5, 0..3, '$', SemanticContent::Prompt);
        // Next 5 are input
        sem_fill(page, 5, 3..8, 'l', SemanticContent::Input);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting input should only include input cells
        hl_check(&s, pt_screen(0, 5), SemanticContent::Input, pt_screen(3, 5), pt_screen(7, 5));
    }

    #[test]
    fn page_list_highlight_semantic_content_input_with_output() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // First 2 cols are prompt
        sem_fill(page, 5, 0..2, '$', SemanticContent::Prompt);
        // Next 3 are input
        sem_fill(page, 5, 2..5, 'c', SemanticContent::Input);
        // Rest is output
        sem_fill(page, 5, 5..10, 'o', SemanticContent::Output);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting input should stop at output
        hl_check(&s, pt_screen(0, 5), SemanticContent::Input, pt_screen(2, 5), pt_screen(4, 5));
    }

    #[test]
    fn page_list_highlight_semantic_content_input_multiline_with_continuation() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // First 2 cols are prompt
        sem_fill(page, 5, 0..2, '$', SemanticContent::Prompt);
        // Rest is input
        sem_fill(page, 5, 2..10, 'c', SemanticContent::Input);
        // Row 6 has continuation prompt then more input
        sem_fill(page, 6, 0..2, '>', SemanticContent::Prompt);
        sem_fill(page, 6, 2..6, 'd', SemanticContent::Input);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting input should span both rows, skipping continuation prompts
        hl_check(&s, pt_screen(0, 5), SemanticContent::Input, pt_screen(2, 5), pt_screen(5, 6));
    }

    #[test]
    fn page_list_highlight_semantic_content_input_no_input_returns_null() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5 with only prompt, then immediately output
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // First 3 cols are prompt
        sem_fill(page, 5, 0..3, '$', SemanticContent::Prompt);
        // Rest is output (no input!)
        sem_fill(page, 5, 3..10, 'o', SemanticContent::Output);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting input should return null when there's no input
        let hl = s.highlight_semantic_content(s.pin(pt_screen(0, 5)).unwrap(), SemanticContent::Input);
        assert!(hl.is_none());
    }

    #[test]
    fn page_list_highlight_semantic_content_input_to_end_of_screen() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Single prompt on row 15, no following prompt
        set_prompt(page, 15, SemanticPrompt::Prompt);
        sem_fill(page, 15, 0..2, '$', SemanticContent::Prompt);
        sem_fill(page, 15, 2..7, 'c', SemanticContent::Input);

        // Highlighting input with no following prompt
        hl_check(&s, pt_screen(0, 15), SemanticContent::Input, pt_screen(2, 15), pt_screen(6, 15));
    }

    #[test]
    fn page_list_highlight_semantic_content_input_prompt_only_returns_null() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5 with only prompt content, no input or output
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // All cells are prompt
        sem_fill(page, 5, 0..10, '$', SemanticContent::Prompt);
        // Mark rows 6-9 as prompt to ensure no input before next prompt
        for y in 6..10 {
            sem_mark(page, y, 0..10, SemanticContent::Prompt);
        }
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting input should return null when there's only prompts
        let hl = s.highlight_semantic_content(s.pin(pt_screen(0, 5)).unwrap(), SemanticContent::Input);
        assert!(hl.is_none());
    }

    #[test]
    fn page_list_highlight_semantic_content_output_basic() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // First 2 cols are prompt
        sem_fill(page, 5, 0..2, '$', SemanticContent::Prompt);
        // Next 3 are input
        sem_fill(page, 5, 2..5, 'l', SemanticContent::Input);
        // Cols 5-7 are output
        sem_fill(page, 5, 5..8, 'o', SemanticContent::Output);
        // Mark remaining cells as prompt to bound the output
        sem_mark(page, 5, 8..10, SemanticContent::Prompt);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting output should only include output cells
        hl_check(&s, pt_screen(0, 5), SemanticContent::Output, pt_screen(5, 5), pt_screen(7, 5));
    }

    #[test]
    fn page_list_highlight_semantic_content_output_multiline() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // First 2 cols are prompt
        sem_fill(page, 5, 0..2, '$', SemanticContent::Prompt);
        // Next 2 are input
        sem_fill(page, 5, 2..4, 'l', SemanticContent::Input);
        // Rest of row 5 is output
        sem_fill(page, 5, 4..10, 'o', SemanticContent::Output);
        // Row 6 is all output
        sem_fill(page, 6, 0..10, 'o', SemanticContent::Output);
        // Row 7 has partial output then input to bound it
        sem_fill(page, 7, 0..5, 'o', SemanticContent::Output);
        sem_mark(page, 7, 5..10, SemanticContent::Input);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting output should span multiple rows
        hl_check(&s, pt_screen(0, 5), SemanticContent::Output, pt_screen(4, 5), pt_screen(4, 7));
    }

    #[test]
    fn page_list_highlight_semantic_content_output_stops_at_next_prompt() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // First 2 cols are prompt
        sem_fill(page, 5, 0..2, '$', SemanticContent::Prompt);
        // Next 2 are input
        sem_fill(page, 5, 2..4, 'l', SemanticContent::Input);
        // Rest is output
        sem_fill(page, 5, 4..10, 'o', SemanticContent::Output);
        // Row 6 has output then prompt starts
        sem_fill(page, 6, 0..3, 'o', SemanticContent::Output);
        // Next prompt marker on same row
        sem_fill(page, 6, 3..6, '$', SemanticContent::Prompt);
        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting output should stop before prompt/input
        hl_check(&s, pt_screen(0, 5), SemanticContent::Output, pt_screen(4, 5), pt_screen(2, 6));
    }

    #[test]
    fn page_list_highlight_semantic_content_output_to_end_of_screen() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Single prompt on row 15, no following prompt
        set_prompt(page, 15, SemanticPrompt::Prompt);
        sem_fill(page, 15, 0..2, '$', SemanticContent::Prompt);
        sem_fill(page, 15, 2..4, 'c', SemanticContent::Input);
        sem_fill(page, 15, 4..10, 'o', SemanticContent::Output);
        // Row 16 has output then prompt to bound it
        sem_fill(page, 16, 0..8, 'o', SemanticContent::Output);
        sem_mark(page, 16, 8..10, SemanticContent::Prompt);

        // Highlighting output with no following prompt
        hl_check(&s, pt_screen(0, 15), SemanticContent::Output, pt_screen(4, 15), pt_screen(7, 16));
    }

    #[test]
    fn page_list_highlight_semantic_content_output_no_output_returns_null() {
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5 with only prompt and input, no output
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // First 3 cols are prompt
        sem_fill(page, 5, 0..3, '$', SemanticContent::Prompt);
        // Rest is input (must explicitly mark all cells to avoid default .output)
        sem_fill(page, 5, 3..10, 'c', SemanticContent::Input);
        // Mark rows 6-9 as input to ensure no output between prompts
        for y in 6..10 {
            sem_mark(page, y, 0..10, SemanticContent::Input);
        }
        // Prompt on row 10 (no output between prompts)
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting output should return null when there's no output
        let hl = s.highlight_semantic_content(s.pin(pt_screen(0, 5)).unwrap(), SemanticContent::Output);
        assert!(hl.is_none());
    }

    #[test]
    fn page_list_highlight_semantic_content_output_skips_empty_cells() {
        // Tests that empty cells with default .output semantic content are
        // not selected as output. This can happen when a prompt/input line
        // doesn't fill the entire row - trailing cells have default .output.
        let s = prompt_list(10, 20);
        let page = pg(first(&s));

        // Prompt on row 5 - only fills first 3 cells, rest are empty with default .output
        set_prompt(page, 5, SemanticPrompt::Prompt);
        // First 3 cols are prompt with text
        sem_fill(page, 5, 0..3, '$', SemanticContent::Prompt);
        // Cells 3-9 are empty (codepoint = 0) with default .output semantic content
        // This simulates what happens when a short prompt is written

        // Row 6 has input (short, doesn't fill line)
        sem_fill(page, 6, 0..4, 'l', SemanticContent::Input);
        // Cells 4-9 are empty with default .output

        // Row 7-8 have actual output with text
        for y in 7..9 {
            sem_fill(page, y, 0..5, 'o', SemanticContent::Output);
        }

        // Prompt on row 10
        set_prompt(page, 10, SemanticPrompt::Prompt);

        // Highlighting output should skip empty cells on rows 5-6 and find
        // the actual output starting at row 7
        hl_check(&s, pt_screen(0, 5), SemanticContent::Output, pt_screen(0, 7), pt_screen(4, 8));
    }

    #[test]
    fn page_list_erase() {
        let mut s = init(80, 24);
        assert_eq!(1, s.total_pages());

        // Grow so we take up at least 5 pages.
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_paused(&mut s, cap_rows * 5);
        assert_eq!(6, s.total_pages());

        // Our total rows should be large
        assert!(s.total_rows > s.rows as usize);

        // Erase the entire history, we should be back to just our active set.
        s.erase_history(None);
        assert_eq!(s.rows as usize, s.total_rows);

        // We should be back to just one page
        assert_eq!(1, s.total_pages());
        assert!(s.pages.first == s.pages.last);
    }

    #[test]
    fn page_list_erase_reaccounts_page_size() {
        let mut s = init(80, 24);
        let start_size = s.page_size;

        // Grow so we take up at least 5 pages.
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_paused(&mut s, cap_rows * 5);
        assert!(s.page_size > start_size);

        // Erase the entire history, we should be back to just our active set.
        s.erase_history(None);
        assert_eq!(start_size, s.page_size);
    }

    #[test]
    fn page_list_erase_row_with_tracked_pin_resets_to_top_left() {
        let mut s = init(80, 24);

        // Grow so we take up at least 5 pages.
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_paused(&mut s, cap_rows * 5);

        // Our total rows should be large
        assert!(s.total_rows > s.rows as usize);

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_history(0, 0)).unwrap()).unwrap();

        // Erase the entire history, we should be back to just our active set.
        s.erase_history(None);
        assert_eq!(s.rows as usize, s.total_rows);

        // Our pin should move to the first page
        assert_eq!(first(&s), tp(p).node);
        assert_eq!(0, tp(p).y);
        assert_eq!(0, tp(p).x);

        // Its row was erased, so it should be garbage
        assert!(tp(p).garbage);

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_erase_history_marks_pins_in_erased_rows_as_garbage() {
        let mut s = init(80, 24);

        // A few rows of history on the same page as the active area.
        for _ in 0..10 {
            s.grow().unwrap();
        }
        assert_eq!(34, s.total_rows);

        let history = s.track_pin(s.pin(pt_history(0, 5)).unwrap()).unwrap();
        let active = s.track_pin(s.pin(pt_active(0, 2)).unwrap()).unwrap();

        s.erase_history(None);
        assert_eq!(s.rows as usize, s.total_rows);

        // The history row is gone, the active row just moved up.
        assert!(tp(history).garbage);
        assert!(!tp(active).garbage);
        assert_eq!(pt_screen(0, 2), s.point_from_pin(Tag::Screen, *tp(active)).unwrap());

        s.untrack_pin(active);
        s.untrack_pin(history);
    }

    #[test]
    fn page_list_erase_row_with_tracked_pin_shifts() {
        let mut s = init(80, 24);

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(2, 4)).unwrap()).unwrap();

        // Erase only a few rows in our active
        s.erase_active(3);
        assert_eq!(s.rows as usize, s.total_rows);

        // Our pin should move to the first page
        assert_eq!(first(&s), tp(p).node);
        assert_eq!(0, tp(p).y);
        assert_eq!(2, tp(p).x);

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_erase_row_with_tracked_pin_is_erased() {
        let mut s = init(80, 24);

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(2, 2)).unwrap()).unwrap();

        // Erase the entire history, we should be back to just our active set.
        s.erase_active(3);
        assert_eq!(s.rows as usize, s.total_rows);

        // Our pin should move to the first page
        assert_eq!(first(&s), tp(p).node);
        assert_eq!(0, tp(p).y);
        assert_eq!(0, tp(p).x);

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_erase_resets_viewport_to_active_if_moves_within_active() {
        let mut s = init(80, 24);

        // Grow so we take up at least 5 pages.
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_paused(&mut s, cap_rows * 5);

        // Move our viewport to the top
        s.scroll(Scroll::DeltaRow(-(s.total_rows as isize)));
        assert!(s.viewport == Viewport::Top);

        // Erase the entire history, we should be back to just our active set.
        s.erase_history(None);
        assert!(s.viewport == Viewport::Active);
    }

    #[test]
    fn page_list_erase_resets_viewport_if_inside_erased_page_but_not_active() {
        let mut s = init(80, 24);

        // Grow so we take up at least 5 pages.
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_paused(&mut s, cap_rows * 5);

        // Move our viewport to the top
        s.scroll(Scroll::DeltaRow(-(s.total_rows as isize)));
        assert!(s.viewport == Viewport::Top);

        // Erase the entire history, we should be back to just our active set.
        s.erase_history(Some(pt_history(0, 2)));
        assert!(s.viewport == Viewport::Top);
    }

    #[test]
    fn page_list_erase_resets_viewport_to_active_if_top_is_inside_active() {
        let mut s = init(80, 24);

        // Grow so we take up at least 5 pages.
        let cap_rows = pg(last(&s)).capacity.rows as usize;
        grow_paused(&mut s, cap_rows * 5);

        // Move our viewport to the top
        s.scroll(Scroll::Top);

        // Erase the entire history, we should be back to just our active set.
        s.erase_history(None);
        assert!(s.viewport == Viewport::Active);
    }

    #[test]
    fn page_list_erase_active_regrows_automatically() {
        let mut s = init(80, 24);
        assert!(s.total_rows() == s.rows as usize);
        s.erase_active(10);
        assert!(s.total_rows() == s.rows as usize);
    }

    #[test]
    fn page_list_erase_a_one_row_active() {
        let mut s = init(10, 1);
        assert_eq!(1, s.total_pages());

        // Write our letter
        let page = pg(first(&s));
        for y in 0..s.rows as usize {
            *cell_at(page, 0, y) = cp('A');
        }

        s.erase_active(0);
        assert_eq!(s.rows as usize, s.total_rows);

        // The row should be empty
        let get = s.get_cell(pt_active(0, 0)).unwrap();
        assert_eq!(0, unsafe { (*get.cell).content_codepoint() });
    }

    #[test]
    fn page_list_erase_row_bounded_less_than_full_row() {
        let mut s = init(80, 10);

        // Pins
        let p_top = s.track_pin(s.pin(pt_active(0, 5)).unwrap()).unwrap();
        let p_bot = s.track_pin(s.pin(pt_active(0, 8)).unwrap()).unwrap();
        let p_out = s.track_pin(s.pin(pt_active(0, 9)).unwrap()).unwrap();

        // Erase only a few rows in our active
        s.erase_row_bounded(pt_active(0, 5), 3);
        assert_eq!(s.rows as usize, s.total_rows());

        // The erased rows should be dirty
        assert!(s.is_dirty(pt_active(0, 5)));
        assert!(s.is_dirty(pt_active(0, 6)));
        assert!(s.is_dirty(pt_active(0, 7)));

        assert_eq!(first(&s), tp(p_top).node);
        assert_eq!(4, tp(p_top).y);
        assert_eq!(0, tp(p_top).x);

        assert_eq!(first(&s), tp(p_bot).node);
        assert_eq!(7, tp(p_bot).y);
        assert_eq!(0, tp(p_bot).x);

        assert_eq!(first(&s), tp(p_out).node);
        assert_eq!(9, tp(p_out).y);
        assert_eq!(0, tp(p_out).x);

        s.untrack_pin(p_out);
        s.untrack_pin(p_bot);
        s.untrack_pin(p_top);
    }

    #[test]
    fn page_list_erase_row_bounded_with_pin_at_top() {
        let mut s = init(80, 10);

        // Pins
        let p_top = s.track_pin(s.pin(pt_active(5, 0)).unwrap()).unwrap();

        // Erase only a few rows in our active
        s.erase_row_bounded(pt_active(0, 0), 3);
        assert_eq!(s.rows as usize, s.total_rows());

        // The erased rows should be dirty
        assert!(s.is_dirty(pt_active(0, 0)));
        assert!(s.is_dirty(pt_active(0, 1)));
        assert!(s.is_dirty(pt_active(0, 2)));

        assert_eq!(first(&s), tp(p_top).node);
        assert_eq!(0, tp(p_top).y);
        assert_eq!(0, tp(p_top).x);

        s.untrack_pin(p_top);
    }

    #[test]
    fn page_list_erase_row_bounded_full_rows_single_page() {
        let mut s = init(80, 10);

        // Pins
        let p_in = s.track_pin(s.pin(pt_active(0, 7)).unwrap()).unwrap();
        let p_out = s.track_pin(s.pin(pt_active(0, 9)).unwrap()).unwrap();

        // Erase only a few rows in our active
        s.erase_row_bounded(pt_active(0, 5), 10);
        assert_eq!(s.rows as usize, s.total_rows());

        // The erased rows should be dirty
        for y in 5..10 {
            assert!(s.is_dirty(pt_active(0, y)));
        }

        // Our pin should move to the first page
        assert_eq!(first(&s), tp(p_in).node);
        assert_eq!(6, tp(p_in).y);
        assert_eq!(0, tp(p_in).x);

        assert_eq!(first(&s), tp(p_out).node);
        assert_eq!(8, tp(p_out).y);
        assert_eq!(0, tp(p_out).x);

        s.untrack_pin(p_out);
        s.untrack_pin(p_in);
    }

    /// Grow so the active area straddles two pages: the first page is
    /// full and the last page holds 5 rows.
    fn straddle_two_pages(s: &mut PageList) {
        let page = pg(last(s));
        page.pause_integrity_checks(true);
        for _ in 0..(page.capacity.rows - page.size.rows) {
            s.grow().unwrap();
        }
        page.pause_integrity_checks(false);
        s.grow_rows(5).unwrap();
        assert_eq!(2, s.total_pages());
        assert_eq!(5, n(last(s)).rows());
    }

    #[test]
    fn page_list_erase_row_bounded_full_rows_two_pages() {
        let mut s = init(80, 10);

        // Grow to two pages so our active area straddles
        straddle_two_pages(&mut s);

        // Pins
        let p_first = s.track_pin(s.pin(pt_active(0, 4)).unwrap()).unwrap();
        let p_first_out = s.track_pin(s.pin(pt_active(0, 3)).unwrap()).unwrap();
        let p_in = s.track_pin(s.pin(pt_active(0, 8)).unwrap()).unwrap();
        let p_out = s.track_pin(s.pin(pt_active(0, 9)).unwrap()).unwrap();

        {
            assert_eq!(prev(last(&s)), tp(p_first).node);
            assert_eq!(n(tp(p_first).node).rows() - 1, tp(p_first).y);
            assert_eq!(0, tp(p_first).x);

            assert_eq!(prev(last(&s)), tp(p_first_out).node);
            assert_eq!(n(tp(p_first_out).node).rows() - 2, tp(p_first_out).y);
            assert_eq!(0, tp(p_first_out).x);

            assert_eq!(last(&s), tp(p_in).node);
            assert_eq!(3, tp(p_in).y);
            assert_eq!(0, tp(p_in).x);

            assert_eq!(last(&s), tp(p_out).node);
            assert_eq!(4, tp(p_out).y);
            assert_eq!(0, tp(p_out).x);
        }

        // Erase only a few rows in our active
        s.erase_row_bounded(pt_active(0, 4), 4);

        // The erased rows should be dirty
        for y in 4..8 {
            assert!(s.is_dirty(pt_active(0, y)));
        }

        // In page in first page is shifted
        assert_eq!(prev(last(&s)), tp(p_first).node);
        assert_eq!(n(tp(p_first).node).rows() - 2, tp(p_first).y);
        assert_eq!(0, tp(p_first).x);

        // Out page in first page should not be shifted
        assert_eq!(prev(last(&s)), tp(p_first_out).node);
        assert_eq!(n(tp(p_first_out).node).rows() - 2, tp(p_first_out).y);
        assert_eq!(0, tp(p_first_out).x);

        // In page is shifted
        assert_eq!(last(&s), tp(p_in).node);
        assert_eq!(2, tp(p_in).y);
        assert_eq!(0, tp(p_in).x);

        // Out page is not shifted
        assert_eq!(last(&s), tp(p_out).node);
        assert_eq!(4, tp(p_out).y);
        assert_eq!(0, tp(p_out).x);

        s.untrack_pin(p_out);
        s.untrack_pin(p_in);
        s.untrack_pin(p_first_out);
        s.untrack_pin(p_first);
    }

    fn uri_for(x: usize) -> Vec<u8> {
        format!("http://example.com/{}", x).into_bytes()
    }

    /// Shared setup for the hyperlink-dense erase tests.
    fn hyperlink_dense_setup(s: &mut PageList, link_count: usize) {
        // Grow to two pages so our active area straddles them: the first
        // page is exactly full and the second page holds the last 5 rows
        // of the active area.
        straddle_two_pages(s);

        // Mark each active row with a codepoint so we can verify the
        // shift afterwards. Row y gets codepoint '0' + y at x = 0.
        for y in 0..10u32 {
            let row_pin = s.pin(pt_active(0, y)).unwrap();
            *pin_rac(row_pin).1 = cpu('0' as u32 + y);
        }

        // Fill the top row of the second page (active y=5) with more
        // unique hyperlinks ('A' through 'J') than the first page's
        // default hyperlink capacity can hold. We must increase the
        // second page's capacity to even create such a row; the first
        // page keeps its default capacity.
        while pg(last(s)).hyperlink_set.layout.cap <= link_count {
            s.increase_capacity(last(s), Some(IncreaseCapacity::HyperlinkBytes))
                .unwrap();
        }
        assert!(pg(first(s)).hyperlink_set.layout.cap < link_count);
        {
            let page = pg(last(s));
            for x in 0..link_count {
                let uri = uri_for(x);
                let id = page
                    .insert_hyperlink(&hyperlink::Hyperlink {
                        id: hyperlink::HyperlinkId::Implicit(x as u32),
                        uri: std::borrow::Cow::Borrowed(&uri),
                    })
                    .unwrap();
                let (row, cell) = rac(page, x, 0);
                *cell = cpu('A' as u32 + x as u32);
                page.set_hyperlink(row, cell, id).unwrap();
                unsafe { page.hyperlink_set.use_(page.memory, id) };
            }
        }
    }

    fn hyperlink_dense_check(s: &PageList, link_count: usize) {
        // Every cell of the dense row must still resolve to a real
        // hyperlink entry with the correct URI. A half-applied erase
        // leaves cells whose hyperlink flag is set but that have no map
        // entry, which aborts in clearCells later.
        for x in 0..link_count {
            let list_cell = s.get_cell(pt_active(x as CellCountInt, 4)).unwrap();
            let cell = unsafe { &*list_cell.cell };
            assert!(cell.hyperlink());
            let page = pg(list_cell.node);
            let id = page.lookup_hyperlink(cell).unwrap();
            let link = unsafe { &*page.hyperlink_set.get(page.memory, id) };
            assert_eq!(&uri_for(x)[..], unsafe { link.uri.slice(page.memory) });
        }

        // All pages must pass integrity checks.
        let mut node_ = s.pages.first;
        while let Some(node) = node_ {
            pg(node).assert_integrity();
            node_ = n(node).next;
        }
    }

    #[test]
    fn page_list_erase_row_hyperlink_dense_row_crosses_page_boundary() {
        // Regression test: when eraseRow shifts rows up across a page
        // boundary, the top row of the next page is cloned into the last
        // row of the previous page. If the previous page doesn't have
        // enough capacity for the managed memory of that row (hyperlinks,
        // styles, etc.) the error propagated out AFTER the previous page
        // had already been rotated and its tracked pins moved, leaving
        // the page list half-mutated.
        //
        // eraseRow must instead increase the destination page's capacity
        // and retry, the same way insertLines/deleteLines and
        // cursorScrollAbove handle their cross-page copies.
        let mut s = init(80, 10);
        let link_count: usize = 10;
        hyperlink_dense_setup(&mut s, link_count);

        // Track a pin in the shifted region of the first page to verify
        // it survives the capacity change of its node.
        let p = s.track_pin(s.pin(pt_active(3, 1)).unwrap()).unwrap();

        // Erase the first active row. The dense hyperlink row must cross
        // the page boundary into the first page, which requires growing
        // the first page's hyperlink capacity.
        s.erase_row(pt_active(0, 0));

        // Every remaining row shifted up by one: the '0' marker row was
        // erased, the dense row moved up across the page boundary to
        // row 4, and the last row was cleared.
        let expected: [u32; 10] = ['1' as u32, '2' as u32, '3' as u32, '4' as u32, 'A' as u32, '6' as u32, '7' as u32, '8' as u32, '9' as u32, 0];
        for (y, cp) in expected.iter().enumerate() {
            let list_cell = s.get_cell(pt_active(0, y as u32)).unwrap();
            assert_eq!(*cp, unsafe { (*list_cell.cell).content_codepoint() });
        }

        hyperlink_dense_check(&s, link_count);

        // Our tracked pin shifted up by one row and still points into
        // the (possibly replaced) first page.
        assert_eq!(first(&s), tp(p).node);
        let p_pt = s.point_from_pin(Tag::Active, *tp(p)).unwrap().coord();
        assert_eq!(3, p_pt.x);
        assert_eq!(0, p_pt.y);

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_erase_row_bounded_hyperlink_dense_row_crosses_page_boundary() {
        // Same as the eraseRow variant above but for eraseRowBounded,
        // which has the same rotate-then-clone structure and had the
        // same bug: a cross-page row clone failure propagated out after
        // the first page had already been rotated.
        let mut s = init(80, 10);
        let link_count: usize = 10;
        hyperlink_dense_setup(&mut s, link_count);

        // Erase the first active row with a limit that extends into the
        // second page (5 rows remain in the first page, so a limit of 6
        // forces the cross-page path). The dense hyperlink row must cross
        // the page boundary into the first page.
        s.erase_row_bounded(pt_active(0, 0), 6);

        // Rows within the limit shifted up by one: the '0' marker row was
        // erased, the dense row moved up across the page boundary to
        // row 4, row 6 is the new blank row, and rows past the limit are
        // unchanged.
        let expected: [u32; 10] = ['1' as u32, '2' as u32, '3' as u32, '4' as u32, 'A' as u32, '6' as u32, 0, '7' as u32, '8' as u32, '9' as u32];
        for (y, cp) in expected.iter().enumerate() {
            let list_cell = s.get_cell(pt_active(0, y as u32)).unwrap();
            assert_eq!(*cp, unsafe { (*list_cell.cell).content_codepoint() });
        }

        hyperlink_dense_check(&s, link_count);
    }

    #[test]
    fn page_list_clone() {
        let s = init(80, 24);
        assert_eq!(s.rows as usize, s.total_rows());

        let s2 = s.clone(clone_opts(pt_screen(0, 0), None)).unwrap();
        assert_eq!(s.rows as usize, s2.total_rows());
    }

    #[test]
    fn page_list_clone_partial_trimmed_right() {
        let mut s = init(80, 20);
        assert_eq!(s.rows as usize, s.total_rows());
        s.grow_rows(30).unwrap();

        let s2 = s.clone(clone_opts(pt_screen(0, 0), Some(pt_screen(0, 39)))).unwrap();
        assert_eq!(40, s2.total_rows());
    }

    #[test]
    fn page_list_clone_partial_trimmed_left() {
        let mut s = init(80, 20);
        assert_eq!(s.rows as usize, s.total_rows());
        s.grow_rows(30).unwrap();

        let s2 = s.clone(clone_opts(pt_screen(0, 10), None)).unwrap();
        assert_eq!(40, s2.total_rows());
    }

    #[test]
    fn page_list_clone_partial_trimmed_left_reclaims_styles() {
        let mut s = init(80, 20);
        assert_eq!(s.rows as usize, s.total_rows());
        s.grow_rows(30).unwrap();

        // Style the rows we're trimming
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            let mut style = stylepkg::Style::default();
            style.flags.bold = true;
            let style_id = unsafe { page.styles.add(page.memory, style) }.unwrap();

            let mut it = s.row_iterator(Direction::LeftUp, pt_screen(0, 0), Some(pt_screen(0, 9)));
            while let Some(p) = it.next() {
                let (row, cell) = pin_rac(p);
                row.set_styled(true);
                let mut c = cp('A');
                c.set_style_id(style_id);
                *cell = c;
                unsafe { page.styles.use_(page.memory, style_id) };
            }

            // We're over-counted by 1 because `add` implies `use`.
            unsafe { page.styles.release(page.memory, style_id) };

            // Expect to have one style
            assert_eq!(1, page.styles.count());
        }

        let s2 = s.clone(clone_opts(pt_screen(0, 10), None)).unwrap();
        assert_eq!(40, s2.total_rows());

        {
            assert!(s2.pages.first == s2.pages.last);
            let page = pg(first(&s2));
            assert_eq!(0, page.styles.count());
        }
    }

    #[test]
    fn page_list_clone_partial_trimmed_both() {
        let mut s = init(80, 20);
        assert_eq!(s.rows as usize, s.total_rows());
        s.grow_rows(30).unwrap();

        let s2 = s.clone(clone_opts(pt_screen(0, 10), Some(pt_screen(0, 35)))).unwrap();
        assert_eq!(26, s2.total_rows());
    }

    #[test]
    fn page_list_clone_less_than_active() {
        let s = init(80, 24);
        assert_eq!(s.rows as usize, s.total_rows());

        let s2 = s.clone(clone_opts(pt_active(0, 5), None)).unwrap();
        assert_eq!(s.rows as usize, s2.total_rows());
    }

    #[test]
    fn page_list_clone_remap_tracked_pin() {
        let mut s = init(80, 24);
        assert_eq!(s.rows as usize, s.total_rows());

        // Put a tracked pin in the screen
        let p = s.track_pin(s.pin(pt_active(0, 6)).unwrap()).unwrap();

        let mut pin_remap = TrackedPinsRemap::new();
        let s2 = s
            .clone(Clone {
                top: pt_active(0, 5),
                bot: None,
                tracked_pins: Some(&mut pin_remap),
            })
            .unwrap();

        // We should be able to find our tracked pin
        let p2 = *pin_remap.get(&p).unwrap();
        assert_eq!(pt_active(0, 1), s2.point_from_pin(Tag::Active, *tp(p2)).unwrap());

        drop(s2);
        s.untrack_pin(p);
    }

    #[test]
    fn page_list_clone_remap_tracked_pin_not_in_cloned_area() {
        let mut s = init(80, 24);
        assert_eq!(s.rows as usize, s.total_rows());

        // Put a tracked pin in the screen
        let p = s.track_pin(s.pin(pt_active(0, 3)).unwrap()).unwrap();

        let mut pin_remap = TrackedPinsRemap::new();
        let s2 = s
            .clone(Clone {
                top: pt_active(0, 5),
                bot: None,
                tracked_pins: Some(&mut pin_remap),
            })
            .unwrap();

        // We should be able to find our tracked pin
        assert!(pin_remap.get(&p).is_none());

        drop(s2);
        s.untrack_pin(p);
    }

    #[test]
    fn page_list_clone_full_dirty() {
        let mut s = init(80, 24);
        assert_eq!(s.rows as usize, s.total_rows());

        // Mark a row as dirty
        s.mark_dirty(pt_active(0, 0));
        s.mark_dirty(pt_active(0, 12));
        s.mark_dirty(pt_active(0, 23));

        let s2 = s.clone(clone_opts(pt_screen(0, 0), None)).unwrap();
        assert_eq!(s.rows as usize, s2.total_rows());

        // Should still be dirty
        assert!(s2.is_dirty(pt_active(0, 0)));
        assert!(!s2.is_dirty(pt_active(0, 1)));
        assert!(s2.is_dirty(pt_active(0, 12)));
        assert!(!s2.is_dirty(pt_active(0, 14)));
        assert!(s2.is_dirty(pt_active(0, 23)));
    }

    fn no_reflow(cols: Option<CellCountInt>, rows: Option<CellCountInt>) -> Resize {
        Resize {
            cols,
            rows,
            reflow: false,
            ..Default::default()
        }
    }

    fn bg_rgb_cell(r: u8, g: u8, b: u8) -> PCell {
        let mut c = PCell::default();
        c.set_content_tag(ContentTag::BgColorRgb);
        c.set_content_color_rgb(pagepkg::Rgb { r, g, b });
        c
    }

    #[test]
    fn page_list_resize_no_reflow_more_rows() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(3, s.total_rows());

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(0, 2)).unwrap()).unwrap();

        // Resize
        s.resize(no_reflow(None, Some(10))).unwrap();
        assert_eq!(10, s.rows);
        assert_eq!(10, s.total_rows());

        // Our cursor should not move because we have no scrollback so
        // we just grew.
        assert_eq!(pt_active(0, 2), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        assert_eq!(pt_screen(0, 0), s.get_cell(pt_active(0, 0)).unwrap().screen_point());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_no_reflow_more_rows_with_history() {
        let mut s = init(10, 3);
        s.grow_rows(50).unwrap();
        assert_eq!(pt_screen(0, 50), s.get_cell(pt_active(0, 0)).unwrap().screen_point());

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(0, 2)).unwrap()).unwrap();

        // Resize
        s.resize(no_reflow(None, Some(5))).unwrap();
        assert_eq!(5, s.rows);
        assert_eq!(53, s.total_rows());

        // Our cursor should move since it's in the scrollback
        assert_eq!(pt_active(0, 4), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        assert_eq!(pt_screen(0, 48), s.get_cell(pt_active(0, 0)).unwrap().screen_point());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_no_reflow_less_rows() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(10, s.total_rows());

        // This is required for our writing below to work
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));

        // Write into all rows so we don't get trim behavior
        for y in 0..s.rows as usize {
            *cell_at(page, 0, y) = cp('A');
        }

        // Resize
        s.resize(no_reflow(None, Some(5))).unwrap();
        assert_eq!(5, s.rows);
        assert_eq!(10, s.total_rows());
        assert_eq!(pt_screen(0, 5), s.get_cell(pt_active(0, 0)).unwrap().screen_point());
    }

    #[test]
    fn page_list_resize_no_reflow_one_rows() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(10, s.total_rows());

        // This is required for our writing below to work
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));

        // Write into all rows so we don't get trim behavior
        for y in 0..s.rows as usize {
            *cell_at(page, 0, y) = cp('A');
        }

        // Resize
        s.resize(no_reflow(None, Some(1))).unwrap();
        assert_eq!(1, s.rows);
        assert_eq!(10, s.total_rows());
        assert_eq!(pt_screen(0, 9), s.get_cell(pt_active(0, 0)).unwrap().screen_point());
    }

    #[test]
    fn page_list_resize_no_reflow_less_rows_cursor_on_bottom() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(10, s.total_rows());

        // This is required for our writing below to work
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));

        // Write into all rows so we don't get trim behavior
        for y in 0..s.rows as usize {
            *cell_at(page, 0, y) = cpu(y as u32);
        }

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(0, 9)).unwrap()).unwrap();
        {
            let cursor = s.point_from_pin(Tag::Active, *tp(p)).unwrap().coord();
            let get = s.get_cell(pt_active(cursor.x, cursor.y)).unwrap();
            assert_eq!(9, unsafe { (*get.cell).content_codepoint() });
        }

        // Resize
        s.resize(no_reflow(None, Some(5))).unwrap();
        assert_eq!(5, s.rows);
        assert_eq!(10, s.total_rows());

        // Our cursor should move since it's in the scrollback
        assert_eq!(pt_active(0, 4), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        assert_eq!(pt_screen(0, 5), s.get_cell(pt_active(0, 0)).unwrap().screen_point());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_no_reflow_less_rows_cursor_in_scrollback() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(10, s.total_rows());

        // This is required for our writing below to work
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));

        // Write into all rows so we don't get trim behavior
        for y in 0..s.rows as usize {
            *cell_at(page, 0, y) = cpu(y as u32);
        }

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(0, 2)).unwrap()).unwrap();
        {
            let cursor = s.point_from_pin(Tag::Active, *tp(p)).unwrap().coord();
            let get = s.get_cell(pt_active(cursor.x, cursor.y)).unwrap();
            assert_eq!(2, unsafe { (*get.cell).content_codepoint() });
        }

        // Resize
        s.resize(no_reflow(None, Some(5))).unwrap();
        assert_eq!(5, s.rows);
        assert_eq!(10, s.total_rows());

        // Our cursor should move since it's in the scrollback
        assert!(s.point_from_pin(Tag::Active, *tp(p)).is_none());
        assert_eq!(pt_screen(0, 2), s.point_from_pin(Tag::Screen, *tp(p)).unwrap());

        assert_eq!(pt_screen(0, 5), s.get_cell(pt_active(0, 0)).unwrap().screen_point());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_no_reflow_less_rows_trims_blank_lines() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 5,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));

        // Write codepoint into first line
        *cell_at(page, 0, 0) = cp('A');

        // Fill remaining lines with a background color
        for y in 1..s.rows as usize {
            *cell_at(page, 0, y) = bg_rgb_cell(0xFF, 0, 0);
        }

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(0, 0)).unwrap()).unwrap();
        {
            let cursor = s.point_from_pin(Tag::Active, *tp(p)).unwrap().coord();
            let get = s.get_cell(pt_active(cursor.x, cursor.y)).unwrap();
            assert_eq!('A' as u32, unsafe { (*get.cell).content_codepoint() });
        }

        // Resize
        s.resize(no_reflow(None, Some(2))).unwrap();
        assert_eq!(2, s.rows);
        assert_eq!(2, s.total_rows());

        // Our cursor should not move since we trimmed
        assert_eq!(pt_active(0, 0), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        assert_eq!(pt_screen(0, 0), s.get_cell(pt_active(0, 0)).unwrap().screen_point());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_no_reflow_less_rows_trims_blank_lines_cursor_in_blank_line() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 5,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));

        // Write codepoint into first line
        *cell_at(page, 0, 0) = cp('A');

        // Fill remaining lines with a background color
        for y in 1..s.rows as usize {
            *cell_at(page, 0, y) = bg_rgb_cell(0xFF, 0, 0);
        }

        // Put a tracked pin in a blank line
        let p = s.track_pin(s.pin(pt_active(0, 3)).unwrap()).unwrap();

        // Resize
        s.resize(no_reflow(None, Some(2))).unwrap();
        assert_eq!(2, s.rows);
        assert_eq!(4, s.total_rows());

        // Our cursor should not move since we trimmed
        assert_eq!(pt_active(0, 1), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_no_reflow_less_rows_trims_blank_lines_erases_pages() {
        let mut s = init_opts(Options {
            cols: 100,
            rows: 5,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));

        // Resize to take up two pages
        {
            let rows = page.capacity.rows + 10;
            s.resize(no_reflow(None, Some(rows))).unwrap();
            assert_eq!(2, s.total_pages());
        }

        // Write codepoint into first line
        *cell_at(page, 0, 0) = cp('A');

        // Resize down. Every row except the first is blank so we
        // should erase the second page.
        s.resize(no_reflow(None, Some(5))).unwrap();
        assert_eq!(5, s.rows);
        assert_eq!(5, s.total_rows());
        assert_eq!(1, s.total_pages());
    }

    #[test]
    fn page_list_resize_no_reflow_more_rows_extends_blank_lines() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));

        // Write codepoint into first line
        *cell_at(page, 0, 0) = cp('A');

        // Fill remaining lines with a background color
        for y in 1..s.rows as usize {
            *cell_at(page, 0, y) = bg_rgb_cell(0xFF, 0, 0);
        }

        // Resize
        s.resize(no_reflow(None, Some(7))).unwrap();
        assert_eq!(7, s.rows);
        assert_eq!(7, s.total_rows());
        assert_eq!(pt_screen(0, 0), s.get_cell(pt_active(0, 0)).unwrap().screen_point());
    }

    #[test]
    fn page_list_resize_no_reflow_more_rows_contains_viewport() {
        // When the rows are increased we need to make sure that the viewport
        // doesn't end up below the active area if it's currently in pin mode.
        let mut s = init_opts(Options {
            cols: 5,
            rows: 5,
            max_size: Some(1),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);

        // Make it so we have scrollback
        s.grow().unwrap();

        assert_eq!(5, s.rows);
        assert_eq!(6, s.total_rows());

        // Set viewport above active by scrolling up one.
        s.scroll(Scroll::DeltaRow(-1));
        // The viewport should be a pin now.
        assert_eq!(Viewport::Top, s.viewport);

        // Resize
        s.resize(no_reflow(None, Some(7))).unwrap();
        assert_eq!(7, s.rows);
        assert_eq!(7, s.total_rows());

        // Question: maybe the viewport should actually be in the active
        // here and not pinned to the top.
        assert_eq!(Viewport::Top, s.viewport);
    }

    /// Assert every screen row has `cols` cells.
    fn check_row_widths(s: &PageList, cols: usize) {
        let mut it = s.row_iterator(Direction::RightDown, pt_screen(0, 0), None);
        while let Some(offset) = it.next() {
            let (row, _) = pin_rac(offset);
            let cells = cells_of(pg(offset.node), row);
            assert_eq!(cols, cells.len());
        }
    }

    #[test]
    fn page_list_resize_no_reflow_less_cols() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });

        // Resize
        s.resize(no_reflow(Some(5), None)).unwrap();
        assert_eq!(5, s.cols);
        assert_eq!(10, s.total_rows());

        check_row_widths(&s, 5);
    }

    #[test]
    fn page_list_resize_no_reflow_less_cols_pin_in_trimmed_cols() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(8, 2)).unwrap()).unwrap();

        // Resize
        s.resize(no_reflow(Some(5), None)).unwrap();
        assert_eq!(5, s.cols);
        assert_eq!(10, s.total_rows());

        check_row_widths(&s, 5);

        assert_eq!(pt_active(4, 2), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_no_reflow_less_cols_clears_graphemes() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });

        // Add a grapheme.
        let page = pg(first(&s));
        {
            let (row, cell) = rac(page, 9, 0);
            *cell = cp('A');
            page.append_grapheme(row, cell, 'A' as u32).unwrap();
        }
        assert_eq!(1, page.grapheme_count());

        // Resize
        s.resize(no_reflow(Some(5), None)).unwrap();
        assert_eq!(5, s.cols);
        assert_eq!(10, s.total_rows());

        let mut it = s.page_iterator(Direction::RightDown, pt_screen(0, 0), None);
        while let Some(chunk) = it.next() {
            assert_eq!(0, pg(chunk.node).grapheme_count());
        }
    }

    #[test]
    fn page_list_resize_no_reflow_more_cols() {
        let mut s = init_opts(Options {
            cols: 5,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });

        // Resize
        s.resize(no_reflow(Some(10), None)).unwrap();
        assert_eq!(10, s.cols);
        assert_eq!(3, s.total_rows());

        check_row_widths(&s, 10);
    }

    #[test]
    fn page_list_resize_no_reflow_more_cols_with_spacer_head() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            {
                let (row, cell) = rac(page, 0, 0);
                row.set_wrap(true);
                *cell = cp('x');
            }
            *cell_at(page, 1, 0) = wide_cell('\0', Wide::SpacerHead);
            *cell_at(page, 0, 1) = wide_cell('😀', Wide::Wide);
            *cell_at(page, 1, 1) = wide_cell('\0', Wide::SpacerTail);
        }

        // Resize
        s.resize(no_reflow(Some(3), None)).unwrap();
        assert_eq!(3, s.cols);
        assert_eq!(3, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            {
                let cell = cell_at(page, 0, 0);
                assert_eq!('x' as u32, cell.content_codepoint());
                assert_eq!(Wide::Narrow, cell.wide());
                // assert!(!row.wrap());
            }
            {
                let cell = cell_at(page, 1, 0);
                assert_eq!(0, cell.content_codepoint());
                assert_eq!(Wide::Narrow, cell.wide());
            }
            {
                let cell = cell_at(page, 2, 0);
                assert_eq!(0, cell.content_codepoint());
                assert_eq!(Wide::Narrow, cell.wide());
            }
        }
    }

    // Regression test for fuzz crash. When we shrink cols and then
    // grow back, the page retains capacity from the original size so the grow
    // takes the fast path (just bumps page.size.cols). If any row has a
    // spacer_head at the old last column, that cell is no longer at the end
    // of the wider row, violating page integrity.
    #[test]
    fn page_list_resize_no_reflow_grow_cols_fast_path_with_spacer_head() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });

        // Shrink to 5 cols. The page keeps capacity for 10 cols.
        s.resize(no_reflow(Some(5), None)).unwrap();
        assert_eq!(5, s.cols);

        // Place a spacer_head at the last column (col 4) on two rows
        // to simulate a wide character that didn't fit at the right edge.
        {
            let page = pg(first(&s));

            // Row 0: 'x' at col 0..3, spacer_head at col 4, wrap = true
            *cell_at(page, 0, 0) = cp('x');
            {
                let (row, cell) = rac(page, 4, 0);
                *cell = wide_cell('\0', Wide::SpacerHead);
                row.set_wrap(true);
            }

            // Row 1: spacer_head at col 4, wrap = true
            {
                let (row, cell) = rac(page, 4, 1);
                *cell = wide_cell('\0', Wide::SpacerHead);
                row.set_wrap(true);
            }
        }

        // Grow back to 10 cols. This must not leave stale spacer_head
        // cells at col 4 (which is no longer the last column).
        s.resize(no_reflow(Some(10), None)).unwrap();
        assert_eq!(10, s.cols);

        // Verify the old spacer_head positions are now narrow.
        {
            let page = pg(first(&s));
            {
                let (row, cell) = rac(page, 4, 0);
                assert_eq!(Wide::Narrow, cell.wide());
                assert!(!row.wrap());
            }
            {
                let (row, cell) = rac(page, 4, 1);
                assert_eq!(Wide::Narrow, cell.wide());
                assert!(!row.wrap());
            }
        }
    }

    // This test is a bit convoluted so I want to explain: what we are trying
    // to verify here is that when we increase cols such that our rows per page
    // shrinks, we don't fragment our rows across many pages because this ends
    // up wasting a lot of memory.
    //
    // This is particularly important for alternate screen buffers where we
    // don't have scrollback so our max size is very small. If we don't do this,
    // we end up pruning our pages and that causes resizes to fail!
    #[test]
    fn page_list_resize_no_reflow_more_cols_forces_less_rows_per_page() {
        let adjust = |cols: CellCountInt| STD_CAPACITY.adjust(pagepkg::Adjustment { cols: Some(cols) }).unwrap();

        // This test requires initially that our rows fit into one page.
        let cols: CellCountInt = 5;
        let rows: CellCountInt = 150;
        assert!(adjust(cols).rows >= rows);
        let mut s = init_opts(Options {
            cols,
            rows,
            max_size: Some(0),
            ..Default::default()
        });

        // Then we need to resize our cols so that our rows per page shrinks.
        // This will force our resize to split our rows across two pages.
        {
            let new_cols = {
                let mut new_cols: CellCountInt = 50;
                let mut cap = adjust(new_cols);
                while cap.rows >= rows {
                    new_cols += 50;
                    cap = adjust(new_cols);
                }
                new_cols
            };
            s.resize(no_reflow(Some(new_cols), None)).unwrap();
            assert_eq!(new_cols, s.cols);
            assert_eq!(rows as usize, s.total_rows());
        }

        // Every page except the last should be full
        let check_full = |s: &PageList| {
            let mut it = s.pages.first;
            while let Some(page) = it {
                if page == last(s) {
                    break;
                }
                assert_eq!(n(page).capacity().rows, n(page).rows());
                it = n(page).next;
            }
        };
        check_full(&s);

        // Now we need to resize again to a col size that further shrinks
        // our last capacity.
        {
            let page = pg(first(&s));
            assert!(page.size.rows == page.capacity.rows);
            let new_cols = {
                let mut new_cols = page.size.cols + 50;
                let mut cap = adjust(new_cols);
                while cap.rows >= page.size.rows {
                    new_cols += 50;
                    cap = adjust(new_cols);
                }
                new_cols
            };

            s.resize(no_reflow(Some(new_cols), None)).unwrap();
            assert_eq!(new_cols, s.cols);
            assert_eq!(rows as usize, s.total_rows());
        }

        // Every page except the last should be full
        check_full(&s);
    }

    #[test]
    fn page_list_resize_no_reflow_less_cols_then_more_cols() {
        let mut s = init_opts(Options {
            cols: 5,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });

        // Resize less
        s.resize(no_reflow(Some(2), None)).unwrap();
        assert_eq!(2, s.cols);

        // Resize
        s.resize(no_reflow(Some(5), None)).unwrap();
        assert_eq!(5, s.cols);
        assert_eq!(3, s.total_rows());

        check_row_widths(&s, 5);
    }

    #[test]
    fn page_list_resize_no_reflow_less_rows_and_cols() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });

        // Resize less
        s.resize(no_reflow(Some(5), Some(7))).unwrap();
        assert_eq!(5, s.cols);
        assert_eq!(7, s.rows);

        check_row_widths(&s, 5);
    }

    #[test]
    fn page_list_resize_less_rows_and_cols_cursor_at_bottom() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(0),
            ..Default::default()
        });

        let cursor_pin = s
            .track_pin(s.pin(pt_active(0, s.rows as u32 - 1)).unwrap())
            .unwrap();

        // Shrink both axes such that the original cursor.y is strictly past the
        // new row count, so resizeWithoutReflow leaves self.rows < c.y + 1.
        s.resize(Resize {
            cols: Some(79),
            rows: Some(20),
            reflow: true,
            cursor: Some(ResizeCursor {
                x: 0,
                y: 23,
                pin: Some(cursor_pin),
            }),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(79, s.cols);
        assert_eq!(20, s.rows);

        // remaining_rows saturates to 0, so the cursor lands on the new bottom row.
        assert_eq!(
            pt_active(0, s.rows as u32 - 1),
            s.point_from_pin(Tag::Active, *tp(cursor_pin)).unwrap()
        );

        s.untrack_pin(cursor_pin);
    }

    #[test]
    fn page_list_resize_less_rows_and_cols_cursor_near_top_pushed_to_scrollback() {
        let mut s = init(80, 24);

        // Fill every active row with non-blank content so that shrinking rows
        // can't trim trailing blank lines and instead pushes the top rows into
        // scrollback.
        {
            let mut it = s.row_iterator(Direction::RightDown, pt_active(0, 0), None);
            while let Some(p) = it.next() {
                let (row, _) = pin_rac(p);
                let cells = cells_of(pg(p.node), row);
                for (x, cell) in cells.iter_mut().enumerate() {
                    *cell = cpu('A' as u32 + (x % 26) as u32);
                }
            }
        }

        // Cursor near the top of the active area. After we shrink rows the active
        // area top moves down past this pin, so it ends up in scrollback.
        let cursor_pin = s.track_pin(s.pin(pt_active(0, 0)).unwrap()).unwrap();

        // Shrink both axes with reflow. resizeWithoutReflow shrinks self.rows
        // first, leaving the cursor pin above the new active area, then resizeCols
        // walks .left_up from the cursor pin toward the active-area top.
        s.resize(Resize {
            cols: Some(79),
            rows: Some(20),
            reflow: true,
            cursor: Some(ResizeCursor {
                x: 0,
                y: 0,
                pin: Some(cursor_pin),
            }),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(79, s.cols);
        assert_eq!(20, s.rows);

        // The active area is anchored to the bottom, so shrinking rows pushed the
        // top-of-screen cursor into scrollback: it no longer resolves to an
        // active-area coordinate, but it remains a valid screen pin.
        assert!(s.point_from_pin(Tag::Active, *tp(cursor_pin)).is_none());
        assert!(s.point_from_pin(Tag::Screen, *tp(cursor_pin)).is_some());

        // Integrity must hold after the resize.
        s.assert_integrity();

        s.untrack_pin(cursor_pin);
    }

    #[test]
    fn page_list_resize_no_reflow_more_rows_and_less_cols() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });

        // Resize less
        s.resize(no_reflow(Some(5), Some(20))).unwrap();
        assert_eq!(5, s.cols);
        assert_eq!(20, s.rows);
        assert_eq!(20, s.total_rows());

        check_row_widths(&s, 5);
    }

    #[test]
    fn page_list_resize_more_rows_and_cols_doesn_t_fit_in_single_std_page() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });

        // Resize to a size that requires more than one page to fit our rows.
        let new_cols = 600;
        let new_rows = 600;
        let cap = STD_CAPACITY
            .adjust(pagepkg::Adjustment { cols: Some(new_cols) })
            .unwrap();
        assert!(cap.rows < new_rows);

        s.resize(Resize {
            cols: Some(new_cols),
            rows: Some(new_rows),
            reflow: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(new_cols, s.cols);
        assert_eq!(new_rows, s.rows);
        assert_eq!(new_rows as usize, s.total_rows());
    }

    #[test]
    fn page_list_resize_no_reflow_empty_screen() {
        let mut s = init_opts(Options {
            cols: 5,
            rows: 5,
            max_size: Some(0),
            ..Default::default()
        });

        // Resize
        s.resize(no_reflow(Some(10), Some(10))).unwrap();
        assert_eq!(10, s.cols);
        assert_eq!(10, s.rows);
        assert_eq!(10, s.total_rows());

        check_row_widths(&s, 10);
    }

    #[test]
    fn page_list_resize_no_reflow_more_cols_forces_smaller_cap() {
        // We want a cap that forces us to have less rows
        let cap = STD_CAPACITY.adjust(pagepkg::Adjustment { cols: Some(100) }).unwrap();
        let cap2 = STD_CAPACITY.adjust(pagepkg::Adjustment { cols: Some(500) }).unwrap();
        assert_eq!(500, cap2.cols);
        assert!(cap2.rows < cap.rows);

        // Create initial cap, fits in one page
        let mut s = init(cap.cols, cap.rows);
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        for y in 0..s.rows as usize {
            for x in 0..s.cols as usize {
                *cell_at(page, x, y) = cp('A');
            }
        }

        // Resize to our large cap
        let rows = s.total_rows();
        s.resize(no_reflow(Some(cap2.cols), None)).unwrap();

        // Our total rows should be the same, and contents should be the same.
        assert_eq!(rows, s.total_rows());
        let mut it = s.row_iterator(Direction::RightDown, pt_screen(0, 0), None);
        while let Some(offset) = it.next() {
            let (row, _) = pin_rac(offset);
            let cells = cells_of(pg(offset.node), row);
            assert_eq!(cap2.cols as usize, cells.len());
            assert_eq!('A' as u32, cells[0].content_codepoint());
        }
    }

    #[test]
    fn page_list_resize_no_reflow_more_rows_adds_blank_rows_if_cursor_at_bottom() {
        let mut s = init(5, 3);

        // Grow to 5 total rows, simulating 3 active + 2 scrollback
        s.grow_rows(2).unwrap();
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        for y in 0..s.total_rows() {
            *cell_at(page, 0, y) = cpu(y as u32);
        }

        // Active should be on row 3
        assert_eq!(pt_screen(0, 2), s.get_cell(pt_active(0, 0)).unwrap().screen_point());

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(0, s.rows as u32 - 2)).unwrap()).unwrap();
        let original_cursor = s.point_from_pin(Tag::Active, *tp(p)).unwrap().coord();
        {
            let get = s.get_cell(pt_active(original_cursor.x, original_cursor.y)).unwrap();
            assert_eq!(3, unsafe { (*get.cell).content_codepoint() });
        }

        // Resize
        s.resize_without_reflow(Resize {
            rows: Some(10),
            reflow: false,
            cursor: Some(ResizeCursor::new(0, s.rows - 2)),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(5, s.cols);
        assert_eq!(10, s.rows);

        // Our cursor should not change
        assert_eq!(original_cursor, s.point_from_pin(Tag::Active, *tp(p)).unwrap().coord());

        // 12 because we have our 10 rows in the active + 2 in the scrollback
        // because we're preserving the cursor.
        assert_eq!(12, s.total_rows());

        // Active should be at the same place it was.
        assert_eq!(pt_screen(0, 2), s.get_cell(pt_active(0, 0)).unwrap().screen_point());

        // Go through our active, we should get only 3,4,5
        for y in 0..3u32 {
            let get = s.get_cell(pt_active(0, y)).unwrap();
            let expected = y + 2;
            assert_eq!(expected, unsafe { (*get.cell).content_codepoint() });
        }

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_reflow_from_max_cols_to_narrow() {
        // A max-width page is large enough that narrowing it to 2 columns
        // frees memory for more rows than a page can hold. The resize must
        // cap the row count instead of overflowing it.
        let mut s = init_opts(Options {
            cols: CellCountInt::MAX,
            rows: 1,
            max_size: Some(0),
            ..Default::default()
        });

        s.resize(Resize {
            cols: Some(2),
            reflow: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(2, s.cols);
        s.assert_integrity();
    }

    fn reflow_cols(cols: CellCountInt) -> Resize {
        Resize {
            cols: Some(cols),
            reflow: true,
            ..Default::default()
        }
    }

    fn set_wrap(page: &Page, y: usize) {
        row_at(page, y).set_wrap(true);
    }

    fn set_wrap_cont(page: &Page, y: usize) {
        row_at(page, y).set_wrap_continuation(true);
    }

    /// Fill cells `0..cols` of row `y` with codepoint `x`.
    fn fill_row_x(page: &Page, y: usize, cols: usize) {
        for x in 0..cols {
            *cell_at(page, x, y) = cpu(x as u32);
        }
    }

    /// Fill cells `0..cols` of row `y` with `c`.
    fn fill_row(page: &Page, y: usize, cols: usize, c: char) {
        for x in 0..cols {
            *cell_at(page, x, y) = cp(c);
        }
    }

    /// The codepoints of the row at the pin.
    fn row_cps(p: Pin) -> Vec<u32> {
        pin_cells(p, CellSubset::All).iter().map(|c| c.content_codepoint()).collect()
    }

    /// Assert the codepoints at the start of the row at `p`.
    fn check_cps(p: Pin, expected: &[u32]) {
        assert_eq!(expected, &row_cps(p)[..expected.len()]);
    }

    fn check_no_text(p: Pin, n_cells: usize) {
        let cells = pin_cells(p, CellSubset::All);
        for cell in &cells[..n_cells] {
            assert!(!cell.has_text());
        }
    }

    /// Fill the first page to capacity then grow `extra` more rows.
    fn grow_first_page_full(s: &mut PageList, extra: usize) {
        let page = pg(first(s));
        page.pause_integrity_checks(true);
        for _ in page.size.rows..page.capacity.rows {
            s.grow().unwrap();
        }
        page.pause_integrity_checks(false);
        assert_eq!(1, s.total_pages());
        s.grow_rows(extra).unwrap();
        assert_eq!(2, s.total_pages());
    }

    #[test]
    fn page_list_resize_reflow_more_cols_no_wrapped_rows() {
        let mut s = init_opts(Options {
            cols: 5,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        for y in 0..s.rows as usize {
            fill_row(page, y, s.cols as usize, 'A');
        }

        // Resize
        s.resize(reflow_cols(10)).unwrap();
        assert_eq!(10, s.cols);
        assert_eq!(3, s.total_rows());

        let mut it = s.row_iterator(Direction::RightDown, pt_screen(0, 0), None);
        while let Some(offset) = it.next() {
            let cells = pin_cells(offset, CellSubset::All);
            assert_eq!(10, cells.len());
            assert_eq!('A' as u32, cells[0].content_codepoint());
        }
    }

    #[test]
    fn page_list_resize_reflow_more_cols_wrapped_rows() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 4,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        for y in 0..s.rows as usize {
            if y % 2 == 0 {
                set_wrap(page, y);
            } else {
                set_wrap_cont(page, y);
            }
            fill_row(page, y, s.cols as usize, 'A');
        }

        // Resize
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(4, s.total_rows());

        // Active should still be on top
        assert_eq!(pt_screen(0, 0), s.get_cell(pt_active(0, 0)).unwrap().screen_point());

        let mut it = s.row_iterator(Direction::RightDown, pt_screen(0, 0), None);
        {
            // First row should be unwrapped
            let offset = it.next().unwrap();
            let (row, _) = pin_rac(offset);
            let cells = pin_cells(offset, CellSubset::All);
            assert!(!row.wrap());
            assert_eq!(4, cells.len());
            assert_eq!('A' as u32, cells[0].content_codepoint());
            assert_eq!('A' as u32, cells[2].content_codepoint());
        }
    }

    #[test]
    fn page_list_resize_reflow_invalidates_viewport_offset_cache() {
        let mut s = init(2, 4);
        s.grow_rows(20).unwrap();

        let page = pg(last(&s));
        for y in 0..s.rows as usize {
            if y % 2 == 0 {
                set_wrap(page, y);
            } else {
                set_wrap_cont(page, y);
            }
            fill_row(page, y, s.cols as usize, 'A');
        }

        // Scroll to a pinned viewport in history
        let pin_y = 10;
        s.scroll(Scroll::Pin(s.pin(pt_screen(0, pin_y as u32)).unwrap()));
        assert!(s.viewport == Viewport::Pin);
        assert_eq!(sb(s.total_rows, pin_y, s.rows as usize), s.scrollbar());

        // Resize with reflow - unwrapping rows changes total_rows
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);

        // Verify scrollbar cache was invalidated during reflow
        assert_eq!(sb(s.total_rows, 5, s.rows as usize), s.scrollbar());
    }

    #[test]
    fn page_list_resize_reflow_more_cols_creates_multiple_pages() {
        // We want a wide viewport so our row limit is rather small. This will
        // force the reflow below to create multiple pages, which we assert.
        let cap = {
            let mut current: CellCountInt = 100;
            loop {
                let cap = STD_CAPACITY
                    .adjust(pagepkg::Adjustment { cols: Some(current) })
                    .unwrap();
                if cap.rows < 100 {
                    break cap;
                }
                current += 100;
            }
        };

        let mut s = init(cap.cols, cap.rows);

        // Wrap every other row so every line is wrapped for reflow
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            for y in 0..s.rows as usize {
                if y % 2 == 0 {
                    set_wrap(page, y);
                } else {
                    set_wrap_cont(page, y);
                }
                *cell_at(page, 0, y) = cp('A');
            }
        }

        // Resize
        let newcap = cap
            .adjust(pagepkg::Adjustment {
                cols: Some(cap.cols + 100),
            })
            .unwrap();
        assert!(newcap.rows < cap.rows);
        s.resize(reflow_cols(newcap.cols)).unwrap();
        assert_eq!(newcap.cols, s.cols);
        assert_eq!(cap.rows as usize, s.total_rows());

        {
            let mut count: usize = 0;
            let mut it = s.pages.first;
            while let Some(page) = it {
                count += 1;

                // All pages should have the new capacity
                assert_eq!(newcap.cols, n(page).capacity().cols);
                assert_eq!(newcap.rows, n(page).capacity().rows);
                it = n(page).next;
            }

            // We should have more than one page, meaning we created at least
            // one page. This is the critical aspect of this test so if this
            // ever goes false we need to adjust this test.
            assert!(count > 1);
        }
    }

    /// Wrap the last row of the first page into the first row of the second page.
    fn wrap_across_boundary(s: &PageList) {
        {
            let page = pg(first(s));
            let y = page.size.rows as usize - 1;
            set_wrap(page, y);
            fill_row_x(page, y, s.cols as usize);
        }
        {
            let page2 = pg(last(s));
            set_wrap_cont(page2, 0);
            fill_row_x(page2, 0, s.cols as usize);
        }
    }

    #[test]
    fn page_list_resize_reflow_more_cols_wrap_across_page_boundary() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(1, s.total_pages());

        // Grow to the capacity of the first page.
        grow_first_page_full(&mut s, 1);

        // At this point, we have some rows on the first page, and some on the second.
        // We can now wrap across the boundary condition.
        wrap_across_boundary(&s);

        // We expect one fewer rows since we unwrapped a row.
        let end_rows = s.total_rows() - 1;

        // Resize
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(end_rows, s.total_rows());

        {
            // PAGE 1 ROW 6280, ACTIVE 8
            let p = s.pin(pt_active(0, 8)).unwrap();
            let (row, _) = pin_rac(p);
            assert!(!row.wrap());
            assert!(!row.wrap_continuation());
            check_no_text(p, 4);
        }
        {
            // PAGE 1 ROW 6281, ACTIVE 9
            let p = s.pin(pt_active(0, 9)).unwrap();
            let (row, _) = pin_rac(p);
            assert!(!row.wrap());
            assert!(!row.wrap_continuation());
            check_cps(p, &[0, 1, 0, 1]);
        }
    }

    #[test]
    fn page_list_resize_reflow_more_cols_wrap_across_page_boundary_cursor_in_second_page() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(1, s.total_pages());

        // Grow to the capacity of the first page.
        grow_first_page_full(&mut s, 1);

        // At this point, we have some rows on the first page, and some on the second.
        // We can now wrap across the boundary condition.
        wrap_across_boundary(&s);

        // Put a tracked pin in wrapped row on the last page
        let p = s.track_pin(s.pin(pt_active(1, 9)).unwrap()).unwrap();
        assert!(tp(p).node == last(&s));

        // We expect one fewer rows since we unwrapped a row.
        let end_rows = s.total_rows() - 1;

        // Resize
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(end_rows, s.total_rows());

        // Our cursor should move to the first row
        assert_eq!(pt_active(3, 9), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        {
            let p2 = s.pin(pt_active(0, 9)).unwrap();
            let (row, _) = pin_rac(p2);
            assert!(!row.wrap());
            check_cps(p2, &[0, 1, 0, 1]);
        }

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_reflow_less_cols_wrap_across_page_boundary_cursor_in_second_page() {
        let mut s = init(5, 10);
        assert_eq!(1, s.total_pages());

        // Grow to the capacity of the first page.
        grow_first_page_full(&mut s, 5);

        // At this point, we have some rows on the first page, and some on the second.
        // We can now wrap across the boundary condition.
        wrap_across_boundary(&s);

        // Put a tracked pin in wrapped row on the last page
        let p = s.track_pin(s.pin(pt_active(2, 5)).unwrap()).unwrap();
        assert!(tp(p).node == last(&s));
        assert!(tp(p).y == 0);

        // Resize
        s.resize(Resize {
            cols: Some(4),
            reflow: true,
            cursor: Some(ResizeCursor::new(2, 5)),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(4, s.cols);

        // Our cursor should remain on the same cell
        assert_eq!(pt_active(3, 5), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        {
            // PAGE 0 ROW 7895, ACTIVE 3
            let p2 = s.pin(pt_active(0, 3)).unwrap();
            let (row, _) = pin_rac(p2);
            assert!(!row.wrap());
            assert!(!row.wrap_continuation());
            check_no_text(p2, 4);
        }
        {
            // PAGE 0 ROW 7896, ACTIVE 4
            let p2 = s.pin(pt_active(0, 4)).unwrap();
            let (row, _) = pin_rac(p2);
            assert!(row.wrap());
            assert!(!row.wrap_continuation());
            check_cps(p2, &[0, 1, 2, 3]);
        }
        {
            // PAGE 0 ROW 7897, ACTIVE 5
            let p2 = s.pin(pt_active(0, 5)).unwrap();
            let (row, _) = pin_rac(p2);
            assert!(row.wrap());
            assert!(row.wrap_continuation());
            check_cps(p2, &[4, 0, 1, 2]);
        }
        {
            // PAGE 0 ROW 7898, ACTIVE 6
            let p2 = s.pin(pt_active(0, 6)).unwrap();
            let (row, _) = pin_rac(p2);
            assert!(!row.wrap());
            assert!(row.wrap_continuation());
            check_cps(p2, &[3, 4]);
        }
        {
            // PAGE 0 ROW 7899, ACTIVE 7
            let p2 = s.pin(pt_active(0, 7)).unwrap();
            let (row, _) = pin_rac(p2);
            assert!(!row.wrap());
            assert!(!row.wrap_continuation());
            check_no_text(p2, 4);
        }

        s.untrack_pin(p);
    }

    /// Two rows: row 0 wrapped, row 1 continuation, both filled with `x`.
    fn two_wrapped_rows(s: &PageList) {
        let page = pg(first(s));
        set_wrap(page, 0);
        fill_row_x(page, 0, s.cols as usize);
        set_wrap_cont(page, 1);
        fill_row_x(page, 1, s.cols as usize);
    }

    #[test]
    fn page_list_resize_reflow_more_cols_cursor_in_wrapped_row() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 4,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        two_wrapped_rows(&s);

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(1, 1)).unwrap()).unwrap();

        // Resize
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(4, s.total_rows());

        // Our cursor should move to the first row
        assert_eq!(pt_active(3, 0), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_reflow_more_cols_cursor_in_not_wrapped_row() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 4,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        two_wrapped_rows(&s);

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(1, 0)).unwrap()).unwrap();

        // Resize
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(4, s.total_rows());

        // Our cursor should move to the first row
        assert_eq!(pt_active(1, 0), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_reflow_more_cols_cursor_in_wrapped_row_that_isn_t_unwrapped() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 4,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        set_wrap(page, 0);
        fill_row_x(page, 0, s.cols as usize);
        set_wrap(page, 1);
        set_wrap_cont(page, 1);
        fill_row_x(page, 1, s.cols as usize);
        set_wrap_cont(page, 2);
        fill_row_x(page, 2, s.cols as usize);

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(1, 2)).unwrap()).unwrap();

        // Resize
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(4, s.total_rows());

        // Our cursor should move to the first row
        assert_eq!(pt_active(1, 1), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_reflow_more_cols_no_reflow_preserves_semantic_prompt() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 4,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            set_prompt(page, 1, SemanticPrompt::Prompt);
        }

        // Resize
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(4, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            assert!(row_at(page, 1).semantic_prompt() == SemanticPrompt::Prompt);
        }
    }

    /// Fill the first page to capacity and add one more row so that we have
    /// two pages total (the shared setup of the capacity-increase reflow tests).
    fn two_pages_setup(s: &mut PageList) {
        grow_first_page_full(s, 1);

        // We now have two pages.
        assert!(first(s) != last(s));
        assert_eq!(s.pages.last, n(first(s)).next);
    }

    fn hl_link<'a>(id: hyperlink::HyperlinkId<'a>, uri: &'a [u8]) -> hyperlink::Hyperlink<'a> {
        hyperlink::Hyperlink {
            id,
            uri: std::borrow::Cow::Borrowed(uri),
        }
    }

    #[test]
    fn page_list_resize_reflow_exceeds_hyperlink_memory_forcing_capacity_increase() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(1, s.total_pages());

        // Grow to the capacity of the first page and add
        // one more row so that we have two pages total.
        two_pages_setup(&mut s);

        // We use almost all string alloc capacity with a hyperlink in the final
        // row of the first page, and do the same on the first row of the second
        // page. We also mark the row as wrapped so that when we resize with more
        // cols the row unwraps and we have a single row that requires almost two
        // times the base string alloc capacity.
        //
        // This forces the reflow to increase capacity.

        let uri = vec![b'a'; pagepkg::STRING_BYTES_DEFAULT - 1];

        // Almost hit string alloc cap in bottom right of first page.
        // Mark the final row as wrapped.
        {
            let page = pg(first(&s));
            let id = page
                .insert_hyperlink(&hl_link(hyperlink::HyperlinkId::Implicit(0), &uri))
                .unwrap();
            let (row, cell) = rac(page, page.size.cols as usize - 1, page.size.rows as usize - 1);
            row.set_wrap(true);
            *cell = cp('X');
            page.set_hyperlink(row, cell, id).unwrap();
            assert_eq!(
                pagepkg::InsertHyperlinkError::StringsOutOfMemory,
                page.insert_hyperlink(&hl_link(
                    hyperlink::HyperlinkId::Implicit(1),
                    b"AAAAAAAAAAAAAAAAAAAAAAAAAA"
                ))
                .unwrap_err()
            );
        }

        // Almost hit string alloc cap in top left of second page.
        // Mark the first row as a wrap continuation.
        {
            let page = pg(last(&s));
            let id = page
                .insert_hyperlink(&hl_link(hyperlink::HyperlinkId::Implicit(1), &uri))
                .unwrap();
            let (row, cell) = rac(page, 0, 0);
            row.set_wrap_continuation(true);
            *cell = cp('X');
            page.set_hyperlink(row, cell, id).unwrap();
            assert_eq!(
                pagepkg::InsertHyperlinkError::StringsOutOfMemory,
                page.insert_hyperlink(&hl_link(
                    hyperlink::HyperlinkId::Implicit(2),
                    b"AAAAAAAAAAAAAAAAAAAAAAAAAA"
                ))
                .unwrap_err()
            );
        }

        // Resize to 1 column wider, unwrapping the row.
        s.resize(reflow_cols(s.cols + 1)).unwrap();
    }

    #[test]
    fn page_list_resize_reflow_hyperlink_dupe_string_alloc_chunk_rounding() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(1, s.total_pages());

        // Grow to the capacity of the first page and add
        // one more row so that we have two pages total.
        two_pages_setup(&mut s);

        // The string allocator hands out 32-byte chunks and every allocation
        // is rounded up to the chunk size independently. Duping a hyperlink
        // during reflow allocates the URI and the explicit ID separately, so
        // two separate allocations can require one more chunk than a single
        // combined allocation of the same total byte length.
        //
        // We arrange for the reflow target page to have exactly two free
        // chunks (64 bytes) remaining when a hyperlink with a 33-byte URI
        // (2 chunks) and a 31-byte explicit ID (1 chunk) is reflowed into
        // it. The combined byte length (64 bytes -> 2 chunks) fits, but the
        // separate allocations (3 chunks) do not, so the reflow must grow
        // the string capacity rather than panic or drop the hyperlink.
        //
        // The two hyperlinked cells are joined as a single wrapped row so
        // that they are always reflowed into the same target page.

        let uri_a = vec![b'a'; pagepkg::STRING_BYTES_DEFAULT - 64];
        let uri_b = [b'b'; 33];
        let id_b = [b'i'; 31];

        // Hyperlink A in the bottom right of the first page. Mark the final
        // row as wrapped.
        {
            let page = pg(first(&s));
            let id = page
                .insert_hyperlink(&hl_link(hyperlink::HyperlinkId::Implicit(0), &uri_a))
                .unwrap();
            let (row, cell) = rac(page, page.size.cols as usize - 1, page.size.rows as usize - 1);
            row.set_wrap(true);
            *cell = cp('A');
            page.set_hyperlink(row, cell, id).unwrap();

            // Sanity check the chunk math: the remaining 64 bytes fit as a
            // single allocation but not as the two separate allocations that
            // inserting (or duping) hyperlink B performs.
            let buf = unsafe { page.string_alloc.alloc::<u8>(page.memory, 64) }.unwrap();
            unsafe { page.string_alloc.free(page.memory, buf) };
            assert_eq!(
                pagepkg::InsertHyperlinkError::StringsOutOfMemory,
                page.insert_hyperlink(&hl_link(
                    hyperlink::HyperlinkId::Explicit(std::borrow::Cow::Borrowed(&id_b)),
                    &uri_b
                ))
                .unwrap_err()
            );
        }

        // Hyperlink B in the top left of the second page. Mark the first
        // row as a wrap continuation.
        {
            let page = pg(last(&s));
            let id = page
                .insert_hyperlink(&hl_link(
                    hyperlink::HyperlinkId::Explicit(std::borrow::Cow::Borrowed(&id_b)),
                    &uri_b,
                ))
                .unwrap();
            let (row, cell) = rac(page, 0, 0);
            row.set_wrap_continuation(true);
            *cell = cp('B');
            page.set_hyperlink(row, cell, id).unwrap();
        }

        // Resize to 1 column wider, unwrapping the row.
        s.resize(reflow_cols(s.cols + 1)).unwrap();

        // Both hyperlinks must have survived the reflow intact.
        let mut found: usize = 0;
        let mut node_it = s.pages.first;
        while let Some(node) = node_it {
            let page = pg(node);
            for y in 0..page.size.rows as usize {
                for x in 0..page.size.cols as usize {
                    let cell = cell_at(page, x, y);
                    if !cell.hyperlink() {
                        continue;
                    }
                    found += 1;

                    let link_id = page.lookup_hyperlink(cell).unwrap();
                    let entry = unsafe { &*page.hyperlink_set.get(page.memory, link_id) };
                    let uri = unsafe { entry.uri.slice(page.memory) };
                    match entry.id {
                        hyperlink::PageEntryId::Implicit(_) => assert_eq!(&uri_a[..], uri),
                        hyperlink::PageEntryId::Explicit(slice) => {
                            assert_eq!(&uri_b[..], uri);
                            assert_eq!(&id_b[..], unsafe { slice.slice(page.memory) });
                        }
                    }
                }
            }
            node_it = n(node).next;
        }
        assert_eq!(2, found);
    }

    #[test]
    fn page_list_resize_reflow_exceeds_grapheme_memory_forcing_capacity_increase() {
        let mut s = init_opts(Options {
            cols: 4,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(1, s.total_pages());

        // Grow to the capacity of the first page and add
        // one more row so that we have two pages total.
        two_pages_setup(&mut s);

        // We use all grapheme alloc capacity with four maximum-sized graphemes on
        // each page. The two rows form one wrapped logical line across the page
        // boundary, so resizing wider moves all eight graphemes into one page and
        // requires almost two times the base grapheme alloc capacity.
        //
        // This forces the reflow to increase capacity.

        let suffixes = [b'a' as u32; pagepkg::GRAPHEME_MAX_LEN];

        // Fill the final row of the first page and mark it as wrapped.
        {
            let page = pg(first(&s));
            let y = page.size.rows as usize - 1;
            row_at(page, y).set_wrap(true);

            for x in 0..page.size.cols as usize {
                let (row, cell) = rac(page, x, y);
                *cell = cp('X');
                page.set_graphemes(row, cell, &suffixes).unwrap();
            }
            assert_eq!(page.grapheme_alloc.capacity_bytes(), unsafe {
                page.grapheme_alloc.used_bytes(page.memory)
            });
            assert!(unsafe { page.grapheme_alloc.alloc::<u32>(page.memory, 16) }.is_err());
        }

        // Fill the first row of the second page and mark it as a continuation.
        {
            let page = pg(last(&s));
            row_at(page, 0).set_wrap_continuation(true);

            for x in 0..page.size.cols as usize {
                let (row, cell) = rac(page, x, 0);
                *cell = cp('X');
                page.set_graphemes(row, cell, &suffixes).unwrap();
            }
            assert!(unsafe { page.grapheme_alloc.alloc::<u32>(page.memory, 16) }.is_err());
        }

        // Resize to 1 column wider, unwrapping the row.
        s.resize(reflow_cols(s.cols + 1)).unwrap();
    }

    #[test]
    fn page_list_resize_reflow_exceeds_style_memory_forcing_capacity_increase() {
        let mut s = init_opts(Options {
            cols: pagepkg::STD_CAPACITY.styles - 1,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(1, s.total_pages());

        // Grow to the capacity of the first page and add
        // one more row so that we have two pages total.
        two_pages_setup(&mut s);

        let rgb = |x: usize| pagepkg::Rgb {
            r: x as u8,
            g: (x >> 8) as u8,
            b: (x >> 16) as u8,
        };

        // Give each cell in the final row of the first page a unique style.
        // Mark the final row as wrapped.
        {
            let page = pg(first(&s));
            for x in 0..s.cols as usize {
                let style = stylepkg::Style {
                    bg_color: stylepkg::Color::Rgb(rgb(x)),
                    ..Default::default()
                };
                let Ok(id) = (unsafe { page.styles.add(page.memory, style) }) else { break };

                let (row, cell) = rac(page, x, page.size.rows as usize - 1);
                row.set_wrap(true);
                row.set_styled(true);
                let mut c = cp('X');
                c.set_style_id(id);
                *cell = c;
            }
        }

        // Do the same for the first row of the second page.
        // Mark the first row as a wrap continuation.
        {
            let page = pg(last(&s));
            for x in 0..s.cols as usize {
                let style = stylepkg::Style {
                    fg_color: stylepkg::Color::Rgb(rgb(x)),
                    ..Default::default()
                };
                let Ok(id) = (unsafe { page.styles.add(page.memory, style) }) else { break };

                let (row, cell) = rac(page, x, 0);
                row.set_wrap_continuation(true);
                row.set_styled(true);
                let mut c = cp('X');
                c.set_style_id(id);
                *cell = c;
            }
        }

        // Resize to twice as wide, fully unwrapping the row.
        s.resize(reflow_cols(s.cols * 2)).unwrap();
    }

    fn check_cell(page: &Page, x: usize, y: usize, cp: u32, wide: Wide) {
        let cell = cell_at(page, x, y);
        assert_eq!(cp, cell.content_codepoint());
        assert_eq!(wide, cell.wide());
    }

    #[test]
    fn page_list_resize_reflow_more_cols_unwrap_wide_spacer_head() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            set_wrap(page, 0);
            *cell_at(page, 0, 0) = cp('x');
            *cell_at(page, 1, 0) = wide_cell('\0', Wide::SpacerHead);
            set_wrap_cont(page, 1);
            *cell_at(page, 0, 1) = wide_cell('😀', Wide::Wide);
            *cell_at(page, 1, 1) = wide_cell('\0', Wide::SpacerTail);
        }

        // Resize
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(2, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            check_cell(page, 0, 0, 'x' as u32, Wide::Narrow);
            assert!(!row_at(page, 0).wrap());
            check_cell(page, 1, 0, '😀' as u32, Wide::Wide);
            check_cell(page, 2, 0, 0, Wide::SpacerTail);
        }
    }

    #[test]
    fn page_list_resize_reflow_more_cols_unwrap_wide_spacer_head_across_two_rows() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            set_wrap(page, 0);
            *cell_at(page, 0, 0) = cp('x');
            *cell_at(page, 1, 0) = cp('x');
            set_wrap_cont(page, 1);
            set_wrap(page, 1);
            *cell_at(page, 0, 1) = cp('x');
            *cell_at(page, 1, 1) = wide_cell('\0', Wide::SpacerHead);
            set_wrap_cont(page, 2);
            *cell_at(page, 0, 2) = wide_cell('😀', Wide::Wide);
            *cell_at(page, 1, 2) = wide_cell('\0', Wide::SpacerTail);
        }

        // Resize
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(3, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            check_cell(page, 0, 0, 'x' as u32, Wide::Narrow);
            assert!(row_at(page, 0).wrap());
            check_cell(page, 1, 0, 'x' as u32, Wide::Narrow);
            check_cell(page, 2, 0, 'x' as u32, Wide::Narrow);
            check_cell(page, 3, 0, 0, Wide::SpacerHead);
            check_cell(page, 0, 1, '😀' as u32, Wide::Wide);
            check_cell(page, 1, 1, 0, Wide::SpacerTail);
        }
    }

    #[test]
    fn page_list_resize_reflow_more_cols_unwrap_still_requires_wide_spacer_head() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            set_wrap(page, 0);
            *cell_at(page, 0, 0) = cp('x');
            *cell_at(page, 1, 0) = cp('x');
            set_wrap_cont(page, 1);
            *cell_at(page, 0, 1) = wide_cell('😀', Wide::Wide);
            *cell_at(page, 1, 1) = wide_cell('\0', Wide::SpacerTail);
        }

        // Resize
        s.resize(reflow_cols(3)).unwrap();
        assert_eq!(3, s.cols);
        assert_eq!(2, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            check_cell(page, 0, 0, 'x' as u32, Wide::Narrow);
            assert!(row_at(page, 0).wrap());
            check_cell(page, 1, 0, 'x' as u32, Wide::Narrow);
            check_cell(page, 2, 0, 0, Wide::SpacerHead);
            check_cell(page, 0, 1, '😀' as u32, Wide::Wide);
            check_cell(page, 1, 1, 0, Wide::SpacerTail);
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_no_reflow_preserves_semantic_prompt() {
        let mut s = init_opts(Options {
            cols: 4,
            rows: 4,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            set_prompt(page, 1, SemanticPrompt::Prompt);
            fill_row_x(page, 1, s.cols as usize);
        }

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(4, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            {
                let p = s.pin(pt_active(0, 1)).unwrap();
                let (row, _) = pin_rac(p);
                assert!(row.wrap());
                assert!(row.semantic_prompt() == SemanticPrompt::Prompt);
            }
            {
                let p = s.pin(pt_active(0, 2)).unwrap();
                let (row, _) = pin_rac(p);
                assert!(row.semantic_prompt() == SemanticPrompt::Prompt);
            }
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_no_reflow_preserves_semantic_prompt_on_first_line() {
        let mut s = init_opts(Options {
            cols: 4,
            rows: 4,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            set_prompt(page, 0, SemanticPrompt::Prompt);
        }

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(4, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            assert!(row_at(page, 0).semantic_prompt() == SemanticPrompt::Prompt);
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_wrap_preserves_semantic_prompt() {
        let mut s = init_opts(Options {
            cols: 4,
            rows: 4,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            set_prompt(page, 0, SemanticPrompt::Prompt);
        }

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(4, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            assert!(row_at(page, 0).semantic_prompt() == SemanticPrompt::Prompt);
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_no_wrapped_rows() {
        let mut s = init_opts(Options {
            cols: 10,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        for y in 0..s.rows as usize {
            let end = 4;
            assert!(end < s.cols as usize);
            fill_row_x(page, y, 4);
        }

        // Resize
        s.resize(reflow_cols(5)).unwrap();
        assert_eq!(5, s.cols);
        assert_eq!(3, s.total_rows());

        let mut it = s.row_iterator(Direction::RightDown, pt_screen(0, 0), None);
        while let Some(offset) = it.next() {
            for x in 0..4 {
                let mut offset_copy = offset;
                offset_copy.x = x as CellCountInt;
                let cells = pin_cells(offset_copy, CellSubset::All);
                assert_eq!(5, cells.len());
                assert_eq!(x as u32, cells[x].content_codepoint());
            }
        }
    }

    /// Check the next row of a row iterator.
    fn check_next_row(it: &mut RowIterator, wrap: bool, len: usize, cp0: u32) -> Pin {
        let offset = it.next().unwrap();
        let (row, _) = pin_rac(offset);
        let cells = pin_cells(offset, CellSubset::All);
        assert_eq!(wrap, row.wrap());
        assert_eq!(len, cells.len());
        assert_eq!(cp0, cells[0].content_codepoint());
        offset
    }

    #[test]
    fn page_list_resize_reflow_less_cols_wrapped_rows() {
        let mut s = init(4, 2);
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        for y in 0..s.rows as usize {
            fill_row_x(page, y, s.cols as usize);
        }

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(4, s.total_rows());

        // Active moves due to scrollback
        assert_eq!(pt_screen(0, 2), s.get_cell(pt_active(0, 0)).unwrap().screen_point());

        let mut it = s.row_iterator(Direction::RightDown, pt_screen(0, 0), None);
        // First row should be wrapped
        check_next_row(&mut it, true, 2, 0);
        check_next_row(&mut it, false, 2, 2);
        // First row should be wrapped
        check_next_row(&mut it, true, 2, 0);
        check_next_row(&mut it, false, 2, 2);
    }

    #[test]
    fn page_list_resize_reflow_less_cols_wrapped_rows_with_graphemes() {
        let mut s = init(4, 2);
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            for y in 0..s.rows as usize {
                fill_row_x(page, y, s.cols as usize);

                let (row, cell) = rac(page, 2, y);
                page.append_grapheme(row, cell, 'A' as u32).unwrap();
            }
        }

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(4, s.total_rows());

        // Active moves due to scrollback
        assert_eq!(pt_screen(0, 2), s.get_cell(pt_active(0, 0)).unwrap().screen_point());

        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        let mut it = s.row_iterator(Direction::RightDown, pt_screen(0, 0), None);
        let check_grapheme = |offset: Pin| {
            let (row, cell) = pin_rac(offset);
            assert!(row.grapheme());
            let cps = page.lookup_grapheme(cell).unwrap();
            assert_eq!(1, cps.len());
            assert_eq!('A' as u32, cps[0]);
        };
        // First row should be wrapped
        check_next_row(&mut it, true, 2, 0);
        check_grapheme(check_next_row(&mut it, false, 2, 2));
        // First row should be wrapped
        check_next_row(&mut it, true, 2, 0);
        check_grapheme(check_next_row(&mut it, false, 2, 2));
    }

    #[test]
    fn page_list_resize_reflow_less_cols_cursor_in_wrapped_row() {
        let mut s = init(4, 2);
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        for y in 0..s.rows as usize {
            fill_row_x(page, y, s.cols as usize);
        }

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(2, 1)).unwrap()).unwrap();

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(4, s.total_rows());

        // Our cursor should move to the first row
        assert_eq!(pt_active(0, 1), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_reflow_less_cols_wraps_spacer_head() {
        let mut s = init_opts(Options {
            cols: 4,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            set_wrap(page, 0);
            *cell_at(page, 0, 0) = cp('x');
            *cell_at(page, 1, 0) = cp('x');
            *cell_at(page, 2, 0) = cp('x');
            *cell_at(page, 3, 0) = wide_cell('\0', Wide::SpacerHead);
            set_wrap_cont(page, 1);
            *cell_at(page, 0, 1) = wide_cell('😀', Wide::Wide);
            *cell_at(page, 1, 1) = wide_cell('\0', Wide::SpacerTail);
        }

        // Resize
        s.resize(reflow_cols(3)).unwrap();
        assert_eq!(3, s.cols);
        assert_eq!(3, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            check_cell(page, 0, 0, 'x' as u32, Wide::Narrow);
            assert!(row_at(page, 0).wrap());
            check_cell(page, 1, 0, 'x' as u32, Wide::Narrow);
            check_cell(page, 2, 0, 'x' as u32, Wide::Narrow);
            check_cell(page, 0, 1, '😀' as u32, Wide::Wide);
            check_cell(page, 1, 1, 0, Wide::SpacerTail);
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_cursor_goes_to_scrollback() {
        let mut s = init(4, 2);
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        for y in 0..s.rows as usize {
            fill_row_x(page, y, s.cols as usize);
        }

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(2, 0)).unwrap()).unwrap();

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(4, s.total_rows());

        // Our cursor should move to the first row
        assert!(s.point_from_pin(Tag::Active, *tp(p)).is_none());

        s.untrack_pin(p);
    }

    /// Pin test helper: fill cols 0..2 of every row, track a pin at
    /// (pin_x, 0), reflow to `new_cols` and return the pin's new point.
    fn reflow_blank_cell_pin(cols: CellCountInt, pin_x: CellCountInt, new_cols: CellCountInt, total: usize) -> Point {
        let mut s = init(cols, 2);
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        for y in 0..s.rows as usize {
            fill_row_x(page, y, 2);
        }

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(pin_x, 0)).unwrap()).unwrap();

        // Resize
        s.resize(reflow_cols(new_cols)).unwrap();
        assert_eq!(new_cols, s.cols);
        assert_eq!(total, s.total_rows());

        let pt = s.point_from_pin(Tag::Active, *tp(p)).unwrap();
        s.untrack_pin(p);
        pt
    }

    #[test]
    fn page_list_resize_reflow_less_cols_cursor_in_unchanged_row() {
        // Our cursor should move to the first row
        assert_eq!(pt_active(1, 0), reflow_blank_cell_pin(4, 1, 2, 2));
    }

    #[test]
    fn page_list_resize_reflow_less_cols_cursor_in_blank_cell() {
        // Our cursor should not move
        assert_eq!(pt_active(2, 0), reflow_blank_cell_pin(6, 2, 4, 2));
    }

    #[test]
    fn page_list_resize_reflow_less_cols_cursor_in_final_blank_cell() {
        // Our cursor should move to the first row
        assert_eq!(pt_active(3, 0), reflow_blank_cell_pin(6, 3, 4, 2));
    }

    #[test]
    fn page_list_resize_reflow_pin_in_blank_cells_after_line_break() {
        struct Case {
            first: &'static [u8],
            wrap: bool,
            pin_x: CellCountInt,
            cols: CellCountInt,
            expected: Coordinate,
        }
        let cases = [
            // A hard line break starts the pin row at column zero.
            Case {
                first: b"abc",
                wrap: false,
                pin_x: 5,
                cols: 8,
                expected: coord(5, 1),
            },
            // A soft continuation uses the remaining destination columns.
            Case {
                first: b"abcdef",
                wrap: true,
                pin_x: 3,
                cols: 8,
                expected: coord(7, 0),
            },
            // Pending wrap starts the continuation at column zero.
            Case {
                first: b"abcdef",
                wrap: true,
                pin_x: 2,
                cols: 3,
                expected: coord(2, 2),
            },
        ];

        for case in cases {
            let mut s = init(6, 4);
            let page = pg(first(&s));
            row_at(page, 0).set_wrap(case.wrap);
            for (x, &c) in case.first.iter().enumerate() {
                *cell_at(page, x, 0) = cpu(c as u32);
            }
            let (row, cell) = rac(page, 0, 1);
            row.set_wrap_continuation(case.wrap);
            *cell = cp('g');

            let p = s.track_pin(s.pin(pt_active(case.pin_x, 1)).unwrap()).unwrap();

            s.resize(reflow_cols(case.cols)).unwrap();
            assert_eq!(Point::Active(case.expected), s.point_from_pin(Tag::Active, *tp(p)).unwrap());
            assert!(pin_rac(*tp(p)).1.is_empty());

            s.untrack_pin(p);
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_cursor_in_wrapped_blank_cell() {
        // Our cursor should move to the first row
        assert_eq!(pt_active(3, 0), reflow_blank_cell_pin(6, 5, 4, 2));
    }

    #[test]
    fn page_list_resize_reflow_less_cols_blank_lines() {
        let mut s = init_opts(Options {
            cols: 4,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        fill_row_x(page, 0, 4);

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(3, s.total_rows());

        let mut it = s.row_iterator(Direction::RightDown, pt_active(0, 0), None);
        // First row should be wrapped
        check_next_row(&mut it, true, 2, 0);
        check_next_row(&mut it, false, 2, 2);
    }

    #[test]
    fn page_list_resize_reflow_less_cols_blank_lines_between() {
        let mut s = init_opts(Options {
            cols: 4,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        fill_row_x(page, 0, 4);
        fill_row_x(page, 2, 4);

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(5, s.total_rows());

        let mut it = s.row_iterator(Direction::RightDown, pt_active(0, 0), None);
        {
            let offset = it.next().unwrap();
            assert!(!pin_rac(offset).0.wrap());
        }
        check_next_row(&mut it, true, 2, 0);
        check_next_row(&mut it, false, 2, 2);
    }

    fn wcu(c: u32, wide: Wide) -> PCell {
        let mut cell = PCell::init(c);
        cell.set_wide(wide);
        cell
    }

    #[test]
    fn page_list_resize_reflow_less_cols_blank_lines_between_no_scrollback() {
        let mut s = init_opts(Options {
            cols: 5,
            rows: 3,
            max_size: Some(0),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        *cell_at(page, 0, 0) = cp('A');
        *cell_at(page, 0, 2) = cp('C');

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(3, s.total_rows());

        let mut it = s.row_iterator(Direction::RightDown, pt_active(0, 0), None);
        check_next_row(&mut it, false, 2, 'A' as u32);
        {
            let offset = it.next().unwrap();
            let cells = pin_cells(offset, CellSubset::All);
            assert_eq!(0, cells[0].content_codepoint());
        }
        check_next_row(&mut it, false, 2, 'C' as u32);
    }

    #[test]
    fn page_list_resize_reflow_less_cols_cursor_not_on_last_line_preserves_location() {
        let mut s = init_opts(Options {
            cols: 5,
            rows: 5,
            max_size: Some(1),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        for y in 0..s.rows as usize {
            fill_row_x(page, y, 2);
        }

        // Grow blank rows to push our rows back into scrollback
        s.grow_rows(5).unwrap();
        assert_eq!(10, s.total_rows());

        // Put a tracked pin in the history
        let p = s.track_pin(s.pin(pt_active(0, 0)).unwrap()).unwrap();

        // Resize
        s.resize(Resize {
            cols: Some(4),
            reflow: true,

            // Important: not on last row
            cursor: Some(ResizeCursor::new(1, 1)),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(10, s.total_rows());

        // Our cursor should move to the first row
        assert_eq!(pt_active(0, 0), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_reflow_less_cols_no_scrollback_pull_blank_active() {
        let mut s = init_opts(Options {
            cols: 5,
            rows: 5,
            max_size: Some(1),
            ..Default::default()
        });
        assert!(s.pages.first == s.pages.last);
        let page = pg(first(&s));
        for y in 0..s.rows as usize {
            fill_row_x(page, y, 2);
        }

        // Grow blank rows to push our rows back into scrollback
        s.grow_rows(5).unwrap();
        assert_eq!(10, s.total_rows());

        let p = s.track_pin(s.pin(pt_active(0, 0)).unwrap()).unwrap();

        // Resize with no cursor. Normally the trailing blank rows would be
        // trimmed and the active area would slide up over our history.
        s.resize(Resize {
            cols: Some(4),
            reflow: true,
            pull_scrollback: false,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(10, s.total_rows());

        // The top of the active area should not move
        assert_eq!(pt_active(0, 0), s.point_from_pin(Tag::Active, *tp(p)).unwrap());

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_resize_reflow_less_cols_copy_style() {
        let mut s = init_opts(Options {
            cols: 4,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            // Create a style
            let mut style = stylepkg::Style::default();
            style.flags.bold = true;
            let style_id = unsafe { page.styles.add(page.memory, style) }.unwrap();

            for x in 0..s.cols as usize - 1 {
                let mut c = cpu(x as u32);
                c.set_style_id(style_id);
                *cell_at(page, x, 0) = c;
                unsafe { page.styles.use_(page.memory, style_id) };
            }

            // We're over-counted by 1 because `add` implies `use`.
            unsafe { page.styles.release(page.memory, style_id) };
        }

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(2, s.total_rows());

        let mut it = s.row_iterator(Direction::RightDown, pt_active(0, 0), None);
        while let Some(offset) = it.next() {
            for x in 0..s.cols as usize - 1 {
                let mut offset_copy = offset;
                offset_copy.x = x as CellCountInt;
                let (row, cell) = pin_rac(offset_copy);
                let style_id = cell.style_id();
                assert!(style_id != 0);

                let page = pg(offset.node);
                let style = unsafe { &*page.styles.get(page.memory, style_id) };
                assert!(style.flags.bold);

                assert!(row.styled());
            }
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_to_eliminate_a_wide_char() {
        let mut s = init_opts(Options {
            cols: 2,
            rows: 1,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            *cell_at(page, 0, 0) = wide_cell('😀', Wide::Wide);
            *cell_at(page, 1, 0) = wide_cell('\0', Wide::SpacerTail);
        }

        // Resize
        s.resize(reflow_cols(1)).unwrap();
        assert_eq!(1, s.cols);
        assert_eq!(1, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            check_cell(page, 0, 0, 0, Wide::Narrow);
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_to_wrap_a_wide_char() {
        let mut s = init_opts(Options {
            cols: 3,
            rows: 1,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            *cell_at(page, 0, 0) = cp('x');
            *cell_at(page, 1, 0) = wide_cell('😀', Wide::Wide);
            *cell_at(page, 2, 0) = wide_cell('\0', Wide::SpacerTail);
        }

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(2, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            check_cell(page, 0, 0, 'x' as u32, Wide::Narrow);
            assert!(row_at(page, 0).wrap());
            check_cell(page, 1, 0, 0, Wide::SpacerHead);
            check_cell(page, 0, 1, '😀' as u32, Wide::Wide);
            check_cell(page, 1, 1, 0, Wide::SpacerTail);
        }
    }

    /// A row of four wide CJK pairs.
    fn wide_pairs_row(page: &Page) {
        for i in 0..4 {
            *cell_at(page, i * 2, 0) = wcu(0x4E00 + i as u32, Wide::Wide);
            *cell_at(page, i * 2 + 1, 0) = wcu(0, Wide::SpacerTail);
        }
    }

    fn check_wide_pairs(page: &Page) {
        for i in 0..4usize {
            let y = i / 2;
            let x = (i % 2) * 2;
            check_cell(page, x, y, 0x4E00 + i as u32, Wide::Wide);
            check_cell(page, x + 1, y, 0, Wide::SpacerTail);
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_wide_char_bulk_run() {
        let mut s = init_opts(Options {
            cols: 8,
            rows: 1,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            // A full row of wide character pairs so the reflow takes
            // the bulk run path.
            wide_pairs_row(pg(first(&s)));
        }

        // Resize to exactly two pairs per row: runs end on the row
        // boundary with no spacer heads needed.
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(2, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            check_wide_pairs(page);

            assert!(row_at(page, 0).wrap());
            assert!(!row_at(page, 0).wrap_continuation());
            assert!(!row_at(page, 1).wrap());
            assert!(row_at(page, 1).wrap_continuation());
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_wide_char_bulk_run_odd_cols_spacer_head() {
        let mut s = init_opts(Options {
            cols: 8,
            rows: 1,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            wide_pairs_row(pg(first(&s)));
        }

        // Resize to an odd number of columns: the bulk run must stop a
        // pair short of the row boundary and the slow path inserts a
        // spacer head in the final column.
        s.resize(reflow_cols(5)).unwrap();
        assert_eq!(5, s.cols);
        assert_eq!(2, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            check_wide_pairs(page);

            check_cell(page, 4, 0, 0, Wide::SpacerHead);
            assert!(row_at(page, 0).wrap());
            assert_eq!(Wide::Narrow, cell_at(page, 4, 1).wide());
            assert!(row_at(page, 1).wrap_continuation());
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_wide_char_bulk_run_mixed_narrow_round_trip() {
        // The emoji-in-prose shape: single wide pairs separated by
        // narrow cells, which must all share a single bulk run.
        let shape: [(u32, Wide); 9] = [
            (0x4E00, Wide::Wide),
            (0, Wide::SpacerTail),
            ('x' as u32, Wide::Narrow),
            (0x4E01, Wide::Wide),
            (0, Wide::SpacerTail),
            ('y' as u32, Wide::Narrow),
            (0x4E02, Wide::Wide),
            (0, Wide::SpacerTail),
            ('z' as u32, Wide::Narrow),
        ];

        let mut s = init_opts(Options {
            cols: 9,
            rows: 1,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            for (x, c) in shape.iter().enumerate() {
                *cell_at(page, x, 0) = wcu(c.0, c.1);
            }
        }

        // Shrink: the run ends exactly on the row boundary after the
        // second narrow cell.
        s.resize(reflow_cols(6)).unwrap();
        assert_eq!(6, s.cols);
        assert_eq!(2, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            for (x, c) in shape[0..6].iter().enumerate() {
                check_cell(page, x, 0, c.0, c.1);
            }
            for (x, c) in shape[6..].iter().enumerate() {
                check_cell(page, x, 1, c.0, c.1);
            }
            assert!(row_at(page, 0).wrap());
            assert!(row_at(page, 1).wrap_continuation());
        }

        // Grow back: the wrapped rows must rejoin into the original
        // single-row layout.
        s.resize(reflow_cols(9)).unwrap();
        assert_eq!(9, s.cols);
        assert_eq!(1, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));
            for (x, c) in shape.iter().enumerate() {
                check_cell(page, x, 0, c.0, c.1);
            }
            assert!(!row_at(page, 0).wrap());
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_wide_char_bulk_run_styled() {
        let mut s = init_opts(Options {
            cols: 8,
            rows: 1,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            // Create a style
            let mut style = stylepkg::Style::default();
            style.flags.bold = true;
            let style_id = unsafe { page.styles.add(page.memory, style) }.unwrap();

            // Styled pairs: the tail shares the wide cell's style, as
            // the print path writes them.
            for i in 0..4 {
                let mut c = wcu(0x4E00 + i as u32, Wide::Wide);
                c.set_style_id(style_id);
                *cell_at(page, i * 2, 0) = c;
                unsafe { page.styles.use_(page.memory, style_id) };

                let mut t = wcu(0, Wide::SpacerTail);
                t.set_style_id(style_id);
                *cell_at(page, i * 2 + 1, 0) = t;
                unsafe { page.styles.use_(page.memory, style_id) };
            }

            // We're over-counted by 1 because `add` implies `use`.
            unsafe { page.styles.release(page.memory, style_id) };
        }

        // Resize
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(2, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            for y in 0..2 {
                for x in 0..4 {
                    let (row, cell) = rac(page, x, y);
                    let style_id = cell.style_id();
                    assert!(style_id != 0);

                    let style = unsafe { &*page.styles.get(page.memory, style_id) };
                    assert!(style.flags.bold);
                    assert!(row.styled());
                    assert_eq!(if x % 2 == 0 { Wide::Wide } else { Wide::SpacerTail }, cell.wide());
                }
            }
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_wide_char_bulk_run_degenerate_spacer_tail_style() {
        let mut s = init_opts(Options {
            cols: 4,
            rows: 1,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            // A styled wide cell whose tail does NOT share its style.
            // This can't come from the print path, but the run scan must
            // reject the pair (slow path) rather than rewrite the tail
            // to the wide cell's style.
            let mut style = stylepkg::Style::default();
            style.flags.bold = true;
            let style_id = unsafe { page.styles.add(page.memory, style) }.unwrap();

            let mut c = wcu(0x4E00, Wide::Wide);
            c.set_style_id(style_id);
            *cell_at(page, 0, 0) = c;
            *cell_at(page, 1, 0) = wcu(0, Wide::SpacerTail);
            *cell_at(page, 2, 0) = cp('x');
        }

        // Resize
        s.resize(reflow_cols(3)).unwrap();
        assert_eq!(3, s.cols);
        assert_eq!(1, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            {
                let cell = cell_at(page, 0, 0);
                assert_eq!(0x4E00, cell.content_codepoint());
                assert_eq!(Wide::Wide, cell.wide());
                assert!(cell.style_id() != 0);
                let style = unsafe { &*page.styles.get(page.memory, cell.style_id()) };
                assert!(style.flags.bold);
            }
            {
                let cell = cell_at(page, 1, 0);
                assert_eq!(Wide::SpacerTail, cell.wide());
                assert_eq!(stylepkg::DEFAULT_ID, cell.style_id());
            }
            check_cell(page, 2, 0, 'x' as u32, Wide::Narrow);
        }
    }

    const FAMILY_SUFFIX: [u32; 6] = [0x200D, 0x1F468, 0x200D, 0x1F466, 0x200D, 0x1F466];

    fn check_family(page: &Page, x: usize, y: usize) {
        let cell = cell_at(page, x, y);
        assert_eq!(0x1F468, cell.content_codepoint());
        assert_eq!(Wide::Wide, cell.wide());
        let cps = page.lookup_grapheme(cell).unwrap();
        assert_eq!(&FAMILY_SUFFIX[..], cps);
    }

    #[test]
    fn page_list_resize_reflow_less_cols_to_wrap_a_multi_codepoint_grapheme_with_a_spacer_head() {
        let mut s = init_opts(Options {
            cols: 4,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            // We want to make the screen look like this:
            //
            // 👨‍👨‍👦‍👦👨‍👨‍👦‍👦

            // First family emoji at (0, 0)
            {
                let (row, cell) = rac(page, 0, 0);
                *cell = wcu(0x1F468, Wide::Wide); // First codepoint of the grapheme
                page.set_graphemes(row, cell, &FAMILY_SUFFIX).unwrap();
            }
            *cell_at(page, 1, 0) = wcu(0, Wide::SpacerTail);
            // Second family emoji at (2, 0)
            {
                let (row, cell) = rac(page, 2, 0);
                *cell = wcu(0x1F468, Wide::Wide); // First codepoint of the grapheme
                page.set_graphemes(row, cell, &FAMILY_SUFFIX).unwrap();
            }
            *cell_at(page, 3, 0) = wcu(0, Wide::SpacerTail);
        }

        // Resize
        s.resize(reflow_cols(3)).unwrap();
        assert_eq!(3, s.cols);
        assert_eq!(2, s.total_rows());

        {
            assert!(s.pages.first == s.pages.last);
            let page = pg(first(&s));

            check_family(page, 0, 0);
            // Row should be wrapped
            assert!(row_at(page, 0).wrap());
            check_cell(page, 1, 0, 0, Wide::SpacerTail);
            check_cell(page, 2, 0, 0, Wide::SpacerHead);
            // (Zig re-checks (0, 0) here; the second emoji lands at (0, 1).)
            check_family(page, 0, 0);
            check_cell(page, 1, 1, 0, Wide::SpacerTail);
        }
    }

    /// Write unicode placeholders in cols `xs` of row 0.
    fn placeholders(page: &Page, xs: core::ops::Range<usize>) {
        for x in xs {
            let (row, cell) = rac(page, x, 0);
            row.set_kitty_virtual_placeholder(true);
            *cell = cpu(graphics_unicode::PLACEHOLDER);
        }
    }

    #[test]
    fn page_list_resize_reflow_less_cols_copy_kitty_placeholder() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut s = init_opts(Options {
            cols: 4,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            // Write unicode placeholders
            placeholders(pg(first(&s)), 0..s.cols as usize - 1);
        }

        // Resize
        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(2, s.total_rows());

        let mut it = s.row_iterator(Direction::RightDown, pt_active(0, 0), None);
        while let Some(offset) = it.next() {
            for x in 0..s.cols as usize - 1 {
                let mut offset_copy = offset;
                offset_copy.x = x as CellCountInt;
                let (row, _) = pin_rac(offset_copy);
                assert!(row.kitty_virtual_placeholder());
            }
        }
    }

    #[test]
    fn page_list_resize_reflow_more_cols_clears_kitty_placeholder() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut s = init_opts(Options {
            cols: 4,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            // Write unicode placeholders
            placeholders(pg(first(&s)), 0..s.cols as usize - 1);
        }

        // Resize smaller then larger
        s.resize(reflow_cols(2)).unwrap();
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert_eq!(2, s.total_rows());

        let mut it = s.row_iterator(Direction::RightDown, pt_active(0, 0), None);
        assert!(pin_rac(it.next().unwrap()).0.kitty_virtual_placeholder());
        assert!(!pin_rac(it.next().unwrap()).0.kitty_virtual_placeholder());
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_resize_reflow_wrap_moves_kitty_placeholder() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }

        let mut s = init_opts(Options {
            cols: 4,
            rows: 2,
            max_size: Some(0),
            ..Default::default()
        });
        {
            assert!(s.pages.first == s.pages.last);
            // Write unicode placeholders
            placeholders(pg(first(&s)), 2..s.cols as usize - 1);
        }

        s.resize(reflow_cols(2)).unwrap();
        assert_eq!(2, s.cols);
        assert_eq!(2, s.total_rows());

        let mut it = s.row_iterator(Direction::RightDown, pt_active(0, 0), None);
        assert!(!pin_rac(it.next().unwrap()).0.kitty_virtual_placeholder());
        assert!(pin_rac(it.next().unwrap()).0.kitty_virtual_placeholder());
        assert!(it.next().is_none());
    }

    #[test]
    fn page_list_reset() {
        let mut s = init(80, 24);
        s.reset();
        assert!(s.viewport == Viewport::Active);
        assert!(s.pages.first.is_some());
        assert_eq!(s.rows as usize, s.total_rows());

        // Active area should be the top
        assert_eq!(pin_at(first(&s), 0, 0), s.get_top_left(Tag::Active));
    }

    #[test]
    fn page_list_reset_invalidates_stale_untracked_refs_even_if_node_memory_is_reused() {
        let mut s = init(80, 24);

        let mut stale: Vec<(NodePtr, u64)> = Vec::new();
        let mut reused: Option<(NodePtr, u64)> = None;

        while stale.len() < PAGE_PREHEAT * 4 && reused.is_none() {
            let old_node = first(&s);
            let old_serial = n(old_node).serial;
            assert!(old_serial >= s.page_serial_epoch);
            assert!(old_serial < s.page_serial);
            stale.push((old_node, old_serial));

            s.reset();

            let new_node = first(&s);
            for &(node, serial) in &stale {
                if node == new_node {
                    reused = Some((node, serial));
                    break;
                }
            }
        }

        assert!(reused.is_some());
        let (old_node, old_serial) = reused.unwrap();
        let new_node = first(&s);
        let new_serial = n(new_node).serial;

        // Reset advances the epoch before rebuilding from the node pool. Reject
        // the stale generation before inspecting its pointer, even when that exact
        // address now belongs to a new live generation.
        assert_eq!(old_node, new_node);
        assert!(old_serial < s.page_serial_epoch);
        assert!(!s.node_is_valid(old_node, old_serial));
        assert!(s.node_is_valid(new_node, new_serial));
        assert!(new_serial >= s.page_serial_epoch);
        assert!(new_serial < s.page_serial);
    }

    #[test]
    fn page_list_reset_across_two_pages() {
        // Find a cap that makes it so that rows don't fit on one page.
        let rows = 100;
        let cap = {
            let mut cap = STD_CAPACITY.adjust(pagepkg::Adjustment { cols: Some(50) }).unwrap();
            while cap.rows >= rows {
                cap = STD_CAPACITY
                    .adjust(pagepkg::Adjustment {
                        cols: Some(cap.cols + 50),
                    })
                    .unwrap();
            }
            cap
        };

        // Init
        let mut s = init(cap.cols, rows);
        s.reset();
        assert!(s.viewport == Viewport::Active);
        assert!(s.pages.first.is_some());
        assert_eq!(s.rows as usize, s.total_rows());
    }

    #[test]
    fn page_list_reset_moves_tracked_pins_and_marks_them_as_garbage() {
        let mut s = init(80, 24);

        // Create a tracked pin into the active area
        let p = s.track_pin(s.pin(pt_active(42, 12)).unwrap()).unwrap();

        s.reset();

        // Our added pin should now be garbage
        assert!(tp(p).garbage);

        // Viewport pin should not be garbage because it makes sense.
        assert!(!vp(&s).garbage);

        s.untrack_pin(p);
    }

    #[test]
    fn page_list_clears_history() {
        let mut s = init(80, 24);
        s.grow_rows(30).unwrap();
        s.reset();
        assert!(s.viewport == Viewport::Active);
        assert!(s.pages.first.is_some());
        assert_eq!(s.rows as usize, s.total_rows());

        // Active area should be the top
        assert_eq!(pin_at(first(&s), 0, 0), s.get_top_left(Tag::Active));
    }

    #[test]
    fn page_list_resize_reflow_grapheme_map_capacity_exceeded() {
        // This test verifies that when reflowing content with many graphemes,
        // the grapheme map capacity is correctly increased when needed.
        let mut s = init_opts(Options {
            cols: 4,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        });
        assert_eq!(1, s.total_pages());

        // Get the grapheme capacity from the page. We need more than this many
        // graphemes in a single destination page to trigger capacity increase
        // during reflow. Since each source page can only hold this many graphemes,
        // we create two source pages with graphemes that will merge into one
        // destination page.
        let grapheme_capacity = pg(first(&s)).grapheme_capacity();
        // Use slightly more than half the capacity per page, so combined they
        // exceed the capacity of a single destination page.
        let graphemes_per_page = grapheme_capacity / 2 + grapheme_capacity / 4;

        // Grow to the capacity of the first page and add more rows
        // so that we have two pages total.
        grow_first_page_full(&mut s, graphemes_per_page);
        // We now have two pages.
        assert!(first(&s) != last(&s));
        assert_eq!(s.pages.last, n(first(&s)).next);

        // Add graphemes to both pages. We add graphemes to rows at the END of the
        // first page, and graphemes to rows at the START of the second page.
        // When reflowing to 2 columns, these rows will wrap and stay together
        // on the same destination page, requiring capacity increase.

        // Add graphemes to the end of the first page (last rows)
        {
            let page = pg(first(&s));
            let start_row = page.size.rows as usize - graphemes_per_page;
            for i in 0..graphemes_per_page {
                let y = start_row + i;
                let (row, cell) = rac(page, 0, y);
                *cell = cp('A');
                page.append_grapheme(row, cell, 0x0301).unwrap();
            }
        }

        // Add graphemes to the beginning of the second page
        {
            let page = pg(last(&s));
            let count = graphemes_per_page.min(page.size.rows as usize);
            for y in 0..count {
                let (row, cell) = rac(page, 0, y);
                *cell = cp('B');
                page.append_grapheme(row, cell, 0x0302).unwrap();
            }
        }

        // Resize to fewer columns to trigger reflow.
        // The graphemes from both pages will be copied to destination pages.
        // They will all end up in a contiguous region of the destination.
        // If the bug exists (hyperlink_bytes increased instead of grapheme_bytes),
        // this will fail with GraphemeMapOutOfMemory when we exceed capacity.
        s.resize(reflow_cols(2)).unwrap();

        // Verify the resize succeeded
        assert_eq!(2, s.cols);
    }

    #[test]
    fn page_list_resize_grow_cols_with_unwrap_fixes_viewport_pin() {
        // Regression test: after resize/reflow, the viewport pin can end up at a
        // position where pin.y + rows > total_rows, causing getBottomRight to panic.

        // The plan is to pin viewport in history, then grow columns to unwrap rows.
        // The unwrap reduces total_rows, but the tracked pin moves to a position
        // that no longer has enough rows below it for the viewport height.
        let mut s = init(2, 10);

        // Make sure we have some history, in this case we have 30 rows of history
        s.grow_rows(30).unwrap();
        assert_eq!(40, s.total_rows());

        // Fill all rows with wrapped content (pairs that unwrap when cols increase)
        let mut it = s.page_iterator(Direction::RightDown, pt_screen(0, 0), None);
        while let Some(chunk) = it.next() {
            let page = pg(chunk.node);
            for y in chunk.start as usize..chunk.end as usize {
                if y % 2 == 0 {
                    set_wrap(page, y);
                } else {
                    set_wrap_cont(page, y);
                }
                fill_row(page, y, s.cols as usize, 'A');
            }
        }

        // Pin viewport at row 28 (in history, 2 rows before active area at row 30).
        // After unwrap: row 28 -> row 14, total_rows 40 -> 20, active starts at 10.
        // Pin at 14 needs rows 14-23, but only 0-19 exist -> overflow.
        s.scroll(Scroll::Pin(s.pin(pt_screen(0, 28)).unwrap()));
        assert!(s.viewport == Viewport::Pin);
        assert!(s.get_bottom_right(Tag::Viewport).is_some());

        // Resize with reflow: unwraps rows, reducing total_rows
        s.resize(reflow_cols(4)).unwrap();
        assert_eq!(4, s.cols);
        assert!(s.total_rows() < 40);

        // Used to panic here, so test that we can get the bottom right.
        let br_after = s.get_bottom_right(Tag::Viewport);
        assert!(br_after.is_some());
    }

    #[test]
    fn page_list_grow_reuses_non_standard_page_without_leak() {
        // Create a PageList with 3 * std_size max so we can fit multiple pages
        // but will still trigger reuse.
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(3 * std_size()),
            ..Default::default()
        });

        // Increase the first page capacity to make it non-standard (larger than std_size).
        while pg(first(&s)).memory.len() <= std_size() {
            s.increase_capacity(first(&s), Some(IncreaseCapacity::GraphemeBytes))
                .unwrap();
        }

        // The first page should now have non-standard memory size.
        assert!(pg(first(&s)).memory.len() > std_size());

        // First, fill up the first page's capacity
        let first_page = first(&s);
        while n(first_page).rows() < n(first_page).capacity().rows {
            s.grow().unwrap();
        }

        // Now grow to create a second page
        s.grow().unwrap();
        assert!(s.pages.first != s.pages.last);

        // Continue growing until we exceed max_size AND the last page is full
        while s.page_size + PagePool::item_size() <= s.limits.max(LimitKey::Bytes)
            || n(last(&s)).rows() < n(last(&s)).capacity().rows
        {
            s.grow().unwrap();
        }

        // The first page should still be non-standard
        assert!(pg(first(&s)).memory.len() > std_size());

        // Verify we have enough rows for active area (so prune path isn't skipped)
        assert!(s.total_rows() >= s.rows as usize);

        // Verify last page is full (so grow will need to allocate/reuse)
        assert!(pg(last(&s)).size.rows == pg(last(&s)).capacity.rows);

        // Remember the first page memory pointer before the reuse attempt
        let first_page_ptr = first(&s);
        let first_page_mem_ptr = pg(first(&s)).memory.ptr();

        // Create a tracked pin pointing to the non-standard first page
        let tracked_pin = s.track_pin(pin_at(first_page_ptr, 0, 0)).unwrap();

        // Now grow one more time to trigger the reuse path. Since the first page
        // is non-standard, it should be destroyed (not reused).
        s.grow().unwrap();

        // After grow, check if the first page is a different one
        // (meaning the non-standard page was pruned, not reused at the end)
        // The original first page should no longer be the first page
        assert!(first(&s) != first_page_ptr);

        // If the non-standard page was properly destroyed and not reused,
        // the last page should not have the same memory pointer
        assert!(pg(last(&s)).memory.ptr() != first_page_mem_ptr);

        // The tracked pin should have been moved to the new first page and marked as garbage
        assert_eq!(first(&s), tp(tracked_pin).node);
        assert_eq!(0, tp(tracked_pin).x);
        assert_eq!(0, tp(tracked_pin).y);
        assert!(tp(tracked_pin).garbage);

        s.untrack_pin(tracked_pin);
    }

    #[test]
    fn page_list_grow_non_standard_page_prune_protection() {
        // This test specifically verifies the fix for the bug where pruning a
        // non-standard page would cause totalRows() < self.rows.
        //
        // Bug trigger conditions (all must be true simultaneously):
        // 1. first page is non-standard (memory.len > std_size)
        // 2. page_size + PagePool.item_size > maxSize() (triggers prune consideration)
        // 3. pages.first != pages.last (have multiple pages)
        // 4. total_rows >= self.rows (have enough rows for active area)
        // 5. total_rows - first.size.rows + 1 < self.rows (prune would lose too many)

        // This is kind of magic and likely depends on std_size.
        let rows_count = 600;
        let mut s = init_opts(Options {
            cols: 80,
            rows: rows_count,
            max_size: Some(std_size()),
            ..Default::default()
        });

        // Make the first page non-standard
        while pg(first(&s)).memory.len() <= std_size() {
            s.increase_capacity(first(&s), Some(IncreaseCapacity::GraphemeBytes))
                .unwrap();
        }
        assert!(pg(first(&s)).memory.len() > std_size());

        let first_page_node = first(&s);
        let first_page_cap = n(first_page_node).capacity().rows;

        // Fill first page to capacity
        while n(first_page_node).rows() < first_page_cap {
            s.grow().unwrap();
        }

        // Grow until we have a second page (first page fills up first)
        while s.pages.first == s.pages.last {
            s.grow().unwrap();
        }
        assert!(s.pages.first != s.pages.last);

        // Fill the second page to capacity so that the next grow() triggers prune
        let last_node = last(&s);
        let second_cap = n(last_node).capacity().rows;
        while n(last_node).rows() < second_cap {
            s.grow().unwrap();
        }

        // Now the last page is full. The next grow must either:
        // 1. Prune the first page and reuse it, OR
        // 2. Allocate a new page
        let total = s.total_rows();
        let would_remain = total - first_page_cap as usize + 1;

        // Verify the bug condition is present: pruning first page would leave < rows
        assert!(would_remain < s.rows as usize);

        // Verify prune path conditions are met
        assert!(s.pages.first != s.pages.last);
        assert!(s.page_size + PagePool::item_size() > s.limits.max(LimitKey::Bytes));
        assert!(s.total_rows() >= s.rows as usize);

        // Verify last page is at capacity (so grow must prune or allocate new)
        assert_eq!(second_cap, n(last_node).rows());

        // The next grow should trigger prune consideration.
        // Without the fix, this would destroy the non-standard first page,
        // leaving only second_cap + 1 rows, which is < self.rows.
        s.grow().unwrap();

        // Verify the invariant holds - the fix prevents the destructive prune
        assert!(s.total_rows() >= s.rows as usize);
    }

    #[test]
    fn page_list_resize_no_reflow_more_cols_remaps_pins_in_backfill_path() {
        // Regression test: when resizeWithoutReflowGrowCols copies rows to a previous
        // page with spare capacity, tracked pins in those rows must be remapped.
        // Without the fix, pins become dangling pointers when the original page is destroyed.
        let cols: CellCountInt = 5;
        let cap = STD_CAPACITY.adjust(pagepkg::Adjustment { cols: Some(cols) }).unwrap();
        let mut s = init(cols, cap.rows);

        // Grow until we have two pages.
        while s.pages.first == s.pages.last {
            s.grow().unwrap();
        }
        let first_page = first(&s);
        let second_page = last(&s);
        assert!(first_page != second_page);

        // Trim a history row so the first page has spare capacity.
        // This triggers the backfill path in resizeWithoutReflowGrowCols.
        s.erase_history(Some(pt_history(0, 0)));
        assert!(n(first_page).rows() < n(first_page).capacity().rows);

        // Ensure the resize takes the slow path (new capacity > current capacity).
        let new_cols: CellCountInt = cols + 1;
        let adjusted = n(second_page)
            .capacity()
            .adjust(pagepkg::Adjustment { cols: Some(new_cols) })
            .unwrap();
        assert!(n(second_page).capacity().cols < adjusted.cols);

        // Track a pin in row 0 of the second page. This row will be copied
        // to the first page during backfill and the pin must be remapped.
        let tracked = s.track_pin(pin_at(second_page, 0, 0)).unwrap();

        // Write a marker character to the tracked cell so we can verify
        // the pin points to the correct cell after resize.
        let marker = 'X' as u32;
        *pin_rac(*tp(tracked)).1 = cpu(marker);

        s.resize(no_reflow(Some(new_cols), None)).unwrap();

        // Verify the pin points to a valid node still in the page list.
        let mut found = false;
        let mut it = s.pages.first;
        while let Some(node) = it {
            if node == tp(tracked).node {
                found = true;
                break;
            }
            it = n(node).next;
        }
        assert!(found);
        assert!(tp(tracked).y < n(tp(tracked).node).rows());

        // Verify the pin still points to the cell with our marker content.
        let cell = pin_rac(*tp(tracked)).1;
        assert_eq!(ContentTag::Codepoint, cell.content_tag());
        assert_eq!(marker, cell.content_codepoint());

        s.untrack_pin(tracked);
    }

    #[test]
    fn page_list_compact_pool_page_produces_exact_size_heap_page() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(0),
            ..Default::default()
        });

        // A freshly created page is pool-owned at std_size.
        let node = first(&s);
        assert_eq!(Owned::Pool, n(node).owned);
        assert!(pg(node).memory.len() <= std_size());
        let original_size = pg(node).size;

        // Compacting it should produce a much smaller exact-size heap page.
        let new_node = s.compact(node).unwrap().unwrap();
        assert_eq!(Owned::Heap, n(new_node).owned);
        assert!(pg(new_node).memory.len() < std_size());
        assert_eq!(original_size.rows, n(new_node).rows());
        assert_eq!(original_size.cols, n(new_node).cols());
        assert_eq!(new_node, first(&s));

        // Our page size accounting should exactly match the compacted
        // page since it is the only page in the list.
        assert_eq!(pg(new_node).memory.len(), s.page_size);

        // Compacting again should be a no-op since it is already exact.
        assert_eq!(None, s.compact(new_node).unwrap());
    }

    #[test]
    fn page_list_compact_then_grow_allocates_new_page() {
        let mut s = init(80, 24);

        // Compact the only page. It now has no spare row capacity.
        let node = s.compact(first(&s)).unwrap().unwrap();
        assert_eq!(n(node).rows(), n(node).capacity().rows);

        // Growing must allocate a fresh standard page from the pool,
        // exercising that a compacted page remains a valid live page.
        s.grow().unwrap();
        assert!(s.pages.first != s.pages.last);
        assert_eq!(Owned::Pool, n(last(&s)).owned);
        assert_eq!(25, s.total_rows());
    }

    #[test]
    fn page_list_compact_then_reset_frees_heap_pages() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(0),
            ..Default::default()
        });

        // Compact the only page so the list contains a sub-std_size
        // heap-owned page.
        let node = s.compact(first(&s)).unwrap().unwrap();
        assert_eq!(Owned::Heap, n(node).owned);
        assert!(pg(node).memory.len() < std_size());

        // Reset must free the heap page and rebuild from the pool.
        s.reset();
        assert_eq!(Owned::Pool, n(first(&s)).owned);
        assert_eq!(s.rows as usize, s.total_rows());
    }

    #[test]
    fn page_list_compact_then_clone() {
        let mut s = init(80, 24);

        // Write a marker so we can verify contents survive.
        *cell_at(pg(first(&s)), 1, 2) = cp('X');

        // Compact so the source list contains a sub-std_size heap page.
        let node = s.compact(first(&s)).unwrap().unwrap();
        assert_eq!(Owned::Heap, n(node).owned);
        assert!(pg(node).memory.len() < std_size());

        let s2 = s.clone(clone_opts(pt_screen(0, 0), None)).unwrap();
        assert_eq!(s.rows as usize, s2.total_rows());

        // Verify the marker survived the clone.
        assert_eq!('X' as u32, cell_at(pg(first(&s2)), 1, 2).content_codepoint());
    }

    #[test]
    fn page_list_compact_oversized_page() {
        let mut s = init(80, 24);

        // Grow until we have multiple pages
        let page1_node = first(&s);
        pg(page1_node).pause_integrity_checks(true);
        for _ in 0..(n(page1_node).capacity().rows - n(page1_node).rows()) {
            s.grow().unwrap();
        }
        pg(page1_node).pause_integrity_checks(false);
        s.grow().unwrap();
        assert!(s.pages.first != s.pages.last);

        let mut node = first(&s);

        // Write content to verify it's preserved
        {
            let page = pg(node);
            for y in 0..page.size.rows as usize {
                for x in 0..s.cols as usize {
                    *cell_at(page, x, y) = cpu((x + y * s.cols as usize) as u32);
                }
            }
        }

        // Create a tracked pin on this page
        let tracked = s.track_pin(pin_at(node, 5, 10)).unwrap();

        // Make the page oversized
        while pg(node).memory.len() <= std_size() {
            node = s.increase_capacity(node, Some(IncreaseCapacity::GraphemeBytes)).unwrap();
        }
        assert!(pg(node).memory.len() > std_size());
        let oversized_len = pg(node).memory.len();
        let original_size = pg(node).size;
        let second_node = next(node);

        // Set dirty flag after increaseCapacity
        pg(node).dirty = true;

        // Compact the page
        let new_node = s.compact(node).unwrap();
        assert!(new_node.is_some());
        let new_node = new_node.unwrap();

        // Verify memory is smaller
        assert!(pg(new_node).memory.len() < oversized_len);

        // Verify size preserved
        assert_eq!(original_size.rows, n(new_node).rows());
        assert_eq!(original_size.cols, n(new_node).cols());

        // Verify dirty flag preserved
        assert!(pg(new_node).dirty);

        // Verify linked list integrity
        assert_eq!(new_node, first(&s));
        assert_eq!(None, n(new_node).prev);
        assert_eq!(Some(second_node), n(new_node).next);
        assert_eq!(Some(new_node), n(second_node).prev);

        // Verify pin updated correctly
        assert_eq!(new_node, tp(tracked).node);
        assert_eq!(5, tp(tracked).x);
        assert_eq!(10, tp(tracked).y);

        // Verify content preserved
        let page = pg(new_node);
        for y in 0..page.size.rows as usize {
            for x in 0..s.cols as usize {
                assert_eq!((x + y * s.cols as usize) as u32, cell_at(page, x, y).content_codepoint());
            }
        }

        s.untrack_pin(tracked);
    }

    #[test]
    fn page_list_destroyed_pool_page_reuse_is_zeroed() {
        let mut s = init(80, 24);

        // Create a page and scribble over its entire backing memory,
        // then destroy it so the buffer returns to the pool free list.
        let node = s.create_page(CreatePage::cap(initial_capacity(80))).unwrap();
        pg(node).size.rows = 1;
        let mem_ptr = pg(node).memory.ptr();
        mem(pg(node)).fill(0xAA);
        s.destroy_node(node);

        // Reusing the buffer must produce a fully valid, zeroed page.
        let node2 = s.create_page(CreatePage::cap(initial_capacity(80))).unwrap();
        assert_eq!(mem_ptr, pg(node2).memory.ptr());
        pg(node2).size.rows = n(node2).capacity().rows;

        let cells_len = n(node2).capacity().cols as usize * n(node2).capacity().rows as usize;
        let page = pg(node2);
        let cells = unsafe { core::slice::from_raw_parts(page.cells.ptr(page.memory), cells_len) };
        assert!(cells.iter().all(|c| c.to_bits() == 0));
        page.assert_integrity();
        s.destroy_node(node2);
    }

    #[test]
    fn page_list_increase_capacity_from_zero_capacity_dimensions() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(0),
            ..Default::default()
        });

        // Compact the only page. A plain page has no styled, grapheme,
        // or hyperlink content so the exact capacity is zero in every
        // managed dimension.
        let mut node = s.compact(first(&s)).unwrap().unwrap();
        assert_eq!(0, n(node).capacity().styles);
        assert_eq!(0, n(node).capacity().grapheme_bytes);
        assert_eq!(0, n(node).capacity().string_bytes);
        assert_eq!(0, n(node).capacity().hyperlink_bytes);

        // Increasing each dimension from zero must actually grow it.
        // Regression: 0 * 2 == 0 used to "succeed" without growing,
        // which turned caller retry loops into infinite loops.
        node = s.increase_capacity(node, Some(IncreaseCapacity::Styles)).unwrap();
        assert!(n(node).capacity().styles > 0);
        node = s.increase_capacity(node, Some(IncreaseCapacity::GraphemeBytes)).unwrap();
        assert!(n(node).capacity().grapheme_bytes > 0);
        node = s.increase_capacity(node, Some(IncreaseCapacity::StringBytes)).unwrap();
        assert!(n(node).capacity().string_bytes > 0);
        node = s.increase_capacity(node, Some(IncreaseCapacity::HyperlinkBytes)).unwrap();
        assert!(n(node).capacity().hyperlink_bytes > 0);

        // Increasing a non-zero dimension still doubles.
        let styles = n(node).capacity().styles;
        node = s.increase_capacity(node, Some(IncreaseCapacity::Styles)).unwrap();
        assert_eq!(styles * 2, n(node).capacity().styles);
    }

    #[test]
    fn page_list_compact_after_increase_capacity() {
        let mut s = init_opts(Options {
            cols: 80,
            rows: 24,
            max_size: Some(0),
            ..Default::default()
        });

        let mut node = first(&s);

        // Grow the page capacity. The content is unchanged, so compaction
        // should always shrink it back down to an exact-size heap page.
        node = s.increase_capacity(node, Some(IncreaseCapacity::GraphemeBytes)).unwrap();
        let grown_len = pg(node).memory.len();

        let new_node = s.compact(node).unwrap().unwrap();
        assert_eq!(Owned::Heap, n(new_node).owned);
        assert!(pg(new_node).memory.len() < grown_len);
        assert!(pg(new_node).memory.len() < std_size());
    }

    fn split_list() -> PageList {
        init_opts(Options {
            cols: 10,
            rows: 10,
            max_size: Some(0),
            ..Default::default()
        })
    }

    /// Row y gets codepoint y at x = 0.
    fn mark_rows(page: &Page) {
        for y in 0..page.size.rows as usize {
            *cell_at(page, 0, y) = cpu(y as u32);
        }
    }

    #[test]
    fn page_list_split_at_middle_row() {
        let mut s = split_list();

        let page = pg(first(&s));

        // Write content to rows: row 0 gets codepoint 0, row 1 gets 1, etc.
        mark_rows(page);

        // Split at row 5 (middle)
        s.split(pin_at(first(&s), 0, 5)).unwrap();

        // Verify two pages exist
        assert!(s.pages.first.is_some());
        assert!(n(first(&s)).next.is_some());

        let first_page = pg(first(&s));
        let second_page = pg(next(first(&s)));

        // First page should have rows 0-4 (5 rows)
        assert_eq!(5, first_page.size.rows);
        // Second page should have rows 5-9 (5 rows)
        assert_eq!(5, second_page.size.rows);

        // Verify content in first page is preserved (rows 0-4 have codepoints 0-4)
        for y in 0..5 {
            assert_eq!(y as u32, cell_at(first_page, 0, y).content_codepoint());
        }

        // Verify content in second page (original rows 5-9, now at y=0-4)
        for y in 0..5 {
            assert_eq!(y as u32 + 5, cell_at(second_page, 0, y).content_codepoint());
        }
    }

    #[test]
    fn page_list_split_at_row_0_is_no_op() {
        let mut s = split_list();

        let page = pg(first(&s));

        // Write content to all rows
        mark_rows(page);

        // Split at row 0 should be a no-op
        s.split(pin_at(first(&s), 0, 0)).unwrap();

        // Verify only one page exists (no split occurred)
        assert!(s.pages.first.is_some());
        assert!(n(first(&s)).next.is_none());

        // Verify all content is still in the original page
        assert_eq!(10, page.size.rows);
        for y in 0..10 {
            assert_eq!(y as u32, cell_at(page, 0, y).content_codepoint());
        }
    }

    #[test]
    fn page_list_split_at_last_row() {
        let mut s = split_list();

        let page = pg(first(&s));

        // Write content to all rows
        mark_rows(page);

        // Split at last row (row 9)
        s.split(pin_at(first(&s), 0, 9)).unwrap();

        // Verify two pages exist
        assert!(s.pages.first.is_some());
        assert!(n(first(&s)).next.is_some());

        let first_page = pg(first(&s));
        let second_page = pg(next(first(&s)));

        // First page should have 9 rows
        assert_eq!(9, first_page.size.rows);
        // Second page should have 1 row
        assert_eq!(1, second_page.size.rows);

        // Verify content in second page (original row 9, now at y=0)
        assert_eq!(9, cell_at(second_page, 0, 0).content_codepoint());
    }

    #[test]
    fn page_list_split_single_row_page_returns_out_of_space() {
        // Initialize with 1 row
        let mut s = init_opts(Options {
            cols: 10,
            rows: 1,
            max_size: Some(0),
            ..Default::default()
        });

        let result = s.split(pin_at(first(&s), 0, 0));

        assert_eq!(Err(SplitError::OutOfSpace), result);
    }

    #[test]
    fn page_list_split_moves_tracked_pins() {
        let mut s = split_list();

        // Track a pin at row 7
        let tracked = s.track_pin(pin_at(first(&s), 3, 7)).unwrap();

        // Split at row 5
        s.split(pin_at(first(&s), 0, 5)).unwrap();

        // The tracked pin should now be in the second page
        assert!(tp(tracked).node == next(first(&s)));
        // y should be adjusted: was 7, split at 5, so new y = 7 - 5 = 2
        assert_eq!(2, tp(tracked).y);
        // x should remain unchanged
        assert_eq!(3, tp(tracked).x);

        s.untrack_pin(tracked);
    }

    #[test]
    fn page_list_split_tracked_pin_before_split_point_unchanged() {
        let mut s = split_list();

        let original_node = first(&s);

        // Track a pin at row 2 (before the split point)
        let tracked = s.track_pin(pin_at(original_node, 5, 2)).unwrap();

        // Split at row 5
        s.split(pin_at(original_node, 0, 5)).unwrap();

        // The tracked pin should remain in the original page
        assert!(tp(tracked).node == first(&s));
        // y and x should be unchanged
        assert_eq!(2, tp(tracked).y);
        assert_eq!(5, tp(tracked).x);

        s.untrack_pin(tracked);
    }

    #[test]
    fn page_list_split_tracked_pin_at_split_point_moves_to_new_page() {
        let mut s = split_list();

        let original_node = first(&s);

        // Track a pin at the exact split point (row 5)
        let tracked = s.track_pin(pin_at(original_node, 4, 5)).unwrap();

        // Split at row 5
        s.split(pin_at(original_node, 0, 5)).unwrap();

        // The tracked pin should be in the new page
        assert!(tp(tracked).node == next(first(&s)));
        // y should be 0 since it was at the split point: 5 - 5 = 0
        assert_eq!(0, tp(tracked).y);
        // x should remain unchanged
        assert_eq!(4, tp(tracked).x);

        s.untrack_pin(tracked);
    }

    #[test]
    fn page_list_split_multiple_tracked_pins_across_regions() {
        let mut s = split_list();

        let original_node = first(&s);

        // Track multiple pins in different regions
        let pin_before = s.track_pin(pin_at(original_node, 0, 1)).unwrap();
        let pin_at_split = s.track_pin(pin_at(original_node, 2, 5)).unwrap();
        let pin_after1 = s.track_pin(pin_at(original_node, 3, 7)).unwrap();
        let pin_after2 = s.track_pin(pin_at(original_node, 8, 9)).unwrap();

        // Split at row 5
        s.split(pin_at(original_node, 0, 5)).unwrap();

        let first_page = first(&s);
        let second_page = next(first_page);

        // Pin before split point stays in original page
        assert!(tp(pin_before).node == first_page);
        assert_eq!(1, tp(pin_before).y);
        assert_eq!(0, tp(pin_before).x);

        // Pin at split point moves to new page with y=0
        assert!(tp(pin_at_split).node == second_page);
        assert_eq!(0, tp(pin_at_split).y);
        assert_eq!(2, tp(pin_at_split).x);

        // Pins after split point move to new page with adjusted y
        assert!(tp(pin_after1).node == second_page);
        assert_eq!(2, tp(pin_after1).y); // 7 - 5 = 2
        assert_eq!(3, tp(pin_after1).x);

        assert!(tp(pin_after2).node == second_page);
        assert_eq!(4, tp(pin_after2).y); // 9 - 5 = 4
        assert_eq!(8, tp(pin_after2).x);

        s.untrack_pin(pin_after2);
        s.untrack_pin(pin_after1);
        s.untrack_pin(pin_at_split);
        s.untrack_pin(pin_before);
    }

    #[test]
    fn page_list_split_tracked_viewport_pin_in_split_region_moves_correctly() {
        let mut s = split_list();

        let original_node = first(&s);

        // Set viewport_pin to row 7 (after split point)
        tp(s.viewport_pin).node = original_node;
        tp(s.viewport_pin).y = 7;
        tp(s.viewport_pin).x = 6;

        // Split at row 5
        s.split(pin_at(original_node, 0, 5)).unwrap();

        // viewport_pin should be in the new page
        assert!(vp(&s).node == next(first(&s)));
        // y should be adjusted: 7 - 5 = 2
        assert_eq!(2, vp(&s).y);
        // x should remain unchanged
        assert_eq!(6, vp(&s).x);
    }

    #[test]
    fn page_list_split_middle_page_preserves_linked_list_order() {
        // Create a single page with 12 rows
        let mut s = init_opts(Options {
            cols: 10,
            rows: 12,
            max_size: Some(0),
            ..Default::default()
        });

        // Split at row 4 to create: page1 (rows 0-3), page2 (rows 4-11)
        let first_node = first(&s);
        s.split(pin_at(first_node, 0, 4)).unwrap();

        // Now we have 2 pages
        let page1 = first(&s);
        let page2 = next(first(&s));
        assert_eq!(4, n(page1).rows());
        assert_eq!(8, n(page2).rows());

        // Split page2 at row 4 to create: page1 -> page2 (rows 0-3) -> page3 (rows 4-7)
        s.split(pin_at(page2, 0, 4)).unwrap();

        // Now we have 3 pages
        let first_n = first(&s);
        let middle = next(first_n);
        let last_n = next(middle);

        // Verify linked list order: first -> middle -> last
        assert_eq!(page1, first_n);
        assert_eq!(page2, middle);
        assert_eq!(last(&s), last_n);

        // Verify prev pointers
        assert!(n(first_n).prev.is_none());
        assert_eq!(first_n, prev(middle));
        assert_eq!(middle, prev(last_n));

        // Verify next pointers
        assert_eq!(middle, next(first_n));
        assert_eq!(last_n, next(middle));
        assert!(n(last_n).next.is_none());

        // Verify row counts
        assert_eq!(4, n(first_n).rows());
        assert_eq!(4, n(middle).rows());
        assert_eq!(4, n(last_n).rows());
    }

    #[test]
    fn page_list_split_last_page_makes_new_page_the_last() {
        // Create a single page with 10 rows
        let mut s = split_list();

        // Split to create 2 pages first
        let first_node = first(&s);
        s.split(pin_at(first_node, 0, 5)).unwrap();

        // Now split the last page
        let last_before_split = last(&s);
        assert_eq!(5, n(last_before_split).rows());

        s.split(pin_at(last_before_split, 0, 2)).unwrap();

        // The new page should be the new last
        let new_last = last(&s);
        assert!(new_last != last_before_split);
        assert_eq!(last_before_split, prev(new_last));
        assert!(n(new_last).next.is_none());

        // Verify row counts: original last has 2 rows, new last has 3 rows
        assert_eq!(2, n(last_before_split).rows());
        assert_eq!(3, n(new_last).rows());
    }

    #[test]
    fn page_list_split_first_page_keeps_original_as_first() {
        // Create 2 pages by splitting
        let mut s = split_list();

        let original_first = first(&s);
        s.split(pin_at(original_first, 0, 5)).unwrap();

        // Get second page (created by first split)
        let second_page = next(first(&s));

        // Now split the first page again
        s.split(pin_at(first(&s), 0, 2)).unwrap();

        // Original first should still be first
        assert_eq!(original_first, first(&s));
        assert!(n(first(&s)).prev.is_none());

        // New page should be inserted between first and second
        let inserted = next(first(&s));
        assert!(inserted != second_page);
        assert_eq!(second_page, next(inserted));

        // Verify row counts: first has 2, inserted has 3, second has 5
        assert_eq!(2, n(first(&s)).rows());
        assert_eq!(3, n(inserted).rows());
        assert_eq!(5, n(second_page).rows());
    }

    #[test]
    fn page_list_split_preserves_wrap_flags() {
        let mut s = split_list();

        let page = pg(first(&s));

        // Set wrap flags on rows that will be in the second page after split
        // Row 5: wrap = true (this is the start of a wrapped line)
        // Row 6: wrap_continuation = true (this continues the wrap)
        // Row 7: wrap = true, wrap_continuation = true (wrapped and continues)
        set_wrap(page, 5);
        set_wrap_cont(page, 6);
        set_wrap(page, 7);
        set_wrap_cont(page, 7);

        // Split at row 5
        s.split(pin_at(first(&s), 0, 5)).unwrap();

        let second_page = pg(next(first(&s)));

        // Verify wrap flags are preserved in new page
        // Original row 5 is now row 0 in second page
        assert!(row_at(second_page, 0).wrap());
        assert!(!row_at(second_page, 0).wrap_continuation());

        // Original row 6 is now row 1 in second page
        assert!(!row_at(second_page, 1).wrap());
        assert!(row_at(second_page, 1).wrap_continuation());

        // Original row 7 is now row 2 in second page
        assert!(row_at(second_page, 2).wrap());
        assert!(row_at(second_page, 2).wrap_continuation());
    }

    #[test]
    fn page_list_split_preserves_styled_cells() {
        let mut s = split_list();

        let page = pg(first(&s));

        // Create a style and apply it to cells in rows 5-7 (which will be in the second page)
        let mut style = stylepkg::Style::default();
        style.flags.bold = true;
        let style_id = unsafe { page.styles.add(page.memory, style) }.unwrap();

        for y in 5..8 {
            let (row, cell) = rac(page, 0, y);
            let mut c = cp('S');
            c.set_style_id(style_id);
            *cell = c;
            row.set_styled(true);
            unsafe { page.styles.use_(page.memory, style_id) };
        }
        // Release the extra ref from add
        unsafe { page.styles.release(page.memory, style_id) };

        // Split at row 5
        s.split(pin_at(first(&s), 0, 5)).unwrap();

        let first_page = pg(first(&s));
        let second_page = pg(next(first(&s)));

        // First page should have no styles (all styled rows moved to second page)
        assert_eq!(0, first_page.styles.count());

        // Second page should have exactly 1 style (the bold style, used by 3 cells)
        assert_eq!(1, second_page.styles.count());

        // Verify styled cells are preserved in new page
        for y in 0..3 {
            let (row, cell) = rac(second_page, 0, y);
            assert_eq!('S' as u32, cell.content_codepoint());
            assert!(cell.style_id() != 0);

            let got_style = unsafe { &*second_page.styles.get(second_page.memory, cell.style_id()) };
            assert!(got_style.flags.bold);
            assert!(row.styled());
        }
    }

    #[test]
    fn page_list_split_preserves_grapheme_clusters() {
        let mut s = split_list();

        let page = pg(first(&s));

        // Add a grapheme cluster to row 6 (will be row 1 in second page after split at 5)
        {
            let (row, cell) = rac(page, 0, 6);
            *cell = cpu(0x1F468); // Man emoji
            page.set_graphemes(
                row,
                cell,
                &[
                    0x200D,  // ZWJ
                    0x1F469, // Woman emoji
                ],
            )
            .unwrap();
        }

        // Split at row 5
        s.split(pin_at(first(&s), 0, 5)).unwrap();

        let first_page = pg(first(&s));
        let second_page = pg(next(first(&s)));

        // First page should have no graphemes (the grapheme row moved to second page)
        assert_eq!(0, first_page.grapheme_count());

        // Second page should have exactly 1 grapheme
        assert_eq!(1, second_page.grapheme_count());

        // Verify grapheme is preserved in new page (original row 6 is now row 1)
        {
            let (row, cell) = rac(second_page, 0, 1);
            assert_eq!(0x1F468, cell.content_codepoint());
            assert!(row.grapheme());

            let cps = second_page.lookup_grapheme(cell).unwrap();
            assert_eq!(2, cps.len());
            assert_eq!(0x200D, cps[0]);
            assert_eq!(0x1F469, cps[1]);
        }
    }

    #[test]
    fn page_list_split_preserves_hyperlinks() {
        let mut s = split_list();

        let page = pg(first(&s));

        // Add a hyperlink to row 7 (will be row 2 in second page after split at 5)
        let hyperlink_id = page
            .insert_hyperlink(&hl_link(hyperlink::HyperlinkId::Implicit(0), b"https://example.com"))
            .unwrap();
        {
            let (row, cell) = rac(page, 0, 7);
            *cell = cp('L');
            page.set_hyperlink(row, cell, hyperlink_id).unwrap();
        }

        // Split at row 5
        s.split(pin_at(first(&s), 0, 5)).unwrap();

        let first_page = pg(first(&s));
        let second_page = pg(next(first(&s)));

        // First page should have no hyperlinks (the hyperlink row moved to second page)
        assert_eq!(0, first_page.hyperlink_set.count());

        // Second page should have exactly 1 hyperlink
        assert_eq!(1, second_page.hyperlink_set.count());

        // Verify hyperlink is preserved in new page (original row 7 is now row 2)
        {
            let cell = cell_at(second_page, 0, 2);
            assert_eq!('L' as u32, cell.content_codepoint());
            assert!(cell.hyperlink());

            let link_id = second_page.lookup_hyperlink(cell).unwrap();
            let link = unsafe { &*second_page.hyperlink_set.get(second_page.memory, link_id) };
            assert_eq!(b"https://example.com", unsafe { link.uri.slice(second_page.memory) });
        }
    }

    fn mark_row_metadata(row: &mut Row) {
        row.set_wrap(true);
        row.set_wrap_continuation(true);
        row.set_semantic_prompt(SemanticPrompt::Prompt);
    }

    fn assert_default_row(row: &Row) {
        assert!(!row.wrap());
        assert!(!row.wrap_continuation());
        assert_eq!(SemanticPrompt::None, row.semantic_prompt());
    }

    fn list_row<'a>(c: &Cell) -> &'a mut Row {
        unsafe { &mut *c.row }
    }

    #[test]
    fn page_list_erase_row_recycled_row_has_default_metadata() {
        let mut s = init(5, 3);

        // Simulate the top row being part of a soft-wrapped, prompt-marked
        // line. Erasing it recycles its Row storage as the new blank
        // bottom row, which must not retain any of this metadata.
        mark_row_metadata(list_row(&s.get_cell(pt_active(0, 0)).unwrap()));

        s.erase_row(pt_active(0, 0));

        assert_default_row(list_row(&s.get_cell(pt_active(0, 2)).unwrap()));
    }

    #[test]
    fn page_list_erase_row_bounded_recycled_row_has_default_metadata() {
        // A limit smaller than the remaining rows in the page exercises
        // the bounded-rotate branch; a larger limit exercises the fallback
        // branch that clears the final row after a full rotation.
        for limit in [1usize, 10] {
            let mut s = init(5, 3);

            mark_row_metadata(list_row(&s.get_cell(pt_active(0, 0)).unwrap()));

            s.erase_row_bounded(pt_active(0, 0), limit);

            let recycled_y = limit.min(2);
            assert_default_row(list_row(&s.get_cell(pt_active(0, recycled_y as u32)).unwrap()));
        }
    }

    #[test]
    fn page_list_erase_active_regrown_rows_have_default_metadata() {
        let mut s = init(5, 3);

        // Mark the rows that will be erased. eraseActive retires their
        // storage into unused page capacity and then regrows the active
        // area, re-exposing the same Row storage via the grow() fast path.
        for y in 0..2 {
            mark_row_metadata(list_row(&s.get_cell(pt_active(0, y)).unwrap()));
        }

        s.erase_active(1);

        for y in 0..3 {
            assert_default_row(list_row(&s.get_cell(pt_active(0, y)).unwrap()));
        }
    }

    fn bg_palette_cell(idx: u8) -> PCell {
        let mut c = PCell::default();
        c.set_content_tag(ContentTag::BgColorPalette);
        c.set_content_color_palette(idx);
        c
    }

    #[test]
    fn page_list_split_retired_rows_have_default_state() {
        let mut s = init(5, 10);

        // Put metadata and a background-colored (non-zero, but text-free)
        // cell on a row that the split will move to the new page. The
        // retired Row storage on the source page goes back into unused
        // capacity that grow() re-exposes without clearing.
        {
            let c = s.get_cell(pt_active(0, 7)).unwrap();
            let row = list_row(&c);
            row.set_wrap(true);
            row.set_semantic_prompt(SemanticPrompt::Prompt);
            unsafe { *c.cell = bg_palette_cell(42) };
        }

        let node = first(&s);
        s.split(s.pin(pt_active(0, 5)).unwrap()).unwrap();

        // The source page was truncated to 5 rows; peek at the retired
        // storage beyond size.rows.
        let page = pg(node);
        assert_eq!(5, page.size.rows);
        let rows = page.rows.ptr(page.memory);
        for y in 5..10 {
            let row = unsafe { *rows.add(y) };
            assert!(!row.wrap());
            assert!(!row.wrap_continuation());
            assert_eq!(SemanticPrompt::None, row.semantic_prompt());
            let cells = unsafe { core::slice::from_raw_parts(row.cells().ptr(page.memory), page.size.cols as usize) };
            for cell in cells {
                assert!(cell.is_zero());
            }
        }
    }

    #[test]
    fn page_list_resize_trimmed_rows_have_default_state() {
        let mut s = init(5, 5);

        // A trailing blank row has no text, so shrinking rows trims it,
        // but it can still carry metadata (e.g. a blank prompt
        // continuation line) and background-colored cells. Trimming
        // retires the storage into unused capacity that grow()
        // re-exposes without clearing.
        {
            let c = s.get_cell(pt_active(0, 4)).unwrap();
            let row = list_row(&c);
            row.set_wrap_continuation(true);
            row.set_semantic_prompt(SemanticPrompt::PromptContinuation);
            unsafe { *c.cell = bg_palette_cell(42) };
        }

        s.resize(no_reflow(None, Some(4))).unwrap();
        s.resize(no_reflow(None, Some(5))).unwrap();

        {
            let c = s.get_cell(pt_active(0, 4)).unwrap();
            assert_default_row(list_row(&c));
            assert!(unsafe { (*c.cell).is_zero() });
        }
    }

    /// Stand-in for Zig's `std.heap.FixedBufferAllocator`: a bump allocator
    /// over a fixed backing buffer that never frees.
    struct FixedBufferAllocator {
        base: *mut u8,
        len: usize,
        end_index: std::sync::atomic::AtomicUsize,
    }

    // SAFETY: test-only; the atomic index makes concurrent use sound.
    unsafe impl Sync for FixedBufferAllocator {}

    unsafe impl GlobalAlloc for FixedBufferAllocator {
        unsafe fn alloc(&self, layout: AllocLayout) -> *mut u8 {
            use std::sync::atomic::Ordering;
            let start = self.end_index.load(Ordering::Relaxed);
            let aligned = (self.base as usize + start).next_multiple_of(layout.align()) - self.base as usize;
            if aligned + layout.size() > self.len {
                return core::ptr::null_mut();
            }
            self.end_index.store(aligned + layout.size(), Ordering::Relaxed);
            unsafe { self.base.add(aligned) }
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: AllocLayout) {}
    }

    #[test]
    fn page_list_memory_pool_never_touches_idle_page_memory() {
        let preheat = PAGE_PREHEAT;

        // Back the page allocator with memory we can inspect.
        let backing = AlignedBuf::new(preheat * std_size());
        let backing_mem = backing.memory();
        let fba: &'static FixedBufferAllocator = Box::leak(Box::new(FixedBufferAllocator {
            base: backing_mem.ptr(),
            len: backing_mem.len(),
            end_index: std::sync::atomic::AtomicUsize::new(0),
        }));

        let mut pool = MemoryPool::init(Allocator::GLOBAL, Allocator::new(fba), preheat).unwrap();

        // Preheat allocated exactly the items.
        let end_index = || fba.end_index.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(preheat * std_size(), end_index());

        // Lay the sentinel down after preheat. The sentinel must differ
        // from Zig's 0xAA undefined pattern so that any write is visible.
        let sentinel: u8 = 0x5A;
        mem_of(backing_mem).fill(sentinel);

        // Every preheated item is handed out untouched and without going
        // back to the page allocator, and destroying it doesn't touch it.
        let mut items: Vec<PagePoolItemPtr> = Vec::new();
        for _ in 0..preheat {
            let item = pool.pages.create().unwrap();
            assert_eq!(preheat * std_size(), end_index());
            assert!(unsafe { pool_item(item) }.iter().all(|&b| b == sentinel));
            items.push(item);
        }
        for item in items {
            pool.pages.destroy(item);
        }
        assert!(mem_of(backing_mem).iter().all(|&b| b == sentinel));

        drop(pool);
        drop(backing);
    }

    fn mem_of<'a>(m: PageMemory) -> &'a mut [u8] {
        unsafe { m.as_mut_slice() }
    }

    #[test]
    fn page_list_memory_pool_fast_path_does_not_allocate() {
        let counting = FailingAllocator::leak(usize::MAX);

        let mut pool = MemoryPool::init(Allocator::GLOBAL, counting.allocator(), PAGE_PREHEAT).unwrap();
        assert_eq!(PAGE_PREHEAT, counting.allocations());

        // Cycle a few thousand pages through the preheated items. As long
        // as no more than the preheat are live at once, create is a
        // free-list pop and never touches the page allocator.
        let mut items: Vec<PagePoolItemPtr> = Vec::with_capacity(PAGE_PREHEAT + 2);
        for _ in 0..1024 {
            for _ in 0..PAGE_PREHEAT {
                items.push(pool.pages.create().unwrap());
            }
            for item in items.drain(..) {
                pool.pages.destroy(item);
            }
        }
        assert_eq!(PAGE_PREHEAT, counting.allocations());
        assert_eq!(0, counting.deallocations());

        // Going past the preheat allocates the extra items once; they are
        // recycled from then on.
        let extra_len = PAGE_PREHEAT + 2;
        for _ in 0..1024 {
            for _ in 0..extra_len {
                items.push(pool.pages.create().unwrap());
            }
            for item in items.drain(..) {
                pool.pages.destroy(item);
            }
        }
        assert_eq!(extra_len, counting.allocations());
        assert_eq!(0, counting.deallocations());
    }
}
