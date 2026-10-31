//! Port of `src/terminal/mem.zig`.
//!
//! STATUS: ported
//!
//! Give the physical memory behind terminal pages back to the operating
//! system while keeping their addresses.
//!
//! Terminal page memory is page-aligned and a whole number of pages long.
//! When a page's contents are no longer needed in place (for example, after
//! the page has been compressed), its owner can release the physical memory
//! behind it but keep the address range. Later, the owner can reuse the same
//! range without allocating again.
//!
//! A page goes through three steps:
//!
//! 1. `decommit` releases the physical memory. The range stays mapped, so
//!    reading it is safe. It reads as zeros, or on some platforms as the old
//!    contents, until it is written again.
//! 2. `recommit` prepares the range for use again.
//! 3. The owner writes new contents into the range.
//!
//! For example, a page whose contents were copied somewhere else first:
//!
//! ```ignore
//! if mem::can_reclaim(DecommitMode::Strict) && mem::decommit(DecommitMode::Strict, page, page.len()) {
//!     // The physical memory is released. `page` is still a valid range.
//!     // ...
//!     // Later, before reusing it:
//!     mem::recommit(page);
//!     page.copy_from_slice(saved_contents);
//! }
//! ```
//!
//! This module does not allocate memory or decide which pages to release.
//!
//! After a successful decommit, the range is still mapped and still counted
//! against the process's memory limits. This is why `recommit` never has to
//! acquire anything and cannot fail. Most platforms get there by asking the
//! kernel to drop the pages in place. FreeBSD has no such request, so it maps
//! fresh memory over the same range instead. That reaches the same state by
//! the time `decommit` returns.
//!
//! Deviation: Zig's comptime `mode` parameter is a runtime argument here;
//! every branch on it is trivially predictable.

#[cfg(windows)]
use crate::os::windows;

const LOG_TARGET: &str = "terminal_mem";

/// The smallest page size the target supports (`std.heap.page_size_min`).
pub const PAGE_SIZE_MIN: usize = if cfg!(all(target_vendor = "apple", target_arch = "aarch64")) {
    16 * 1024
} else {
    4 * 1024
};

/// What guarantee decommit must provide when the OS cannot discard a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecommitMode {
    /// The dirty prefix must read as zero after this call, even when physical
    /// reclamation is unavailable. Bytes after the prefix must already be zero.
    Zero,

    /// Physical-memory reclamation is required. Do not touch the mapping when
    /// reclamation is unsupported or fails; report the failure to the caller.
    Strict,
}

/// Returns whether this build can release physical memory for `mode` while
/// keeping the address range.
///
/// Check this before calling `decommit` in strict mode. When it returns
/// false, strict callers should keep their memory as it is:
///
/// ```ignore
/// if !mem::can_reclaim(DecommitMode::Strict) { return; } // Nothing to gain on this target.
/// ```
///
/// Release builds support 64-bit Linux, Darwin, Windows, and FreeBSD. Test
/// builds always return true, because there `decommit` imitates a release by
/// writing zeros. Zero mode works on every target even when this returns
/// false, because `decommit` falls back to writing zeros itself.
#[inline]
pub const fn can_reclaim(mode: DecommitMode) -> bool {
    // Both modes use the same retained-mapping primitives. Keeping the match
    // exhaustive makes additions to DecommitMode choose target support
    // explicitly rather than inheriting it accidentally.
    match mode {
        DecommitMode::Zero | DecommitMode::Strict => {
            // Tests never call into the OS because their allocator ranges can
            // share mappings with unrelated allocations. `decommit` simulates
            // successful reclamation by zeroing the requested range instead,
            // so both modes are always available to tests on every target.
            if cfg!(test) {
                return true;
            }

            // Compression currently retains complete page mappings for its
            // lifetime. Limit the initial runtime support to 64-bit address
            // spaces where that virtual-memory cost is negligible and where
            // the retained-mapping behavior has been validated.
            if !cfg!(target_pointer_width = "64") {
                return false;
            }

            // Linux provides MADV_DONTNEED, which immediately discards pages
            // from a private anonymous mapping and faults them back as zeroes.
            if cfg!(target_os = "linux") {
                return true;
            }

            // Darwin provides the paired MADV_FREE_REUSABLE/FREE_REUSE
            // operations used below.
            if cfg!(target_vendor = "apple") {
                return true;
            }

            // Windows provides DiscardVirtualMemory, which releases the
            // physical pages behind a committed range while keeping it
            // committed, so nothing has to be committed again before reuse.
            // Page memory is already a VirtualAlloc region (see page.zig)
            // and kernel32 is linked by every Windows build.
            if cfg!(windows) {
                return true;
            }

            // FreeBSD maps fresh memory over the range (see decommit_freebsd).
            // The old pages are freed right away and the range reads as zeros
            // afterward.
            if cfg!(target_os = "freebsd") {
                return true;
            }

            // Other targets have no retained-mapping reclamation contract in
            // this module. Zero mode can still clear through its memset
            // fallback, but strict callers must leave their mapping resident.
            false
        }
    }
}

