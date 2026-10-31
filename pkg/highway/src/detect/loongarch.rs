//! Port of `src/highway/src/detect/loongarch.zig`.
//!
//! STATUS: ported

#![cfg(any(target_arch = "loongarch32", target_arch = "loongarch64"))]

use super::HwyTargets;

pub fn detect() -> i64 {
    #[allow(unused_mut)]
    let mut t = HwyTargets::default();

    #[cfg(target_os = "linux")]
    {
        const AT_HWCAP: usize = 16;
        let hwcap = super::linux::getauxval(AT_HWCAP);

        // From Linux arch/loongarch/include/uapi/asm/hwcap.h
        const HWCAP_LSX: usize = 1 << 4;
        const HWCAP_LASX: usize = 1 << 5;

        if hwcap & HWCAP_LSX != 0 {
            t.set_lsx(true);

            if hwcap & HWCAP_LASX != 0 {
                t.set_lasx(true);
            }
        }
    }

    t.to_bits()
}
