//! Port of `src/highway/src/targets.zig`.
//!
//! STATUS: ported

macro_rules! targets {
    ($($(#[$doc:meta])* $name:ident, $set:ident, $bit:expr;)*) => {
        /// Highway's target bitmask (`HWY_*` target constants), as a
        /// `packed struct(i64)` of flags in the Zig source.
        #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct Targets(i64);

        impl Targets {
            $(
                $(#[$doc])*
                #[inline]
                pub const fn $name(self) -> bool {
                    (self.0 >> $bit) & 1 != 0
                }

                #[inline]
                pub fn $set(&mut self, v: bool) {
                    if v {
                        self.0 |= 1i64 << $bit;
                    } else {
                        self.0 &= !(1i64 << $bit);
                    }
                }
            )*

            /// The bit position of each named flag (`@bitOffsetOf`).
            pub fn bit_pos(field_name: &str) -> Option<u32> {
                match field_name {
                    $(stringify!($name) => Some($bit),)*
                    _ => None,
                }
            }
        }
    };
}

targets! {
    // x86_64
    avx10_2_512, set_avx10_2_512, 3;
    avx3_spr, set_avx3_spr, 4;
    avx10_2, set_avx10_2, 5;
    avx3_zen4, set_avx3_zen4, 6;
    avx3_dl, set_avx3_dl, 7;
    avx3, set_avx3, 8;
    avx2, set_avx2, 9;
    sse4, set_sse4, 11;
    ssse3, set_ssse3, 12;
    sse2, set_sse2, 14;

    // aarch64
    sve2_128, set_sve2_128, 18;
    sve_256, set_sve_256, 19;
    sve2, set_sve2, 23;
    sve, set_sve, 24;
    neon_bf16, set_neon_bf16, 26;
    neon, set_neon, 28;
    neon_without_aes, set_neon_without_aes, 29;

    // risc-v
    rvv, set_rvv, 37;

    // LoongArch
    lasx, set_lasx, 40;
    lsx, set_lsx, 41;

    // IBM Power
    ppc10, set_ppc10, 47;
    ppc9, set_ppc9, 48;
    ppc8, set_ppc8, 49;
    z15, set_z15, 50;
    z14, set_z14, 51;

    // WebAssembly
    wasm_emu256, set_wasm_emu256, 58;
    wasm, set_wasm, 59;

    // Emulation
    emu128, set_emu128, 61;
    scalar, set_scalar, 62;
}

impl Targets {
    /// `@bitCast` to the raw Highway mask.
    #[inline]
    pub const fn to_bits(self) -> i64 {
        self.0
    }

    /// `@bitCast` from the raw Highway mask.
    #[inline]
    pub const fn from_bits(bits: i64) -> Self {
        Self(bits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verify that each flag field matches its Highway bit constant
    // (a comptime block in the Zig source).
    #[test]
    fn bit_positions() {
        let expected: &[(&str, u32)] = &[
            ("avx10_2_512", 3),
            ("avx3_spr", 4),
            ("avx10_2", 5),
            ("avx3_zen4", 6),
            ("avx3_dl", 7),
            ("avx3", 8),
            ("avx2", 9),
            ("sse4", 11),
            ("ssse3", 12),
            ("sse2", 14),
            ("sve2_128", 18),
            ("sve_256", 19),
            ("sve2", 23),
            ("sve", 24),
            ("neon_bf16", 26),
            ("neon", 28),
            ("neon_without_aes", 29),
            ("rvv", 37),
            ("lasx", 40),
            ("lsx", 41),
            ("ppc10", 47),
            ("ppc9", 48),
            ("ppc8", 49),
            ("z15", 50),
            ("z14", 51),
            ("wasm_emu256", 58),
            ("wasm", 59),
            ("emu128", 61),
            ("scalar", 62),
        ];
        for &(name, bit) in expected {
            assert_eq!(Targets::bit_pos(name), Some(bit), "{name}");
        }
    }

    #[test]
    fn set_and_get() {
        let mut t = Targets::default();
        t.set_avx2(true);
        t.set_scalar(true);
        assert!(t.avx2());
        assert!(t.scalar());
        assert!(!t.sse2());
        assert_eq!(t.to_bits(), (1 << 9) | (1 << 62));
        t.set_avx2(false);
        assert_eq!(t.to_bits(), 1 << 62);
        assert_eq!(Targets::from_bits(1 << 14), {
            let mut s = Targets::default();
            s.set_sse2(true);
            s
        });
    }
}