fn assert_page_range(memory: &[u8]) {
    debug_assert!(!memory.is_empty());
    debug_assert!(memory.as_ptr() as usize % PAGE_SIZE_MIN == 0);
    debug_assert!(memory.len() % PAGE_SIZE_MIN == 0);
}

/// Release the physical memory behind `memory` while keeping its address
/// range.
///
/// `memory` must start on a page boundary and be a whole number of pages
/// long, using the smallest page size the target supports. `dirty_len` is
/// the length of the prefix that may hold nonzero bytes. Everything after it
/// must already be zero. Strict mode requires `dirty_len == memory.len()`,
/// because a successful decommit throws away everything in the range.
///
/// Returns true when the operating system released the memory. What false
/// means depends on `mode`:
///
/// - `Strict`: nothing was released and the contents are unchanged, so the
///   caller can keep using the memory as it was.
/// - `Zero`: the memory may still be in use, but the dirty prefix has been
///   cleared. The whole range reads as zeros either way.
///
/// Zero mode suits memory that is about to be reused and only needs to be
/// clean. Only the bytes that were actually written have to be cleared:
///
/// ```ignore
/// // Only the first `used` bytes of `page` were ever written.
/// let _ = mem::decommit(DecommitMode::Zero, page, used);
/// // `page` now reads as all zeros.
/// ```
///
/// In test builds this never calls into the operating system. It clears the
/// dirty prefix and returns true, which imitates a successful release.
///
/// The memory must be private to this process (e.g. its own anonymous
/// mapping or page allocation): the OS may discard every byte in the range.
pub fn decommit(mode: DecommitMode, memory: &mut [u8], dirty_len: usize) -> bool {
    assert_page_range(memory);
    debug_assert!(dirty_len <= memory.len());
    if mode == DecommitMode::Strict {
        debug_assert!(dirty_len == memory.len());
    }

    // Testing allocator ranges may share an OS mapping with unrelated memory,
    // so madvise is not safe. Zeroing models the only content guarantee callers
    // have after a successful discard.
    if cfg!(test) {
        memory[..dirty_len].fill(0);
        return true;
    }

    // DONTNEED immediately reclaims private anonymous pages on Linux and
    // faults them back as zero-filled pages. We deliberately avoid MADV_FREE:
    // it does not reduce RSS until memory pressure and does not guarantee that
    // the next read is zero.
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `memory` is a valid, page-aligned, page-multiple range that
        // the caller owns exclusively (we hold `&mut`).
        let rc = unsafe {
            libc::madvise(
                memory.as_mut_ptr().cast(),
                memory.len(),
                libc::MADV_DONTNEED,
            )
        };
        if rc == 0 {
            return true;
        }
        let err = std::io::Error::last_os_error();
        log::warn!(target: LOG_TARGET, "madvise(DONTNEED) failed err={err}");
        if mode == DecommitMode::Strict {
            return false;
        }
        // Zero mode falls through to the memset below.
    }

    // FREE_REUSABLE removes the range from the Darwin process footprint while
    // retaining its mapping. Zero mode clears its dirty prefix first because
    // the kernel may preserve the contents. Strict mode avoids that write
    // because its caller will replace the entire mapping after recommit.
    #[cfg(target_vendor = "apple")]
    {
        if mode == DecommitMode::Zero {
            memory[..dirty_len].fill(0);
        }

        // SAFETY: see the Linux branch.
        let rc = unsafe {
            libc::madvise(
                memory.as_mut_ptr().cast(),
                memory.len(),
                libc::MADV_FREE_REUSABLE,
            )
        };
        if rc == 0 {
            return true;
        }
        match mode {
            DecommitMode::Strict => {
                let err = std::io::Error::last_os_error();
                log::warn!(target: LOG_TARGET, "madvise(FREE_REUSABLE) failed err={err}");
                return false;
            }
            DecommitMode::Zero => {
                // Plain FREE can still reclaim the already-zero mapping
                // under pressure and does not require a reuse pairing.
                // SAFETY: see the Linux branch.
                unsafe {
                    libc::madvise(memory.as_mut_ptr().cast(), memory.len(), libc::MADV_FREE);
                }
                return false;
            }
        }
    }

    // DiscardVirtualMemory releases the physical pages behind the range but
    // leaves it committed, so the commit charge stays with the process and a
    // later access finds a zero page or the old contents instead of faulting.
    // Zero mode clears its dirty prefix first, as on Darwin: the bytes read
    // as zero afterward whether or not the discard took. Strict mode skips
    // that write because its caller replaces the entire mapping after
    // recommit. The call reports failure through its return value rather
    // than the thread's last error.
    #[cfg(windows)]
    {
        if mode == DecommitMode::Zero {
            memory[..dirty_len].fill(0);
        }

        // SAFETY: see the Linux branch.
        let rc = unsafe {
            windows::exp::kernel32::DiscardVirtualMemory(memory.as_mut_ptr().cast(), memory.len())
        };
        if rc == windows::ERROR_SUCCESS {
            return true;
        }

        // Zero mode has already cleared its bytes and strict callers must
        // leave the still-resident mapping alone, so there is nothing more
        // to do for either mode.
        log::warn!(target: LOG_TARGET, "DiscardVirtualMemory failed err={rc}");
        return false;
    }

    // FreeBSD's madvise cannot release pages right away, so map fresh memory
    // over the range instead. See decommit_freebsd for the details.
    #[cfg(target_os = "freebsd")]
    {
        if decommit_freebsd(memory) {
            return true;
        }
        if mode == DecommitMode::Strict {
            return false;
        }
        // Zero mode falls through to the memset below.
    }

    #[allow(unreachable_code)]
    {
        if mode == DecommitMode::Zero {
            memory[..dirty_len].fill(0);
        }
        false
    }
}

