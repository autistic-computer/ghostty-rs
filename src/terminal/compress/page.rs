//! Port of `src/terminal/compress/Page.zig`.
//!
//! STATUS: ported
//!
//! A compressed terminal page with an optional resident virtual mapping.
//!
//! Terminal pages have two kinds of state: the large `Page.memory` allocation
//! and a comparatively small `Page` value containing offsets, dimensions,
//! dirty state, and allocator metadata. Not all of the latter state lives in
//! the backing memory, so preserving only the memory bytes is insufficient.
//! We preserve the complete `Page` value instead. This follows the same model
//! as `Page.cloneBuf`: page internals use offsets, so a shallow page copy remains
//! valid when its memory contents are restored in another mapping.
//!
//! Live `PageList` nodes keep their virtual range allocated while asking the
//! operating system to discard physical pages. Immutable clones store an empty
//! memory slice and reserve a private range on first read. The saved raw length
//! and offsets let clones restore without retaining a source address.
//! Live restoration reuses its range and needs no allocation.
//!
//! The live-page state transition is:
//!
//! 1. Create this value while the source page is resident.
//! 2. Ask the OS to decommit the source page's memory.
//! 3. Replace the PageList node's resident state with this value.
//! 4. To restore, recommit the retained range and call `restore`.
//! 5. After committing the resident state, call `deinit` to free the encoded
//!    bytes.
//!
//! If decommit is unavailable or fails, the caller should deinitialize the
//! compressed candidate and leave the source page resident. This type does not
//! perform any virtual-memory operations itself.
//!
//! The planned native implementation uses `MADV_DONTNEED` on Linux and pairs
//! `MADV_FREE_REUSABLE` with `MADV_FREE_REUSE` on Darwin. Targets without a
//! reliable retained-mapping decommit operation should leave page compression
//! disabled rather than free and later reallocate the resident memory.
//!
//! Port notes: `encoded` is a raw slice pointer allocated from `alloc`
//! (Zig: `[]u8` owned by an `Allocator`), and `deinit` stays explicit
//! because the PageList node owning this value decides when to release it.
//! `shared` is a raw pointer to the reference count allocated from `alloc`
//! next to `encoded`; clones made by `clone_for` with the same allocator
//! share both and the last `deinit` frees them.

use core::fmt;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::lz4;
use crate::terminal::page::{Page as TerminalPage, PageMemory};
use crate::terminal::page_list::Allocator;
use crate::terminal::size::AllocError;

/// Copy `bytes` into a new allocation from `alloc` (Zig: `alloc.dupe`).
fn dupe(alloc: Allocator, bytes: &[u8]) -> Result<*mut [u8], AllocError> {
    let ptr = alloc.alloc_bytes(bytes.len(), 1)?;
    // SAFETY: `ptr` is a fresh allocation of `bytes.len()` bytes.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.as_ptr(), bytes.len()) };
    Ok(core::ptr::slice_from_raw_parts_mut(ptr.as_ptr(), bytes.len()))
}

/// Free an encoding allocated by `dupe`.
///
/// # Safety
///
/// `encoded` must come from `dupe` with the same allocator and not be used
/// afterwards.
unsafe fn free_encoded(alloc: Allocator, encoded: *mut [u8]) {
    if let Some(ptr) = NonNull::new(encoded as *mut u8) {
        unsafe { alloc.free_bytes(ptr, encoded.len(), 1) };
    }
}

/// A compressed terminal page (Zig: `compress.Page`).
#[derive(Debug)]
pub struct Page {
    /// Complete page metadata together with an optional resident mapping.
    ///
    /// The bytes in `page.memory` may have been discarded by the OS while this
    /// value is in compressed state. They must not be read until `restore` has
    /// successfully decoded the page.
    pub page: TerminalPage,

    /// Length of the raw page memory the encoding decodes to. An immutable
    /// clone keeps it while `page.memory` is empty.
    pub raw_len: usize,

    /// Allocator for the private raw mapping of an immutable clone, set by
    /// `clone_for`. A live page restores into its retained mapping instead.
    pub mapping_alloc: Option<Allocator>,

    /// Exact raw LZ4 block for `page.memory`.
    pub encoded: *mut [u8],

