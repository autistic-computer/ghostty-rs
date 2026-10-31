//! Port of `src/cli/edit_config.zig`.
//!
//! STATUS: ported

use std::io::{self, BufWriter, Write};

use super::args::{self, cli_struct, Error, ParseHooks};
use super::ghostty::{global_args, Action, RunResult};
use crate::config::{self as configpkg, Config};
use crate::os as internal_os;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {}
    with { custom_hooks }
}

impl ParseHooks for Options {
    /// Enables `-h` and `--help` to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig `help_strings.Action.@"edit-config"`).
pub const HELP: &str = "The `edit-config` command opens the Ghostty configuration file in the
editor specified by the `$VISUAL` or `$EDITOR` environment variables.

IMPORTANT: This command will not reload the configuration after
editing. You will need to manually reload the configuration using the
application menu, configured keybind, or by restarting Ghostty. We
plan to auto-reload in the future, but Ghostty isn't capable of
this yet.

The filepath opened is the default user-specific configuration
file, which is typically located at `$XDG_CONFIG_HOME/ghostty/config.ghostty`.
On macOS, this may also be located at
`~/Library/Application Support/com.mitchellh.ghostty/config.ghostty`.
On macOS, whichever path exists and is non-empty will be prioritized,
prioritizing the Application Support directory if neither are
non-empty.

This command prefers the `$VISUAL` environment variable over `$EDITOR`,
if both are set. If neither are set, it will print an error
and exit.";

/// The `edit-config` command opens the Ghostty configuration file in the
/// editor specified by the `$VISUAL` or `$EDITOR` environment variables.
/// See [`HELP`].
pub fn run() -> RunResult {
    // Implementation note (by @mitchellh): I do proper memory cleanup
    // throughout this command, even though we plan on doing `exec`.
    // I do this out of good hygiene in case we ever change this to
    // not using `exec` anymore and because this command isn't performance
    // critical where setting up the defer cleanup is a problem.

    let mut stderr = BufWriter::with_capacity(1024, io::stderr().lock());

    let mut opts = Options::default();

    {
        let mut iter = args::args_iterator(global_args());
        args::parse(&mut opts, &mut iter)?;
    }

    let result = run_inner(&mut stderr);
    // Flushing *shouldn't* fail but...
    let _ = stderr.flush();
    result
}

fn run_inner<W: Write + ?Sized>(stderr: &mut W) -> RunResult {
    // We load the configuration once because that will write our
    // default configuration files to disk. We don't use the config.
    let _config = Config::load()?;

    // Find the preferred path.
    let path = configpkg::preferred_default_file_path()?;

    // We don't currently support Windows because we use the exec syscall.
    if cfg!(windows) {
        write!(
            stderr,
            "The `ghostty +edit-config` command is not supported on Windows.
Please edit the configuration file manually at the following path:

"
        )?;
        return Ok(1);
    }

    let command = match internal_os::get_config_edit_command(
        &path,
        internal_os::edit::Options {
            default_editor: internal_os::edit::DefaultEditor::Failure,
        },
    ) {
        Ok(command) => command,
        Err(internal_os::edit::Error::NoEditorConfigured) => {
            write!(
                stderr,
                "The $EDITOR or $VISUAL environment variable is not set or is empty.
This environment variable is required to edit the Ghostty configuration
via this CLI command.

Please set the environment variable to your preferred terminal
text editor and try again.

If you prefer to edit the configuration file another way,
you can find the configuration file at the following path:

"
            )?;

            // Output the path using the OSC8 sequence so that it is linked.
            writeln!(stderr, "\x1b]8;;file://{}\x1b\\{}\x1b]8;;\x1b\\", path, path)?;

            return Ok(1);
        }
    };

    // Run/replace process (using execve)
    let err = exec_shell(&command);

    // If we reached this point then exec failed.
    write!(
        stderr,
        "Failed to execute the editor (E{}).

This is usually due to the executable path not existing, invalid
permissions, or the shell environment not being set up
correctly.

Command: {}
",
        errno_tag(&err),
        command
    )?;
    // Output the path using the OSC8 sequence so that it is linked.
    writeln!(stderr, "Path: \x1b]8;;file://{}\x1b\\{}\x1b]8;;\x1b\\", path, path)?;
    Ok(1)
}

/// Replace the process with `/bin/sh -c <command>` (inheriting the
/// environment). Only returns on failure.
#[cfg(unix)]
fn exec_shell(command: &str) -> io::Error {
    use std::os::unix::process::CommandExt;
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .exec()
}

#[cfg(not(unix))]
fn exec_shell(_command: &str) -> io::Error {
    io::Error::from(io::ErrorKind::Unsupported)
}

/// The Zig `posix.E` tag name (without the leading `E`) of an exec error.
fn errno_tag(err: &io::Error) -> String {
    let Some(code) = err.raw_os_error() else {
        return "IO".to_owned();
    };
    let name = match code {
        libc::ENOENT => "NOENT",
        libc::EACCES => "ACCES",
        libc::EPERM => "PERM",
        libc::E2BIG => "2BIG",
        libc::ENOEXEC => "NOEXEC",
        libc::ENOMEM => "NOMEM",
        libc::ENOTDIR => "NOTDIR",
        libc::ELOOP => "LOOP",
        libc::ENAMETOOLONG => "NAMETOOLONG",
        libc::ETXTBSY => "TXTBSY",
        libc::EIO => "IO",
        libc::EINVAL => "INVAL",
        libc::EFAULT => "FAULT",
        libc::EMFILE => "MFILE",
        libc::ENFILE => "NFILE",
        libc::EISDIR => "ISDIR",
        _ => return code.to_string(),
    };
    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errno_tags() {
        assert_eq!("NOENT", errno_tag(&io::Error::from_raw_os_error(libc::ENOENT)));
        assert_eq!("ACCES", errno_tag(&io::Error::from_raw_os_error(libc::EACCES)));
    }
}
