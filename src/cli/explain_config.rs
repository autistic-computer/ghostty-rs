//! Port of `src/cli/explain_config.zig`.
//!
//! STATUS: ported
//!
//! Zig's `help_strings.Config` is [`FieldInfo::help`](super::args::FieldInfo::help)
//! of `Config::FIELDS`; `help_strings.KeybindAction` is
//! `input::helpgen_actions::help_strings::keybind_action`.

use std::io::{self, Write};

use super::args::{self, cli_struct, ArgIterator, CliEnum, Error, Fields, ParseHooks};
use super::ghostty::{global_args, Action, RunResult};
use super::pager::Pager;
use crate::config::key::Key as ConfigKey;
use crate::config::Config;
use crate::input::binding::ActionKey as KeybindAction;
use crate::input::helpgen_actions::help_strings;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// The config option to explain. For example:
        ///
        ///   ghostty +explain-config --option=font-size
        pub option: Option<String> = None,

        /// The keybind action to explain. For example:
        ///
        ///   ghostty +explain-config --keybind=copy_to_clipboard
        pub keybind: Option<String> = None,
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    /// Enables `-h` and `--help` to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig `help_strings.Action.@"explain-config"`).
pub const HELP: &str = "The `explain-config` command prints the documentation for a single
Ghostty configuration option or keybind action.

Examples:

  ghostty +explain-config font-size
  ghostty +explain-config copy_to_clipboard
  ghostty +explain-config --option=font-size
  ghostty +explain-config --keybind=copy_to_clipboard

Flags:

  * `--option`: The name of the configuration option to explain.
  * `--keybind`: The name of the keybind action to explain.
  * `--no-pager`: Disable automatic paging of output.";

/// The `explain-config` command prints the documentation for a single
/// Ghostty configuration option or keybind action. See [`HELP`].
pub fn run() -> RunResult {
    let mut option_name: Option<String> = None;
    let mut keybind_name: Option<String> = None;
    let mut positional: Option<String> = None;
    let mut no_pager = false;

    let mut iter = args::args_iterator(global_args());
    while let Some(arg) = iter.next() {
        if let Some(rest) = arg.strip_prefix("--option=") {
            option_name = Some(rest.to_owned());
        } else if let Some(rest) = arg.strip_prefix("--keybind=") {
            keybind_name = Some(rest.to_owned());
        } else if arg == "--no-pager" {
            no_pager = true;
        } else if arg == "--help" || arg == "-h" {
            return Err(Box::new(Action::HELP_ERROR));
        } else if !arg.starts_with('-') {
            positional = Some(arg);
        }
    }

    // Resolve what to look up. Explicit flags go directly to their
    // respective lookup. A bare positional argument tries config
    // options first, then keybind actions as a fallback.
    let Some(name) = keybind_name
        .as_deref()
        .or(option_name.as_deref())
        .or(positional.as_deref())
    else {
        let mut stderr = io::stderr().lock();
        stderr.write_all(b"Usage: ghostty +explain-config <option>\n")?;
        stderr.write_all(b"       ghostty +explain-config --option=<option>\n")?;
        stderr.write_all(b"       ghostty +explain-config --keybind=<action>\n")?;
        stderr.flush()?;
        return Ok(1);
    };

    let text = if keybind_name.is_some() {
        explain_keybind(name).map(str::to_owned)
    } else if option_name.is_some() {
        explain_option(name)
    } else {
        explain_option(name).or_else(|| explain_keybind(name).map(str::to_owned))
    };

    let mut pager = if !no_pager { Pager::init() } else { Pager::default() };
    let writer = pager.writer();

    if let Some(t) = text {
        writer.write_all(t.as_bytes())?;
        writer.write_all(b"\n")?;
    } else {
        writer.write_all(b"Unknown: '")?;
        writer.write_all(name.as_bytes())?;
        writer.write_all(b"'.\n")?;
        writer.flush()?;
        return Ok(1);
    }

    writer.flush()?;
    Ok(0)
}

fn explain_option(name: &str) -> Option<String> {
    let key = ConfigKey::from_tag(name)?;
    let field = &Config::FIELDS[key.index()];
    if field.has_help() {
        Some(field.help())
    } else {
        None
    }
}

fn explain_keybind(name: &str) -> Option<&'static str> {
    let tag = KeybindAction::from_tag_name(name.as_bytes())?;
    help_strings::keybind_action(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain() {
        // Config options
        assert!(explain_option("font-size").is_some());
        assert!(explain_option("copy_to_clipboard").is_none());
        assert!(explain_option("unknown-option").is_none());

        // Keybind actions
        assert!(explain_keybind("copy_to_clipboard").is_some());
        assert!(explain_keybind("font-size").is_none());
        assert!(explain_keybind("unknown_keybind").is_none());
    }
}
