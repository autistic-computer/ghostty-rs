//! Port of `src/os/edit.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::fmt::Write as _;

use super::shell::ShellEscapeWriter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DefaultEditor {
    /// Return an error if $EDITOR or $VISUAL is not defined.
    Failure,
    /// Default to "vi" if $EDITOR or $VISUAL is not defined.
    #[default]
    Vi,
    /// Default to "nano" if $EDITOR or $VISUAL is not defined.
    Nano,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    pub default_editor: DefaultEditor,
}

impl Options {
    pub const DEFAULT: Options = Options {
        default_editor: DefaultEditor::Vi,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NoEditorConfigured,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NoEditorConfigured")
    }
}

impl std::error::Error for Error {}

/// Returns the value of `name` if it is set and non-empty. Values that are
/// not valid UTF-8 are treated as unset (Zig: the `getAlloc` error case).
fn env_unempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Build the command used to edit the config file at `path`: the user's
/// editor followed by the shell-escaped path.
pub fn get_config_edit_command(path: &str, options: Options) -> Result<String, Error> {
    // VISUAL vs. EDITOR: https://unix.stackexchange.com/questions/4859/visual-vs-editor-what-s-the-difference
    let editor = match env_unempty("VISUAL").or_else(|| env_unempty("EDITOR")) {
        Some(editor) => editor,
        None => match options.default_editor {
            DefaultEditor::Failure => {
                log::warn!(target: "os_edit", "$EDITOR or $VISUAL must be set to open config in a new window");
                return Err(Error::NoEditorConfigured);
            }
            DefaultEditor::Vi => {
                log::warn!(target: "os_edit", "$EDITOR or $VISUAL not set, falling back to vi");
                "vi".to_string()
            }
            DefaultEditor::Nano => {
                log::warn!(target: "os_edit", "$EDITOR or $VISUAL not set, falling back to nano");
                "nano".to_string()
            }
        },
    };

    let mut buffer = String::new();
    buffer.push_str(&editor);
    buffer.push(' ');
    {
        let mut sh = ShellEscapeWriter::init(&mut buffer);
        // Writing to a String cannot fail.
        sh.write_str(path).unwrap();
    }

    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_edit_command_escapes_path() {
        let cmd = get_config_edit_command("/a b/config", Options::DEFAULT).unwrap();
        assert!(cmd.ends_with(" /a\\ b/config"));
        let editor = cmd.strip_suffix(" /a\\ b/config").unwrap();
        let expected = env_unempty("VISUAL")
            .or_else(|| env_unempty("EDITOR"))
            .unwrap_or_else(|| "vi".to_string());
        assert_eq!(expected, editor);

        if env_unempty("VISUAL").is_none() && env_unempty("EDITOR").is_none() {
            assert_eq!(
                Err(Error::NoEditorConfigured),
                get_config_edit_command(
                    "x",
                    Options {
                        default_editor: DefaultEditor::Failure
                    }
                )
            );
            assert_eq!(
                "nano x",
                get_config_edit_command(
                    "x",
                    Options {
                        default_editor: DefaultEditor::Nano
                    }
                )
                .unwrap()
            );
        }
    }
}
