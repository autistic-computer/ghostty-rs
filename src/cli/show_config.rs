//! Port of `src/cli/show_config.zig`.
//!
//! STATUS: ported

use std::io::Write;

use super::args::{self, cli_struct, Error, ParseHooks};
use super::ghostty::{global_args, Action, RunResult};
use super::pager::Pager;
use crate::config::{Config, FileFormatter};

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// If true, do not load the user configuration, only load the defaults.
        pub default: bool = false,

        /// Only show the options that have been changed from the default.
        /// This has no effect if `--default` is specified.
        pub changes_only as "changes-only": bool = true,

        /// If true print the documentation above each option as a comment,
        /// if available.
        pub docs: bool = false,

        /// Disable automatic paging of output.
        pub no_pager as "no-pager": bool = false,
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    /// Enables `-h` and `--help` to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig `help_strings.Action.@"show-config"`).
pub const HELP: &str = "The `show-config` command shows the current configuration in a valid Ghostty
configuration file format.

When executed without any arguments this will output the current
configuration that is different from the default configuration. If you're
using the default configuration this will output nothing.

If you are a new user and want to see all available options with
documentation, run `ghostty +show-config --default --docs`.

The output is not in any specific order, but the order should be consistent
between runs. The output is not guaranteed to be exactly match the input
configuration files, but it will result in the same behavior. Comments,
whitespace, and other formatting is not preserved from user configuration
files.

Flags:

  * `--default`: Show the default configuration instead of loading
    the user configuration.

  * `--changes-only`: Only show the options that have been changed
    from the default. This has no effect if `--default` is specified.

  * `--docs`: Print the documentation above each option as a comment,
    This is very noisy but is very useful to learn about available
    options, especially paired with `--default`.

  * `--no-pager`: Disable automatic paging of output.";

/// The `show-config` command shows the current configuration in a valid Ghostty
/// configuration file format. See [`HELP`].
pub fn run() -> RunResult {
    let mut opts = Options::default();

    {
        let mut iter = args::args_iterator(global_args());
        args::parse(&mut opts, &mut iter)?;
    }

    let config = if opts.default {
        Config::default()
    } else {
        Config::load()?
    };

    let configfmt = FileFormatter {
        config: &config,
        changed: !opts.default && opts.changes_only,
        docs: opts.docs,
    };

    let mut pager = if !opts.no_pager {
        Pager::init()
    } else {
        Pager::default()
    };
    let writer = pager.writer();

    let mut out = String::new();
    configfmt.format(&mut out)?;
    writer.write_all(out.as_bytes())?;
    writer.flush()?;
    Ok(0)
}
