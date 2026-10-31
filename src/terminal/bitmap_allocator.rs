//! Port of `src/terminal/bitmap_allocator.zig`.
//!
//! STATUS: ported

use crate::terminal::size::{align_forward, AllocError, Base, Offset, OffsetBuf};

/// A relatively naive bitmap allocator that uses memory offsets against
/// a fixed backing buffer so that the backing buffer can be easily moved
/// without having to update pointers.
///
/// The chunk size determines the size of each chunk in bytes. This is the
/// minimum distributed unit of memory. For example, if you request a
/// 1-byte allocation, you'll use a chunk of chunk_size bytes. Likewise,
/// if your chunk size is 4, and you request a 5-byte allocation, you'll
/// use 2 chunks.
///
/// The allocator is susceptible to fragmentation. If you allocate and free
/// memory in a way that leaves small holes in the memory, you may not be
/// able to allocate large chunks of memory even if there is enough free
/// memory in aggregate. To avoid fragmentation, use a chunk size that is
/// large enough to cover most of your allocations.
///
/// # Safety model
///
/// The allocator itself only stores offsets. Every method that touches the
/// backing memory takes the base pointer and is `unsafe`: the caller must
/// pass the *true base* of a buffer that is at least `layout.total_size`
/// bytes past the [`OffsetBuf`] used at init, aligned to [`Self::BASE_ALIGN`],
/// and valid for reads and writes. The buffer may be moved/copied between
/// calls (that is the point of the offset design) as long as the new base
/// is passed.
//
// Notes for contributors: this is highly contributor friendly part of
// the code. If you can improve this, add tests, show benchmarks, then
// please do so!
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitmapAllocator<const CHUNK_SIZE: usize> {
    /// The bitmap of available chunks. Each bit represents a chunk. A
    /// 0 means the chunk is free and a 1 means it's used, so an
    /// all-zero bitmap is a fully free allocator.
    pub bitmap: Offset<u64>,
    pub bitmap_count: usize,

    /// Lowest bitmap word index that may contain a free bit; words
    /// below it are fully allocated, so alloc scans can start here
    /// instead of at zero.
    pub search_start: usize,

    /// The contiguous buffer of chunks.
    pub chunks: Offset<u8>,
}

/// The memory layout of a [`BitmapAllocator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub total_size: usize,
    pub bitmap_count: usize,
    pub bitmap_start: usize,
    pub chunks_start: usize,
}

impl<const CHUNK_SIZE: usize> BitmapAllocator<CHUNK_SIZE> {
    const CHUNK_SIZE_IS_POW2: () = assert!(CHUNK_SIZE.is_power_of_two());

    pub const BASE_ALIGN: usize = core::mem::align_of::<u64>();
    pub const BITMAP_BIT_SIZE: usize = u64::BITS as usize;

    /// Initialize the allocator map with a given buf and memory layout.
    ///
    /// # Safety
    ///
    /// `buf` must point to writable memory of at least `l.total_size`
    /// bytes starting at `buf.start()`, aligned to [`Self::BASE_ALIGN`].
    pub unsafe fn init(buf: OffsetBuf, l: Layout) -> Self {
        debug_assert!((buf.start() as usize) % Self::BASE_ALIGN == 0);

        // Clear our bitmaps to note that all chunks are free.
        let bitmap = buf.member::<u64>(l.bitmap_start);
        // SAFETY: the bitmap region lies within the buffer per the contract.
        unsafe { core::ptr::write_bytes(bitmap.ptr(buf), 0, l.bitmap_count) };

        // SAFETY: we just zeroed the bitmap region.
        unsafe { Self::init_assume_zeroed(buf, l) }
    }

    /// Initialize the allocator map over memory that the caller
    /// guarantees is already zero-filled.
    ///
    /// This writes nothing to the buffer: an all-zero bitmap already
    /// marks every chunk as free.
    ///
    /// # Safety
    ///
    /// Same as [`Self::init`]; additionally behavior is undefined if the
    /// bitmap region is not zero.
    pub unsafe fn init_assume_zeroed(buf: OffsetBuf, l: Layout) -> Self {
        #[allow(clippy::let_unit_value)]
        let () = Self::CHUNK_SIZE_IS_POW2;
        debug_assert!((buf.start() as usize) % Self::BASE_ALIGN == 0);
        Self {
            bitmap: buf.member::<u64>(l.bitmap_start),
            bitmap_count: l.bitmap_count,
            search_start: 0,
            chunks: buf.member::<u8>(l.chunks_start),
        }
    }

    /// Returns the number of bytes required to allocate n elements of
    /// type T. This accounts for the chunk size alignment used by the
    /// bitmap allocator.
    pub const fn bytes_required<T>(n: usize) -> usize {
        let byte_count = core::mem::size_of::<T>() * n;
        align_forward(byte_count, CHUNK_SIZE)
    }