    /// Allocator which owns `encoded`.
    ///
    /// The allocator's backing state must outlive this compressed page. Storing
    /// the allocator here lets a PageList node restore and discard its compressed
    /// state without needing a reference back to the PageList.
    pub alloc: Allocator,

    /// Reference count of `encoded`, shared by every clone of this page
    /// made with the same allocator. Allocated from `alloc`.
    pub shared: NonNull<Shared>,
}

/// The reference count shared by compressed clones (Zig: `Page.Shared`).
#[derive(Debug)]
pub struct Shared {
    pub references: AtomicUsize,
}

/// Whether two allocator handles are the same allocator, so memory from one
/// can be freed through the other (Zig: `sameAllocator`).
///
/// Zig compares vtables first and recognizes its singleton allocators, whose
/// context pointers are undefined. Here the Rust global allocator handle is
/// the singleton, and a backend is a `'static` allocator object that is the
/// same allocator only as the same object with the same implementation. A
/// false negative only costs a copy of the encoding.
fn same_allocator(left: Allocator, right: Allocator) -> bool {
    match (left.backend(), right.backend()) {
        (None, None) => true,
        (Some(l), Some(r)) => core::ptr::eq(l, r),
        _ => false,
    }
}

/// `Allocator.Error || lz4.CompressError` (Zig: the error set of `init`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    OutOfMemory,
    InputTooLarge,
    OutputTooSmall,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for InitError {}

impl From<lz4::CompressError> for InitError {
    fn from(e: lz4::CompressError) -> Self {
        match e {
            lz4::CompressError::InputTooLarge => InitError::InputTooLarge,
            lz4::CompressError::OutputTooSmall => InitError::OutputTooSmall,
        }
    }
}

impl Page {
    /// Return the largest scratch buffer that can produce a useful compressed
    /// representation for `raw_len` bytes.
    ///
    /// This is deliberately smaller than the general LZ4 compression bound. The
    /// compressed representation includes an additional slice compared to a
    /// resident terminal page, so an encoded block which fills more than this
    /// buffer cannot reduce resident memory. Limiting the output here lets a
    /// PageList borrow a standard page-pool item as scratch instead of retaining a
    /// larger, compression-bound allocation.
    ///
    /// Callers are expected to reuse the scratch memory when practical. The
    /// scratch buffer and hash table are never retained by this type.
    pub fn required_scratch(raw_len: usize) -> Result<usize, lz4::CompressError> {
        // Validate the codec's input limit before doing arithmetic based on the
        // raw size. The bound itself is intentionally not returned; see above.
        lz4::compress_bound(raw_len)?;

        let representation_overhead = core::mem::size_of::<Page>() - core::mem::size_of::<TerminalPage>();
        if raw_len <= representation_overhead {
            return Ok(0);
        }

        // The savings comparison is strict, so reserve one fewer byte than the
        // break-even encoded size.
        Ok(raw_len - representation_overhead - 1)
    }

