//! Port of `src/build/mdgen/main_ghostty_5.zig`: the `ghostty(5)` man page
//! markdown, written to stdout.
//!
//! STATUS: ported

// Each man page uses only some of the generators.
#[allow(dead_code)]
#[path = "mdgen.rs"]
mod gen;

use std::io::{self, Write};

fn main() -> io::Result<()> {
    let mut writer = io::BufWriter::new(io::stdout().lock());
    gen::substitute(include_str!("ghostty_5_header.md"), &mut writer)?;
    gen::gen_config(&mut writer, false)?;
    gen::gen_keybind_actions(&mut writer)?;
    gen::substitute(include_str!("ghostty_5_footer.md"), &mut writer)?;
    writer.flush()
}
