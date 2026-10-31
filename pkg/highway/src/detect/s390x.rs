//! Port of `src/highway/src/detect/s390x.zig`.
//!
//! STATUS: ported

#![cfg(target_arch = "s390x")]

use super::HwyTargets;

pub fn detect() -> i64 {
    #[allow(unused_mut)]
    let mut t = HwyTargets::default();

    #[cfg(target_os = "linux")]
    {
        const AT_HWCAP: usize = 16;
        let hwcap = super::linux::getauxval(AT_HWCAP);

        // From Linux arch/s390/include/asm/elf.h
        const HWCAP_VX: usize = 1 << 11;
        const HWCAP_VXE: usize = 1 << 13; // z14
        const HWCAP_VXE2: usize = 1 << 15; // z15

        if hwcap & HWCAP_VX != 0 && hwcap & HWCAP_VXE != 0 {
            t.set_z14(true);

            if hwcap & HWCAP_VXE2 != 0 {
                t.set_z15(true);
            }
        }
    }

    t.to_bits()
}
