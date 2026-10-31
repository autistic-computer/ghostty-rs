//! Port of `src/unicode/symbols_uucode.zig`.
//!
//! STATUS: ported
//!
//! The Zig file is the build-time generator of `symbols_table`. In Rust the
//! committed table is produced by `tools/gen_unicode_tables.py`; [`main`] is
//! kept as the equivalent generator (writing Rust instead of Zig) and the
//! tests check that both agree.

use super::lut;
use super::uucode;

struct Ctx;

impl lut::Context<bool> for Ctx {
    fn get(&self, cp: u32) -> bool {
        if cp > uucode::config::MAX_CODE_POINT {
            false
        } else {
            uucode::is_symbol(cp)
        }
    }

    fn eql(&self, a: &bool, b: &bool) -> bool {
        a == b
    }
}

/// Generate the lookup tables.
pub fn generate() -> Result<lut::OwnedTables<bool>, lut::GenerateError> {
    lut::Generator::new(Ctx).generate()
}

/// Runnable entry point to generate the lookup tables and output to stdout.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let t = generate()?;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    t.as_tables().write_rust(&mut lock, "bool")?;
    std::io::Write::flush(&mut lock)?;

    // Uncomment when manually debugging to see our table sizes.
    // log::warn!("stage1={} stage2={} stage3={}", t.stage1.len(), t.stage2.len(), t.stage3.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unicode::symbols_table::TABLE;

    #[test]
    fn unicode_symbols_tables_match_uucode() {
        for cp in 0..0x1FFFFFu32 {
            let t = TABLE.get(cp);
            let uu = if cp > uucode::config::MAX_CODE_POINT {
                false
            } else {
                uucode::is_symbol(cp)
            };
            assert_eq!(t, uu, "mismatch cp=U+{cp:x}");
        }
    }

    /// The committed table is exactly what this generator produces.
    #[test]
    fn generator_reproduces_committed_table() {
        let t = generate().unwrap();
        assert_eq!(TABLE.stage1, &t.stage1[..]);
        assert_eq!(TABLE.stage2, &t.stage2[..]);
        assert_eq!(TABLE.stage3, &t.stage3[..]);
    }
}
