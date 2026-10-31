//! Port of `src/highway/src/main.zig`.
//!
//! STATUS: ported
//!
//! Ghostty's Zig build vendors Google Highway (C++) for SIMD dispatch, with
//! the CPU feature detection written in Zig (`detect.zig`) and a small C++
//! shim (`hwy_supported_targets`). The Rust port doesn't link Highway: Rust
//! code uses `std::arch` intrinsics with `is_x86_feature_detected!` & co. for
//! runtime dispatch. This module keeps the Highway target mask and the
//! detection logic so callers that inspect supported targets still work;
//! [`supported_targets`] reproduces `hwy::SupportedTargets()` from the
//! vendored `targets.cpp` (scalar + emu128 + detected targets).

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "detect.rs"]
pub mod detect;
#[path = "targets.rs"]
pub mod targets;
// @generated-mods-end

pub use targets::Targets;

/// `HWY_SCALAR | HWY_EMU128`: always supported.
const BASELINE: i64 = (1 << 62) | (1 << 61);

pub fn supported_targets() -> Targets {
    Targets::from_bits(BASELINE | detect::ghostty_hwy_detect_targets())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_targets_test() {
        let t = supported_targets();
        assert!(t.scalar());
        assert!(t.emu128());
        #[cfg(target_arch = "x86_64")]
        assert!(t.sse2());
    }
}
