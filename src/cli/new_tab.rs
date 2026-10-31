//! Port of `src/cli/new_tab.zig`.
//!
//! STATUS: ported

use std::ffi::CStr;
use std::io::{self, BufWriter, Write};

use super::args::{self, cli_struct, ArgIterator, Error, Fields, ParseHooks, ParseNumberError, StringZ};
use super::diagnostics::DiagnosticList;
use super::ghostty::{global_args, Action, RunResult};
use super::new_window::{self, ASCII_WHITESPACE};
use crate::apprt;
use crate::global;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// If set, open up a new tab in a custom instance of Ghostty.
        pub class: Option<StringZ> = None,

        /// The surface to target.
        pub surface_id as "surface-id": Option<u64> = None,
    }
    private {
        /// Did the user specify a `--working-directory` argument on the command line?
        pub _working_directory_seen: bool = false,

        /// All of the arguments after `+new-tab`. They will be sent to Ghosttty
        /// for processing.
        pub _arguments: Vec<StringZ> = Vec::new(),

        /// Enable arg parsing diagnostics so that we don't get an error if
        /// there is a "normal" config setting on the cli.
        pub _diagnostics: DiagnosticList = DiagnosticList::default(),
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    fn diagnostics_mut(&mut self) -> Option<&mut DiagnosticList> {
        Some(&mut self._diagnostics)
    }

    /// Enables "-h" and "--help" to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }

    /// Manual parse hook, collect all of the arguments after `+new-tab`.
    fn parse_manually_hook<I: ArgIterator + ?Sized>(&mut self, arg: &str, iter: &mut I) -> Result<bool, Error> {
        let mut e_seen = arg == "-e";

        // Include the argument that triggered the manual parse hook.
        if let Some(a) = self.check_arg(arg)? {
            self._arguments.push(a);
        }

        // Gather up the rest of the arguments to use as the command.
        while let Some(param) = iter.next() {
            if e_seen {
                self._arguments.push(StringZ::from(param.as_str()));
                continue;
            }
            if param == "-e" {
                e_seen = true;
                self._arguments.push(StringZ::from(param.as_str()));
                continue;
            }
            if let Some(a) = self.check_arg(&param)? {
                self._arguments.push(a);
            }
        }

        Ok(false)
    }
}

/// Zig `std.fmt.parseUnsigned(u64, s, 0)`.
fn parse_unsigned(s: &str) -> Result<u64, ParseNumberError> {
    if s.starts_with(['+', '-']) {
        return Err(ParseNumberError::InvalidCharacter);
    }
    args::parse_int::<u64>(s, 0)
}

fn parse_number_error(err: ParseNumberError) -> Error {
    match err {
        ParseNumberError::InvalidCharacter => Error::Other("InvalidCharacter"),
        ParseNumberError::Overflow => Error::Other("Overflow"),
    }
}

impl Options {
    fn check_arg(&mut self, arg: &str) -> Result<Option<StringZ>, Error> {
        if let Some(rest) = arg.strip_prefix("--surface-id=") {
            self.surface_id =
                Some(parse_unsigned(rest.trim_matches(ASCII_WHITESPACE)).map_err(parse_number_error)?);
            return Ok(None);
        }

        if let Some(rest) = arg.strip_prefix("--class=") {
            self.class = Some(StringZ::from(rest.trim_matches(ASCII_WHITESPACE)));
            return Ok(None);
        }

        if let Some(rest) = arg.strip_prefix("--working-directory=") {
            let stripped = rest.trim_matches(ASCII_WHITESPACE);
            if stripped == "home" {
                return Ok(Some(StringZ::from(arg)));
            }
            if stripped == "inherit" {
                return Ok(Some(StringZ::from(arg)));
            }
            let realpath = new_window::resolve_working_directory(stripped)?;
            self._working_directory_seen = true;
            return Ok(Some(StringZ::from(format!("--working-directory={}", realpath).as_str())));
        }

        Ok(Some(StringZ::from(arg)))
    }
}