    /// Allocate n elements of type T. This will return
    /// `AllocError::OutOfMemory` if there isn't enough space in the
    /// backing buffer.
    ///
    /// Use [`crate::terminal::size::get_offset`] to get the base offset
    /// from the backing memory for portable storage.
    ///
    /// The returned memory is not initialized.
    ///
    /// # Safety
    ///
    /// `base` must be the true base of the buffer this allocator was
    /// initialized in (see the type-level docs).
    pub unsafe fn alloc<T>(&mut self, base: impl Base, n: usize) -> Result<*mut [T], AllocError> {
        let base = base.base_ptr();

        // note: we don't handle alignment yet, we just require that all
        // types are properly aligned. This is a limitation that should be
        // fixed but we haven't needed it. Contributor friendly: add tests
        // and fix this.
        debug_assert!(CHUNK_SIZE % core::mem::align_of::<T>() == 0);
        debug_assert!(n > 0);

        let byte_count = core::mem::size_of::<T>()
            .checked_mul(n)
            .ok_or(AllocError::OutOfMemory)?;
        let chunk_count = byte_count.div_ceil(CHUNK_SIZE);

        // Find the index of the free chunk. This also marks it as used.
        // Words below search_start have no free bits, so no free span
        // can start in or cross them and it is safe to skip them.
        let bitmaps_ptr = self.bitmap.ptr(base);
        // SAFETY: the bitmap region is `bitmap_count` u64s inside the buffer.
        let bitmaps = unsafe { core::slice::from_raw_parts_mut(bitmaps_ptr, self.bitmap_count) };
        let start = self.search_start.min(self.bitmap_count);
        let rel = find_free_chunks(&mut bitmaps[start..], chunk_count).ok_or(AllocError::OutOfMemory)?;
        let idx = start * Self::BITMAP_BIT_SIZE + rel;

        // Advance past any words the allocation just filled so the
        // next scan starts at the first word with a free bit.
        self.search_start = {
            let mut new_start = start;
            while new_start < self.bitmap_count && bitmaps[new_start] == u64::MAX {
                new_start += 1;
            }
            new_start
        };

        let chunks = self.chunks.ptr(base);
        // SAFETY: idx * CHUNK_SIZE is within the chunks region because the
        // layout reserves one chunk per bitmap bit.
        let ptr = unsafe { chunks.add(idx * CHUNK_SIZE) }.cast::<T>();
        Ok(core::ptr::slice_from_raw_parts_mut(ptr, n))
    }

    /// Free a previously allocated slice.
    ///
    /// # Safety
    ///
    /// `base` must be the true base of the buffer this allocator was
    /// initialized in and `slice` must have been returned by [`Self::alloc`]
    /// (relative to the same buffer contents) and not freed since.
    pub unsafe fn free<T>(&mut self, base: impl Base, slice: *const [T]) {
        let base = base.base_ptr();

        // Convert the slice of whatever type to a slice of bytes. We
        // can then use the byte len and chunk size to determine the
        // number of chunks that were allocated.
        let bytes_len = slice.len() * core::mem::size_of::<T>();
        let aligned_len = align_forward(bytes_len, CHUNK_SIZE);
        let chunk_count = aligned_len / CHUNK_SIZE;
        debug_assert!(aligned_len % CHUNK_SIZE == 0);

        // From the pointer, we can calculate the exact index.
        let chunks = self.chunks.ptr(base);
        let diff = slice.cast::<u8>() as usize - chunks as usize;
        debug_assert!(diff % CHUNK_SIZE == 0);
        let chunk_idx = diff / CHUNK_SIZE;

        // The freed word gains free bits, so scans must not skip it.
        self.search_start = self.search_start.min(chunk_idx / Self::BITMAP_BIT_SIZE);

        // SAFETY: the bitmap region is `bitmap_count` u64s inside the buffer.
        let bitmaps =
            unsafe { core::slice::from_raw_parts_mut(self.bitmap.ptr(base), self.bitmap_count) };

        // Current bitmap index.
        let mut i: usize = chunk_idx / 64;
        // Number of chunks we still have to mark as free.
        let mut rem: usize = chunk_count;

        // Mark any bits in the starting bitmap that need to be marked.
        {
            // Bit index.
            let bit = chunk_idx % 64;
            // Number of bits we need to mark in this bitmap.
            let bits = rem.min(64 - bit);

            bitmaps[i] &= !((!0u64 >> (64 - bits)) << bit);
            rem -= bits;
        }

        // Mark any full bitmaps worth of bits that need to be marked.
        i += 1;
        while rem > 64 {
            bitmaps[i] = 0;
            rem -= 64;
            i += 1;
        }

        // Mark any bits at the start of this last bitmap if it needs it.
        if rem > 0 {
            bitmaps[i] &= !(!0u64 >> (64 - rem));
        }
    }

    /// Returns the total capacity in bytes.
    pub fn capacity_bytes(&self) -> usize {
        self.bitmap_count * Self::BITMAP_BIT_SIZE * CHUNK_SIZE
    }

    /// Returns the number of bytes currently in use.
    ///
    /// # Safety
    ///
    /// `base` must be the true base of the buffer this allocator was
    /// initialized in.
    pub unsafe fn used_bytes(&self, base: impl Base) -> usize {
        // SAFETY: the bitmap region is `bitmap_count` u64s inside the buffer.
        let bitmaps = unsafe { core::slice::from_raw_parts(self.bitmap.ptr(base), self.bitmap_count) };
        let mut used_chunks: usize = 0;
        for bitmap in bitmaps {
            used_chunks += bitmap.count_ones() as usize;
        }
        used_chunks * CHUNK_SIZE
    }

