//! Port of `src/quirks_memset.zig`.
//!
//! STATUS: ported
//!
//! Provides a fast `memset` that overrides the slow scalar
//! implementation provided by Zig 0.16's compiler_rt.
//!
//! ## Background
//!
//! Zig 0.16.0 globally disabled LLVM loop auto-vectorization to work
//! around an LLVM 21 miscompilation (llvm/llvm-project#186922, see
//! the 0.16.0 release notes). compiler_rt's memset is a naive
//! byte-at-a-time loop that relied entirely on auto-vectorization,
//! so it now compiles to a scalar loop roughly 24x slower than what
//! Zig 0.15 produced.
//!
//! In benchmarks this made `ghostty-bench +terminal-stream` 2.8x slower on
//! ASCII input (memset was 63% of all executed instructions!).
//!
//! ## Rust port
//!
//! The Rust toolchain does not have this problem: `ptr::write_bytes` and
//! `slice::fill` lower to the C library's `memset` (glibc's is hand
//! vectorized) and LLVM auto-vectorization is enabled. Exporting a strong
//! `memset` symbol from Rust would only risk clashing with libc, so this
//! port keeps the *implementation* (the same vectorized algorithm, as a
//! plain function with the C ABI) but deliberately does **not** export it
//! under the `memset` symbol name. It is available for callers that want
//! an inlineable fill with these exact characteristics, and its tests
//! keep the algorithm honest.
//!
//! Other mem functions deliberately not overridden:
//!
//! - memcpy/memmove: compiler_rt's implementations are manually
//!   vectorized upstream ("memcpyFast") and remain fast.
//! - memcmp/bcmp/strlen: also scalar in 0.16, but they never showed
//!   up in our profiles.
//!
//! ## References
//!
//! I referenced the musl asm + c memset implementation (MIT licensed).
//! The primary thing I took away from that is the dc zva trick for
//! aarch64. The remainder is fairly obvious memset work.

use std::ffi::c_int;
use std::ptr;

/// The native vector length for u8 (`std.simd.suggestVectorLength(u8)`).
const SUGGESTED_VECTOR_LENGTH: Option<usize> =
    if cfg!(all(target_arch = "x86_64", target_feature = "avx512bw")) {
        Some(64)
    } else if cfg!(all(
        any(target_arch = "x86_64", target_arch = "x86"),
        target_feature = "avx2"
    )) {
        Some(32)
    } else if cfg!(any(
        all(
            any(target_arch = "x86_64", target_arch = "x86"),
            target_feature = "sse2"
        ),
        all(target_arch = "aarch64", target_feature = "neon"),
        all(target_arch = "wasm32", target_feature = "simd128"),
    )) {
        Some(16)
    } else {
        None
    };

/// Whether the override would be emitted (on targets without SIMD
/// compiler_rt's scalar operations are going to be just as good).
pub const ENABLED: bool = SUGGESTED_VECTOR_LENGTH.is_some();

/// Bytes stored per loop iteration. Twice the native vector size so
/// each iteration issues two vector stores (e.g. `stp q0, q0` on
/// aarch64). Capped at 128 both because the small path below relies
/// on len < 128 once the loop is skipped, and because wider single
/// iterations stop paying off anyway (e.g. on scalable-vector
/// targets that suggest very large lengths).
const VEC_BYTES: usize = {
    let n = 2 * match SUGGESTED_VECTOR_LENGTH {
        Some(n) => n,
        None => 8,
    };
    if n < 128 {
        n
    } else {
        128
    }
};

/// Whether the `dc zva` fast path for large zero fills is available.
/// `dc zva` zeroes a whole cacheline per instruction without moving data
/// through the store pipeline. Based on musl.
const ZVA_ENABLED: bool = cfg!(target_arch = "aarch64");

/// Only use `dc zva` at or above this many bytes. Below this our
/// plain vector loop measures faster. Empirically mesaured.
const ZVA_THRESHOLD: usize = 16384;

/// Store `N` copies of `byte` at `p` (unaligned). This is the Rust
/// equivalent of storing a `@Vector(N, u8)` splat.
///
/// # Safety
/// `p..p+N` must be valid for writes.
#[inline(always)]
unsafe fn splat_store<const N: usize>(p: *mut u8, byte: u8) {
    ptr::write_unaligned(p as *mut [u8; N], [byte; N]);
}

