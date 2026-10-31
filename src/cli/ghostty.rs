//! Port of `src/cli/ghostty.zig`.
//!
//! STATUS: ported
//!
//! Zig's build-generated `help_strings.Action` (the doc comment of each
//! action's `run` function) is each action module's `HELP` constant.

use std::io::Write;

use super::action::{self as actionpkg, SpecialCase};
use super::args::{self, cli_enum, CliEnum, FieldInfo, Fields, ProcessArgs};
use crate::global;

use super::{
    boo, crash_report, edit_config, explain_config, help, list_actions, list_colors, list_fonts,
    list_keybinds, list_themes, new_tab, new_window, show_config, show_face, ssh, ssh_cache,
    toggle_quick_terminal, validate_config, version,
};

/// The error type of action `run` functions (Zig: inferred error sets).
pub type RunError = Box<dyn std::error::Error + Send + Sync>;

/// The result of running an action: the exit code.
pub type RunResult = Result<u8, RunError>;

cli_enum! {
    /// Special commands that can be invoked via CLI flags. These are all
    /// invoked by using `+<action>` as a CLI flag. The only exception is
    /// "version" which can be invoked additionally with `--version`.
    pub enum Action {
        /// Output the version and exit
        Version => "version",

        /// Output help information for the CLI or configuration
        Help => "help",

        /// List available fonts
        ListFonts => "list-fonts",

        /// List available keybinds
        ListKeybinds => "list-keybinds",

        /// List available themes
        ListThemes => "list-themes",

        /// List named RGB colors
        ListColors => "list-colors",

        /// List keybind actions
        ListActions => "list-actions",

        /// Wrap `ssh` to configure Ghostty terminal integration on remote hosts
        Ssh => "ssh",

        /// Manage SSH terminfo cache for automatic remote host setup
        SshCache => "ssh-cache",

        /// Edit the config file in the configured terminal editor.
        EditConfig => "edit-config",

        /// Dump the config to stdout
        ShowConfig => "show-config",

        /// Explain a single config option
        ExplainConfig => "explain-config",

        // Validate passed config file
        ValidateConfig => "validate-config",

        // Show which font face Ghostty loads a codepoint from.
        ShowFace => "show-face",

        // List, (eventually) view, and (eventually) send crash reports.
        CrashReport => "crash-report",

        // Boo!
        Boo => "boo",

        // Use IPC to tell the running Ghostty to open a new window.
        NewWindow => "new-window",

        // Use IPC to tell the running Ghostty to open a new tab.
        NewTab => "new-tab",

        // Use IPC to tell the running Ghostty to toggle the quick terminal.
        ToggleQuickTerminal => "toggle-quick-terminal",
    }
}

impl actionpkg::Action for Action {
    fn detect_special_case(arg: &str) -> Option<SpecialCase<Action>> {
        // If we see a "-e" and we haven't seen a command yet, then
        // we are done looking for commands. This special case enables
        // `ghostty -e ghostty +command`. If we've seen a command we
        // still want to keep looking because
        // `ghostty +command -e +command` is invalid.
        if arg == "-e" {
            return Some(SpecialCase::AbortIfNoAction);
        }

        // Special case, --version always outputs the version no
        // matter what, no matter what other args exist.
        if arg == "--version" {
            return Some(SpecialCase::Action(Action::Version));
        }

        // --help matches "help" but if a subcommand is specified
        // then we match the subcommand.
        if arg == "--help" || arg == "-h" {
            return Some(SpecialCase::Fallback(Action::Help));
        }

        None
    }
}

/// The process arguments (including argv0) from the global state (Zig
/// `global.args()`), converted lossily to UTF-8.
pub fn global_args() -> ProcessArgs {
    ProcessArgs::new(
        global::args()
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned()),
    )
}

/// Returns true if `err` is [`Action::HELP_ERROR`].
pub fn is_help_error(err: &RunError) -> bool {
    err.downcast_ref::<args::Error>() == Some(&Action::HELP_ERROR)
}

impl Action {
    /// This should be returned by actions that want to print the help text.
    pub const HELP_ERROR: args::Error = args::Error::Other("ActionHelpRequested");

    /// The `help` decl result for action `Options` structs: `-h`/`--help`
    /// returns [`Action::HELP_ERROR`].
    pub fn help_result() -> Option<Result<(), args::Error>> {
        Some(Err(Action::HELP_ERROR))
    }

    /// Run the action. This returns the exit code to exit with.
    pub fn run(self) -> RunResult {
        match self.run_main() {
            // If help is requested, then we find this action in the help
            // strings and output that.
            Err(err) if is_help_error(&err) => {
                // Future note: for now we just output the help text directly
                // to stdout. In the future we can style this much prettier
                // for all commands by just changing this one place.
                let text = format!("{}\n", self.help_string());
                let mut stdout = std::io::stdout().lock();
                if let Err(write_err) = stdout.write_all(text.as_bytes()) {
                    log::warn!("failed to write help text: {}\n", write_err);
                    return Ok(1);
                }
                if let Err(flush_err) = stdout.flush() {
                    log::warn!("failed to flush help text: {}\n", flush_err);
                    return Ok(1);
                }
                Ok(0)
            }
            other => other,
        }
    }