/// The help text of this action (Zig `help_strings.Action.@"new-tab"`).
pub const HELP: &str = include_str!("new_tab_help.txt");

/// The `new-tab` will use native platform IPC to open up a new tab in a running
/// instance of Ghostty. See [`HELP`].
pub fn run() -> RunResult {
    let mut iter = args::args_iterator(global_args());

    let mut stderr = BufWriter::with_capacity(1024, io::stderr().lock());

    let result = run_args(&mut iter, &mut stderr);
    let _ = stderr.flush();
    result
}

fn run_args<I: ArgIterator + ?Sized, W: Write + ?Sized>(args_iter: &mut I, stderr: &mut W) -> RunResult {
    let mut opts = Options::default();

    match args::parse(&mut opts, args_iter) {
        Ok(()) => {}
        Err(err) if err == Action::HELP_ERROR => return Err(Box::new(err)),
        Err(err) => {
            writeln!(stderr, "Error parsing args: {}", err)?;
            return Ok(1);
        }
    }

    // Print out any diagnostics, unless it's likely that the diagnostic was
    // generated trying to parse a "normal" configuration setting. Exit with an
    // error code if any diagnostics were printed.
    if !opts._diagnostics.empty()
        && new_window::print_own_diagnostics(&opts._diagnostics, Options::FIELDS, stderr)?
    {
        return Ok(1);
    }

    if !opts._working_directory_seen {
        let wd = std::fs::canonicalize(".")?;
        // This should be inserted at the beginning of the list, just in case `-e` was used.
        opts._arguments.insert(
            0,
            StringZ::from(format!("--working-directory={}", wd.to_string_lossy()).as_str()),
        );
    }

    let surface_id = opts.surface_id.unwrap_or_else(|| {
        let env = global::environ_map();
        env.get("GHOSTTY_SURFACE_ID")
            .and_then(|e| parse_unsigned(e).ok())
            .unwrap_or(0)
    });

    let arguments: Vec<&CStr> = opts._arguments.iter().map(StringZ::as_c_str).collect();
    let target = match opts.class.as_ref() {
        Some(class) => apprt::ipc::Target::Class(class.as_c_str()),
        None => apprt::ipc::Target::Detect,
    };

    match apprt::App::perform_ipc(
        &target,
        &apprt::ipc::Action::NewTab(apprt::ipc::NewTab {
            surface_id,
            arguments: if arguments.is_empty() { None } else { Some(&arguments) },
        }),
    ) {
        Ok(true) => return Ok(0),
        Ok(false) => {}
        Err(apprt::ipc::Errors::IpcFailed) => {
            // The apprt should have printed a more specific error message
            // already.
            return Ok(1);
        }
    }

    // If we get here, the platform is not supported.
    write!(stderr, "+new-tab is not supported on this platform.\n")?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::ArgIteratorGeneral;

    #[test]
    fn collects_arguments_and_surface_id() {
        let mut opts = Options::default();
        let mut iter = ArgIteratorGeneral::init("--surface-id=0x10 --title=foo -e echo --surface-id=1");
        args::parse(&mut opts, &mut iter).unwrap();
        assert_eq!(Some(16), opts.surface_id);
        let args: Vec<&str> = opts._arguments.iter().map(StringZ::as_str).collect();
        assert_eq!(vec!["--title=foo", "-e", "echo", "--surface-id=1"], args);
    }

    #[test]
    fn invalid_surface_id() {
        let mut opts = Options::default();
        let mut iter = ArgIteratorGeneral::init("--surface-id=abc");
        assert_eq!(
            Err(Error::Other("InvalidCharacter")),
            args::parse(&mut opts, &mut iter)
        );
        assert_eq!(Err(ParseNumberError::InvalidCharacter), parse_unsigned("-1"));
    }
}
