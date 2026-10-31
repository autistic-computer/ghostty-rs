//! Port of `src/extra/zsh.zig`.
//!
//! STATUS: ported
//!
//! Zig generates the completions at comptime; Rust generates them on first
//! use from `Config`'s fields and the CLI actions (see [`crate::extra`]).

use std::sync::LazyLock;

use super::{config_fields, FieldType, ACTIONS};

/// A zsh completions configuration that contains all the available commands
/// and options.
pub fn completions() -> &'static str {
    static COMPLETIONS: LazyLock<String> = LazyLock::new(|| {
        let mut out = String::new();
        write_zsh_completions(&mut out);
        out
    });
    &COMPLETIONS
}

const EQUALS_REQUIRED: &str = "=-:::";

fn write_zsh_completions(writer: &mut String) {
    writer.push_str(r#"#compdef ghostty

_fonts () {
  local font_list=$(ghostty +list-fonts | grep -Z '^[A-Z]')
  local fonts=(${(f)font_list})
  _describe -t fonts 'fonts' fonts
}

_themes() {
  local theme_list=$(ghostty +list-themes | sed -E 's/^(.*) \(.*$/\1/')
  local themes=(${(f)theme_list})
  _describe -t themes 'themes' themes
}
"#);

    writer.push_str("_config() {\n");
    writer.push_str("  _arguments \\\n");
    writer.push_str("    \"--help\" \\\n");
    writer.push_str("    \"--version\" \\\n");
    for field in config_fields() {
        let name = field.name;
        if name.starts_with('_') {
            continue;
        }
        writer.push_str("    \"--");
        writer.push_str(name);

        if name.starts_with("font-family") {
            writer.push_str(EQUALS_REQUIRED);
            writer.push_str("_fonts");
        } else if name == "theme" {
            writer.push_str(EQUALS_REQUIRED);
            writer.push_str("_themes");
        } else if name == "working-directory" {
            writer.push_str(EQUALS_REQUIRED);
            writer.push_str("{_files -/}");
        } else if field.ty == FieldType::RepeatablePath {
            writer.push_str(EQUALS_REQUIRED);
            writer.push_str("_files"); // todo check if this is needed
        } else {
            match field.ty {
                FieldType::Bool => {}
                FieldType::Enum(names) => {
                    writer.push_str(EQUALS_REQUIRED);
                    writer.push('(');
                    writer.push_str(&names.join(" "));
                    writer.push(')');
                }
                FieldType::PackedStruct(names) => {
                    writer.push_str(EQUALS_REQUIRED);
                    writer.push('(');
                    for (i, field_name) in names.iter().enumerate() {
                        if i > 0 {
                            writer.push(' ');
                        }
                        writer.push_str(field_name);
                        writer.push_str(" no-");
                        writer.push_str(field_name);
                    }
                    writer.push(')');
                }
                // Other structs (resize-overlay-duration, keybind,
                // window-padding-x ...-y, link, palette, background,
                // foreground, font-variation*, font-feature), optionals,
                // unions and everything else.
                _ => {
                    writer.push_str(EQUALS_REQUIRED);
                    writer.push_str("( )");
                }
            }
        }

        writer.push_str("\" \\\n");
    }
    writer.push_str("\n}\n\n");

    writer.push_str(r#"_ghostty() {
  typeset -A opt_args
  local context state line
  local opt=('-e' '--help' '--version')

  _arguments -C \
    '1:actions:->actions' \
    '*:: :->rest' \

  if [[ "$line[1]" == "--help" || "$line[1]" == "--version" || "$line[1]" == "-e" ]]; then
    return
  fi

  if [[ "$line[1]" == -* ]]; then
    _config
    return
  fi

  case "$state" in
    (actions)
      local actions; actions=(
"#);

    {
        // how to get 'commands'
        const PADDING: &str = "        ";
        for action in ACTIONS {
            writer.push_str(PADDING);
            writer.push_str("'+");
            writer.push_str(action.name());
            writer.push_str("'\n");
        }
    }

    writer.push_str(r#"      )
      _describe '' opt
      _describe -t action 'action' actions
    ;;
    (rest)
      if [[ "$line[2]" == "--help" ]]; then
        return
      fi

      local help=('--help')
      _describe '' help

      case $line[1] in
"#);
    {
        const PADDING: &str = "        ";
        for action in ACTIONS {
            // assumes options will never be created with only <_name> members
            if action.options.is_empty() {
                continue;
            }

            writer.push_str(&format!("{PADDING}(+{})\n", action.name()));
            writer.push_str(&format!("{PADDING}  _arguments \\\n"));
            for opt in action.options {
                if opt.name.starts_with('_') {
                    continue;
                }

                writer.push_str(&format!("{PADDING}    '--"));
                writer.push_str(opt.name);

                match opt.ty {
                    FieldType::Bool => {}
                    FieldType::Enum(names) => {
                        writer.push_str(EQUALS_REQUIRED);
                        writer.push('(');
                        writer.push_str(&names.join(" "));
                        writer.push(')');
                    }
                    FieldType::OptionalEnum(names) => {
                        writer.push_str(EQUALS_REQUIRED);
                        writer.push('(');
                        writer.push_str(&names.join(" "));
                        writer.push(')');
                    }
                    _ => {
                        writer.push_str(EQUALS_REQUIRED);
                        if opt.name == "config-file" {
                            writer.push_str("_files");
                        } else {
                            writer.push_str("( )");
                        }
                    }
                }

                writer.push_str("' \\\n");
            }
            writer.push_str(&format!("{PADDING};;\n"));
        }
    }
    writer.push_str(r#"      esac
    ;;
  esac
}

_ghostty "$@"
"#);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zsh_completions() {
        let c = completions();
        assert!(c.starts_with("#compdef ghostty\n\n_fonts () {\n"));
        assert!(c.contains("_config() {\n  _arguments \\\n    \"--help\" \\\n    \"--version\" \\\n"));
        assert!(c.contains("    \"--font-family=-:::_fonts\" \\\n"));
        assert!(c.contains("    \"--theme=-:::_themes\" \\\n"));
        assert!(c.contains("    \"--working-directory=-:::{_files -/}\" \\\n"));
        assert!(c.contains("    \"--config-file=-:::_files\" \\\n"));
        assert!(c.contains("    \"--focus-follows-mouse\" \\\n"));
        assert!(c.contains("    \"--command=-:::( )\" \\\n"));
        assert!(c.contains(
            "    \"--shell-integration=-:::(none detect bash elvish fish nushell zsh)\" \\\n"
        ));
        assert!(c.contains("        '+list-themes'\n"));
        assert!(c.contains(
            "        (+list-themes)\n          _arguments \\\n            '--path' \\\n            '--plain' \\\n            '--color=-:::(all dark light)' \\\n        ;;\n"
        ));
        assert!(c.contains("            '--config-file=-:::_files' \\\n"));
        assert!(c.contains("            '--presentation=-:::(text emoji)' \\\n"));
        assert!(!c.contains("(+version)"));
        assert!(c.ends_with("_ghostty \"$@\"\n"));
    }
}
