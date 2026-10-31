//! Port of `src/cli/new_window.zig`.
//!
//! STATUS: ported

use std::ffi::CStr;
use std::io::{self, BufWriter, Write};

use super::args::{self, cli_struct, ArgIterator, Error, Fields, ParseHooks, StringZ};
use super::diagnostics::{DiagnosticList, LocationKey};
use super::ghostty::{global_args, Action, RunResult};
use crate::apprt;
use crate::global;
use crate::os::homedir;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// If set, open up a new window in a custom instance of Ghostty.
        pub class: Option<StringZ> = None,
    }
    private {
        /// Did the user specify a `--working-directory` argument on the command line?
        pub _working_directory_seen: bool = false,

        /// All of the arguments after `+new-window`. They will be sent to Ghosttty
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

    /// Manual parse hook, collect all of the arguments after `+new-window`.
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

/// The Zig error name of an I/O error from `realPathFile`.
pub(crate) fn realpath_error_name(err: &io::Error) -> &'static str {
    match err.kind() {
        io::ErrorKind::NotFound => "FileNotFound",
        io::ErrorKind::PermissionDenied => "AccessDenied",
        io::ErrorKind::NotADirectory => "NotDir",
        io::ErrorKind::InvalidFilename => "NameTooLong",
        _ => "Unexpected",
    }
}

/// Resolve a `--working-directory` value (expand `~/`, make absolute).
pub(crate) fn resolve_working_directory(stripped: &str) -> Result<String, Error> {
    let mut expandhome_buf = vec![0u8; libc::PATH_MAX as usize];
    let env = global::environ_map();
    let expanded = homedir::expand_home(&env, stripped, &mut expandhome_buf).map_err(|e| match e {
        homedir::Error::BufferTooSmall => Error::Other("BufferTooSmall"),
        homedir::Error::HomeDetectionFailed => Error::Other("HomeDetectionFailed"),
    })?;
    let realpath = std::fs::canonicalize(expanded).map_err(|e| Error::Other(realpath_error_name(&e)))?;
    Ok(realpath.to_string_lossy().into_owned())
}

/// Zig `std.ascii.whitespace`.
pub(crate) const ASCII_WHITESPACE: &[char] = &[' ', '\t', '\n', '\r', '\x0b', '\x0c'];

impl Options {
    fn check_arg(&mut self, arg: &str) -> Result<Option<StringZ>, Error> {
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
            let realpath = resolve_working_directory(stripped)?;
            self._working_directory_seen = true;
            return Ok(Some(StringZ::from(format!("--working-directory={}", realpath).as_str())));
        }

        Ok(Some(StringZ::from(arg)))
    }
}

/// The help text of this action (Zig `help_strings.Action.@"new-window"`).
pub const HELP: &str = include_str!("new_window_help.txt");

/// The `new-window` will use native platform IPC to open up a new window in a
/// running instance of Ghostty. See [`HELP`].
pub fn run() -> RunResult {
    let mut iter = args::args_iterator(global_args());

    let mut stderr = BufWriter::with_capacity(1024, io::stderr().lock());

    let result = run_args(&mut iter, &mut stderr);
    let _ = stderr.flush();
    result
}

/// Print the diagnostics that are about our own options. Returns true if
/// any were printed (Zig: the `exit` flag).
pub(crate) fn print_own_diagnostics<W: Write + ?Sized>(
    diagnostics: &DiagnosticList,
    fields: &[args::FieldInfo],
    stderr: &mut W,
) -> io::Result<bool> {
    let mut exit = false;
    for diagnostic in diagnostics.items() {
        if diagnostic.location.key() != LocationKey::Cli {
            continue;
        }
        for field in fields {
            if field.name.starts_with('_') {
                continue;
            }
            if field.name == diagnostic.key.as_str() {
                writeln!(stderr, "config error: {}", diagnostic)?;
                exit = true;
            }
        }
    }
    Ok(exit)
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
    if !opts._diagnostics.empty() && print_own_diagnostics(&opts._diagnostics, Options::FIELDS, stderr)? {
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

    let arguments: Vec<&CStr> = opts._arguments.iter().map(StringZ::as_c_str).collect();
    let class = opts.class.as_ref().map(StringZ::as_c_str);
    let target = match class {
        Some(class) => apprt::ipc::Target::Class(class),
        None => apprt::ipc::Target::Detect,
    };

    match apprt::App::perform_ipc(
        &target,
        &apprt::ipc::Action::NewWindow(apprt::ipc::NewWindow {
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
    write!(stderr, "+new-window is not supported on this platform.\n")?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::ArgIteratorGeneral;

    #[test]
    fn collects_arguments() {
        let mut opts = Options::default();
        let mut iter = ArgIteratorGeneral::init("--class=com.example.app --title=foo -e echo --class=x");
        args::parse(&mut opts, &mut iter).unwrap();
        assert_eq!(Some("com.example.app"), opts.class.as_deref());
        let args: Vec<&str> = opts._arguments.iter().map(StringZ::as_str).collect();
        assert_eq!(vec!["--title=foo", "-e", "echo", "--class=x"], args);
        assert!(!opts._working_directory_seen);
    }

    #[test]
    fn working_directory_is_resolved() {
        let mut opts = Options::default();
        let mut iter = ArgIteratorGeneral::init("--working-directory=. --working-directory=home");
        args::parse(&mut opts, &mut iter).unwrap();
        assert!(opts._working_directory_seen);
        let cwd = std::fs::canonicalize(".").unwrap();
        assert_eq!(
            format!("--working-directory={}", cwd.display()),
            opts._arguments[0].as_str()
        );
        assert_eq!("--working-directory=home", opts._arguments[1].as_str());
    }
}
