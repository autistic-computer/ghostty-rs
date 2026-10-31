//! Port of `src/unicode/props_uucode.zig`.
//!
//! STATUS: ported
//!
//! The Zig file is the build-time generator of `props_table`. In Rust the
//! committed table is produced by `tools/gen_unicode_tables.py`; [`main`] is
//! kept as the equivalent generator (writing Rust instead of Zig) and the
//! tests check that both agree.

use super::lut;
use super::props::Properties;
use super::uucode;

pub fn get(cp: u32) -> Properties {
    if cp > uucode::config::MAX_CODE_POINT {
        return Properties {
            width: 1,
            width_zero_in_grapheme: true,
            grapheme_break: uucode::types::GraphemeBreakNoControl::Other,
            emoji_vs_base: false,
        };
    }

    Properties {
        width: uucode::width(cp),
        width_zero_in_grapheme: uucode::wcwidth_zero_in_grapheme(cp),
        grapheme_break: uucode::grapheme_break_no_control(cp),
        emoji_vs_base: uucode::is_emoji_vs_base(cp),
    }
}

struct Ctx;

impl lut::Context<Properties> for Ctx {
    fn get(&self, cp: u32) -> Properties {
        get(cp)
    }

    fn eql(&self, a: &Properties, b: &Properties) -> bool {
        Properties::eql(*a, *b)
    }
}

/// Generate the lookup tables.
pub fn generate() -> Result<lut::OwnedTables<Properties>, lut::GenerateError> {
    lut::Generator::new(Ctx).generate()
}

/// Runnable entry point to generate the lookup tables and output to stdout.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let t = generate()?;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    t.as_tables().write_rust(&mut lock, "Properties")?;
    std::io::Write::flush(&mut lock)?;

    // Uncomment when manually debugging to see our table sizes.
    // log::warn!("stage1={} stage2={} stage3={}", t.stage1.len(), t.stage2.len(), t.stage3.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unicode::props_table::TABLE;

    #[test]
    fn unicode_props_tables_match_uucode() {
        let min = 0xFF + 1; // start outside ascii
        let max = 0x1FFFFF + 1;
        for cp in min..max {
            let t = TABLE.get(cp);
            let uu = if cp > uucode::config::MAX_CODE_POINT {
                1
            } else {
                uucode::width(cp)
            };
            assert_eq!(t.width, uu, "mismatch cp=U+{cp:x}");
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