/// The FreeBSD implementation of `decommit`. It maps fresh memory over
/// `memory` at the same address.
///
/// Returns true when the old pages have been released and the range reads
/// as zeros. Returns false when nothing changed and the original contents
/// are still in place.
///
/// FreeBSD's `madvise` cannot do this job. Both `MADV_DONTNEED` and
/// `MADV_FREE` only mark pages as unused. The pages stay in memory, so the
/// process's memory use does not go down, and the old bytes can still be
/// read until the kernel needs that memory for something else.
///
/// Mapping new memory over the range with `MAP_FIXED` does work. The kernel
/// removes the old mapping and adds the new one while holding the process's
/// memory map lock, so no other thread sees the range in between. Terminal
/// pages are private to this process, so removing the old mapping frees
/// their physical memory immediately. The new mapping starts with no
/// physical memory at all. The kernel supplies a zero-filled page the first
/// time each page is touched. The kernel code this relies on
/// (`vm_map_fixed`) behaves the same from FreeBSD 13.0 through 15.1.
///
/// One failure needs extra care. The kernel can remove the old mapping and
/// then fail to add the new one, which leaves nothing mapped at the address.
/// With default settings this cannot happen. It can happen when swap
/// accounting is strict (the `vm.overcommit` sysctl) or a swap limit is set.
/// The new mapping has to reserve swap space before the old mapping's
/// reservation is returned, so for a moment the process needs twice as much.
/// When this happens, we map the range a second time.
#[cfg(target_os = "freebsd")]
fn decommit_freebsd(memory: &mut [u8]) -> bool {
    match map_fixed_freebsd(memory) {
        Some(err) => log::warn!(target: LOG_TARGET, "mmap(MAP_FIXED) failed err={err}"),
        None => return true,
    }

    // Find out whether the kernel removed the old mapping before it failed.
    // It removes the whole range or none of it, so checking the first page
    // is enough. mincore fails with ENOMEM when the address is not mapped.
    let mut vec = [0 as libc::c_char; 1];
    // SAFETY: the address is page aligned and `vec` holds one page's status.
    let rc = unsafe { libc::mincore(memory.as_mut_ptr().cast(), PAGE_SIZE_MIN, vec.as_mut_ptr()) };
    if rc == 0 {
        return false;
    }
    if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOMEM) {
        return false;
    }

    // The old contents are already gone, so if this second mapping works,
    // the result is the same as a successful decommit. The old mapping's
    // swap reservation has been returned by now, so this attempt needs no
    // more swap than the process already had.
    if let Some(err) = map_fixed_freebsd(memory) {
        // Nothing is mapped at this address, but the caller still owns the
        // range and will use it again. Stop here with a clear message
        // instead of crashing later on an unexplained memory fault.
        panic!("mmap(MAP_FIXED) could not restore a discarded page err={err}");
    }
    true
}

/// Map fresh, zero-filled, private memory over `memory` at the same address.
/// Returns None on success or the error code on failure.
#[cfg(target_os = "freebsd")]
fn map_fixed_freebsd(memory: &mut [u8]) -> Option<std::io::Error> {
    // SAFETY: MAP_FIXED over a range this process owns exclusively.
    let rc = unsafe {
        libc::mmap(
            memory.as_mut_ptr().cast(),
            memory.len(),
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED,
            -1,
            0,
        )
    };
    if rc == libc::MAP_FAILED {
        return Some(std::io::Error::last_os_error());
    }
    debug_assert!(rc as usize == memory.as_ptr() as usize);
    None
}

