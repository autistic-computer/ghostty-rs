//! Port of `src/extra/bash.zig`.
//!
//! STATUS: ported
//!
//! Zig generates the completions at comptime; Rust generates them on first
//! use from `Config`'s fields and the CLI actions (see [`crate::extra`]).

use std::sync::LazyLock;

use super::{config_fields, FieldType, ACTIONS};

/// A bash completions configuration that contains all the available commands
/// and options.
///
/// Notes: bash completion support for --<key>=<value> depends on setting the completion
/// system to _not_ print a space following each successful completion (see -o nospace).
/// This results leading or tailing spaces being necessary to move onto the next match.
///
/// bash completion will read = as it's own completiong word regardless of whether or not
/// it's part of an on going completion like --<key>=. Working around this requires looking
/// backward in the command line args to pretend the = is an empty string
/// see: https://www.gnu.org/software/gnuastro/manual/html_node/Bash-TAB-completion-tutorial.html
pub fn completions() -> &'static str {
    static COMPLETIONS: LazyLock<String> = LazyLock::new(|| {
        let mut out = String::new();
        write_bash_completions(&mut out);
        out
    });
    &COMPLETIONS
}

fn write_bash_completions(writer: &mut String) {
    const PAD1: &str = "  ";
    const PAD2: &str = "    ";
    const PAD3: &str = "      ";
    const PAD4: &str = "        ";
    const PAD5: &str = "          ";

    writer.push_str(r#"_ghostty() {

  # compat: mapfile -t COMPREPLY < <( "$@" )
  _compreply() {
    COMPREPLY=()
    while IFS='' read -r line; do COMPREPLY+=("$line"); done < <( "$@" )
  }

  # -o nospace requires we add back a space when a completion is finished
  # and not part of a --key= completion
  _add_spaces() {
    for idx in "${!COMPREPLY[@]}"; do
      [ -n "${COMPREPLY[idx]}" ] && COMPREPLY[idx]="${COMPREPLY[idx]} ";
    done
  }

  _fonts() {
    local IFS=$'\n'
    COMPREPLY=()
    while read -r line; do COMPREPLY+=("$line"); done < <( compgen -P '"' -S '"' -W "$($ghostty +list-fonts | grep '^[A-Z]' )" -- "$cur")
  }

  _themes() {
    local IFS=$'\n'
    COMPREPLY=()
    while read -r line; do COMPREPLY+=("$line"); done < <( compgen -P '"' -S '"' -W "$($ghostty +list-themes | sed -E 's/^(.*) \(.*$/\1/')" -- "$cur")
  }

  _files() {
    _compreply compgen -o filenames -f -- "$cur"
    for i in "${!COMPREPLY[@]}"; do
      if [[ -d "${COMPREPLY[i]}" ]]; then
        COMPREPLY[i]="${COMPREPLY[i]}/";
      fi
      if [[ -f "${COMPREPLY[i]}" ]]; then
        COMPREPLY[i]="${COMPREPLY[i]} ";
      fi
    done
  }

  _dirs() {
    _compreply compgen -o dirnames -d -- "$cur"
    for i in "${!COMPREPLY[@]}"; do
      if [[ -d "${COMPREPLY[i]}" ]]; then
        COMPREPLY[i]="${COMPREPLY[i]}/";
      fi
    done
    if [[ "${#COMPREPLY[@]}" == 0 && -d "$cur" ]]; then
      COMPREPLY=( "$cur " )
    fi
  }

  _handle_config() {
    local config="--help"
    config+=" --version"
"#);

    for field in config_fields() {
        if field.name.starts_with('_') {
            continue;
        }
        if field.ty.is_bool_or_optional_bool() {
            writer.push_str(&format!("{PAD2}config+=\" '--{} '\"\n", field.name));
        } else {
            writer.push_str(&format!("{PAD2}config+=\" --{}=\"\n", field.name));
        }
    }

    writer.push_str(r#"
    case "$prev" in
"#);

    for field in config_fields() {
        if field.name.starts_with('_') {
            continue;
        }
        writer.push_str(&format!("{PAD3}--{}) ", field.name));

        if field.name.starts_with("font-family") {
            writer.push_str("_fonts ;;");
        } else if field.name == "theme" {
            writer.push_str("_themes ;;");
        } else if field.name == "working-directory" {
            writer.push_str("_dirs ;;");
        } else if field.ty == FieldType::RepeatablePath {
            writer.push_str("_files ;;");
        } else {
            const COMPGEN_PREFIX: &str = "_compreply compgen -W \"";
            const COMPGEN_SUFFIX: &str = "\" -- \"$cur\"; _add_spaces ;;";
            match field.ty {
                FieldType::Bool => writer.push_str("return ;;"),
                FieldType::Enum(names) => {
                    writer.push_str(COMPGEN_PREFIX);
                    writer.push_str(&names.join(" "));
                    writer.push_str(COMPGEN_SUFFIX);
                }
                FieldType::PackedStruct(names) => {
                    writer.push_str(COMPGEN_PREFIX);
                    for (i, f) in names.iter().enumerate() {
                        if i > 0 {
                            writer.push(' ');
                        }
                        writer.push_str(&format!("{f} no-{f}"));
                    }
                    writer.push_str(COMPGEN_SUFFIX);
                }
                _ => writer.push_str("return ;;"),
            }
        }

        writer.push('\n');
    }

    writer.push_str(r#"      *) _compreply compgen -W "$config" -- "$cur" ;;
    esac

    return 0
  }

  _handle_actions() {
"#);

    for action in ACTIONS {
        // assumes options will never be created with only <_name> members
        if action.options.is_empty() {
            continue;
        }

        let bash_name = action.name().replace('-', "_");
        writer.push_str(&format!("{PAD2}local {bash_name}=\""));

        {
            let mut count = 0;
            for opt in action.options {
                if opt.name.starts_with('_') {
                    continue;
                }
                if count > 0 {
                    writer.push(' ');
                }
                if opt.ty.is_bool_or_optional_bool() {
                    writer.push_str(&format!("'--{} '", opt.name));
                } else {
                    writer.push_str(&format!("--{}=", opt.name));
                }
                count += 1;
            }
        }
        writer.push_str(" --help\"\n");
    }

    writer.push_str(r#"
    case "${COMP_WORDS[1]}" in
"#);

    for action in ACTIONS {
        if action.options.is_empty() {
            continue;
        }

        // bash doesn't allow variable names containing '-' so replace them
        let bash_name = action.name().replace('-', "_");

        writer.push_str(&format!("{PAD3}+{})\n", action.name()));
        writer.push_str(&format!("{PAD4}case $prev in\n"));
        for opt in action.options {
            if opt.name.starts_with('_') {
                continue;
            }

            writer.push_str(&format!("{PAD5}--{}) ", opt.name));

            const COMPGEN_PREFIX: &str = "_compreply compgen -W \"";
            const COMPGEN_SUFFIX: &str = "\" -- \"$cur\"; _add_spaces ;;";
            match opt.ty {
                FieldType::Bool => writer.push_str("return ;;"),
                FieldType::Enum(names) | FieldType::OptionalEnum(names) => {
                    writer.push_str(COMPGEN_PREFIX);
                    writer.push_str(&names.join(" "));
                    writer.push_str(COMPGEN_SUFFIX);
                }
                FieldType::OptionalBool | FieldType::Optional => {
                    if opt.name == "config-file" {
                        writer.push_str("return ;;");
                    } else {
                        writer.push_str("return;;");
                    }
                }
                _ => {
                    if opt.name == "config-file" {
                        writer.push_str("_files ;;");
                    } else {
                        writer.push_str("return;;");
                    }
                }
            }
            writer.push('\n');
        }
        writer.push_str(&format!(
            "{PAD5}*) _compreply compgen -W \"${bash_name}\" -- \"$cur\" ;;\n"
        ));
        writer.push_str(r#"        esac
      ;;
"#);
    }

    writer.push_str(r#"      *) _compreply compgen -W "--help" -- "$cur" ;;
    esac

    return 0
  }

  # begin main logic
  local topLevel="-e"
  topLevel+=" --help"
  topLevel+=" --version"
"#);

    for action in ACTIONS {
        writer.push_str(&format!("{PAD1}topLevel+=\" +{}\"\n", action.name()));
    }

    writer.push_str(r#"
  local cur=""; local prev=""; local prevWasEq=false; COMPREPLY=()
  local ghostty="$1"

  # script assumes default COMP_WORDBREAKS of roughly $' \t\n"\'><=;|&(:'
  # if = is missing this script will degrade to matching on keys only.
  # eg: --key=
  # this can be improved if needed see: https://github.com/ghostty-org/ghostty/discussions/2994

  if [ "$2" = "=" ]; then cur=""
  else                    cur="$2"
  fi

  if [ "$3" = "=" ]; then prev="${COMP_WORDS[COMP_CWORD-2]}"; prevWasEq=true;
  else                    prev="${COMP_WORDS[COMP_CWORD-1]}"
  fi

  # current completion is double quoted add a space so the cursor progresses
  if [[ "$2" == \"*\" ]]; then
    COMPREPLY=( "$cur " );
    return;
  fi

  case "$COMP_CWORD" in
    1)
      case "${COMP_WORDS[1]}" in
        -e | --help | --version) return 0 ;;
        --*) _handle_config ;;
        *) _compreply compgen -W "${topLevel}" -- "$cur"; _add_spaces ;;
      esac
      ;;
    *)
      case "$prev" in
        -e | --help | --version) return 0 ;;
        *)
          if [[ "=" != "${COMP_WORDS[COMP_CWORD]}" && $prevWasEq != true ]]; then
            # must be completing with a space after the key eg: '--<key> '
            # clear out prev so we don't run any of the key specific completions
            prev=""
          fi

          case "${COMP_WORDS[1]}" in
            --*) _handle_config ;;
            +*) _handle_actions ;;
          esac
          ;;
      esac
      ;;
  esac

  return 0
}

