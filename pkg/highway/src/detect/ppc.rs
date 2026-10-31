//! Port of `src/highway/src/detect/ppc.zig`.
//!
//! STATUS: ported

#![cfg(any(target_arch = "powerpc", target_arch = "powerpc64"))]

use super::HwyTargets;

pub fn detect() -> i64 {
    #[allow(unused_mut)]
    let mut t = HwyTargets::default();

    #[cfg(target_os = "linux")]
    {
        const AT_HWCAP: usize = 16;
        const AT_HWCAP2: usize = 26;
        let hwcap = super::linux::getauxval(AT_HWCAP);
        let hwcap2 = super::linux::getauxval(AT_HWCAP2);

        // From Linux arch/powerpc/include/uapi/asm/cputable.h
        const PPC_FEATURE_HAS_ALTIVEC: usize = 0x10000000;
        const PPC_FEATURE_HAS_VSX: usize = 0x00000080;
        const PPC_FEATURE2_ARCH_2_07: usize = 0x80000000; // POWER8
        const PPC_FEATURE2_VEC_CRYPTO: usize = 0x02000000;
        const PPC_FEATURE2_ARCH_3_00: usize = 0x00800000; // POWER9
        const PPC_FEATURE2_ARCH_3_1: usize = 0x00040000; // POWER10
        const PPC_FEATURE2_MMA: usize = 0x00020000;

        if hwcap & PPC_FEATURE_HAS_ALTIVEC != 0
            && hwcap & PPC_FEATURE_HAS_VSX != 0
            && hwcap2 & PPC_FEATURE2_ARCH_2_07 != 0
            && hwcap2 & PPC_FEATURE2_VEC_CRYPTO != 0
        {
            t.set_ppc8(true);

            if hwcap2 & PPC_FEATURE2_ARCH_3_00 != 0 {
                t.set_ppc9(true);

                if hwcap2 & PPC_FEATURE2_ARCH_3_1 != 0 && hwcap2 & PPC_FEATURE2_MMA != 0 {
                    t.set_ppc10(true);
                }
            }
        }
    }

    t.to_bits()
}
