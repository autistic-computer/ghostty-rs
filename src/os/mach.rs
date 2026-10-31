//! Port of `src/os/mach.zig`.
//!
//! STATUS: ported
//!
//! The tagged page allocator is meaningful on macOS (where the mmap fd
//! argument of an anonymous mapping carries the VM tag); it is compiled on
//! every Unix since other kernels ignore that argument.

#[cfg(unix)]
use std::sync::atomic::{AtomicPtr, Ordering};

#[cfg(unix)]
use crate::lib::allocator::ZigAllocator;

/// macOS virtual memory tags for use with mach_vm_map/mach_vm_allocate.
/// These identify memory regions in tools like vmmap and Instruments.
///
/// Non-exhaustive (Zig: `enum(u8) { ..., _ }`), so it is a newtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VMTag(pub u8);

impl VMTag {
    pub const APPLICATION_SPECIFIC_1: VMTag = VMTag(240);
    pub const APPLICATION_SPECIFIC_2: VMTag = VMTag(241);
    pub const APPLICATION_SPECIFIC_3: VMTag = VMTag(242);
    pub const APPLICATION_SPECIFIC_4: VMTag = VMTag(243);
    pub const APPLICATION_SPECIFIC_5: VMTag = VMTag(244);
    pub const APPLICATION_SPECIFIC_6: VMTag = VMTag(245);
    pub const APPLICATION_SPECIFIC_7: VMTag = VMTag(246);
    pub const APPLICATION_SPECIFIC_8: VMTag = VMTag(247);
    pub const APPLICATION_SPECIFIC_9: VMTag = VMTag(248);
    pub const APPLICATION_SPECIFIC_10: VMTag = VMTag(249);
    pub const APPLICATION_SPECIFIC_11: VMTag = VMTag(250);
    pub const APPLICATION_SPECIFIC_12: VMTag = VMTag(251);
    pub const APPLICATION_SPECIFIC_13: VMTag = VMTag(252);
    pub const APPLICATION_SPECIFIC_14: VMTag = VMTag(253);
    pub const APPLICATION_SPECIFIC_15: VMTag = VMTag(254);
    pub const APPLICATION_SPECIFIC_16: VMTag = VMTag(255);

    // We ignore the rest because we never realistic set them.

    /// Converts the tag to the format expected by
    /// mach_vm_map/mach_vm_allocate. Equivalent to C macro:
    /// VM_MAKE_TAG(tag)
    pub const fn make(self) -> i32 {
        ((self.0 as u32) << 24) as i32
    }
}

/// Creates a page allocator that tags all allocated memory with the given
/// VMTag.
#[cfg(unix)]
pub fn tagged_page_allocator(tag: VMTag) -> TaggedPageAllocator {
    TaggedPageAllocator { tag }
}

/// Static local next_mmap_addr_hint since this has been removed from
/// stdlib now.
#[cfg(unix)]
static NEXT_MMAP_ADDR_HINT: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());

#[cfg(unix)]
fn page_size() -> usize {
    // SAFETY: sysconf has no preconditions.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

#[cfg(unix)]
fn align_forward(addr: usize, alignment: usize) -> usize {
    (addr + alignment - 1) & !(alignment - 1)
}

/// This is based heavily on the Zig 0.15.2 PageAllocator implementation,
/// with only the posix implementation. Zig 0.15.2 is MIT licensed.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaggedPageAllocator {
    pub tag: VMTag,
}

