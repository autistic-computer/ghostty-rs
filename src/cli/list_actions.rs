//! Port of `src/cli/list_actions.zig`.
//!
//! STATUS: ported

use std::io::{self, BufWriter, Write};

use super::args::{self, cli_struct, Error, ParseHooks};
use super::ghostty::{global_args, Action, RunResult};
use crate::input::helpgen_actions;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// If `true`, print out documentation about the action associated with the
        /// keybinds.
        pub docs: bool = false,
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    /// Enables `-h` and `--help` to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig `help_strings.Action.@"list-actions"`).
pub const HELP: &str = "The `list-actions` command is used to list all the available keybind
actions for Ghostty. These are distinct from the CLI Actions which can
be listed via `+help`

Flags:

  * `--docs`: will print out the documentation for each action.";

/// The `list-actions` command is used to list all the available keybind
/// actions for Ghostty. These are distinct from the CLI Actions which can
/// be listed via `+help`
///
/// Flags:
///
///   * `--docs`: will print out the documentation for each action.
pub fn run() -> RunResult {
    let mut opts = Options::default();

    {
        let mut iter = args::args_iterator(global_args());
        args::parse(&mut opts, &mut iter)?;
    }

    let mut stdout = BufWriter::with_capacity(4096, io::stdout().lock());
    helpgen_actions::generate(&mut stdout, helpgen_actions::Format::Plaintext, opts.docs)?;
    stdout.flush()?;

    Ok(0)
}