/// Prepare memory released by `decommit` for reuse.
///
/// Call this before writing new contents into a range that was decommitted.
/// Only Darwin does any work here. It tells the kernel the pages are in use
/// again (`MADV_FREE_REUSE`), so they count toward the process's memory
/// footprint once more. On every other platform, the range is already ready
/// to use when `decommit` returns, so this does nothing.
///
/// This cannot fail in a way the caller needs to handle. If the Darwin call
/// fails, only the memory accounting is off. The range is still usable, so
/// the failure is logged and ignored.
pub fn recommit(memory: &mut [u8]) {
    assert_page_range(memory);

    if cfg!(test) {
        return;
    }

    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: see decommit.
        let rc = unsafe {
            libc::madvise(
                memory.as_mut_ptr().cast(),
                memory.len(),
                libc::MADV_FREE_REUSE,
            )
        };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            log::warn!(target: LOG_TARGET, "madvise(FREE_REUSE) failed err={err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{alloc_zeroed, dealloc, Layout};

    /// A page-aligned heap allocation for tests.
    struct PageAlloc {
        ptr: *mut u8,
        layout: Layout,
    }

    impl PageAlloc {
        fn new(len: usize) -> Self {
            let layout = Layout::from_size_align(len, PAGE_SIZE_MIN).unwrap();
            // SAFETY: layout has non-zero size.
            let ptr = unsafe { alloc_zeroed(layout) };
            assert!(!ptr.is_null());
            Self { ptr, layout }
        }

        fn slice(&mut self) -> &mut [u8] {
            // SAFETY: ptr is a live allocation of layout.size() bytes.
            unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
        }
    }

    impl Drop for PageAlloc {
        fn drop(&mut self) {
            // SAFETY: allocated with the same layout.
            unsafe { dealloc(self.ptr, self.layout) }
        }
    }

    #[test]
    fn decommit_with_zero_fallback_clears_the_dirty_prefix() {
        let memory_len = 2 * PAGE_SIZE_MIN;
        let mut alloc = PageAlloc::new(memory_len);
        let memory = alloc.slice();

        memory.fill(0xAA);
        let len = memory.len();
        let _ = decommit(DecommitMode::Zero, memory, len);
        assert!(memory.iter().all(|&b| b == 0));

        // The tail is already zero by contract, so a partially dirty mapping only
        // needs its dirty prefix cleared.
        memory[..1024].fill(0xAA);
        let _ = decommit(DecommitMode::Zero, memory, 1024);
        assert!(memory.iter().all(|&b| b == 0));
    }

    #[test]
    fn strict_decommit_retains_the_mapping_for_recommit() {
        let memory_len = 2 * PAGE_SIZE_MIN;
        let mut alloc = PageAlloc::new(memory_len);
        let memory = alloc.slice();
        memory.fill(0xAA);

        let original_ptr = memory.as_ptr();
        let original_len = memory.len();
        assert!(decommit(DecommitMode::Strict, memory, original_len));
        assert_eq!(original_ptr, memory.as_ptr());
        assert_eq!(original_len, memory.len());
        assert!(memory.iter().all(|&b| b == 0));

        recommit(memory);
        memory.fill(0xBB);
        assert!(memory.iter().all(|&b| b == 0xBB));
    }

    #[cfg(target_os = "freebsd")]
    #[test]
    fn freebsd_decommit_releases_pages_and_reads_back_zeros() {
        // In test builds, decommit only writes zeros and never calls into the
        // operating system. Call the FreeBSD implementation directly instead, on
        // memory this test maps for itself and shares with nothing else.
        const PAGES: usize = 4;
        let len = PAGES * PAGE_SIZE_MIN;
        // SAFETY: fresh anonymous mapping.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        assert!(ptr != libc::MAP_FAILED);
        // SAFETY: the mapping is len bytes long and owned by this test.
        let memory = unsafe { std::slice::from_raw_parts_mut(ptr as *mut u8, len) };
        memory.fill(0xAA);

        assert!(decommit_freebsd(memory));

        let mut vec = [0 as libc::c_char; PAGES];
        // SAFETY: valid mapping and status vector.
        assert_eq!(0, unsafe { libc::mincore(ptr, len, vec.as_mut_ptr()) });
        for v in vec {
            assert_eq!(0, v & 1);
        }
        assert!(memory.iter().all(|&b| b == 0));
        // SAFETY: unmapping our own mapping.
        unsafe { libc::munmap(ptr, len) };
    }

    #[test]
    fn test_builds_can_reclaim_retained_mappings() {
        assert!(can_reclaim(DecommitMode::Zero));
        assert!(can_reclaim(DecommitMode::Strict));
    }
}
