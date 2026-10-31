//! Port of `src/synthetic/cli.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "cli/ascii.rs"]
pub mod ascii;
#[path = "cli/kitty.rs"]
pub mod kitty;
#[path = "cli/osc.rs"]
pub mod osc;
#[path = "cli/styled.rs"]
pub mod styled;
#[path = "cli/utf8.rs"]
pub mod utf8;
// @generated-mods-end

use std::fmt;
use std::io::{self, Write as _};

use super::{DefaultPrng, Random};
use crate::cli::action::{self as cli_action, DetectError};
use crate::cli::args::{self as cli_args, cli_enum, ArgIteratorGeneral, ParseHooks, ProcessArgs};

cli_enum! {
    /// The available actions for the CLI. This is the list of available
    /// synthetic generators. View docs for each individual one in the
    /// predictably named files under `cli/`.
    pub enum Action {
        Ascii => "ascii",
        Kitty => "kitty",
        Osc => "osc",
        Styled => "styled",
        Utf8 => "utf8",
    }
}

impl cli_action::Action for Action {}

/// Errors from the synthetic CLI (Zig's inferred error sets merged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No action was given on the command line.
    NoAction,
    /// Action detection failed.
    Detect(DetectError),
    /// Parsing the action's options failed.
    Args(cli_args::Error),
    /// An option had an invalid value.
    InvalidValue,
    /// Writing failed.
    WriteFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoAction => f.write_str("NoAction"),
            Error::Detect(e) => write!(f, "{e}"),
            Error::Args(e) => write!(f, "{e}"),
            Error::InvalidValue => f.write_str("InvalidValue"),
            Error::WriteFailed => f.write_str("WriteFailed"),
        }
    }
}

impl std::error::Error for Error {}

impl From<DetectError> for Error {
    fn from(e: DetectError) -> Self {
        Error::Detect(e)
    }
}

impl From<cli_args::Error> for Error {
    fn from(e: cli_args::Error) -> Self {
        Error::Args(e)
    }
}

impl From<super::generator::Error> for Error {
    fn from(_: super::generator::Error) -> Self {
        Error::WriteFailed
    }
}

impl From<io::Error> for Error {
    fn from(_: io::Error) -> Self {
        Error::WriteFailed
    }
}

/// The struct associated with an action (Zig `Action.Struct(action)`).
/// It provides:
///
///   - `Options`: The CLI options for the action.
///   - `create`: Create a new instance of the action from options.
///   - `run`: Write generated data to the writer until it fails.
///
/// `destroy` is `Drop`.
pub trait ActionImpl: Sized {
    type Options: ParseHooks + Default;

    fn create(opts: Self::Options) -> Result<Box<Self>, Error>;

    fn run(&mut self, writer: &mut dyn io::Write, rand: Random) -> Result<(), Error>;
}

/// An entrypoint for the synthetic generator CLI.
pub fn main() -> Result<(), Error> {
    let action_ = cli_action::detect_args::<Action>(ProcessArgs::current())?;
    let action = action_.ok_or(Error::NoAction)?;
    main_action(action, Args::Cli(ProcessArgs::current()))
}

#[derive(Debug, Clone)]
pub enum Args {
    /// The arguments passed to the CLI via argc/argv (including argv0).
    Cli(ProcessArgs),

    /// Simple string arguments, parsed via ArgIteratorGeneral.
    String(String),
}

pub fn main_action(action: Action, args: Args) -> Result<(), Error> {
    match action {
        Action::Ascii => main_action_impl::<ascii::Ascii>(args),
        Action::Kitty => main_action_impl::<kitty::Kitty>(args),
        Action::Osc => main_action_impl::<osc::Osc>(args),
        Action::Styled => main_action_impl::<styled::Styled>(args),
        Action::Utf8 => main_action_impl::<utf8::Utf8>(args),
    }
}

fn main_action_impl<Impl: ActionImpl>(args: Args) -> Result<(), Error> {
    // First, parse our CLI options.
    let mut opts = Impl::Options::default();
    match args {
        Args::Cli(process_args) => {
            let mut iter = cli_args::args_iterator(process_args);
            cli_args::parse(&mut opts, &mut iter)?;
        }
        Args::String(str) => {
            let mut iter = ArgIteratorGeneral::init(&str);
            cli_args::parse(&mut opts, &mut iter)?;
        }
    }

    // TODO: Make this a command line option.
    let seed: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let prng = DefaultPrng::init(seed);
    let rand = prng.random();

    // Our output always goes to stdout.
    let stdout = io::stdout();
    let mut writer = io::BufWriter::with_capacity(2048, stdout.lock());

    // Create our implementation
    let mut impl_ = Impl::create(opts)?;
    impl_.run(&mut writer, rand)?;

    // Always flush
    let _ = writer.flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::CliEnum;

    #[test]
    fn actions() {
        // Make sure we ref all our actions
        for name in <Action as CliEnum>::FIELD_NAMES {
            assert!(<Action as CliEnum>::from_tag(name).is_some());
        }
    }
}
