//! Port of `src/simd/main.zig`.
//!
//! STATUS: ported
//!
//! SIMD-optimized routines. The Zig build calls C++ (Google Highway,
//! simdutf) when `build_options.simd` is true and otherwise falls back to
//! pure Zig scalar implementations. This port is pure Rust: it implements
//! the scalar semantics, vectorized where it matters (`vt`, `index_of`) with
//! `std::arch` / `memchr`.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "base64.rs"]
pub mod base64;
#[path = "base64_scalar.rs"]
pub mod base64_scalar;
#[path = "codepoint_width.rs"]
pub mod codepoint_width;
#[path = "index_of.rs"]
pub mod index_of;
#[path = "vt.rs"]
pub mod vt;
// @generated-mods-end

pub use codepoint_width::codepoint_width;

/// The native SIMD register width in bytes for the compile target, or None
/// if the target has no usable SIMD support (`std.simd.suggestVectorLength`).
const fn native_vector_bytes() -> Option<usize> {
    if cfg!(any(target_feature = "avx512bw", target_feature = "avx512f")) {
        Some(64)
    } else if cfg!(target_feature = "avx2") {
        Some(32)
    } else if cfg!(any(
        target_feature = "sse2",
        target_feature = "neon",
        target_feature = "simd128",
        target_feature = "altivec",
    )) {
        Some(16)
    } else {
        None
    }
}

/// The number of vector lanes to use for manually vectorized hot
/// loops operating on elements of type T, or None if the target has
/// no usable SIMD support and a plain scalar loop should be used
/// instead (e.g. wasm32 without simd128).
///
/// This is twice the native vector width so that each iteration works
/// two vector registers, giving better instruction-level parallelism.
pub const fn lanes<T>() -> Option<usize> {
    match native_vector_bytes() {
        Some(bytes) => {
            let size = std::mem::size_of::<T>();
            if size == 0 || size > bytes {
                None
            } else {
                Some(bytes / size * 2)
            }
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_test() {
        #[cfg(target_arch = "x86_64")]
        {
            let l = lanes::<u8>().unwrap();
            assert!(l >= 32);
            assert_eq!(l / 4, lanes::<u32>().unwrap());
        }
        if let Some(l) = lanes::<u64>() {
            assert!(l >= 2);
        }
    }
}
