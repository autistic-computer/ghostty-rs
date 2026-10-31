//! Port of `src/extra/fish.zig`.
//!
//! STATUS: ported
//!
//! Fish completions.
//!
//! Zig generates the completions at comptime; Rust generates them on first
//! use from `Config`'s fields and the CLI actions (see [`crate::extra`]).

use std::sync::LazyLock;

use super::{config_fields, config_help, FieldType, ACTIONS};

/// A fish completions configuration that contains all the available commands
/// and options.
pub fn completions() -> &'static str {
    static COMPLETIONS: LazyLock<String> = LazyLock::new(|| {
        let mut out = String::new();
        write_completions(&mut out);
        out
    });
    &COMPLETIONS
}

fn write_completions(writer: &mut String) {
    {
        writer.push_str("set -l commands \"");
        let mut count: usize = 0;
        for action in ACTIONS {
            if count > 0 {
                writer.push(' ');
            }
            writer.push('+');
            writer.push_str(action.name());
            count += 1;
        }
        writer.push_str("\"\n");
    }

    writer.push_str("complete -c ghostty -f\n");

    writer.push_str("complete -c ghostty -s e -l help -f\n");
    writer.push_str(
        "complete -c ghostty -n \"not __fish_seen_subcommand_from $commands\" -l version -f\n",
    );

    for field in config_fields() {
        if field.name.starts_with('_') {
            continue;
        }

        writer.push_str("complete -c ghostty -n \"not __fish_seen_subcommand_from $commands\" -l ");
        writer.push_str(field.name);
        writer.push_str(if field.ty != FieldType::Bool { " -r" } else { " " });
        if field.name.starts_with("font-family") {
            writer.push_str(" -f  -a \"(ghostty +list-fonts | grep '^[A-Z]')\"");
        } else if field.name == "theme" {
            writer.push_str(" -f -a \"(ghostty +list-themes | sed -E 's/^(.*) \\(.*\\$/\\1/')\"");
        } else if field.name == "working-directory" {
            writer.push_str(" -f -k -a \"(__fish_complete_directories)\"");
        } else {
            writer.push_str(if field.ty != FieldType::RepeatablePath {
                " -f"
            } else {
                " -F"
            });
            match field.ty {
                FieldType::Enum(names) => {
                    writer.push_str(" -a \"");
                    writer.push_str(&names.join(" "));
                    writer.push('"');
                }
                FieldType::PackedStruct(names) => {
                    writer.push_str(" -a \"");
                    for (i, f) in names.iter().enumerate() {
                        if i > 0 {
                            writer.push(' ');
                        }
                        writer.push_str(f);
                        writer.push_str(" no-");
                        writer.push_str(f);
                    }
                    writer.push('"');
                }
                _ => {}
            }
        }

        if let Some(help) = config_help(field.name) {
            let desc = get_description(&help);
            writer.push_str(" -d \"");
            writer.push_str(&desc);
            writer.push('"');
        }

        writer.push('\n');
    }

    {
        writer.push_str(
            "complete -c ghostty -n \"string match -q -- '+*' (commandline -pt)\" -f -a \"",
        );
        let mut count: usize = 0;
        for action in ACTIONS {
            if count > 0 {
                writer.push(' ');
            }
            writer.push('+');
            writer.push_str(action.name());
            count += 1;
        }
        writer.push_str("\"\n");
    }

    for action in ACTIONS {
        if action.name() == "help" {
            continue;
        }
        if action.name() == "version" {
            continue;
        }

        for opt in action.options {
            if opt.name.starts_with('_') {
                continue;
            }
            writer.push_str(&format!(
                "complete -c ghostty -n \"__fish_seen_subcommand_from +{}\" -l ",
                action.name()
            ));
            writer.push_str(opt.name);
            writer.push_str(if opt.ty != FieldType::Bool { " -r" } else { "" });

            // special case +validate_config --config-file
            if opt.name == "config-file" {
                writer.push_str(" -F");
            } else {
                writer.push_str(" -f");
            }

            match opt.ty {
                FieldType::Enum(names) | FieldType::OptionalEnum(names) => {
                    writer.push_str(" -a \"");
                    writer.push_str(&names.join(" "));
                    writer.push('"');
                }
                _ => {}
            }
            writer.push('\n');
        }
    }
}

/// The first sentence of a help string, on one line, with double quotes
/// escaped.
fn get_description(help: &str) -> String {
    let help = help.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(help.len() * 2);
    let mut prev_was_space = false;

    for (i, &c) in help.iter().enumerate() {
        match c {
            b'.' => {
                out.push(b'.');

                if i + 1 >= help.len() {
                    break;
                }
                let next = help[i + 1];
                if next == b' ' || next == b'\n' {
                    break;
                }
            }
            b'\n' => {
                if !prev_was_space && !out.is_empty() {
                    out.push(b' ');
                    prev_was_space = true;
                }
            }
            b'"' => {
                out.push(b'\\');
                out.push(b'"');
                prev_was_space = false;
            }
            _ => {
                out.push(c);
                prev_was_space = c == b' ';
            }
        }
    }

    // Only ASCII bytes are inserted or removed, so the result is UTF-8.
    String::from_utf8(out).expect("description is UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_description_test() {
        let input = "First sentence with \"quotes\"\nand newlines. Second sentence.";
        let expected = "First sentence with \\\"quotes\\\" and newlines.";

        let result = get_description(input);
        assert_eq!(expected, result);
    }

    #[test]
    fn fish_completions() {
        let c = completions();
        assert!(c.starts_with("set -l commands \"+version +help +list-fonts "));
        assert!(c.contains("complete -c ghostty -f\ncomplete -c ghostty -s e -l help -f\n"));
        assert!(c.contains(
            "complete -c ghostty -n \"not __fish_seen_subcommand_from $commands\" -l font-family -r -f  -a \"(ghostty +list-fonts | grep '^[A-Z]')\""
        ));
        assert!(c.contains(
            "complete -c ghostty -n \"not __fish_seen_subcommand_from $commands\" -l focus-follows-mouse  -f -d \""
        ));
        assert!(c.contains(
            "complete -c ghostty -n \"not __fish_seen_subcommand_from $commands\" -l config-file -r -F"
        ));
        assert!(c.contains(
            "complete -c ghostty -n \"__fish_seen_subcommand_from +list-themes\" -l color -r -f -a \"all dark light\"\n"
        ));
        assert!(c.contains(
            "complete -c ghostty -n \"__fish_seen_subcommand_from +validate-config\" -l config-file -r -F\n"
        ));
        assert!(c.contains(
            "complete -c ghostty -n \"__fish_seen_subcommand_from +list-fonts\" -l bold -f\n"
        ));
        assert!(!c.contains("__fish_seen_subcommand_from +help\""));
        assert!(c.ends_with("\n"));
    }
}