#[cfg(unix)]
impl TaggedPageAllocator {
    /// Map `n` bytes with the given alignment (log2) and tag.
    pub fn map(n: usize, alignment: u8, tag: VMTag) -> *mut u8 {
        let page_size = page_size();
        if n >= usize::MAX - page_size {
            return std::ptr::null_mut();
        }
        let alignment_bytes = 1usize << alignment;

        let aligned_len = align_forward(n, page_size);
        let max_drop_len = alignment_bytes - alignment_bytes.min(page_size);
        let overalloc_len = if max_drop_len <= aligned_len - n {
            aligned_len
        } else {
            align_forward(aligned_len + max_drop_len, page_size)
        };
        let hint = NEXT_MMAP_ADDR_HINT.load(Ordering::Relaxed);
        // SAFETY: anonymous private mapping; the hint is only a hint.
        let slice = unsafe {
            libc::mmap(
                hint.cast(),
                overalloc_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                tag.make(),
                0,
            )
        };
        if slice == libc::MAP_FAILED {
            return std::ptr::null_mut();
        }
        let slice = slice as *mut u8;
        let result_ptr = align_forward(slice as usize, alignment_bytes) as *mut u8;
        // Unmap the extra bytes that were only requested in order to
        // guarantee that the range of memory we were provided had a proper
        // alignment in it somewhere. The extra bytes could be at the
        // beginning, or end, or both.
        let drop_len = result_ptr as usize - slice as usize;
        if drop_len != 0 {
            // SAFETY: unmapping part of our own mapping.
            unsafe { libc::munmap(slice.cast(), drop_len) };
        }
        let remaining_len = overalloc_len - drop_len;
        if remaining_len > aligned_len {
            // SAFETY: unmapping part of our own mapping.
            unsafe {
                libc::munmap(
                    result_ptr.add(aligned_len).cast(),
                    remaining_len - aligned_len,
                )
            };
        }
        // SAFETY: still within (one past) our mapping.
        let new_hint = unsafe { result_ptr.add(aligned_len) };
        let _ = NEXT_MMAP_ADDR_HINT.compare_exchange(
            hint,
            new_hint,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        result_ptr
    }

    pub fn unmap(memory: *mut u8, len: usize) {
        let page_aligned_len = align_forward(len, page_size());
        // SAFETY: unmapping memory we mapped.
        unsafe { libc::munmap(memory.cast(), page_aligned_len) };
    }

    pub fn realloc(memory: *mut u8, memory_len: usize, new_len: usize, may_move: bool) -> *mut u8 {
        let page_size = page_size();
        let new_size_aligned = align_forward(new_len, page_size);

        let page_aligned_len = align_forward(memory_len, page_size);
        if new_size_aligned == page_aligned_len {
            return memory;
        }

        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            // TODO: if the next_mmap_addr_hint is within the remapped
            // range, update it
            let flags = if may_move { libc::MREMAP_MAYMOVE } else { 0 };
            // SAFETY: remapping our own mapping.
            let new_memory = unsafe { libc::mremap(memory.cast(), memory_len, new_len, flags) };
            if new_memory == libc::MAP_FAILED {
                return std::ptr::null_mut();
            }
            return new_memory as *mut u8;
        }

        #[allow(unreachable_code)]
        {
            let _ = may_move;
            if new_size_aligned < page_aligned_len {
                // SAFETY: within our mapping.
                let ptr = unsafe { memory.add(new_size_aligned) };
                // TODO: if the next_mmap_addr_hint is within the unmapped
                // range, update it
                // SAFETY: unmapping the tail of our own mapping.
                unsafe { libc::munmap(ptr.cast(), page_aligned_len - new_size_aligned) };
                return memory;
            }

            std::ptr::null_mut()
        }
    }
}

#[cfg(unix)]
impl ZigAllocator for TaggedPageAllocator {
    fn alloc(&self, n: usize, alignment: u8, _ra: usize) -> *mut u8 {
        debug_assert!(n > 0);
        Self::map(n, alignment, self.tag)
    }

    fn resize(
        &self,
        memory: *mut u8,
        memory_len: usize,
        _alignment: u8,
        new_len: usize,
        _ra: usize,
    ) -> bool {
        !Self::realloc(memory, memory_len, new_len, false).is_null()
    }

    fn remap(
        &self,
        memory: *mut u8,
        memory_len: usize,
        _alignment: u8,
        new_len: usize,
        _ra: usize,
    ) -> *mut u8 {
        Self::realloc(memory, memory_len, new_len, true)
    }

    fn free(&self, memory: *mut u8, memory_len: usize, _alignment: u8, _ra: usize) {
        Self::unmap(memory, memory_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_tag_make() {
        assert_eq!((240u32 << 24) as i32, VMTag::APPLICATION_SPECIFIC_1.make());
    }

    #[cfg(unix)]
    #[test]
    fn tagged_page_allocator_roundtrip() {
        let alloc = tagged_page_allocator(VMTag::APPLICATION_SPECIFIC_1);
        let p = alloc.alloc(100, 14, 0);
        assert!(!p.is_null());
        assert_eq!(0, p as usize % (1 << 14));
        // SAFETY: 100 bytes were mapped.
        unsafe { std::ptr::write_bytes(p, 0xAA, 100) };
        // Same page count: resize in place succeeds.
        assert!(alloc.resize(p, 100, 14, 200, 0));
        alloc.free(p, 200, 14, 0);
    }
}
