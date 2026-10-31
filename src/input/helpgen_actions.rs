//! Port of `src/input/helpgen_actions.zig`.
//!
//! STATUS: ported
//!
//! This module is a help generator for keybind actions documentation.
//! It can generate documentation in different formats (plaintext for CLI,
//! markdown for website) while maintaining consistent content.
//!
//! Zig gets the action doc comments from the build-generated `help_strings`
//! module (`src/helpgen.zig` parses `Binding.zig` and extracts the `///`
//! comments of each `Action` field). The Rust port needs no build step: the
//! `define_actions!` macro in `binding.rs` captures each variant's doc
//! attributes at compile time ([`ActionKey::doc_lines`]), so the docs can
//! never get out of sync with the enum. [`help_strings::keybind_action`]
//! turns those lines into the same string `helpgen.zig` produces (common
//! leading-space prefix removed, lines joined with `\n`).

use std::io::{self, Write};

use super::binding::ActionKey;

/// Equivalent of the build-generated `help_strings` module, for keybind
/// actions (`help_strings.KeybindAction`).
pub mod help_strings {
    use std::sync::LazyLock;

    use super::ActionKey;

    /// `@field(help_strings.KeybindAction, @tagName(key))`, or `None` if
    /// the action has no doc comment (`!@hasDecl(...)`).
    pub fn keybind_action(key: ActionKey) -> Option<&'static str> {
        static HELP: LazyLock<Vec<Option<String>>> = LazyLock::new(|| {
            ActionKey::ALL
                .iter()
                .map(|k| extract_doc_comments(k.doc_lines()))
                .collect()
        });
        HELP[key as usize].as_deref()
    }

    /// `helpgen.zig` `extractDocComments`: convert the doc comment lines
    /// (each the text after `///`) to a multiline string with the common
    /// leading-space prefix removed and no trailing newline.
    fn extract_doc_comments(lines: &[&str]) -> Option<String> {
        if lines.is_empty() {
            return None;
        }

        let prefix = find_common_prefix(lines);
        let stripped: Vec<&str> = lines
            .iter()
            .map(|line| &line[prefix.min(line.len())..])
            .collect();
        Some(stripped.join("\n"))
    }

    /// `helpgen.zig` `findCommonPrefix`
    fn find_common_prefix(lines: &[&str]) -> usize {
        let mut m = usize::MAX;
        for line in lines {
            let n = line.bytes().position(|c| c != b' ').unwrap_or(usize::MAX);
            m = m.min(n);
        }
        m
    }
}

/// Format options for generating keybind actions documentation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Plain text output with indentation
    Plaintext,
    /// Markdown formatted output
    Markdown,
}

impl Format {
    fn format_field_name<W: Write + ?Sized>(self, writer: &mut W, field_name: &str) -> io::Result<()> {
        match self {
            Format::Plaintext => {
                writer.write_all(field_name.as_bytes())?;
                writer.write_all(b":\n")
            }
            Format::Markdown => {
                writer.write_all(b"## `")?;
                writer.write_all(field_name.as_bytes())?;
                writer.write_all(b"`\n")
            }
        }
    }

    fn format_doc_line<W: Write + ?Sized>(self, writer: &mut W, line: &str) -> io::Result<()> {
        match self {
            Format::Plaintext => {
                writer.write_all(b"  ")?;
                writer.write_all(line.as_bytes())?;
                writer.write_all(b"\n")
            }
            Format::Markdown => {
                writer.write_all(line.as_bytes())?;
                writer.write_all(b"\n")
            }
        }
    }

    fn header(self) -> Option<&'static str> {
        match self {
            Format::Plaintext => None,
            Format::Markdown => Some(concat!(
                "---\n",
                "title: Keybinding Action Reference\n",
                "description: Reference of all Ghostty keybinding actions.\n",
                "editOnGithubLink: https://github.com/ghostty-org/ghostty/edit/main/src/input/Binding.zig\n",
                "---\n",
                "\n",
                "This is a reference of all Ghostty keybinding actions.\n",
                "\n",
            )),
        }
    }
}

/// Generate keybind actions documentation with the specified format
pub fn generate<W: Write + ?Sized>(writer: &mut W, format: Format, show_docs: bool) -> io::Result<()> {
    if let Some(header) = format.header() {
        writer.write_all(header.as_bytes())?;
    }

    let mut stream: Vec<u8> = Vec::new();

    for &key in ActionKey::ALL {
        let field = key.as_str();
        if field.starts_with('_') {
            continue;
        }

        let help = help_strings::keybind_action(key);

        // Write previously stored doc comment below all related actions
        if show_docs && help.is_some() {
            writer.write_all(&stream)?;
            writer.write_all(b"\n")?;
            stream.clear();
        }

        if show_docs {
            format.format_field_name(writer, field)?;
        } else {
            writer.write_all(field.as_bytes())?;
            writer.write_all(b"\n")?;
        }

        if let (true, Some(help)) = (show_docs, help) {
            let mut iter = help.split('\n').peekable();
            while let Some(s) = iter.next() {
                // If it is the last line and empty, then skip it.
                if iter.peek().is_none() && s.is_empty() {
                    continue;
                }
                format.format_doc_line(&mut stream, s)?;
            }
        }
    }

    // Write any remaining buffered documentation
    if !stream.is_empty() {
        writer.write_all(&stream)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Zig has no tests for this file; these check the port's help string
    // extraction and the generator output shape.

    #[test]
    fn help_strings_match_zig_extraction() {
        assert_eq!(
            Some(
                "Ignore this key combination.\n\
                 \n\
                 Ghostty will not process this combination nor forward it to the child\n\
                 process within the terminal, but it may still be processed by the OS or\n\
                 other applications."
            ),
            help_strings::keybind_action(ActionKey::Ignore)
        );
        assert_eq!(
            Some("Send an `ESC` sequence."),
            help_strings::keybind_action(ActionKey::Esc)
        );

        // Indented list items keep their indentation relative to the
        // common prefix.
        let help = help_strings::keybind_action(ActionKey::CopyToClipboard).unwrap();
        assert!(help.contains("\n  - `plain`\n"));
    }

    #[test]
    fn generate_plaintext_and_markdown() {
        let mut names = Vec::new();
        generate(&mut names, Format::Plaintext, false).unwrap();
        let names = String::from_utf8(names).unwrap();
        assert!(names.starts_with("ignore\nunbind\ncsi\n"));
        assert_eq!(ActionKey::ALL.len(), names.lines().count());

        let mut plain = Vec::new();
        generate(&mut plain, Format::Plaintext, true).unwrap();
        let plain = String::from_utf8(plain).unwrap();
        assert!(plain.starts_with("\nignore:\n  Ignore this key combination.\n  \n"));

        let mut md = Vec::new();
        generate(&mut md, Format::Markdown, true).unwrap();
        let md = String::from_utf8(md).unwrap();
        assert!(md.starts_with("---\ntitle: Keybinding Action Reference\n"));
        assert!(md.contains("\n## `crash`\nCrash Ghostty in the desired thread"));
    }
}