    /// For testing only.
    #[cfg(test)]
    unsafe fn is_allocated<T>(&self, base: impl Base, slice: *const [T]) -> bool {
        let base = base.base_ptr();
        let bytes_len = slice.len() * core::mem::size_of::<T>();
        let aligned_len = align_forward(bytes_len, CHUNK_SIZE);
        let chunk_count = aligned_len / CHUNK_SIZE;

        let chunks = self.chunks.ptr(base);
        let chunk_idx = (slice.cast::<u8>() as usize - chunks as usize) / CHUNK_SIZE;

        // SAFETY: the bitmap region is `bitmap_count` u64s inside the buffer.
        let bitmaps = unsafe { core::slice::from_raw_parts(self.bitmap.ptr(base), self.bitmap_count) };

        for i in chunk_idx..chunk_idx + chunk_count {
            let bitmap = i / Self::BITMAP_BIT_SIZE;
            let bit = i % Self::BITMAP_BIT_SIZE;
            if bitmaps[bitmap] & (1u64 << bit) == 0 {
                return false;
            }
        }

        true
    }

    /// For debugging
    ///
    /// # Safety
    ///
    /// `base` must be the true base of the buffer this allocator was
    /// initialized in.
    #[allow(dead_code)]
    unsafe fn dump_bitmaps(&self, base: impl Base) {
        // SAFETY: the bitmap region is `bitmap_count` u64s inside the buffer.
        let bitmaps = unsafe { core::slice::from_raw_parts(self.bitmap.ptr(base), self.bitmap_count) };
        for (idx, bitmap) in bitmaps.iter().enumerate() {
            log::warn!("bm={:b} idx={}", bitmap, idx);
        }
    }

    /// Get the layout for the given capacity. The capacity is in
    /// number of bytes, not chunks. The capacity will likely be
    /// rounded up to the nearest chunk size and bitmap size so
    /// everything is perfectly divisible.
    pub const fn layout(cap: usize) -> Layout {
        // Align the cap forward to our chunk size so we always have
        // a full chunk at the end.
        let aligned_cap = align_forward(cap, CHUNK_SIZE);

        // Calculate the number of bitmaps. We need 1 bitmap per 64 chunks.
        // We align the chunk count forward so our bitmaps are full so we
        // don't have to handle the case where we have a partial bitmap.
        let chunk_count = aligned_cap / CHUNK_SIZE;
        let aligned_chunk_count = align_forward(chunk_count, 64);
        let bitmap_count = aligned_chunk_count / 64;

        let bitmap_start = 0;
        let bitmap_end = core::mem::size_of::<u64>() * bitmap_count;
        let chunks_start = align_forward(bitmap_end, core::mem::align_of::<u8>());

        // The chunks region must be exactly the bytes addressable by
        // the bitmaps: one chunk per bit of every bitmap. Anything more
        // is unreachable waste, while anything less would let alloc
        // hand out memory beyond the region, since init marks every
        // bitmap bit as free.
        let chunks_end = chunks_start + (aligned_chunk_count * CHUNK_SIZE);
        let total_size = chunks_end;

        Layout {
            total_size,
            bitmap_count,
            bitmap_start,
            chunks_start,
        }
    }
}

