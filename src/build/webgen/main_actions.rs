//! Port of `src/build/webgen/main_actions.zig`: the website's keybind
//! action reference (`share/ghostty/webdata/actions.mdx`), written to stdout.
//!
//! STATUS: ported
//!
//! NOTE(port): the Zig `main` never flushes its 2 KiB stdout buffer, so
//! Zig's output loses its last partial buffer; this port flushes, and its
//! output otherwise matches Zig's byte for byte.

use std::io::{self, Write};

use ghostty::input::helpgen_actions::{generate, Format};

fn main() -> io::Result<()> {
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    generate(&mut stdout, Format::Markdown, true)?;
    stdout.flush()
}
