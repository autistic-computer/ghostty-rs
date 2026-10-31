//! Port of `src/build/webgen/main_config.zig`: the website's configuration
//! reference (`share/ghostty/webdata/config.mdx`), written to stdout.
//!
//! STATUS: ported
//!
//! NOTE(port): the Zig `main` never flushes its 2 KiB stdout buffer, so
//! Zig's output loses its last partial buffer; this port flushes, and its
//! output otherwise matches Zig's byte for byte.

use std::io::{self, Write};

use ghostty::cli::args::Fields;
use ghostty::config::Config;

fn main() -> io::Result<()> {
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    gen_config(&mut stdout)?;
    stdout.flush()
}

/// The markdown block being written for a help string.
#[derive(Clone, Copy)]
enum Block {
    /// Plaintext, do nothing.
    Text,

    /// Code block, wrap in triple backticks. We use indented
    /// code blocks in our comments but the website parser only
    /// supports triple backticks.
    Code,

    /// Callouts. We detect these based on paragraphs starting
    /// with "Note:", "Warning:", etc. (case-insensitive).
    CalloutNote,
    CalloutWarning,
}

fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

pub fn gen_config<W: Write>(writer: &mut W) -> io::Result<()> {
    // Write the header
    writer.write_all(
        b"---
title: Reference
description: Reference of all Ghostty configuration options.
editOnGithubLink: https://github.com/ghostty-org/ghostty/edit/main/src/config/Config.zig
---

This is a reference of all Ghostty configuration options. These
options are ordered roughly by how common they are to be used
and grouped with related options. I recommend utilizing your
browser's search functionality to find the option you're looking
for.

In the future, we'll have a more user-friendly way to view and
organize these options.

",
    )?;

    let fields = <Config as Fields>::FIELDS;
    for (i, field) in fields.iter().enumerate() {
        if field.name.starts_with('_') || !field.has_help() {
            continue;
        }

        // Write the field name.
        writeln!(writer, "## `{}`", field.name)?;

        // For all subsequent fields with no docs, they are grouped
        // with the previous field.
        for next_field in &fields[i + 1..] {
            if next_field.name.starts_with('_') || next_field.has_help() {
                break;
            }
            writeln!(writer, "## `{}`", next_field.name)?;
        }

        // Newline after our headers
        writer.write_all(b"\n")?;

        // We do some really rough markdown "parsing" here so that
        // we can fix up some styles for what our website expects.
        let help = field.help();
        let mut block: Option<Block> = None;
        for s in help.split('\n') {
            // Empty line resets our block
            if s.is_empty() {
                end_block(writer, block)?;
                block = None;

                writer.write_all(b"\n")?;
                continue;
            }

            // If we don't have a block figure out our type.
            let first = block.is_none();
            let current = match block {
                Some(b) => b,
                None => {
                    let b = if s.starts_with("    ") {
                        writer.write_all(b"```\n")?;
                        Block::Code
                    } else if starts_with_ignore_case(s, "note:") {
                        writer.write_all(b"<Note>\n")?;
                        Block::CalloutNote
                    } else if starts_with_ignore_case(s, "warning:") {
                        writer.write_all(b"<Warning>\n")?;
                        Block::CalloutWarning
                    } else {
                        Block::Text
                    };
                    block = Some(b);
                    b
                }
            };

            writer.write_all(
                match current {
                    Block::Text => s,
                    Block::CalloutNote if first => &s["note:".len()..],
                    Block::CalloutWarning if first => &s["warning:".len()..],
                    Block::CalloutNote | Block::CalloutWarning => s,
                    Block::Code => s.strip_prefix("    ").unwrap_or(s),
                }
                .as_bytes(),
            )?;
            writer.write_all(b"\n")?;
        }
        end_block(writer, block)?;
        writer.write_all(b"\n")?;
    }
    Ok(())
}

fn end_block<W: Write>(writer: &mut W, block: Option<Block>) -> io::Result<()> {
    match block {
        None | Some(Block::Text) => Ok(()),
        Some(Block::Code) => writer.write_all(b"```\n"),
        Some(Block::CalloutNote) => writer.write_all(b"</Note>\n"),
        Some(Block::CalloutWarning) => writer.write_all(b"</Warning>\n"),
    }
}
