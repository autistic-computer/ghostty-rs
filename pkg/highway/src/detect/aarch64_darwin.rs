//! Port of `src/highway/src/detect/aarch64_darwin.zig`.
//!
//! STATUS: ported

#![cfg(all(target_arch = "aarch64", any(target_os = "macos", target_os = "ios")))]

use super::HwyTargets;

pub fn detect(t: &mut HwyTargets) -> i64 {
    // All Apple Silicon has AES.
    t.set_neon(true);

    // Every Apple chip from A11 (2017) onward has FP16 + DotProd.
    // BF16 arrived with M2 / A15 (ARM_BLIZZARD_AVALANCHE, 2022).
    // We probe hw.optional.arm.FEAT_BF16 to be precise.
    let has_bf16 = darwin_sysctl_bool(c"hw.optional.arm.FEAT_BF16");
    if has_bf16 {
        t.set_neon_bf16(true);
    }

    // Apple Silicon does not support SVE.
    t.to_bits()
}

fn darwin_sysctl_bool(name: &std::ffi::CStr) -> bool {
    let mut value: libc::c_int = 0;
    let mut len: usize = std::mem::size_of::<libc::c_int>();
    // SAFETY: value/len describe a valid c_int buffer; name is NUL-terminated.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut value as *mut libc::c_int).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    rc == 0 && value != 0
}