/// Matches the C memset ABI.
///
/// # Safety
/// `dest` must be valid for `len` bytes of writes (it may be null only if
/// `len == 0`).
pub unsafe extern "C" fn memset(dest: *mut u8, c: c_int, len: usize) -> *mut u8 {
    if len == 0 {
        return dest;
    }
    let d = dest;

    // Only the low byte of `c` is used. The C ABI allows a "int"
    // as the c parameter but this behavior of just grabbing the
    // low byte seems consistent across implementations.
    let byte: u8 = c as u32 as u8;

    // Large path: full-width vector stores.
    if len >= VEC_BYTES {
        // Very large zero fills: zero whole cachelines with `dc zva`
        // instead. Only implemented for the (universal in practice)
        // 64-byte block size; anything else falls through to the
        // vector loop.
        #[cfg(target_arch = "aarch64")]
        if ZVA_ENABLED && len >= ZVA_THRESHOLD && byte == 0 && zva::size() == 64 {
            // Head: one unaligned store covering every byte up
            // to the first 64-aligned address (and usually a bit
            // beyond it; the overlap is fine).
            let addr = d as usize;
            let aligned = (addr + 63) & !63usize;
            splat_store::<64>(d, 0);

            // Zero whole aligned cachelines while at least one
            // full line remains in range. `dc zva` requires the
            // WHOLE line to be inside the buffer: it always
            // zeroes all 64 bytes.
            let mut p = aligned;
            let end_addr = addr + len;
            while p + 64 <= end_addr {
                std::arch::asm!("dc zva, {ptr}", ptr = in(reg) p, options(nostack));
                p += 64;
            }

            // Tail: overlapping unaligned store anchored to the
            // end of the buffer, covering whatever the line loop
            // could not. len >= zva_threshold >= 64 so this
            // cannot underflow.
            splat_store::<64>(d.add(len - 64), 0);
            return dest;
        }
        let _ = (ZVA_ENABLED, ZVA_THRESHOLD);

        // Fill [0, N) where N is len rounded down to a multiple of
        // vec_bytes.
        let mut i: usize = 0;
        while i + VEC_BYTES <= len {
            splat_store::<VEC_BYTES>(d.add(i), byte);
            i += VEC_BYTES;
        }

        // Fill the remaining [N, len) tail, if any, with one more
        // full-width store anchored to the END of the buffer. It
        // overlaps up to vec_bytes-1 bytes that the loop already
        // wrote, which is fine (same value), and cannot underflow
        // because len >= vec_bytes in this path.
        if i != len {
            splat_store::<VEC_BYTES>(d.add(len - VEC_BYTES), byte);
        }
        return dest;
    }

    // Small path (len < vec_bytes): pairs of narrower stores, one
    // anchored to the start of the buffer and one to the end.
    //
    // The invariant for each branch below: a store of width w at
    // [0..w] plus a store at [len-w..len] covers every byte exactly
    // when w <= len <= 2*w. Smaller lens are handled by a later
    // branch; larger lens by an earlier one.
    const { assert!(VEC_BYTES <= 128) };
    if VEC_BYTES > 64 && len >= 64 {
        // 64 <= len < vec_bytes <= 128 (asserted above): one
        // pair of 64-byte stores covers len <= 128. Only taken
        // when vec_bytes > 64 (e.g. AVX-512), otherwise the
        // vector loop above already handled these lengths.
        splat_store::<64>(d, byte);
        splat_store::<64>(d.add(len - 64), byte);
        return dest;
    }
    if len >= 16 {
        // 16 <= len < @min(vec_bytes, 64), so at most 63. One pair
        // of 16-byte stores covers len <= 32. For 32 < len < 64, a
        // second pair extends the covered prefix to [0..32] and the
        // covered suffix to [len-32..len], which meet or overlap in
        // the middle.
        splat_store::<16>(d, byte);
        splat_store::<16>(d.add(len - 16), byte);
        if len > 32 {
            splat_store::<16>(d.add(16), byte);
            splat_store::<16>(d.add(len - 32), byte);
        }
        return dest;
    }
    if len >= 8 {
        // 8 <= len <= 15: one 8-byte pair (covers len <= 16).
        splat_store::<8>(d, byte);
        splat_store::<8>(d.add(len - 8), byte);
        return dest;
    }
    if len >= 4 {
        // 4 <= len <= 7: one 4-byte pair (covers len <= 8).
        splat_store::<4>(d, byte);
        splat_store::<4>(d.add(len - 4), byte);
        return dest;
    }

    // 1 <= len <= 3: too short for the pair trick; write bytes one
    // at a time.
    let mut i: usize = 0;
    while i < len {
        *d.add(i) = byte;
        i += 1;
    }
    dest
}

