//! Port of `src/build/mdgen/mdgen.zig`: generates the man page markdown
//! (later converted with pandoc) from Ghostty's help strings.
//!
//! STATUS: ported

use std::io::{self, Write};

use ghostty::build_config;
use ghostty::cli::args::Fields;
use ghostty::cli::ghostty::Action;
use ghostty::config::Config;
use ghostty::extra::ACTIONS;
use ghostty::input::binding::ActionKey;
use ghostty::input::helpgen_actions::help_strings;

pub fn substitute<W: Write>(input: &str, writer: &mut W) -> io::Result<()> {
    writer.write_all(input.replace("@@VERSION@@", build_config::VERSION_STRING).as_bytes())
}

/// Write a help string as a markdown definition: the first line prefixed
/// with `:   `, the following ones indented by four spaces.
fn write_definition<W: Write>(writer: &mut W, help: &str) -> io::Result<()> {
    for (i, s) in help.split('\n').enumerate() {
        writer.write_all(if i == 0 { b":   " } else { b"    " })?;
        writer.write_all(s.as_bytes())?;
        writer.write_all(b"\n")?;
    }
    writer.write_all(b"\n\n")
}

pub fn gen_config<W: Write>(writer: &mut W, cli: bool) -> io::Result<()> {
    writer.write_all(b"\n# CONFIGURATION OPTIONS\n\n")?;

    for field in <Config as Fields>::FIELDS {
        if field.name.starts_with('_') {
            continue;
        }

        writer.write_all(b"**`")?;
        if cli {
            writer.write_all(b"--")?;
        }
        writer.write_all(field.name.as_bytes())?;
        writer.write_all(b"`**\n\n")?;
        if field.has_help() {
            write_definition(writer, &field.help())?;
        }
    }
    Ok(())
}

pub fn gen_actions<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(b"\n# COMMAND LINE ACTIONS\n\n")?;

    for action in ACTIONS {
        match action.action {
            Action::Help => writer.write_all(b"**`--help`**\n\n")?,
            Action::Version => writer.write_all(b"**`--version`**\n\n")?,
            _ => {
                writer.write_all(b"**`+")?;
                writer.write_all(action.name().as_bytes())?;
                writer.write_all(b"`**\n\n")?;
            }
        }

        // Every action has a help string (helpgen requires the doc comment).
        write_definition(writer, action.help())?;
    }
    Ok(())
}

pub fn gen_keybind_actions<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(b"\n# KEYBIND ACTIONS\n\n")?;

    for &key in ActionKey::ALL {
        let field = key.as_str();
        if field.starts_with('_') {
            continue;
        }

        writer.write_all(b"**`")?;
        writer.write_all(field.as_bytes())?;
        writer.write_all(b"`**\n\n")?;

        if let Some(help) = help_strings::keybind_action(key) {
            write_definition(writer, help)?;
        }
    }
    Ok(())
}