    /// Create a compressed representation of `source`.
    ///
    /// `source` is not modified and continues to own its backing allocation. The
    /// scratch buffer must be at least `requiredScratch(source.memory.len)` bytes.
    /// It must not overlap the source memory. On success, the returned value aliases
    /// the source's resident mapping and owns only its exact-sized `encoded`
    /// allocation. The allocator and its backing state must remain valid until
    /// `deinit` is called.
    ///
    /// Returns null if retaining the compressed representation would not use less
    /// memory than the resident representation. An `OutputTooSmall` result from
    /// the codec also means the encoding crossed that break-even point and is
    /// therefore returned as null. This comparison includes the in-memory size of
    /// both representation structs but does not include allocator metadata or the
    /// retained virtual address range.
    pub fn init(
        alloc: Allocator,
        source: &TerminalPage,
        scratch: &mut [u8],
        table: &mut lz4::HashTable,
    ) -> Result<Option<Page>, InitError> {
        let required = Self::required_scratch(source.memory.len())?;
        if scratch.len() < required {
            return Err(InitError::OutputTooSmall);
        }
        if required == 0 {
            return Ok(None);
        }

        // SAFETY: the source page is resident and its memory is valid for
        // reads; the scratch buffer doesn't overlap it.
        let input = unsafe { source.memory.as_slice() };
        let encoded_len = match lz4::compress(input, &mut scratch[..required], table) {
            Ok(v) => v,
            // The scratch limit is the largest representation worth retaining.
            // Running out of room therefore means compression cannot save memory,
            // rather than that the caller failed to provide the documented size.
            Err(lz4::CompressError::OutputTooSmall) => return Ok(None),
            Err(err @ lz4::CompressError::InputTooLarge) => return Err(err.into()),
        };

        // requiredScratch already accounts for the representation structs. The
        // retained virtual range contributes no resident bytes after PageList
        // decommits it.
        debug_assert!(
            core::mem::size_of::<Page>() + encoded_len
                < core::mem::size_of::<TerminalPage>() + source.memory.len()
        );

        let encoded = dupe(alloc, &scratch[..encoded_len]).map_err(|_| InitError::OutOfMemory)?;
        let shared = match alloc.create(Shared {
            references: AtomicUsize::new(1),
        }) {
            Ok(v) => v,
            Err(_) => {
                // SAFETY: allocated by `dupe` above.
                unsafe { free_encoded(alloc, encoded) };
                return Err(InitError::OutOfMemory);
            }
        };

        Ok(Some(Page {
            page: *source,
            raw_len: source.memory.len(),
            mapping_alloc: None,
            encoded,
            alloc,
            shared,
        }))
    }

    /// Make an immutable clone of this page for another owner.
    ///
    /// The clone starts with an empty `page.memory`; its owner maps a
    /// private range of `raw_len` bytes from `page_alloc` on first read.
    /// The encoding is shared when `alloc` is the allocator that owns it,
    /// and copied into `alloc` otherwise.
    pub fn clone_for(&self, alloc: Allocator, page_alloc: Allocator) -> Result<Page, AllocError> {
        let mut result = Page {
            page: self.page,
            raw_len: self.raw_len,
            mapping_alloc: self.mapping_alloc,
            encoded: self.encoded,
            alloc: self.alloc,
            shared: self.shared,
        };
        result.page.memory = PageMemory::EMPTY;
        result.mapping_alloc = Some(page_alloc);
        if same_allocator(self.alloc, alloc) {
            // SAFETY: `shared` lives while any page holding it lives.
            unsafe { self.shared.as_ref() }.references.fetch_add(1, Ordering::Relaxed);
            return Ok(result);
        }
        result.encoded = dupe(alloc, self.encoded())?;
        result.shared = match alloc.create(Shared {
            references: AtomicUsize::new(1),
        }) {
            Ok(v) => v,
            Err(e) => {
                // SAFETY: allocated by `dupe` above.
                unsafe { free_encoded(alloc, result.encoded) };
                return Err(e);
            }
        };
        result.alloc = alloc;
        Ok(result)
    }

    /// The encoded block (Zig: reading `encoded`).
    #[inline]
    pub fn encoded(&self) -> &[u8] {
        // SAFETY: `encoded` is owned by this value until `deinit`.
        unsafe { &*self.encoded }
    }

    /// Release this page's reference to the encoded block, freeing it with
    /// the last reference.
    ///
    /// This intentionally does not free `page.memory`. The PageList node which
    /// supplied the source page continues to own that pool or heap allocation.
    pub fn deinit(&mut self) {
        // SAFETY: `shared` lives while any page holding it lives. AcqRel
        // orders every other owner's last use of `encoded` before the free.
        if unsafe { self.shared.as_ref() }.references.fetch_sub(1, Ordering::AcqRel) == 1 {
            // SAFETY: the last reference; both were allocated from `alloc`.
            unsafe {
                free_encoded(self.alloc, self.encoded);
                self.alloc.destroy(self.shared);
            }
        }
        self.encoded = core::ptr::slice_from_raw_parts_mut(NonNull::<u8>::dangling().as_ptr(), 0);
        self.shared = NonNull::dangling();
    }