#[cfg(target_arch = "aarch64")]
mod zva {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Cached `dc zva` block size: 0 = not yet queried, 1 = unavailable
    /// or prohibited, otherwise the block size in bytes. Racing threads
    /// store the same value so the memory ordering is irrelevant.
    static ZVA_SIZE: AtomicUsize = AtomicUsize::new(0);

    pub fn size() -> usize {
        let cached = ZVA_SIZE.load(Ordering::Relaxed);
        if cached != 0 {
            return cached;
        }
        let dczid: u64;
        // SAFETY: reading DCZID_EL0 is permitted at EL0.
        unsafe {
            std::arch::asm!("mrs {out}, dczid_el0", out = out(reg) dczid, options(nomem, nostack));
        }
        // Bit 4 (DZP) prohibits DC ZVA; bits 0-3 are log2 of the block
        // size in 4-byte words.
        let size: usize = if dczid & 0x10 != 0 {
            1
        } else {
            4usize << (dczid & 0xF)
        };
        ZVA_SIZE.store(size, Ordering::Relaxed);
        size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memset() {
        // A buffer larger than anything the small/vector paths special
        // case, with room for offset (alignment) variations. Guard bytes
        // around the fill region must remain untouched.
        let mut buf = vec![0u8; 4 * VEC_BYTES + 33];
        let buf_len = buf.len();
        for offset in 0..VEC_BYTES + 1 {
            for len in 0..buf_len - offset {
                for c in [0x00u8, 0x5C, 0xFF] {
                    buf.fill(0xAA);
                    let region = buf[offset..][..len].as_mut_ptr();
                    let ret = unsafe { memset(region, c as c_int, len) };
                    assert_eq!(region, ret);
                    for &b in &buf[..offset] {
                        assert_eq!(0xAA, b);
                    }
                    for &b in &buf[offset..][..len] {
                        assert_eq!(c, b);
                    }
                    for &b in &buf[offset + len..] {
                        assert_eq!(0xAA, b);
                    }
                }
            }
        }

        // Zero length must not touch memory and must tolerate null.
        assert_eq!(ptr::null_mut(), unsafe { memset(ptr::null_mut(), 0x11, 0) });
        let mut one = [0xAAu8; 1];
        unsafe { memset(one.as_mut_ptr(), 0x11, 0) };
        assert_eq!(0xAA, one[0]);
    }

    #[test]
    fn memset_truncates_c_int_fill_value() {
        let mut buf = [0u8; 4];
        unsafe { memset(buf.as_mut_ptr(), -1, buf.len()) };
        for b in buf {
            assert_eq!(0xFF, b);
        }

        unsafe { memset(buf.as_mut_ptr(), 0x1234, buf.len()) };
        for b in buf {
            assert_eq!(0x34, b);
        }
    }

    #[test]
    fn memset_large_zero_fills_dc_zva_path() {
        // Sizes at and around the ZVA threshold, with a guard byte on
        // each side and every offset within a cacheline so the head and
        // tail alignment handling is fully exercised.
        let mut buf = vec![0u8; 2 * ZVA_THRESHOLD + 66];

        for len in [
            ZVA_THRESHOLD - 1,
            ZVA_THRESHOLD,
            ZVA_THRESHOLD + 63,
            2 * ZVA_THRESHOLD,
        ] {
            for offset in 0..65 {
                buf.fill(0xAA);
                let region = buf[1 + offset..][..len].as_mut_ptr();
                unsafe { memset(region, 0x00, len) };
                for &b in &buf[..1 + offset] {
                    assert_eq!(0xAA, b);
                }
                for &b in &buf[1 + offset..][..len] {
                    assert_eq!(0x00, b);
                }
                for &b in &buf[1 + offset + len..] {
                    assert_eq!(0xAA, b);
                }
            }
        }

        // Nonzero fills of ZVA-eligible sizes must not take the ZVA path
        // (it can only write zeroes).
        buf.fill(0xAA);
        unsafe { memset(buf.as_mut_ptr().add(1), 0x5C, ZVA_THRESHOLD) };
        assert_eq!(0xAA, buf[0]);
        for &b in &buf[1..1 + ZVA_THRESHOLD] {
            assert_eq!(0x5C, b);
        }
        assert_eq!(0xAA, buf[1 + ZVA_THRESHOLD]);
    }
}