complete -o nospace -o bashdefault -F _ghostty ghostty
"#);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_completions() {
        let c = completions();
        assert!(c.starts_with("_ghostty() {\n\n  # compat: mapfile"));
        assert!(c.ends_with("complete -o nospace -o bashdefault -F _ghostty ghostty\n"));

        // Config keys.
        assert!(c.contains("    config+=\" --font-family=\"\n"));
        assert!(c.contains("    config+=\" '--focus-follows-mouse '\"\n"));
        assert!(c.contains("      --font-family) _fonts ;;\n"));
        assert!(c.contains("      --theme) _themes ;;\n"));
        assert!(c.contains("      --working-directory) _dirs ;;\n"));
        assert!(c.contains("      --config-file) _files ;;\n"));
        assert!(c.contains("      --focus-follows-mouse) return ;;\n"));
        assert!(c.contains(
            "      --shell-integration) _compreply compgen -W \"none detect bash elvish fish nushell zsh\" -- \"$cur\"; _add_spaces ;;\n"
        ));
        assert!(c.contains(
            "      --shell-integration-features) _compreply compgen -W \"cursor no-cursor sudo no-sudo title no-title ssh-env no-ssh-env ssh-terminfo no-ssh-terminfo path no-path\" -- \"$cur\"; _add_spaces ;;\n"
        ));

        // Actions.
        assert!(c.contains("    local list_themes=\"'--path ' '--plain ' --color= --help\"\n"));
        assert!(!c.contains("local version="));
        assert!(c.contains(
            "      +list-themes)\n        case $prev in\n          --path) return ;;\n          --plain) return ;;\n          --color) _compreply compgen -W \"all dark light\" -- \"$cur\"; _add_spaces ;;\n          *) _compreply compgen -W \"$list_themes\" -- \"$cur\" ;;\n        esac\n      ;;\n"
        ));
        assert!(c.contains("          --config-file) return ;;\n"));
        assert!(c.contains("          --presentation) _compreply compgen -W \"text emoji\" -- \"$cur\"; _add_spaces ;;\n"));
        assert!(c.contains("          --ssh) return;;\n"));
        assert!(c.contains("  topLevel+=\" +version\"\n  topLevel+=\" +help\"\n"));
    }
}
