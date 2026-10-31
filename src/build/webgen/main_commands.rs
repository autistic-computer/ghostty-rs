//! Port of `src/build/webgen/main_commands.zig`: the website's CLI action
//! reference (`share/ghostty/webdata/commands.mdx`), written to stdout.
//!
//! STATUS: ported
//!
//! NOTE(port): the Zig `main` never flushes its 2 KiB stdout buffer, so
//! Zig's output loses its last partial buffer; this port flushes, and its
//! output otherwise matches Zig's byte for byte.

use std::io::{self, Write};

use ghostty::cli::ghostty::Action;
use ghostty::extra::ACTIONS;

fn main() -> io::Result<()> {
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    gen_actions(&mut stdout)?;
    stdout.flush()
}

// Note: as a shortcut for defining inline editOnGithubLinks per cli action the user
// is directed to the folder view on Github. This includes a README pointing them to
// the files to edit.
pub fn gen_actions<W: Write>(writer: &mut W) -> io::Result<()> {
    // Write the header
    writer.write_all(
        b"---
title: Reference
description: Reference of all Ghostty action subcommands.
editOnGithubLink: https://github.com/ghostty-org/ghostty/tree/main/src/cli
---
Ghostty includes a number of utility actions that can be accessed as subcommands.
Actions provide utilities to work with config, list keybinds, list fonts, demo themes,
and debug.
",
    )?;

    for action in ACTIONS {
        let field = action.name();
        writeln!(writer, "## {field}")?;

        // Every action has a help string (helpgen requires the doc comment).
        for s in action.help().split('\n') {
            writeln!(writer, "{s}")?;
        }
        writer.write_all(b"\n```\n")?;
        match action.action {
            Action::Help | Action::Version => writeln!(writer, "ghostty --{field}")?,
            _ => writeln!(writer, "ghostty +{field}")?,
        }
        writer.write_all(b"```\n\n")?;
    }
    Ok(())
}
