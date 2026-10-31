//! Port of `src/apprt/gtk/class/Overrides.zig`.
//!
//! STATUS: ported
//!
//! The arena allocator is dropped: the parsed values are owned. Zig takes a
//! `*glib.VariantIter`; the port takes any iterator of `glib::Variant`
//! (e.g. `glib::Variant::iter`). GVariant strings are UTF-8, so the string
//! overrides are `String`s.
#![cfg(feature = "gtk")]

use crate::cli::args::ParseCli;
use crate::config::command::Command;
use crate::config::config::ShellIntegration;

const LOG: &str = "gtk_ghostty_overrides";

/// `Allocator.Error || error{ValueRequired}`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    OutOfMemory,
    ValueRequired,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overrides {
    pub command: Option<Command>,
    pub shell_integration: Option<ShellIntegration>,
    pub working_directory: Option<String>,
    pub title: Option<String>,
}

/// `std.ascii.whitespace`
fn is_ascii_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

impl Overrides {
    pub const NONE: Overrides = Overrides {
        command: None,
        shell_integration: None,
        working_directory: None,
        title: None,
    };

    pub fn parse(
        arguments_it: impl IntoIterator<Item = glib::Variant>,
    ) -> Result<Overrides, ParseError> {
        let mut args: Vec<String> = Vec::new();

        let mut working_directory: Option<String> = None;
        let mut title: Option<String> = None;
        let mut command: Option<Command> = None;
        let mut parsed_shell_integration: Option<ShellIntegration> = None;

        let mut e_seen = false;

        for (i, value) in arguments_it.into_iter().enumerate() {
            // just to be sure
            if !value.is_type(glib::VariantTy::STRING) {
                continue;
            }

            let Some(str) = value.str() else { continue };

            log::debug!(target: LOG, "argument: {} {}", i, str);

            if e_seen {
                args.push(str.to_owned());
                continue;
            }

            if str == "-e" {
                e_seen = true;
                continue;
            }

            if let Some(v) = str.strip_prefix("--command=") {
                let cmd = match Command::parse_cli_new(Some(v)) {
                    Ok(cmd) => cmd,
                    Err(err) => {
                        log::warn!(target: LOG, "unable to parse command: {err:?}");
                        return Err(match err {
                            crate::cli::args::Error::OutOfMemory => ParseError::OutOfMemory,
                            _ => ParseError::ValueRequired,
                        });
                    }
                };
                command = Some(cmd);
                continue;
            }

            if let Some(v) = str.strip_prefix("--shell-integration=") {
                match ShellIntegration::parse_cli_new(Some(v.trim_matches(is_ascii_whitespace))) {
                    Ok(si) => parsed_shell_integration = Some(si),
                    Err(err) => {
                        log::warn!(target: LOG, "unable to parse shell integration {v}: {err:?}");
                        continue;
                    }
                }
                continue;
            }

            if let Some(v) = str.strip_prefix("--working-directory=") {
                working_directory = Some(v.trim_matches(is_ascii_whitespace).to_owned());
                continue;
            }

            if let Some(v) = str.strip_prefix("--title=") {
                title = Some(v.trim_matches(is_ascii_whitespace).to_owned());
                continue;
            }
        }

        if !args.is_empty() {
            command = Some(Command::Direct(args.into_iter().map(Into::into).collect()));
        }

        Ok(Overrides {
            command,
            shell_integration: parsed_shell_integration,
            working_directory,
            title,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glib::prelude::*;

    fn variants(args: &[&str]) -> Vec<glib::Variant> {
        args.iter().map(|s| s.to_variant()).collect()
    }

    #[test]
    fn parse_none() {
        assert_eq!(Overrides::parse(variants(&[])).unwrap(), Overrides::NONE);
    }

    #[test]
    fn parse_flags() {
        let o = Overrides::parse(variants(&[
            "--working-directory= /tmp ",
            "--title=hello",
            "--shell-integration=fish",
            "--command=direct:vim a",
            "ignored",
        ]))
        .unwrap();
        assert_eq!(o.working_directory.as_deref(), Some("/tmp"));
        assert_eq!(o.title.as_deref(), Some("hello"));
        assert_eq!(o.shell_integration, Some(ShellIntegration::Fish));
        assert_eq!(o.command, Some(Command::Direct(vec!["vim".into(), "a".into()])));
    }

    #[test]
    fn parse_dash_e() {
        let mut vs = variants(&["--title=x", "-e", "nvim", "--title=y"]);
        // Non-string values are skipped.
        vs.insert(0, 5i32.to_variant());
        let o = Overrides::parse(vs).unwrap();
        assert_eq!(o.title.as_deref(), Some("x"));
        assert_eq!(
            o.command,
            Some(Command::Direct(vec!["nvim".into(), "--title=y".into()]))
        );
    }

    #[test]
    fn parse_bad_values() {
        // An invalid shell integration is ignored.
        let o = Overrides::parse(variants(&["--shell-integration=bogus"])).unwrap();
        assert_eq!(o.shell_integration, None);

        // An empty command is an error.
        assert_eq!(
            Overrides::parse(variants(&["--command="])),
            Err(ParseError::ValueRequired)
        );
    }

    #[test]
    fn parse_from_variant_iter() {
        let v = vec!["--title=t".to_owned()].to_variant();
        let o = Overrides::parse(v.iter()).unwrap();
        assert_eq!(o.title.as_deref(), Some("t"));
    }
}
