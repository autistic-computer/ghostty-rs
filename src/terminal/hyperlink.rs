//! Port of `src/terminal/hyperlink.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - [`Hyperlink`] borrows or owns its strings via `Cow` so that `dupe`
//!   (Zig: allocate copies) and `deinit` (Zig: free them) map onto Rust
//!   ownership; `deinit` is simply a drop.

use std::borrow::Cow;

use crate::terminal::hash_map::{AutoOffsetHashMap, Wyhash};
use crate::terminal::page::{Cell, Page};
use crate::terminal::ref_counted_set::{self, RefCountedSet, RefCountedSetContext};
use crate::terminal::size::{self, AllocError, Base, Offset, OffsetSlice};

/// The unique identifier for a hyperlink. This is at most the number of cells
/// that can fit in a single terminal page, since each cell can only contain
/// at most one hyperlink.
pub type Id = size::HyperlinkCountInt;

/// The mapping of cell to hyperlink. We use an offset hash map to save space
/// since its very unlikely a cell is a hyperlink, so its a waste to store
/// the hyperlink ID in the cell itself.
pub type Map = AutoOffsetHashMap<Offset<Cell>, Id, 80>;

/// A fully decoded hyperlink that may or may not have its
/// memory within a page. The memory location of this is dependent
/// on the context so users should check with the source of the
/// hyperlink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hyperlink<'a> {
    pub id: HyperlinkId<'a>,
    pub uri: Cow<'a, [u8]>,
}

/// See [`PageEntryId`]. (Zig: `Hyperlink.Id`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HyperlinkId<'a> {
    Explicit(Cow<'a, [u8]>),
    Implicit(size::OffsetInt),
}

impl<'a> Hyperlink<'a> {
    /// Deinit and deallocate all the owned values.
    ///
    /// WARNING: This should only be called if the hyperlink was
    /// heap-allocated. This DOES NOT need to be unconditionally
    /// called. (In Rust this is just a drop.)
    pub fn deinit(self) {}

    /// Duplicate a hyperlink by allocating all values. The returned
    /// hyperlink owns its memory.
    pub fn dupe(&self) -> Hyperlink<'static> {
        let uri: Cow<'static, [u8]> = Cow::Owned(self.uri.to_vec());
        let id = match &self.id {
            HyperlinkId::Implicit(v) => HyperlinkId::Implicit(*v),
            HyperlinkId::Explicit(v) => HyperlinkId::Explicit(Cow::Owned(v.to_vec())),
        };
        Hyperlink { id, uri }
    }
}

/// A hyperlink that has been committed to page memory. This
/// is a "page entry" because while it represents a hyperlink,
/// some decoding (pointer chasing) is still necessary to get the
/// fully realized ID, URI, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageEntry {
    pub id: PageEntryId,
    pub uri: OffsetSlice<u8>,
}

/// (Zig: `PageEntry.Id`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageEntryId {
    /// An explicitly provided ID via the OSC8 sequence.
    Explicit(OffsetSlice<u8>),

    /// No ID was provided so we auto-generate the ID based on an
    /// incrementing counter attached to the screen.
    Implicit(size::OffsetInt),
}

impl PageEntry {
    /// Duplicate this hyperlink from one page to another.
    pub fn dupe(&self, self_page: &Page, dst_page: &mut Page) -> Result<PageEntry, AllocError> {
        let mut copy = *self;

        // If the pages are the same then we can return a shallow copy.
        if core::ptr::eq(self_page, dst_page) {
            return Ok(copy);
        }

        let dst_mem = dst_page.memory;

        // Copy the URI
        {
            // SAFETY: the entry's strings live in `self_page`'s memory.
            let uri = unsafe { self.uri.slice(self_page.memory) };
            // SAFETY: `dst_mem` is the true base of the destination page.
            let buf = unsafe { dst_page.string_alloc.alloc::<u8>(dst_mem, uri.len())? };
            // SAFETY: `buf` was just allocated with `uri.len()` bytes.
            unsafe { core::ptr::copy_nonoverlapping(uri.as_ptr(), buf.cast::<u8>(), uri.len()) };
            copy.uri = OffsetSlice::new(size::get_offset(dst_mem, buf.cast::<u8>()), uri.len());
        }

        // Copy the ID
        match copy.id {
            PageEntryId::Implicit(_) => {} // Shallow is fine
            PageEntryId::Explicit(slice) => {
                // SAFETY: the entry's strings live in `self_page`'s memory.
                let id = unsafe { slice.slice(self_page.memory) };
                // SAFETY: `dst_mem` is the true base of the destination page.
                let buf = match unsafe { dst_page.string_alloc.alloc::<u8>(dst_mem, id.len()) } {
                    Ok(v) => v,
                    Err(e) => {
                        // errdefer: free the URI we already copied.
                        // SAFETY: allocated above from the same allocator.
                        unsafe {
                            dst_page
                                .string_alloc
                                .free(dst_mem, copy.uri.slice_ptr(dst_mem))
                        };
                        return Err(e);
                    }
                };
                // SAFETY: `buf` was just allocated with `id.len()` bytes.
                unsafe { core::ptr::copy_nonoverlapping(id.as_ptr(), buf.cast::<u8>(), id.len()) };
                copy.id = PageEntryId::Explicit(OffsetSlice::new(
                    size::get_offset(dst_mem, buf.cast::<u8>()),
                    id.len(),
                ));
            }
        }

        Ok(copy)
    }

