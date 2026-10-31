//! Port of `src/helpgen.zig`.
//!
//! STATUS: ported
//!
//! This program is used to generate the help strings from the configuration
//! file and CLI actions for Ghostty. These can then be used to generate
//! help, docs, website, etc.
//!
//! Zig parses `config/Config.zig`, `input/Binding.zig` and the `cli/*.zig`
//! action files with `std.zig.Ast` to find the doc comments. The Rust port
//! takes the same doc comment lines from the compiled types instead:
//! `Config::FIELDS` (captured by `cli_struct!`), `ActionKey::doc_lines`
//! (captured by `define_actions!`) and the CLI actions' help strings
//! ([`crate::cli::ghostty::Action::help_string`], via
//! [`crate::extra::ACTIONS`]). The output (a Zig source file, the
//! `help_strings` module) is the same.

use std::io::{self, Write};

use crate::cli::args::Fields;
use crate::config::Config;
use crate::extra::{find_common_prefix, ACTIONS};
use crate::input::binding::ActionKey;

pub fn main() -> io::Result<()> {
    let mut out = String::new();
    generate(&mut out);

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writer.write_all(out.as_bytes())?;
    writer.flush()
}

/// Generate the whole `help_strings` Zig source.
pub fn generate(writer: &mut String) {
    writer.push_str("// THIS FILE IS AUTO GENERATED\n\n");

    gen_config(writer);
    gen_actions(writer);
    gen_keybind_actions(writer);
}

fn gen_config(writer: &mut String) {
    writer.push_str("/// Configuration help\npub const Config = struct {\n\n");

    for field in <Config as Fields>::FIELDS {
        if field.name.starts_with('_') {
            continue;
        }
        let lines: Vec<&str> = field.doc_lines().collect();
        gen_config_field(writer, field.name, &lines);
    }

    writer.push_str("};\n");
}

/// Write one `pub const <name>: [:0]const u8 = ...` declaration. Fields
/// without doc comments are skipped (Zig only considers identifiers that
/// are preceded by doc comments).
fn gen_config_field(writer: &mut String, field: &str, doc_lines: &[&str]) {
    if doc_lines.is_empty() {
        return;
    }

    let comment = extract_doc_comments(doc_lines);
    writer.push_str("pub const ");
    writer.push_str(&zig_identifier(field));
    writer.push_str(": [:0]const u8 = \n");
    writer.push_str(&comment);
    writer.push('\n');
}

fn gen_actions(writer: &mut String) {
    writer.push_str("\n/// Actions help\npub const Action = struct {\n\n");

    for action in ACTIONS {
        // The function must be preceded by a doc comment.
        if action.help().is_empty() {
            eprint!(
                "doc comment must be present on run function of the {} action!",
                action.name()
            );
            std::process::exit(1);
        }

        let lines: Vec<&str> = action.help().split('\n').collect();
        let comment = extract_doc_comments(&lines);
        writer.push_str("pub const @\"");
        writer.push_str(action.name());
        writer.push_str("\" = \n");
        writer.push_str(&comment);
        writer.push_str("\n\n");
    }

    writer.push_str("};\n");
}

fn gen_keybind_actions(writer: &mut String) {
    writer.push_str("/// keybind actions help\npub const KeybindAction = struct {\n\n");

    for key in ActionKey::ALL {
        let field = key.as_str();
        if field.starts_with('_') {
            continue;
        }
        gen_config_field(writer, field, key.doc_lines());
    }

    writer.push_str("};\n");
}

/// Convert doc comment lines (the text after `///`) to a Zig multiline
/// string literal with the common leading-space prefix removed.
fn extract_doc_comments(lines: &[&str]) -> String {
    let mut buffer = String::new();
    let prefix = find_common_prefix(lines);
    for line in lines {
        buffer.push_str("    \\\\");
        buffer.push_str(&line[prefix.min(line.len())..]);
        buffer.push('\n');
    }
    buffer.push_str(";\n");
    buffer
}

/// The identifier as written in Zig source: names that aren't valid bare
/// identifiers (e.g. `font-family`) or are keywords use `@"..."`.
fn zig_identifier(name: &str) -> String {
    const RESERVED: &[&str] = &[
        "addrspace", "align", "allowzero", "and", "anyframe", "anytype", "asm", "break",
        "callconv", "catch", "comptime", "const", "continue", "defer", "else", "enum",
        "errdefer", "error", "export", "extern", "fn", "for", "if", "inline", "linksection",
        "noalias", "noinline", "nosuspend", "opaque", "or", "orelse", "packed", "pub",
        "resume", "return", "struct", "suspend", "switch", "test", "threadlocal", "try",
        "union", "unreachable", "var", "volatile", "while", "true", "false", "null",
        "undefined", "bool", "void", "type", "anyerror", "anyopaque", "noreturn",
    ];

    let bytes = name.as_bytes();
    let valid = !bytes.is_empty()
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
        && !RESERVED.contains(&name);
    if valid {
        name.to_string()
    } else {
        format!("@\"{name}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_doc_comments_strips_common_prefix() {
        assert_eq!(
            "    \\\\a\n    \\\\  b\n    \\\\\n;\n",
            extract_doc_comments(&[" a", "   b", ""])
        );
    }

    #[test]
    fn identifiers() {
        assert_eq!("language", zig_identifier("language"));
        assert_eq!("@\"font-family\"", zig_identifier("font-family"));
        assert_eq!("@\"error\"", zig_identifier("error"));
    }

    #[test]
    fn generate_help_strings() {
        let mut out = String::new();
        generate(&mut out);
        assert!(out.starts_with(
            "// THIS FILE IS AUTO GENERATED\n\n/// Configuration help\npub const Config = struct {\n\n"
        ));
        assert!(out.contains("pub const @\"font-family\": [:0]const u8 = \n    \\\\"));
        assert!(out.contains(
            "\n/// Actions help\npub const Action = struct {\n\npub const @\"version\" = \n    \\\\The `version` command is used to display information about Ghostty. Recognized as\n    \\\\either `+version` or `--version`.\n;\n\n\n"
        ));
        assert!(out.contains("/// keybind actions help\npub const KeybindAction = struct {\n\n"));
        assert!(out.ends_with("};\n"));
    }
}
