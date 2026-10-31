//! Port of `src/cli/help.zig`.
//!
//! STATUS: ported

use std::io::{self, BufWriter, Write};

use super::args::{self, cli_struct};
use super::ghostty::{global_args, Action, RunResult};

cli_struct! {
    // Note that this options struct doesn't implement the `help` decl like other
    // actions. That is because the help command is special and wants to handle its
    // own logic around help detection.
    #[derive(Debug, Clone)]
    pub struct Options {
        /// This must be registered so that it isn't an error to pass `--help`
        pub help: bool = false,
    }
}

/// The help text of this action (Zig `help_strings.Action.help`).
pub const HELP: &str = "The `help` command shows general help about Ghostty. Recognized as either
`-h`, `--help`, or like other actions `+help`.

You can also specify `--help` or `-h` along with any action such as
`+list-themes` to see help for a specific action.";

/// The `help` command shows general help about Ghostty. Recognized as either
/// `-h`, `--help`, or like other actions `+help`.
///
/// You can also specify `--help` or `-h` along with any action such as
/// `+list-themes` to see help for a specific action.
pub fn run() -> RunResult {
    let mut opts = Options::default();

    {
        let mut iter = args::args_iterator(global_args());
        args::parse(&mut opts, &mut iter)?;
    }

    let mut stdout = BufWriter::with_capacity(2048, io::stdout().lock());
    stdout.write_all(
        b"Usage: ghostty [+action] [options]

Run the Ghostty terminal emulator or a specific helper action.

If no `+action` is specified, run the Ghostty terminal emulator.
All configuration keys are available as command line options.
To specify a configuration key, use the `--<key>=<value>` syntax
where key and value are the same format you'd put into a configuration
file. For example, `--font-size=12` or `--font-family=\"Fira Code\"`.

To see a list of all available configuration options, please see
the `src/config/Config.zig` file. A future update will allow seeing
the list of configuration options from the command line.

A special command line argument `-e <command>` can be used to run
the specific command inside the terminal emulator. For example,
`ghostty -e top` will run the `top` command inside the terminal.

On macOS, launching the terminal emulator from the CLI is not
supported and only actions are supported. Use `open -na Ghostty.app`
instead, or `open -na ghostty.app --args --foo=bar --baz=quz` to pass
arguments.

Available actions:

",
    )?;

    for action in Action::all() {
        writeln!(stdout, "  +{}", args::CliEnum::tag_name(action))?;
    }

    stdout.write_all(
        b"
Specify `+<action> --help` to see the help for a specific action,
where `<action>` is one of actions listed above.
",
    )?;
    stdout.flush()?;

    Ok(0)
}
