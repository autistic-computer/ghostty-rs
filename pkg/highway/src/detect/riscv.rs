//! Port of `src/highway/src/detect/riscv.zig`.
//!
//! STATUS: ported

#![cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]

use super::HwyTargets;

pub fn detect() -> i64 {
    #[allow(unused_mut)]
    let mut t = HwyTargets::default();

    #[cfg(target_os = "linux")]
    {
        const AT_HWCAP: usize = 16;
        let hwcap = super::linux::getauxval(AT_HWCAP);

        // ISA extension bit for 'V' (vector).
        // Letter-based bits: bit position = letter - 'A'.
        const HWCAP_V: usize = 1 << (b'V' - b'A');

        if hwcap & HWCAP_V != 0 {
            t.set_rvv(true);
        }
    }

    t.to_bits()
}
