//! Port of `src/cli/validate_config.zig`.
//!
//! STATUS: ported

use std::io::{self, BufWriter, Write};

use super::args::{self, cli_struct, Error, ParseHooks, StringZ};
use super::ghostty::{global_args, Action, RunResult};
use crate::config::Config;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {
        /// The path of the config file to validate. If this isn't specified,
        /// then the default config file paths will be validated.
        pub config_file as "config-file": Option<StringZ> = None,
    }
    with { custom_hooks }
}

impl ParseHooks for Options {
    /// Enables "-h" and "--help" to work.
    fn help(&mut self) -> Option<Result<(), Error>> {
        Action::help_result()
    }
}

/// The help text of this action (Zig `help_strings.Action.@"validate-config"`).
pub const HELP: &str = "The `validate-config` command is used to validate a Ghostty config file.

When executed without any arguments, this will load the config from the default
location.

Flags:

  * `--config-file`: can be passed to validate a specific target config file in
    a non-default location";

/// The `validate-config` command is used to validate a Ghostty config file.
/// See [`HELP`].
pub fn run() -> RunResult {
    let mut opts = Options::default();

    {
        let mut iter = args::args_iterator(global_args());
        args::parse(&mut opts, &mut iter)?;
    }

    let mut stdout = BufWriter::with_capacity(1024, io::stdout().lock());
    let result = run_inner(&opts, &mut stdout);
    stdout.flush()?;
    result
}

fn run_inner<W: Write + ?Sized>(opts: &Options, stdout: &mut W) -> RunResult {
    let mut cfg = Config::default();

    // If a config path is passed, validate it, otherwise validate default configs
    if let Some(config_path) = &opts.config_file {
        let abs_path = std::fs::canonicalize(config_path.as_str())?;
        cfg.load_file(&abs_path.to_string_lossy())?;
        cfg.load_recursive_files()?;
    } else {
        cfg = Config::load()?;
    }

    cfg.finalize()?;

    if !cfg._diagnostics.items().is_empty() {
        for diag in cfg._diagnostics.items() {
            writeln!(stdout, "{}", diag)?;
        }
        return Ok(1);
    }

    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_file_with_errors() {
        let dir = std::env::temp_dir().join(format!("ghostty-validate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config");
        std::fs::write(&path, "font-size = 12\nnot-a-real-key = 1\n").unwrap();

        let opts = Options {
            config_file: Some(StringZ::from(path.to_str().unwrap())),
        };
        let mut out: Vec<u8> = Vec::new();
        let code = run_inner(&opts, &mut out).unwrap();
        assert_eq!(1, code);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("not-a-real-key"), "{}", out);

        std::fs::write(&path, "font-size = 12\n").unwrap();
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(0, run_inner(&opts, &mut out).unwrap());
        assert!(out.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
