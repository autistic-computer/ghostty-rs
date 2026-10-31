//! Port of `src/cli/toggle_quick_terminal.zig`.
//!
//! STATUS: ported

use std::io::{self, Write};

use super::args::{cli_struct, Error, ParseHooks, StringZ};
use super::ghostty::{Action, RunResult};
use crate::apprt;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// If set, connect to a custom instance of Ghostty.
        pub class: Option<StringZ> = None,
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    /// Enables "-h" and "--help" to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig
/// `help_strings.Action.@"toggle-quick-terminal"`).
pub const HELP: &str = include_str!("toggle_quick_terminal_help.txt");

/// The `+toggle-quick-terminal` command will use native platform IPC to toggle
/// the quick terminal in a running instance of Ghostty. See [`HELP`].
pub fn run() -> RunResult {
    let mut stderr = io::stderr().lock();

    // Note: like the Zig version, the options (and so `--class`) are not
    // parsed; the default instance is always targeted.
    match apprt::App::perform_ipc(&apprt::ipc::Target::Detect, &apprt::ipc::Action::ToggleQuickTerminal) {
        Ok(true) => return Ok(0),
        Ok(false) => {}
        Err(apprt::ipc::Errors::IpcFailed) => return Ok(1),
    }

    write!(stderr, "+toggle-quick-terminal is not supported on this platform.\n")?;
    Ok(1)
}