    pub fn hash<B: Base + Copy>(&self, base: B) -> u64 {
        let mut hasher = Wyhash::init(0);
        // autoHash(activeTag): the tag of a two-field union(enum) is a u1,
        // hashed as one byte.
        let tag: u8 = match self.id {
            PageEntryId::Explicit(_) => 0,
            PageEntryId::Implicit(_) => 1,
        };
        hasher.update(&[tag]);
        match self.id {
            PageEntryId::Implicit(v) => hasher.update(&v.to_ne_bytes()),
            // SAFETY: `base` is the base of the memory holding the strings.
            PageEntryId::Explicit(slice) => hasher.update(unsafe { slice.slice(base) }),
        }
        // SAFETY: as above.
        hasher.update(unsafe { self.uri.slice(base) });
        hasher.final_()
    }

    pub fn eql<B1: Base + Copy, B2: Base + Copy>(&self, self_base: B1, other: &PageEntry, other_base: B2) -> bool {
        match (self.id, other.id) {
            (PageEntryId::Implicit(a), PageEntryId::Implicit(b)) => {
                if a != b {
                    return false;
                }
            }
            (PageEntryId::Explicit(a), PageEntryId::Explicit(b)) => {
                // SAFETY: each entry's strings live in its own base.
                let (a, b) = unsafe { (a.slice(self_base), b.slice(other_base)) };
                if a != b {
                    return false;
                }
            }
            _ => return false,
        }

        // SAFETY: each entry's strings live in its own base.
        unsafe { self.uri.slice(self_base) == other.uri.slice(other_base) }
    }

    /// Free the memory for this entry from its page.
    ///
    /// # Safety
    ///
    /// `page` must point to the live page whose string allocator holds this
    /// entry's strings. Only the page's `memory` and `string_alloc` fields
    /// are accessed (so this may be called while other fields of the page,
    /// e.g. the hyperlink set, are mutably borrowed).
    pub unsafe fn free(&self, page: *mut Page) {
        unsafe {
            let memory = (*page).memory;
            let alloc = &mut *core::ptr::addr_of_mut!((*page).string_alloc);
            match self.id {
                PageEntryId::Implicit(_) => {}
                PageEntryId::Explicit(v) => {
                    if v.len > 0 {
                        alloc.free(memory, v.slice_ptr(memory));
                    }
                }
            }
            if self.uri.len > 0 {
                alloc.free(memory, self.uri.slice_ptr(memory));
            }
        }
    }
}

/// The context for [`Set`] (Zig: the anonymous context struct).
#[derive(Debug, Clone, Copy)]
pub struct SetContext {
    /// The page which holds the strings for items in this set.
    pub page: *mut Page,

    /// The page which holds the strings for items
    /// looked up with, e.g., `add` or `lookup`,
    /// if different from the destination page.
    pub src_page: *const Page,
}

impl Default for SetContext {
    fn default() -> Self {
        Self {
            page: core::ptr::null_mut(),
            src_page: core::ptr::null(),
        }
    }
}

impl SetContext {
    /// A context for the given destination page.
    pub fn new(page: *mut Page) -> Self {
        Self {
            page,
            src_page: core::ptr::null(),
        }
    }

    fn src_memory(&self) -> crate::terminal::page::PageMemory {
        // SAFETY: the context is only used with live pages (Zig `.?`).
        unsafe {
            if !self.src_page.is_null() {
                (*self.src_page).memory
            } else {
                assert!(!self.page.is_null());
                (*self.page).memory
            }
        }
    }
}

impl RefCountedSetContext<PageEntry> for SetContext {
    fn hash(&self, link: &PageEntry) -> u64 {
        link.hash(self.src_memory())
    }

    fn eql(&self, a: &PageEntry, b: &PageEntry) -> bool {
        assert!(!self.page.is_null());
        // SAFETY: `page` is a live page (Zig `.?`).
        let page_mem = unsafe { (*self.page).memory };
        a.eql(self.src_memory(), b, page_mem)
    }

    fn deleted(&self, link: &PageEntry) {
        assert!(!self.page.is_null());
        // SAFETY: `page` is the live page owning the set's strings.
        unsafe { link.free(self.page) };
    }
}

/// The set of hyperlinks. This is ref-counted so that a set of cells
/// can share the same hyperlink without duplicating the data.
pub type Set = RefCountedSet<PageEntry, Id, size::CellCountInt, SetContext>;

/// The item type of [`Set`] (Zig: `Set.Item`).
pub type SetItem = ref_counted_set::Item<PageEntry, Id, size::CellCountInt>;