/// Find `n` sequential free chunks in the given bitmaps and return the index
/// of the first chunk. If no chunks are found, return `None`. This also updates
/// the bitmap to mark the chunks as used.
fn find_free_chunks(bitmaps: &mut [u64], n: usize) -> Option<usize> {
    // NOTE: This is a naive implementation that just iterates through the
    // bitmaps. There is very likely a more efficient way to do this but
    // I'm not a bit twiddling expert. Perhaps even SIMD could be used here
    // but unsure. Contributor friendly: let's benchmark and improve this!

    // Large chunks require special handling.
    if n > u64::BITS as usize {
        let mut i: usize = 0;
        'search: while i < bitmaps.len() {
            // Number of chunks available at the end of this bitmap.
            let prefix = bitmaps[i].leading_zeros() as usize;

            // If there are no chunks available at the end of this bitmap
            // then we can't start in it, so we'll try the next one.
            if prefix == 0 {
                i += 1;
                continue;
            }

            // Starting position if we manage to find the span we need here.
            let start_bitmap = i;
            let start_bit = 64 - prefix;

            // The remaining number of sequential free chunks we need to find.
            let mut rem: usize = n - prefix;

            i += 1;
            while rem > 64 {
                // We ran out of bitmaps, there's no sufficiently large gap.
                if i >= bitmaps.len() {
                    return None;
                }

                // There's more than 64 remaining chunks and this bitmap has
                // content in it, so we try starting again with this bitmap.
                if bitmaps[i] != 0 {
                    continue 'search;
                }

                // This bitmap is completely free, we can subtract 64 from
                // our remaining number.
                rem -= 64;
                i += 1;
            }

            // NOTE: the Zig code indexes `bitmaps[i]` here unconditionally;
            // running past the end is a bounds-check panic there too, so we
            // treat it the same as "no sufficiently large gap".
            if i >= bitmaps.len() {
                return None;
            }

            // If the number of available chunks at the start of this bitmap
            // is less than the remaining required, we have to try again.
            if (bitmaps[i].trailing_zeros() as usize) < rem {
                continue;
            }

            let suffix = (n - prefix) % 64;

            // Found! Mark everything between our start and end as used.
            bitmaps[start_bitmap] |= (!0u64 >> start_bit) << start_bit;
            let full_bitmaps = (n - prefix - suffix) / 64;
            for bitmap in &mut bitmaps[start_bitmap + 1..][..full_bitmaps] {
                *bitmap = u64::MAX;
            }
            if suffix > 0 {
                bitmaps[i] |= !0u64 >> (64 - suffix);
            }

            return Some(start_bitmap * 64 + start_bit);
        }

        return None;
    }

    debug_assert!(n <= u64::BITS as usize);
    for (idx, bitmap) in bitmaps.iter_mut().enumerate() {
        // Shift the bitmap to find `n` sequential free chunks.
        // EXAMPLE:
        // n = 4
        // shifted = 001111001011110010
        //         & 000111100101111001
        //         & 000011110010111100
        //         & 000001111001011110
        //         = 000001000000010000
        //                ^       ^
        // In this example there are 2 places with at least 4 sequential 1s.
        // Work on the inverted word so that free chunks are 1 bits.
        let free = !*bitmap;
        let mut shifted: u64 = free;
        for i in 1..n {
            shifted &= free >> i;
        }

        // If we have zero then we have no matches
        if shifted == 0 {
            continue;
        }

        // Trailing zeroes gets us the index of the first bit index with at
        // least `n` sequential 1s. In the example above, that would be `4`.
        let bit = shifted.trailing_zeros() as usize;

        // Calculate the mask so we can mark it as used
        let mask = (u64::MAX >> (64 - n)) << bit;
        *bitmap |= mask;

        return Some((idx * 64) + bit);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::size::AlignedBuf;

    #[test]
    fn find_free_chunks_single_found() {
        let mut bitmaps = [0b01111111_11111111_11111111_11111111_11111111_11111111_11110001_11111111u64];
        let idx = find_free_chunks(&mut bitmaps, 2).unwrap();
        assert_eq!(9usize, idx);
        assert_eq!(
            0b01111111_11111111_11111111_11111111_11111111_11111111_11110111_11111111u64,
            bitmaps[0]
        );
    }

    #[test]
    fn find_free_chunks_single_not_found() {
        let mut bitmaps = [0b01111000_11111111_11111111_11111111_11111111_11111111_11111111_11111111u64];
        let idx = find_free_chunks(&mut bitmaps, 4);
        assert!(idx.is_none());
    }

    #[test]
    fn find_free_chunks_multiple_found() {
        let mut bitmaps = [
            0b01111000_11111111_11111111_11111111_11111111_11111111_11111111_10001111u64,
            0b01111111_11000001_11111111_11111111_11111111_11111111_11000001_11111111u64,
        ];
        let idx = find_free_chunks(&mut bitmaps, 4).unwrap();
        assert_eq!(73usize, idx);
        assert_eq!(
            0b01111111_11000001_11111111_11111111_11111111_11111111_11011111_11111111u64,
            bitmaps[1]
        );
    }

    #[test]
    fn find_free_chunks_exactly_64_chunks() {
        let mut bitmaps = [0u64];
        let idx = find_free_chunks(&mut bitmaps, 64).unwrap();
        assert_eq!(u64::MAX, bitmaps[0]);
        assert_eq!(0usize, idx);
    }

    #[test]
    fn find_free_chunks_larger_than_64_chunks() {
        let mut bitmaps = [0u64, 0u64];
        let idx = find_free_chunks(&mut bitmaps, 65).unwrap();
        assert_eq!(u64::MAX, bitmaps[0]);
        assert_eq!(
            0b00000000_00000000_00000000_00000000_00000000_00000000_00000000_00000001u64,
            bitmaps[1]
        );
        assert_eq!(0usize, idx);
    }

    #[test]
    fn find_free_chunks_larger_than_64_chunks_not_at_beginning() {
        let mut bitmaps = [
            0b00000000_11111111_11111111_11111111_11111111_11111111_11111111_11111111u64,
            0u64,
            0u64,
        ];
        let idx = find_free_chunks(&mut bitmaps, 65).unwrap();
        assert_eq!(u64::MAX, bitmaps[0]);
        assert_eq!(
            0b00000001_11111111_11111111_11111111_11111111_11111111_11111111_11111111u64,
            bitmaps[1]
        );
        assert_eq!(0u64, bitmaps[2]);
        assert_eq!(56usize, idx);
    }

    #[test]
    fn find_free_chunks_larger_than_64_chunks_exact() {
        let mut bitmaps = [0u64, 0u64];
        let idx = find_free_chunks(&mut bitmaps, 128).unwrap();
        assert_eq!(u64::MAX, bitmaps[0]);
        assert_eq!(u64::MAX, bitmaps[1]);
        assert_eq!(0usize, idx);
    }

    #[test]
    fn bitmap_allocator_layout() {
        type Alloc = BitmapAllocator<4>;
        let cap = 64 * 4;

        let layout = Alloc::layout(cap);

        // We expect to use one bitmap since the cap is bytes.
        assert_eq!(1usize, layout.bitmap_count);
    }

    fn check_layout_chunks_region<const CHUNK: usize>() {
        type A<const C: usize> = BitmapAllocator<C>;
        for cap in [1, CHUNK, CHUNK + 1, 48, 64, 512, 1024, 2048, 8192, 8193] {
            let layout = A::<CHUNK>::layout(cap);
            let chunks_size = layout.total_size - layout.chunks_start;

            // Reserved == addressable by the bitmaps. This must match
            // capacityBytes() which is computed from the bitmap count.
            assert_eq!(
                layout.bitmap_count * A::<CHUNK>::BITMAP_BIT_SIZE * CHUNK,
                chunks_size
            );

            // We always reserve at least the requested capacity.
            assert!(chunks_size >= cap);
        }
    }

    #[test]
    fn bitmap_allocator_layout_chunks_region_matches_bitmap_addressable_bytes() {
        // The chunks region must be exactly the bytes addressable by the
        // bitmaps: one chunk per bit of every bitmap. Prior to this being
        // fixed, the region was over-reserved by a factor of chunk_size
        // (~186 KiB of dead space per standard page), while capacities
        // smaller than one full bitmap were under-reserved, allowing
        // out-of-bounds allocations.
        check_layout_chunks_region::<1>();
        check_layout_chunks_region::<2>();
        check_layout_chunks_region::<4>();
        check_layout_chunks_region::<16>();
        check_layout_chunks_region::<32>();
    }

    fn new_alloc<const C: usize>(cap: usize) -> (AlignedBuf, BitmapAllocator<C>) {
        let layout = BitmapAllocator::<C>::layout(cap);
        let buf = AlignedBuf::new(layout.total_size, BitmapAllocator::<C>::BASE_ALIGN);
        // SAFETY: buf is total_size bytes aligned to BASE_ALIGN.
        let bm = unsafe { BitmapAllocator::<C>::init(OffsetBuf::init(buf.ptr()), layout) };
        (buf, bm)
    }

    fn addr<T>(s: *mut [T]) -> usize {
        s.cast::<u8>() as usize
    }

    unsafe fn fill(s: *mut [u8], v: u8) {
        unsafe { (&mut *s).fill(v) };
    }

    unsafe fn all_eq(s: *mut [u8], v: u8) -> bool {
        unsafe { (&*s).iter().all(|&b| b == v) }
    }

    unsafe fn bitmaps<const C: usize>(bm: &BitmapAllocator<C>, buf: &AlignedBuf, n: usize) -> Vec<u64> {
        unsafe { core::slice::from_raw_parts(bm.bitmap.ptr(buf.ptr()), n).to_vec() }
    }

    #[test]
    fn bitmap_allocator_layout_small_capacity_cannot_alloc_out_of_bounds() {
        // Regression test: for capacities smaller than one full bitmap of
        // chunks, init marks all bitmap bits as free, so alloc will hand out
        // chunks up to the full bitmap. The layout must reserve that entire
        // addressable region or those allocations would be out of bounds of
        // the backing buffer.
        type Alloc = BitmapAllocator<16>;
        let cap = 48; // 3 chunks, bitmap addresses 64

        let (buf, mut bm) = new_alloc::<16>(cap);

        // Allocate every chunk the bitmap can hand out and verify each is
        // fully within the backing buffer. Writing to each allocation lets
        // the testing allocator catch any out-of-bounds corruption.
        let buf_start = buf.ptr() as usize;
        let buf_end = buf_start + buf.len();
        let mut count: usize = 0;
        loop {
            match unsafe { bm.alloc::<u8>(buf.ptr(), 16) } {
                Ok(slice) => {
                    assert!(addr(slice) >= buf_start);
                    assert!(addr(slice) + slice.len() <= buf_end);
                    unsafe { fill(slice, 0xAA) };
                    count += 1;
                }
                Err(err) => {
                    assert_eq!(AllocError::OutOfMemory, err);
                    break;
                }
            }
        }
        assert_eq!(Alloc::BITMAP_BIT_SIZE, count);
    }

    #[test]
    fn bitmap_allocator_alloc_sequentially() {
        let (buf, mut bm) = new_alloc::<4>(64);
        unsafe {
            let ptr = bm.alloc::<u8>(buf.ptr(), 1).unwrap();
            *ptr.cast::<u8>() = b'A';

            let ptr2 = bm.alloc::<u8>(buf.ptr(), 1).unwrap();
            assert_ne!(addr(ptr), addr(ptr2));

            // Should grab the next chunk
            assert_eq!(addr(ptr) + 4, addr(ptr2));

            // Free ptr and next allocation should be back
            bm.free(buf.ptr(), ptr);
            let ptr3 = bm.alloc::<u8>(buf.ptr(), 1).unwrap();
            assert_eq!(addr(ptr), addr(ptr3));
        }
    }

    // Zig's `u21` (size 4, align 4) is `u32` in Rust.
    #[test]
    fn bitmap_allocator_alloc_non_byte() {
        let (buf, mut bm) = new_alloc::<4>(128);
        unsafe {
            let ptr = bm.alloc::<u32>(buf.ptr(), 1).unwrap();
            *ptr.cast::<u32>() = 'A' as u32;

            let ptr2 = bm.alloc::<u32>(buf.ptr(), 1).unwrap();
            assert_ne!(addr(ptr), addr(ptr2));
            assert_eq!(addr(ptr) + 4, addr(ptr2));

            // Free ptr and next allocation should be back
            bm.free(buf.ptr(), ptr);
            let ptr3 = bm.alloc::<u32>(buf.ptr(), 1).unwrap();
            assert_eq!(addr(ptr), addr(ptr3));
        }
    }

    #[test]
    fn bitmap_allocator_alloc_non_byte_multi_chunk() {
        const C: usize = 4 * core::mem::size_of::<u32>();
        let (buf, mut bm) = new_alloc::<C>(128);
        unsafe {
            let ptr = bm.alloc::<u32>(buf.ptr(), 6).unwrap();
            assert_eq!(6usize, ptr.len());
            (&mut *ptr).fill('A' as u32);

            let ptr2 = bm.alloc::<u32>(buf.ptr(), 1).unwrap();
            assert_ne!(addr(ptr), addr(ptr2));
            assert_eq!(addr(ptr) + (core::mem::size_of::<u32>() * 4 * 2), addr(ptr2));

            // Free ptr and next allocation should be back
            bm.free(buf.ptr(), ptr);
            let ptr3 = bm.alloc::<u32>(buf.ptr(), 1).unwrap();
            assert_eq!(addr(ptr), addr(ptr3));
        }
    }

    #[test]
    fn bitmap_allocator_alloc_large() {
        let (buf, mut bm) = new_alloc::<2>(256);
        unsafe {
            let ptr = bm.alloc::<u8>(buf.ptr(), 129).unwrap();
            *ptr.cast::<u8>() = b'A';
            bm.free(buf.ptr(), ptr);
        }
    }

    const BITS: usize = BitmapAllocator::<1>::BITMAP_BIT_SIZE;

    #[test]
    fn bitmap_allocator_alloc_and_free_one_bitmap() {
        // Capacity such that we'll have 3 bitmaps.
        let (buf, mut bm) = new_alloc::<1>(BITS * 3);
        unsafe {
            // Allocate exactly one bitmap worth of bytes.
            let slice = bm.alloc::<u8>(buf.ptr(), BITS).unwrap();
            assert_eq!(BITS, slice.len());

            fill(slice, 0x11);
            assert!(all_eq(slice, 0x11));

            // Free it
            assert!(bm.is_allocated(buf.ptr(), slice));
            bm.free(buf.ptr(), slice);
            assert!(!bm.is_allocated(buf.ptr(), slice));

            // All of our bitmaps should be free.
            assert_eq!(vec![0u64; 3], bitmaps(&bm, &buf, 3));
        }
    }

    #[test]
    fn bitmap_allocator_search_hint_skips_full_words() {
        // Capacity such that we'll have 3 bitmaps.
        let (buf, mut bm) = new_alloc::<1>(BITS * 3);
        assert_eq!(0usize, bm.search_start);
        unsafe {
            // Fill the first bitmap word exactly with single-chunk allocations.
            let mut first: *mut [u8] = core::ptr::slice_from_raw_parts_mut(core::ptr::null_mut(), 0);
            for i in 0..BITS {
                let slice = bm.alloc::<u8>(buf.ptr(), 1).unwrap();
                if i == 0 {
                    first = slice;
                }
            }

            // The first word is exhausted so scans start at the second word.
            assert_eq!(1usize, bm.search_start);

            // The next allocation is the first chunk of the second word, so
            // the skipped-word index arithmetic must still yield the right
            // chunk address.
            let next = bm.alloc::<u8>(buf.ptr(), 1).unwrap();
            assert_eq!(addr(first) + BITS, addr(next));

            // Freeing a chunk in the first word lowers the hint so the freed
            // space is found again.
            bm.free(buf.ptr(), first);
            assert_eq!(0usize, bm.search_start);
            let again = bm.alloc::<u8>(buf.ptr(), 1).unwrap();
            assert_eq!(addr(first), addr(again));
        }
    }

    #[test]
    fn bitmap_allocator_search_hint_does_not_skip_partial_words() {
        // Capacity such that we'll have 2 bitmaps.
        let (buf, mut bm) = new_alloc::<1>(BITS * 2);
        unsafe {
            // Allocate all but 4 chunks of the first word.
            let mut first: *mut [u8] = core::ptr::slice_from_raw_parts_mut(core::ptr::null_mut(), 0);
            for i in 0..BITS - 4 {
                let slice = bm.alloc::<u8>(buf.ptr(), 1).unwrap();
                if i == 0 {
                    first = slice;
                }
            }

            // An 8-chunk run can't fit the 4 remaining bits (small runs never
            // span words), so it comes from the second word — but the hint
            // must stay at the first word, which still has free bits.
            let big = bm.alloc::<u8>(buf.ptr(), 8).unwrap();
            assert_eq!(addr(first) + BITS, addr(big));
            assert_eq!(0usize, bm.search_start);

            // A 4-chunk run fits the first word's remaining bits exactly; a
            // hint that skipped the partial word would wrongly place this in
            // the second word (or report OutOfMemory once that filled).
            let small = bm.alloc::<u8>(buf.ptr(), 4).unwrap();
            assert_eq!(addr(first) + BITS - 4, addr(small));

            // Now the first word is full and the hint advances past it.
            assert_eq!(1usize, bm.search_start);
        }
    }

    #[test]
    fn bitmap_allocator_alloc_and_free_half_bitmap() {
        // Capacity such that we'll have 3 bitmaps.
        let (buf, mut bm) = new_alloc::<1>(BITS * 3);
        unsafe {
            // Allocate exactly half a bitmap worth of bytes.
            let slice = bm.alloc::<u8>(buf.ptr(), BITS / 2).unwrap();
            assert_eq!(BITS / 2, slice.len());

            fill(slice, 0x11);
            assert!(all_eq(slice, 0x11));

            // Free it
            assert!(bm.is_allocated(buf.ptr(), slice));
            bm.free(buf.ptr(), slice);
            assert!(!bm.is_allocated(buf.ptr(), slice));

            // All of our bitmaps should be free.
            assert_eq!(vec![0u64; 3], bitmaps(&bm, &buf, 3));
        }
    }

    #[test]
    fn bitmap_allocator_alloc_and_free_two_half_bitmaps() {
        // Capacity such that we'll have 3 bitmaps.
        let (buf, mut bm) = new_alloc::<1>(BITS * 3);
        unsafe {
            // Allocate exactly one bitmap worth of bytes across two allocations.
            let slice = bm.alloc::<u8>(buf.ptr(), BITS / 2).unwrap();
            assert_eq!(BITS / 2, slice.len());

            fill(slice, 0x11);
            assert!(all_eq(slice, 0x11));

            let slice2 = bm.alloc::<u8>(buf.ptr(), BITS / 2).unwrap();
            assert_eq!(BITS / 2, slice2.len());

            fill(slice2, 0x22);
            assert!(all_eq(slice2, 0x22));
            assert!(all_eq(slice, 0x11));

            // Free them
            assert!(bm.is_allocated(buf.ptr(), slice2));
            bm.free(buf.ptr(), slice2);
            assert!(!bm.is_allocated(buf.ptr(), slice2));
            assert!(bm.is_allocated(buf.ptr(), slice));
            bm.free(buf.ptr(), slice);
            assert!(!bm.is_allocated(buf.ptr(), slice));

            // All of our bitmaps should be free.
            assert_eq!(vec![0u64; 3], bitmaps(&bm, &buf, 3));
        }
    }

    #[test]
    fn bitmap_allocator_alloc_and_free_1_5_bitmaps() {
        // Capacity such that we'll have 3 bitmaps.
        let (buf, mut bm) = new_alloc::<1>(BITS * 3);
        unsafe {
            // Allocate exactly 1.5 bitmaps worth of bytes.
            let slice = bm.alloc::<u8>(buf.ptr(), 3 * BITS / 2).unwrap();
            assert_eq!(3 * BITS / 2, slice.len());

            fill(slice, 0x11);
            assert!(all_eq(slice, 0x11));

            // Free them
            assert!(bm.is_allocated(buf.ptr(), slice));
            bm.free(buf.ptr(), slice);
            assert!(!bm.is_allocated(buf.ptr(), slice));

            // All of our bitmaps should be free.
            assert_eq!(vec![0u64; 3], bitmaps(&bm, &buf, 3));
        }
    }

    #[test]
    fn bitmap_allocator_alloc_and_free_two_1_5_bitmaps() {
        // Capacity such that we'll have 3 bitmaps.
        let (buf, mut bm) = new_alloc::<1>(BITS * 3);
        unsafe {
            // Allocate exactly 3 bitmaps worth of bytes across two allocations.
            let slice = bm.alloc::<u8>(buf.ptr(), 3 * BITS / 2).unwrap();
            assert_eq!(3 * BITS / 2, slice.len());

            fill(slice, 0x11);
            assert!(all_eq(slice, 0x11));

            let slice2 = bm.alloc::<u8>(buf.ptr(), 3 * BITS / 2).unwrap();
            assert_eq!(3 * BITS / 2, slice2.len());

            fill(slice2, 0x22);
            assert!(all_eq(slice2, 0x22));
            assert!(all_eq(slice, 0x11));

            // Free them
            assert!(bm.is_allocated(buf.ptr(), slice2));
            bm.free(buf.ptr(), slice2);
            assert!(!bm.is_allocated(buf.ptr(), slice2));
            assert!(bm.is_allocated(buf.ptr(), slice));
            bm.free(buf.ptr(), slice);
            assert!(!bm.is_allocated(buf.ptr(), slice));

            // All of our bitmaps should be free.
            assert_eq!(vec![0u64; 3], bitmaps(&bm, &buf, 3));
        }
    }

    #[test]
    fn bitmap_allocator_alloc_and_free_1_5_bitmaps_offset_by_0_75() {
        // Capacity such that we'll have 3 bitmaps.
        let (buf, mut bm) = new_alloc::<1>(BITS * 3);
        unsafe {
            // Allocate three quarters of a bitmap first.
            let slice = bm.alloc::<u8>(buf.ptr(), 3 * BITS / 4).unwrap();
            assert_eq!(3 * BITS / 4, slice.len());

            fill(slice, 0x11);
            assert!(all_eq(slice, 0x11));

            // Then a 1.5 bitmap sized allocation, so that it spans
            // from 0.75 to 2.25, occupying bits in 3 different bitmaps.
            let slice2 = bm.alloc::<u8>(buf.ptr(), 3 * BITS / 2).unwrap();
            assert_eq!(3 * BITS / 2, slice2.len());

            fill(slice2, 0x22);
            assert!(all_eq(slice2, 0x22));
            assert!(all_eq(slice, 0x11));

            // Free them
            assert!(bm.is_allocated(buf.ptr(), slice2));
            bm.free(buf.ptr(), slice2);
            assert!(!bm.is_allocated(buf.ptr(), slice2));
            assert!(bm.is_allocated(buf.ptr(), slice));
            bm.free(buf.ptr(), slice);
            assert!(!bm.is_allocated(buf.ptr(), slice));

            // All of our bitmaps should be free.
            assert_eq!(vec![0u64; 3], bitmaps(&bm, &buf, 3));
        }
    }

    #[test]
    fn bitmap_allocator_alloc_and_free_three_0_75_bitmaps() {
        // Capacity such that we'll have 3 bitmaps.
        let (buf, mut bm) = new_alloc::<1>(BITS * 3);
        unsafe {
            // Allocate three quarters of a bitmap three times.
            let slice = bm.alloc::<u8>(buf.ptr(), 3 * BITS / 4).unwrap();
            assert_eq!(3 * BITS / 4, slice.len());

            fill(slice, 0x11);
            assert!(all_eq(slice, 0x11));

            let slice2 = bm.alloc::<u8>(buf.ptr(), 3 * BITS / 4).unwrap();
            assert_eq!(3 * BITS / 4, slice2.len());

            fill(slice2, 0x22);
            assert!(all_eq(slice2, 0x22));
            assert!(all_eq(slice, 0x11));

            let slice3 = bm.alloc::<u8>(buf.ptr(), 3 * BITS / 4).unwrap();
            assert_eq!(3 * BITS / 4, slice3.len());

            fill(slice3, 0x33);
            assert!(all_eq(slice3, 0x33));
            assert!(all_eq(slice2, 0x22));
            assert!(all_eq(slice, 0x11));

            // Free them
            assert!(bm.is_allocated(buf.ptr(), slice2));
            bm.free(buf.ptr(), slice2);
            assert!(!bm.is_allocated(buf.ptr(), slice2));
            assert!(bm.is_allocated(buf.ptr(), slice));
            bm.free(buf.ptr(), slice);
            assert!(!bm.is_allocated(buf.ptr(), slice));
            assert!(bm.is_allocated(buf.ptr(), slice3));
            bm.free(buf.ptr(), slice3);
            assert!(!bm.is_allocated(buf.ptr(), slice3));

            // All of our bitmaps should be free.
            assert_eq!(vec![0u64; 3], bitmaps(&bm, &buf, 3));
        }
    }

    #[test]
    fn bitmap_allocator_alloc_and_free_two_1_5_bitmaps_offset_0_75() {
        // Capacity such that we'll have 4 bitmaps.
        let (buf, mut bm) = new_alloc::<1>(BITS * 4);
        unsafe {
            // First allocate a 0.75 bitmap
            let slice = bm.alloc::<u8>(buf.ptr(), 3 * BITS / 4).unwrap();
            assert_eq!(3 * BITS / 4, slice.len());

            fill(slice, 0x11);
            assert!(all_eq(slice, 0x11));

            // Then two 1.5 bitmaps
            let slice2 = bm.alloc::<u8>(buf.ptr(), 3 * BITS / 2).unwrap();
            assert_eq!(3 * BITS / 2, slice2.len());

            fill(slice2, 0x22);
            assert!(all_eq(slice2, 0x22));
            assert!(all_eq(slice, 0x11));

            let slice3 = bm.alloc::<u8>(buf.ptr(), 3 * BITS / 2).unwrap();
            assert_eq!(3 * BITS / 2, slice3.len());

            fill(slice3, 0x33);
            assert!(all_eq(slice3, 0x33));
            assert!(all_eq(slice2, 0x22));
            assert!(all_eq(slice, 0x11));

            // Free them
            assert!(bm.is_allocated(buf.ptr(), slice2));
            bm.free(buf.ptr(), slice2);
            assert!(!bm.is_allocated(buf.ptr(), slice2));
            assert!(bm.is_allocated(buf.ptr(), slice));
            bm.free(buf.ptr(), slice);
            assert!(!bm.is_allocated(buf.ptr(), slice));
            assert!(bm.is_allocated(buf.ptr(), slice3));
            bm.free(buf.ptr(), slice3);
            assert!(!bm.is_allocated(buf.ptr(), slice3));

            // All of our bitmaps should be free.
            assert_eq!(vec![0u64; 4], bitmaps(&bm, &buf, 4));
        }
    }

    #[test]
    fn bitmap_allocator_bytes_required() {
        // Chunk size of 16 bytes (like grapheme_chunk in page.zig)
        {
            type Alloc = BitmapAllocator<16>;

            // Single byte rounds up to chunk size
            assert_eq!(16, Alloc::bytes_required::<u8>(1));
            assert_eq!(16, Alloc::bytes_required::<u8>(16));
            assert_eq!(32, Alloc::bytes_required::<u8>(17));

            // u21 (4 bytes each)
            assert_eq!(16, Alloc::bytes_required::<u32>(1)); // 4 bytes -> 16
            assert_eq!(16, Alloc::bytes_required::<u32>(4)); // 16 bytes -> 16
            assert_eq!(32, Alloc::bytes_required::<u32>(5)); // 20 bytes -> 32
            assert_eq!(32, Alloc::bytes_required::<u32>(6)); // 24 bytes -> 32
        }

        // Chunk size of 4 bytes
        {
            type Alloc = BitmapAllocator<4>;

            assert_eq!(4, Alloc::bytes_required::<u8>(1));
            assert_eq!(4, Alloc::bytes_required::<u8>(4));
            assert_eq!(8, Alloc::bytes_required::<u8>(5));

            // u32 (4 bytes each) - exactly one chunk per element
            assert_eq!(4, Alloc::bytes_required::<u32>(1));
            assert_eq!(8, Alloc::bytes_required::<u32>(2));
        }

        // Chunk size of 32 bytes (like string_chunk in page.zig)
        {
            type Alloc = BitmapAllocator<32>;

            assert_eq!(32, Alloc::bytes_required::<u8>(1));
            assert_eq!(32, Alloc::bytes_required::<u8>(32));
            assert_eq!(64, Alloc::bytes_required::<u8>(33));
        }
    }
}
