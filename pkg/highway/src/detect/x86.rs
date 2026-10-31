//! Port of `src/highway/src/detect/x86.zig`.
//!
//! STATUS: ported

#![cfg(any(target_arch = "x86_64", target_arch = "x86"))]

use super::HwyTargets;

#[cfg(target_arch = "x86")]
use std::arch::x86 as arch;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64 as arch;

struct CpuidResult {
    eax: u32,
    ebx: u32,
    ecx: u32,
    edx: u32,
}

fn cpuid(leaf: u32, subleaf: u32) -> CpuidResult {
    // SAFETY: CPUID is available on every x86_64 CPU and on every x86 CPU
    // Rust supports (i586+). (Newer Rust versions make this a safe fn.)
    #[allow(unused_unsafe)]
    let r = unsafe { arch::__cpuid_count(leaf, subleaf) };
    CpuidResult {
        eax: r.eax,
        ebx: r.ebx,
        ecx: r.ecx,
        edx: r.edx,
    }
}

#[inline]
fn bit(val: u32, pos: u32) -> bool {
    (val >> pos) & 1 != 0
}

fn xgetbv0() -> u32 {
    let eax: u32;
    // SAFETY: only called after CPUID reported OSXSAVE support.
    unsafe {
        std::arch::asm!(
            "xgetbv",
            in("ecx") 0u32,
            out("eax") eax,
            out("edx") _,
            options(nomem, nostack, preserves_flags),
        );
    }
    eax
}

pub fn detect() -> i64 {
    let mut t = HwyTargets::default();

    // x86_64 always has SSE2.
    if cfg!(target_arch = "x86_64") {
        t.set_sse2(true);
    }

    let leaf0 = cpuid(0, 0);
    let max_leaf = leaf0.eax;
    if max_leaf < 1 {
        return t.to_bits();
    }

    let leaf1 = cpuid(1, 0);

    // -- SSE2 on 32-bit x86 -------------------------------------------------
    if cfg!(target_arch = "x86") && bit(leaf1.edx, 25) && bit(leaf1.edx, 26) {
        t.set_sse2(true);
    }

    // -- SSSE3 ---------------------------------------------------------------
    if bit(leaf1.ecx, 0) && // SSE3
        bit(leaf1.ecx, 9)
    // SSSE3
    {
        t.set_ssse3(true);
    }

    // -- SSE4 ----------------------------------------------------------------
    if bit(leaf1.ecx, 19) && // SSE4.1
        bit(leaf1.ecx, 20) && // SSE4.2
        bit(leaf1.ecx, 1) && // PCLMUL
        bit(leaf1.ecx, 25)
    // AES
    {
        t.set_sse4(true);
    }

    // Check XSAVE / AVX OS support before enabling any AVX-dependent target.
    let has_xsave = bit(leaf1.ecx, 27);
    let has_avx_bit = bit(leaf1.ecx, 28);
    let xcr0: u32 = if has_xsave && has_avx_bit {
        xgetbv0()
    } else {
        0
    };
    let has_avx_save = (xcr0 & 0x6) == 0x6; // SSE + AVX state

    // Darwin lazily saves AVX-512 context on first use.
    let has_avx512_save = if cfg!(any(target_os = "macos", target_os = "ios")) {
        true
    } else {
        (xcr0 & 0xE0) == 0xE0 // opmask + zmm_hi256 + hi16_zmm
    };

    // -- AVX2 ----------------------------------------------------------------
    if has_avx_save && max_leaf >= 7 {
        let leaf7 = cpuid(7, 0);

        if bit(leaf7.ebx, 5) && // AVX2
            bit(leaf1.ecx, 12) && // FMA
            bit(leaf1.ecx, 29)
        // F16C
        {
            // Also need LZCNT (extended leaf), BMI, BMI2.
            let leaf_ext = cpuid(0x80000001, 0);
            if bit(leaf_ext.ecx, 5) && // LZCNT
                bit(leaf7.ebx, 3) && // BMI
                bit(leaf7.ebx, 8)
            // BMI2
            {
                t.set_avx2(true);
            }
        }

        // -- AVX-512 ---------------------------------------------------------
        if has_avx512_save {
            if bit(leaf7.ebx, 16) && // AVX512F
                bit(leaf7.ebx, 31) && // AVX512VL
                bit(leaf7.ebx, 17) && // AVX512DQ
                bit(leaf7.ebx, 30) && // AVX512BW
                bit(leaf7.ebx, 28)
            // AVX512CD
            {
                t.set_avx3(true);
            }

            if bit(leaf7.ecx, 11) && // AVX512VNNI
                bit(leaf7.ecx, 10) && // VPCLMULQDQ (AVX save ok)
                bit(leaf7.ecx, 1) && // AVX512VBMI
                bit(leaf7.ecx, 6) && // AVX512VBMI2
                bit(leaf7.ecx, 9) && // VAES (AVX save ok)
                bit(leaf7.ecx, 14) && // AVX512VPOPCNTDQ
                bit(leaf7.ecx, 12) && // AVX512BITALG
                bit(leaf7.ecx, 8)
            // GFNI
            {
                t.set_avx3_dl(true);
            }

            // AVX512BF16 is in leaf 7 sub-1.
            if t.avx3_dl() && leaf7.eax >= 1 {
                let leaf7_1 = cpuid(7, 1);
                if bit(leaf7_1.eax, 5) && is_amd() {
                    // AVX512BF16
                    t.set_avx3_zen4(true);
                }

                if bit(leaf7.edx, 23) && // AVX512FP16
                    bit(leaf7_1.eax, 5)
                // AVX512BF16
                {
                    t.set_avx3_spr(true);
                }
            } else if bit(leaf7.edx, 23) {
                // AVX512FP16 without sub-leaf
                // Can't check BF16 without sub-leaf support, skip avx3_spr.
            }
        }

        // -- AVX10 -----------------------------------------------------------
        if max_leaf >= 7 && cpuid(7, 0).eax >= 1 {
            let leaf7_1 = cpuid(7, 1);
            if bit(leaf7_1.edx, 19) {
                // AVX10.1-256
                if max_leaf >= 0x24 {
                    let leaf24 = cpuid(0x24, 0);
                    if bit(leaf24.ebx, 18) {
                        // AVX10.1-512
                        t.set_avx3_spr(true);
                        t.set_avx3_dl(true);
                        t.set_avx3(true);
                    }
                }

                // AVX10.2 detection would require a leaf we can't
                // reliably check yet; leave for future.
            }
        }
    }

    t.to_bits()
}

fn is_amd() -> bool {
    let leaf0 = cpuid(0, 0);
    // "Auth" "enti" "cAMD"
    leaf0.ebx == 0x68747541 && leaf0.ecx == 0x444d4163 && leaf0.edx == 0x69746e65
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_matches_std() {
        let t = HwyTargets::from_bits(detect());
        if cfg!(target_arch = "x86_64") {
            assert!(t.sse2());
        }
        if t.avx2() {
            assert!(std::arch::is_x86_feature_detected!("avx2"));
        }
        if t.ssse3() {
            assert!(std::arch::is_x86_feature_detected!("ssse3"));
        }
    }
}