    fn run_main(self) -> RunResult {
        match self {
            Action::Version => version::run(),
            Action::Help => help::run(),
            Action::ListFonts => list_fonts::run(),
            Action::ListKeybinds => list_keybinds::run(),
            Action::ListThemes => list_themes::run(),
            Action::ListColors => list_colors::run(),
            Action::ListActions => list_actions::run(),
            Action::SshCache => ssh_cache::run(),
            Action::Ssh => ssh::run(),
            Action::EditConfig => edit_config::run(),
            Action::ShowConfig => show_config::run(),
            Action::ExplainConfig => explain_config::run(),
            Action::ValidateConfig => validate_config::run(),
            Action::CrashReport => crash_report::run(),
            Action::ShowFace => show_face::run(),
            Action::Boo => boo::run(),
            Action::NewWindow => new_window::run(),
            Action::NewTab => new_tab::run(),
            Action::ToggleQuickTerminal => toggle_quick_terminal::run(),
        }
    }

    /// The help text of the action (Zig `help_strings.Action`: the doc
    /// comment of the action's `run` function).
    pub fn help_string(self) -> &'static str {
        match self {
            Action::Version => version::HELP,
            Action::Help => help::HELP,
            Action::ListFonts => list_fonts::HELP,
            Action::ListKeybinds => list_keybinds::HELP,
            Action::ListThemes => list_themes::HELP,
            Action::ListColors => list_colors::HELP,
            Action::ListActions => list_actions::HELP,
            Action::SshCache => ssh_cache::HELP,
            Action::Ssh => ssh::HELP,
            Action::EditConfig => edit_config::HELP,
            Action::ShowConfig => show_config::HELP,
            Action::ExplainConfig => explain_config::HELP,
            Action::ValidateConfig => validate_config::HELP,
            Action::CrashReport => crash_report::HELP,
            Action::ShowFace => show_face::HELP,
            Action::Boo => boo::HELP,
            Action::NewWindow => new_window::HELP,
            Action::NewTab => new_tab::HELP,
            Action::ToggleQuickTerminal => toggle_quick_terminal::HELP,
        }
    }

    /// Returns the filename associated with an action. This is a relative
    /// path from the root src/ directory (the Rust port's file).
    pub fn file(self) -> String {
        format!("cli/{}.rs", self.tag_name().replace('-', "_"))
    }

    /// Returns the options of action. Supports generating shell completions
    /// without duplicating the mapping from Action to relevant Option
    /// declaration. (Zig returns the `Options` type; Rust returns its
    /// field table.)
    pub fn options(self) -> &'static [FieldInfo] {
        match self {
            Action::Version => version::Options::FIELDS,
            Action::Help => help::Options::FIELDS,
            Action::ListFonts => list_fonts::Options::FIELDS,
            Action::ListKeybinds => list_keybinds::Options::FIELDS,
            Action::ListThemes => list_themes::Options::FIELDS,
            Action::ListColors => list_colors::Options::FIELDS,
            Action::ListActions => list_actions::Options::FIELDS,
            Action::SshCache => ssh_cache::Options::FIELDS,
            Action::Ssh => ssh::Options::FIELDS,
            Action::EditConfig => edit_config::Options::FIELDS,
            Action::ShowConfig => show_config::Options::FIELDS,
            Action::ExplainConfig => explain_config::Options::FIELDS,
            Action::ValidateConfig => validate_config::Options::FIELDS,
            Action::CrashReport => crash_report::Options::FIELDS,
            Action::ShowFace => show_face::Options::FIELDS,
            Action::Boo => boo::Options::FIELDS,
            Action::NewWindow => new_window::Options::FIELDS,
            Action::NewTab => new_tab::Options::FIELDS,
            Action::ToggleQuickTerminal => toggle_quick_terminal::Options::FIELDS,
        }
    }

    /// All actions in declaration order.
    pub fn all() -> impl Iterator<Item = Action> {
        <Action as CliEnum>::FIELD_NAMES
            .iter()
            .map(|n| Action::from_tag(n).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::action::{detect_iter, DetectError};
    use crate::cli::args::ArgIteratorGeneral;

    #[test]
    fn parse_action_none() {
        let mut iter = ArgIteratorGeneral::init("--a=42 --b --b-f=false");
        let action = detect_iter::<Action, _>(&mut iter).unwrap();
        assert!(action.is_none());
    }

    #[test]
    fn parse_action_version() {
        for line in [
            "--a=42 --b --b-f=false --version",
            "--version --a=42 --b --b-f=false",
            "--c=84 --d --version --a=42 --b --b-f=false",
        ] {
            let mut iter = ArgIteratorGeneral::init(line);
            let action = detect_iter::<Action, _>(&mut iter).unwrap();
            assert_eq!(Some(Action::Version), action);
        }
    }

    #[test]
    fn parse_action_plus() {
        for line in [
            "--a=42 --b --b-f=false +version",
            "+version --a=42 --b --b-f=false",
            "--c=84 --d +version --a=42 --b --b-f=false",
        ] {
            let mut iter = ArgIteratorGeneral::init(line);
            let action = detect_iter::<Action, _>(&mut iter).unwrap();
            assert_eq!(Some(Action::Version), action);
        }
    }

    #[test]
    fn parse_action_plus_ignores_e() {
        {
            let mut iter = ArgIteratorGeneral::init("--a=42 -e +version");
            let action = detect_iter::<Action, _>(&mut iter).unwrap();
            assert!(action.is_none());
        }

        {
            let mut iter = ArgIteratorGeneral::init("+list-fonts --a=42 -e +version");
            assert_eq!(
                Err(DetectError::MultipleActions),
                detect_iter::<Action, _>(&mut iter)
            );
        }
    }

    #[test]
    fn action_file_and_help() {
        assert_eq!("cli/list_fonts.rs", Action::ListFonts.file());
        for action in Action::all() {
            assert!(!action.help_string().is_empty(), "{:?}", action);
            let _ = action.options();
        }
    }
}
