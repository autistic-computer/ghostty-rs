//! Port of `src/highway/src/detect.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "detect/aarch64_darwin.rs"]
pub mod aarch64_darwin;
#[path = "detect/aarch64_linux.rs"]
pub mod aarch64_linux;
#[path = "detect/linux.rs"]
pub mod linux;
#[path = "detect/loongarch.rs"]
pub mod loongarch;
#[path = "detect/ppc.rs"]
pub mod ppc;
#[path = "detect/riscv.rs"]
pub mod riscv;
#[path = "detect/s390x.rs"]
pub mod s390x;
#[path = "detect/x86.rs"]
pub mod x86;
// @generated-mods-end

#[allow(unused_imports)]
use super::targets::Targets as HwyTargets;

/// Detect Highway targets at runtime using minimal, direct CPU feature
/// probing.
///
/// Previous versions called std.zig.system.resolveTargetQuery which
/// drags in the full Zig target/CPU model tables for every architecture,
/// bloating the binary by ~300 KB and causing code-layout regressions in
/// unrelated hot paths (icache / branch-predictor pressure).
///
/// This version uses only inline assembly (CPUID on x86, MRS on AArch64)
/// and lightweight syscalls (sysctlbyname on Darwin, getauxval on Linux),
/// so it adds no data tables and no std.Target dependency.
///
/// (Exported to C in the Zig build for the vendored Highway C++; the Rust
/// port has no C++ consumer, so it isn't `#[no_mangle]`.)
pub extern "C" fn ghostty_hwy_detect_targets() -> i64 {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        x86::detect()
    }
    #[cfg(target_arch = "aarch64")]
    {
        detect_aarch64()
    }
    #[cfg(any(target_arch = "powerpc", target_arch = "powerpc64"))]
    {
        ppc::detect()
    }
    #[cfg(target_arch = "s390x")]
    {
        s390x::detect()
    }
    #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
    {
        riscv::detect()
    }
    #[cfg(any(target_arch = "loongarch32", target_arch = "loongarch64"))]
    {
        loongarch::detect()
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "x86",
        target_arch = "aarch64",
        target_arch = "powerpc",
        target_arch = "powerpc64",
        target_arch = "s390x",
        target_arch = "riscv32",
        target_arch = "riscv64",
        target_arch = "loongarch32",
        target_arch = "loongarch64",
    )))]
    {
        0
    }
}

#[cfg(target_arch = "aarch64")]
fn detect_aarch64() -> i64 {
    let mut t = HwyTargets::default();

    // All AArch64 implementations have NEON.
    t.set_neon_without_aes(true);

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        aarch64_darwin::detect(&mut t)
    }
    #[cfg(target_os = "linux")]
    {
        aarch64_linux::detect(&mut t)
    }

    // Other OS: return baseline NEON.
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
    {
        t.to_bits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_runs() {
        let bits = ghostty_hwy_detect_targets();
        // Detection never reports the emulation targets itself.
        let t = HwyTargets::from_bits(bits);
        assert!(!t.scalar());
        assert!(!t.emu128());
    }
}
