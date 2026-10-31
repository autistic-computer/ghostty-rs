//! Port of `src/main_build_data.zig`.
//!
//! STATUS: ported
//!
//! This CLI is used to generate data that is used by the build process.
//!
//! We used to do this directly in our `build.zig` but the problem with
//! that approach is that any changes to the dependencies of this data would
//! force a rebuild of our build binary. If we're just doing something like
//! running tests and not emitting any of the info below, then that is a
//! complete waste.

use std::fmt;
use std::io::{self, Write};

use crate::cli::action::{self as cli_action, DetectError};
use crate::cli::args::{cli_enum, ProcessArgs};
use crate::extra;

cli_enum! {
    pub enum Action {
        // Shell completions
        Bash => "bash",
        Fish => "fish",
        Zsh => "zsh",

        // Editor syntax files
        Sublime => "sublime",
        VimSyntax => "vim-syntax",
        VimFtdetect => "vim-ftdetect",
        VimFtplugin => "vim-ftplugin",
        VimCompiler => "vim-compiler",

        // Other
        Terminfo => "terminfo",
    }
}

impl cli_action::Action for Action {}

#[derive(Debug)]
pub enum Error {
    NoAction,
    Detect(DetectError),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoAction => f.write_str("NoAction"),
            Error::Detect(e) => write!(f, "{e}"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<DetectError> for Error {
    fn from(e: DetectError) -> Self {
        Error::Detect(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub fn main() -> Result<(), Error> {
    let action_ = cli_action::detect_args::<Action>(ProcessArgs::current())?;
    let action = action_.ok_or(Error::NoAction)?;

    // Our output always goes to stdout.
    let stdout = io::stdout();
    let mut writer = io::BufWriter::with_capacity(1024, stdout.lock());
    write_action(&mut writer, action)?;
    writer.flush()?;
    Ok(())
}

/// Write the data for the given action.
pub fn write_action<W: Write + ?Sized>(writer: &mut W, action: Action) -> io::Result<()> {
    match action {
        Action::Bash => writer.write_all(extra::bash::completions().as_bytes()),
        Action::Fish => writer.write_all(extra::fish::completions().as_bytes()),
        Action::Zsh => writer.write_all(extra::zsh::completions().as_bytes()),
        Action::Sublime => writer.write_all(extra::sublime::syntax().as_bytes()),
        Action::VimSyntax => writer.write_all(extra::vim::syntax().as_bytes()),
        Action::VimFtdetect => writer.write_all(extra::vim::FTDETECT.as_bytes()),
        Action::VimFtplugin => writer.write_all(extra::vim::FTPLUGIN.as_bytes()),
        Action::VimCompiler => writer.write_all(extra::vim::COMPILER.as_bytes()),
        Action::Terminfo => crate::terminfo::ghostty::GHOSTTY.encode(writer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::CliEnum;

    #[test]
    fn all_actions_write() {
        for name in <Action as CliEnum>::FIELD_NAMES {
            let action = <Action as CliEnum>::from_tag(name).unwrap();
            let mut out = Vec::new();
            write_action(&mut out, action).unwrap();
            assert!(!out.is_empty(), "{name}");
        }
    }

    #[test]
    fn detect_action() {
        let args = ProcessArgs::new(["ghostty-build-data".to_string(), "+vim-syntax".to_string()]);
        assert_eq!(
            Some(Action::VimSyntax),
            cli_action::detect_args::<Action>(args).unwrap()
        );
    }
}
