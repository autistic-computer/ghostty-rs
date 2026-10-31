//! Port of `src/terminal/compress/Page.zig`.
//!
//! STATUS: ported
//!
//! A compressed terminal page which retains its resident virtual mapping.
//!
//! Terminal pages have two kinds of state: the large `Page.memory` allocation
//! and a comparatively small `Page` value containing offsets, dimensions,
//! dirty state, and allocator metadata. Not all of the latter state lives in
//! the backing memory, so preserving only the memory bytes is insufficient.
//! We preserve the complete `Page` value instead. This follows the same model
//! as `Page.cloneBuf`: page internals use offsets, so a shallow page copy remains
//! valid when its memory contents are restored at the same address.
//!
//! The resident memory is deliberately not freed by this type. `PageList`
//! keeps the virtual range allocated while asking the operating system to
//! discard its physical pages. Keeping the range has two useful properties:
//! the embedded page never contains a dangling pointer, and restoring the page
//! does not require a fallible allocation.
//!
//! The intended state transition is:
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

use core::fmt;
use core::ptr::NonNull;

use super::lz4;
use crate::terminal::page::{Page as TerminalPage, PageMemory};
use crate::terminal::page_list::Allocator;

/// A compressed terminal page (Zig: `compress.Page`).
#[derive(Debug)]
pub struct Page {
    /// Complete page metadata together with the retained resident mapping.
    ///
    /// The bytes in `page.memory` may have been discarded by the OS while this
    /// value is in compressed state. They must not be read until `restore` has
    /// successfully decoded the page.
    pub page: TerminalPage,

    /// Exact raw LZ4 block for `page.memory`.
    pub encoded: *mut [u8],

    /// Allocator which owns `encoded`.
    ///
    /// The allocator's backing state must outlive this compressed page. Storing
    /// the allocator here lets a PageList node restore and discard its compressed
    /// state without needing a reference back to the PageList.
    pub alloc: Allocator,
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

        // alloc.dupe
        let ptr = alloc
            .alloc_bytes(encoded_len, 1)
            .map_err(|_| InitError::OutOfMemory)?;
        // SAFETY: `ptr` is a fresh allocation of `encoded_len` bytes.
        unsafe { core::ptr::copy_nonoverlapping(scratch.as_ptr(), ptr.as_ptr(), encoded_len) };

        Ok(Some(Page {
            page: *source,
            encoded: core::ptr::slice_from_raw_parts_mut(ptr.as_ptr(), encoded_len),
            alloc,
        }))
    }

    /// The encoded block (Zig: reading `encoded`).
    #[inline]
    pub fn encoded(&self) -> &[u8] {
        // SAFETY: `encoded` is owned by this value until `deinit`.
        unsafe { &*self.encoded }
    }

    /// Free the encoded block.
    ///
    /// This intentionally does not free `page.memory`. The PageList node which
    /// supplied the source page continues to own that pool or heap allocation.
    pub fn deinit(&mut self) {
        let len = self.encoded.len();
        if let Some(ptr) = NonNull::new(self.encoded as *mut u8) {
            // SAFETY: allocated with `alloc_bytes(len, 1)` in `init`.
            unsafe { self.alloc.free_bytes(ptr, len, 1) };
        }
        self.encoded = core::ptr::slice_from_raw_parts_mut(NonNull::<u8>::dangling().as_ptr(), 0);
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
        assert!(memory.len() >= self.page.memory.len());

        // Page internals are offsets into the backing buffer, so all metadata can
        // be copied verbatim when paired with an equally laid-out mapping.
        let mut result = self.page;
        result.memory = memory.prefix(self.page.memory.len());
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
}