    /// Restore the embedded terminal page into its retained resident mapping.
    ///
    /// The caller must recommit `page.memory` before calling this on platforms
    /// which require an explicit recommit operation. The compressed value remains
    /// valid whether decoding succeeds or fails, allowing the caller to retry or
    /// discard it. On success the returned page aliases the same resident mapping;
    /// it does not own a new allocation.
    pub fn restore(&self) -> Result<TerminalPage, lz4::DecompressError> {
        let result = self.page;
        assert_eq!(result.memory.len(), self.raw_len);
        // SAFETY: the retained mapping is valid (and recommitted) memory owned
        // by the node; nothing else accesses it while it is compressed.
        let output = unsafe { result.memory.as_mut_slice() };
        lz4::decompress(self.encoded(), output)?;
        Ok(result)
    }

    /// Clone this page into caller-owned memory without restoring its mapping.
    ///
    /// `memory` must be at least as large as the retained page mapping. Decoding
    /// writes only to that buffer, so both the discarded contents of `page.memory`
    /// and this value's encoded representation remain unchanged. The returned Page
    /// borrows `memory`; the caller must keep it alive for the Page's lifetime and
    /// release it directly rather than calling `TerminalPage.deinit`.
    ///
    /// # Safety
    ///
    /// `memory` must be valid, page-aligned, writable memory that doesn't
    /// overlap the retained mapping.
    pub unsafe fn clone_buf(&self, memory: PageMemory) -> Result<TerminalPage, lz4::DecompressError> {
        assert!(memory.len() >= self.raw_len);

        // Page internals are offsets into the backing buffer, so all metadata can
        // be copied verbatim when paired with an equally laid-out mapping.
        let mut result = self.page;
        result.memory = memory.prefix(self.raw_len);
        // SAFETY: caller contract.
        let output = unsafe { result.memory.as_mut_slice() };
        lz4::decompress(self.encoded(), output)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::hyperlink::{Hyperlink, HyperlinkId};
    use crate::terminal::page::{Capacity, Cell};
    use crate::terminal::page_list::testing::{AlignedBuf, FailingAllocator, TestPage};
    use crate::terminal::style::Style;
    use std::borrow::Cow;

    fn new_table() -> Box<lz4::HashTable> {
        Box::new([0; 4096])
    }

    #[test]
    fn compressed_page_retained_mapping_round_trip() {
        let mut resident = TestPage(
            TerminalPage::init(Capacity {
                styles: 8,
                grapheme_bytes: 128,
                string_bytes: 128,
                ..Capacity::new(12, 9)
            })
            .unwrap(),
        );
        resident.size.cols = 10;
        resident.size.rows = 7;
        resident.dirty = true;

        // Put state in both the backing memory and the Page value. In particular,
        // the style and hyperlink sets retain live counters outside Page.memory.
        let rac = resident.get_row_and_cell(2, 3);
        let (row, cell) = unsafe { (&mut *rac.row, &mut *rac.cell) };
        *cell = Cell::init('A' as u32);
        resident.append_grapheme(row, cell, 0x0301).unwrap();

        let mut bold = Style::default();
        bold.flags.bold = true;
        let mem = resident.memory;
        let style_id = unsafe { resident.styles.add(mem, bold) }.unwrap();
        cell.set_style_id(style_id);
        row.set_styled(true);

        let hyperlink_id = resident
            .insert_hyperlink(&Hyperlink {
                id: HyperlinkId::Explicit(Cow::Borrowed(b"compressed-page")),
                uri: Cow::Borrowed(b"https://ghostty.org/docs"),
            })
            .unwrap();
        resident.set_hyperlink(row, cell, hyperlink_id).unwrap();
        resident.verify_integrity().unwrap();

        let expected = unsafe { resident.memory.as_slice() }.to_vec();
        let memory_ptr = resident.memory.ptr();
        let memory_len = resident.memory.len();

        let mut scratch = vec![0u8; Page::required_scratch(resident.memory.len()).unwrap()];
        let mut table = new_table();
        let mut compressed = Page::init(Allocator::GLOBAL, &resident, &mut scratch, &mut table)
            .unwrap()
            .unwrap();

        assert_eq!(memory_ptr, compressed.page.memory.ptr());
        assert_eq!(memory_len, compressed.page.memory.len());
        assert!(compressed.encoded().len() < resident.memory.len());
        let expected_encoded = compressed.encoded().to_vec();

        // Virtual-memory operations belong to PageList, so clearing the contents
        // models a successful decommit: none of the resident bytes remain.
        unsafe { resident.memory.as_mut_slice() }.fill(0);

        // A clone decodes into independent storage for read-only consumers which
        // must not change the compressed representation. In particular, it does
        // not recommit or overwrite the retained source mapping.
        let clone_memory = AlignedBuf::new(resident.memory.len());
        let cloned = unsafe { compressed.clone_buf(clone_memory.memory()) }.unwrap();
        assert_ne!(cloned.memory.ptr(), memory_ptr);
        assert!(unsafe { resident.memory.as_slice() }.iter().all(|&b| b == 0));
        assert_eq!(&expected_encoded[..], compressed.encoded());
        assert_eq!(&expected[..], unsafe { cloned.memory.as_slice() });
        assert_eq!(resident.size, cloned.size);
        assert!(cloned.dirty);
        cloned.verify_integrity().unwrap();

        let restored = compressed.restore().unwrap();
        assert_eq!(memory_ptr, restored.memory.ptr());
        assert_eq!(memory_len, restored.memory.len());
        assert_eq!(&expected[..], unsafe { restored.memory.as_slice() });
        assert_eq!(resident.size, restored.size);
        assert!(restored.dirty);
        restored.verify_integrity().unwrap();

        let restored_rac = restored.get_row_and_cell(2, 3);
        let restored_cell = unsafe { &*restored_rac.cell };
        assert_eq!(&[0x0301u32][..], restored.lookup_grapheme(restored_cell).unwrap());
        assert!(unsafe { (*restored.styles.get(restored.memory, restored_cell.style_id())).flags.bold });

        let restored_hyperlink_id = restored.lookup_hyperlink(restored_cell).unwrap();
        let restored_hyperlink = unsafe { &*restored.hyperlink_set.get(restored.memory, restored_hyperlink_id) };
        assert_eq!(
            b"https://ghostty.org/docs",
            unsafe { restored_hyperlink.uri.slice(restored.memory) }
        );

        compressed.deinit();
    }

    #[test]
    fn compressed_page_requires_the_maximum_useful_scratch() {
        let resident = TestPage(TerminalPage::init(Capacity::new(4, 4)).unwrap());
        let expected = unsafe { resident.memory.as_slice() }.to_vec();

        let required = Page::required_scratch(resident.memory.len()).unwrap();
        assert_eq!(
            resident.memory.len() - (core::mem::size_of::<Page>() - core::mem::size_of::<TerminalPage>()) - 1,
            required
        );
        assert!(required < lz4::compress_bound(resident.memory.len()).unwrap());

        let mut scratch = vec![0u8; required - 1];
        let mut table = new_table();

        assert_eq!(
            InitError::OutputTooSmall,
            Page::init(Allocator::GLOBAL, &resident, &mut scratch, &mut table).unwrap_err()
        );
        assert_eq!(&expected[..], unsafe { resident.memory.as_slice() });
    }

    #[test]
    fn compressed_page_rejects_a_representation_without_savings() {
        let resident = TestPage(
            TerminalPage::init(Capacity {
                styles: 0,
                grapheme_bytes: 0,
                string_bytes: 0,
                hyperlink_bytes: 0,
                ..Capacity::new(4, 4)
            })
            .unwrap(),
        );

        let mut prng = crate::terminal::compress::lz4::test_rng::Prng::init(0x4C5A_3402);
        prng.bytes(unsafe { resident.memory.as_mut_slice() });

        let mut scratch = vec![0u8; Page::required_scratch(resident.memory.len()).unwrap()];
        let mut table = new_table();
        let failing = FailingAllocator::leak(0);

        assert!(Page::init(failing.allocator(), &resident, &mut scratch, &mut table)
            .unwrap()
            .is_none());
        assert!(!failing.has_induced_failure());
    }

    #[test]
    fn compressed_page_can_retry_after_malformed_encoded_data() {
        let resident = TestPage(TerminalPage::init(Capacity::new(8, 8)).unwrap());
        let expected = unsafe { resident.memory.as_slice() }.to_vec();

        let mut scratch = vec![0u8; Page::required_scratch(resident.memory.len()).unwrap()];
        let mut table = new_table();
        let mut compressed = Page::init(Allocator::GLOBAL, &resident, &mut scratch, &mut table)
            .unwrap()
            .unwrap();

        let full_encoded = compressed.encoded;
        let first_byte = unsafe { (*full_encoded)[0] };
        compressed.encoded = core::ptr::slice_from_raw_parts_mut(full_encoded as *mut u8, 1);
        unsafe { (*compressed.encoded)[0] = 0xF0 };

        let clone_memory = AlignedBuf::new(resident.memory.len());
        assert_eq!(
            lz4::DecompressError::TruncatedInput,
            unsafe { compressed.clone_buf(clone_memory.memory()) }.unwrap_err()
        );
        assert_eq!(lz4::DecompressError::TruncatedInput, compressed.restore().unwrap_err());

        compressed.encoded = full_encoded;
        unsafe { (*compressed.encoded)[0] = first_byte };
        unsafe { resident.memory.as_mut_slice() }.fill(0);
        let restored = compressed.restore().unwrap();
        assert_eq!(&expected[..], unsafe { restored.memory.as_slice() });

        compressed.deinit();
    }

    fn refs(page: &Page) -> usize {
        unsafe { page.shared.as_ref() }.references.load(Ordering::Relaxed)
    }

    #[test]
    fn copy_on_write_compressed_singleton_allocators_share_encoding() {
        // Zig checks its c, page and smp allocators; the Rust singletons are
        // the global allocator handle and the OS page allocator.
        let allocators = [Allocator::GLOBAL, Allocator::page()];
        let other = FailingAllocator::leak(usize::MAX);
        let resident = TestPage(TerminalPage::init(Capacity::new(12, 9)).unwrap());
        unsafe { *resident.get_row_and_cell(2, 3).cell = Cell::init('Q' as u32) };
        let mut scratch = vec![0u8; Page::required_scratch(resident.memory.len()).unwrap()];
        let memory = AlignedBuf::new(resident.memory.len());
        let mut table = new_table();
        for alloc in allocators {
            assert!(same_allocator(alloc, alloc));
            assert!(!same_allocator(alloc, other.allocator()));
            let mut source = Page::init(alloc, &resident, &mut scratch, &mut table).unwrap().unwrap();
            let mut frozen = source.clone_for(alloc, Allocator::GLOBAL).unwrap();
            assert_eq!(source.encoded as *mut u8, frozen.encoded as *mut u8);
            assert_eq!(2, refs(&source));
            source.deinit();
            frozen.page.memory = memory.memory();
            let restored = frozen.restore().unwrap();
            assert_eq!('Q' as u32, unsafe { (*restored.get_row_and_cell(2, 3).cell).codepoint() });
            frozen.deinit();
        }
    }

    #[test]
    fn copy_on_write_compressed_custom_allocators_preserve_allocator_identity() {
        // Zig uses two FixedBufferAllocators; here two distinct allocator
        // objects.
        let left = FailingAllocator::leak(usize::MAX).allocator();
        let right = FailingAllocator::leak(usize::MAX).allocator();
        assert!(same_allocator(left, left));
        assert!(!same_allocator(left, right));
        let resident = TestPage(TerminalPage::init(Capacity::new(12, 9)).unwrap());
        unsafe { *resident.get_row_and_cell(4, 5).cell = Cell::init('R' as u32) };
        let mut scratch = vec![0u8; Page::required_scratch(resident.memory.len()).unwrap()];
        let memory = AlignedBuf::new(resident.memory.len());
        let mut table = new_table();
        let mut source = Page::init(left, &resident, &mut scratch, &mut table).unwrap().unwrap();
        let mut shared = source.clone_for(left, Allocator::GLOBAL).unwrap();
        let mut copied = source.clone_for(right, Allocator::GLOBAL).unwrap();
        assert_eq!(source.encoded as *mut u8, shared.encoded as *mut u8);
        assert_ne!(source.encoded as *mut u8, copied.encoded as *mut u8);
        assert_eq!(source.encoded(), copied.encoded());
        assert_eq!(2, refs(&source));
        assert_eq!(1, refs(&copied));
        copied.page.memory = memory.memory();
        let restored = copied.restore().unwrap();
        assert_eq!('R' as u32, unsafe { (*restored.get_row_and_cell(4, 5).cell).codepoint() });
        copied.deinit();
        shared.deinit();
        source.deinit();
    }
}
