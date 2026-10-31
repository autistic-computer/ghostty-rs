//! Port of `src/benchmark/cli.zig`.
//!
//! STATUS: ported

use std::fmt;

use super::benchmark::{self, Benchmark, RunMode};
use crate::cli::action::{self as cli_action, DetectError};
use crate::cli::args::{self as cli_args, cli_enum, ArgIteratorGeneral, ParseHooks, ProcessArgs};
use crate::global;

cli_enum! {
    /// The available actions for the CLI. This is the list of available
    /// benchmarks. View docs for each individual one in the predictably
    /// named files.
    pub enum Action {
        ApcParser => "apc-parser",
        CodepointWidth => "codepoint-width",
        GraphemeBreak => "grapheme-break",
        HyperlinkMap => "hyperlink-map",
        PageCompression => "page-compression",
        ScrollbackCompression => "scrollback-compression",
        ScreenClone => "screen-clone",
        TerminalFormatter => "terminal-formatter",
        TerminalParser => "terminal-parser",
        TerminalResize => "terminal-resize",
        TerminalSnapshot => "terminal-snapshot",
        TerminalStream => "terminal-stream",
        IsSymbol => "is-symbol",
        OscParser => "osc-parser",
    }
}

impl cli_action::Action for Action {}

/// The struct associated with an action (Zig `Action.Struct(action)`).
/// It provides:
///
///   - `Options`: The CLI options for the action.
///   - `create`: Create a new instance of the action from options.
///   - `benchmark`: Returns a `Benchmark` instance for the action.
///
/// `destroy` is `Drop`. See TerminalStream for an example.
pub trait BenchmarkImpl: Sized + 'static {
    type Options: ParseHooks + Default;
    type CreateError: std::error::Error + Send + Sync + 'static;

    fn create(opts: Self::Options) -> Result<Box<Self>, Self::CreateError>;
    fn benchmark(&mut self) -> Benchmark<'_>;
}

/// Errors from the benchmark CLI (Zig's inferred error sets merged).
#[derive(Debug)]
pub enum Error {
    /// No action was given on the command line.
    NoAction,
    /// Global initialization failed.
    Init(global::InitError),
    /// Action detection failed.
    Detect(DetectError),
    /// Parsing the action's options failed.
    Args(cli_args::Error),
    /// Creating the benchmark implementation failed.
    Create(Box<dyn std::error::Error + Send + Sync>),
    /// The benchmark itself failed.
    Benchmark(benchmark::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoAction => f.write_str("NoAction"),
            Error::Init(e) => write!(f, "{e}"),
            Error::Detect(e) => write!(f, "{e}"),
            Error::Args(e) => write!(f, "{e}"),
            Error::Create(e) => write!(f, "{e}"),
            Error::Benchmark(e) => write!(f, "{e}"),
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

impl From<benchmark::Error> for Error {
    fn from(e: benchmark::Error) -> Self {
        Error::Benchmark(e)
    }
}

/// An entrypoint for the benchmark CLI.
pub fn main() -> Result<(), Error> {
    global::init(global::InitOpts::Tool(global::Minimal::from_process())).map_err(Error::Init)?;
    let action_ = cli_action::detect_args::<Action>(ProcessArgs::current())?;
    let action = action_.ok_or(Error::NoAction)?;
    main_action(action, Args::Cli(ProcessArgs::current()))
}

/// Arguments that can be passed to the benchmark.
#[derive(Debug, Clone)]
pub enum Args {
    /// The arguments passed to the CLI via argc/argv (including argv0).
    Cli(ProcessArgs),

    /// Simple string arguments, parsed via ArgIteratorGeneral.
    String(String),
}

pub fn main_action(action: Action, args: Args) -> Result<(), Error> {
    use super::*;
    match action {
        Action::ApcParser => main_action_impl::<apc_parser::ApcParser>(args),
        Action::CodepointWidth => main_action_impl::<codepoint_width::CodepointWidth>(args),
        Action::GraphemeBreak => main_action_impl::<grapheme_break::GraphemeBreak>(args),
        Action::HyperlinkMap => main_action_impl::<hyperlink_map::HyperlinkMap>(args),
        Action::PageCompression => main_action_impl::<page_compression::PageCompression>(args),
        Action::ScrollbackCompression => {
            main_action_impl::<scrollback_compression::ScrollbackCompression>(args)
        }
        Action::ScreenClone => main_action_impl::<screen_clone::ScreenClone>(args),
        Action::TerminalFormatter => main_action_impl::<terminal_formatter::TerminalFormatter>(args),
        Action::TerminalParser => main_action_impl::<terminal_parser::TerminalParser>(args),
        Action::TerminalResize => main_action_impl::<terminal_resize::TerminalResize>(args),
        Action::TerminalSnapshot => main_action_impl::<terminal_snapshot::TerminalSnapshot>(args),
        Action::TerminalStream => main_action_impl::<terminal_stream::TerminalStream>(args),
        Action::IsSymbol => main_action_impl::<is_symbol::IsSymbol>(args),
        Action::OscParser => main_action_impl::<osc_parser::OscParser>(args),
    }
}

fn main_action_impl<Impl: BenchmarkImpl>(args: Args) -> Result<(), Error> {
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

    // Create our implementation
    let mut impl_ = Impl::create(opts).map_err(|e| Error::Create(Box::new(e)))?;

    // Initialize our benchmark
    let mut b = impl_.benchmark();
    let _ = b.run(RunMode::Once)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::CliEnum;

    #[test]
    fn actions() {
        // Every action has a tag that maps back to it.
        for name in <Action as CliEnum>::FIELD_NAMES {
            assert!(<Action as CliEnum>::from_tag(name).is_some());
        }
    }

    #[test]
    fn main_action_string_args() {
        main_action(Action::TerminalParser, Args::String(String::new())).unwrap();
        main_action(Action::CodepointWidth, Args::String("--mode=table".into())).unwrap();
        assert!(matches!(
            main_action(Action::CodepointWidth, Args::String("--nope=1".into())),
            Err(Error::Args(_))
        ));
    }
}
