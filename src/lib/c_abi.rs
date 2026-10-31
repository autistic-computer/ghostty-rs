//! Port of `src/lib/c_abi.zig`.
//!
//! STATUS: ported
//!
//! Memory-layout properties shared with C API callers.
//!
//! The C ABI defines how values are represented in memory so code written in
//! different languages can safely exchange them. One part of that contract is
//! alignment: some values must begin at an address divisible by 2, 4, 8, or
//! another power of two. Reading a value from a less-aligned address can be
//! slow on some CPUs and invalid on others.
//!
//! This module derives those properties from the compile target. It does not
//! call or link libc. Keeping the calculation here gives allocators and ABI
//! metadata one source of truth.

use std::ffi::{c_char, c_double, c_float, c_int, c_long, c_longlong, c_short};
use std::mem::align_of;

/// Alignment of C `long double` on the current target. Rust has no
/// `long double` type, so this mirrors what Zig's `cTypeAlignment` reports.
const LONG_DOUBLE_ALIGNMENT: usize = if cfg!(all(target_arch = "x86_64", not(windows))) {
    16
} else if cfg!(all(
    target_arch = "aarch64",
    not(any(target_vendor = "apple", windows))
)) {
    16
} else if cfg!(any(
    target_arch = "wasm32",
    target_arch = "wasm64",
    target_arch = "riscv64",
    target_arch = "powerpc64",
    target_arch = "s390x"
)) {
    16
} else if cfg!(target_arch = "x86") {
    4
} else {
    align_of::<c_double>()
};

const fn max(a: usize, b: usize) -> usize {
    if a > b {
        a
    } else {
        b
    }
}

/// Largest address alignment required by a fundamental C ABI type, in bytes.
///
/// For example, a value of 16 means storage intended to hold an arbitrary C
/// value must begin at an address evenly divisible by 16. The Wasm allocator
/// uses this value for caller-owned ABI storage, and `ghostty_type_json`
/// publishes it so hosts know the guarantee made by that allocator.
pub const MAX_ALIGNMENT: u16 = {
    let mut result = align_of::<*const ()>();
    result = max(result, align_of::<c_char>());
    result = max(result, align_of::<c_short>());
    result = max(result, align_of::<c_int>());
    result = max(result, align_of::<c_long>());
    result = max(result, align_of::<c_longlong>());
    result = max(result, align_of::<c_float>());
    result = max(result, align_of::<c_double>());
    result = max(result, LONG_DOUBLE_ALIGNMENT);
    result as u16
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_alignment_is_power_of_two() {
        assert!(MAX_ALIGNMENT.is_power_of_two());
        assert!(MAX_ALIGNMENT as usize >= align_of::<u64>());
    }
}
