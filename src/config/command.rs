//! Port of `src/config/command.zig`.
//!
//! STATUS: ported

use crate::cli::args::{ArgIterator as _, ArgIteratorGeneral, Error, ParseCli, StringZ};
use crate::config::formatter::{self, EntryFormatter, FormatEntry};

/// A command to execute (argv0 and args).
///
/// A command is specified as a simple string such as "nvim a b c".
/// By default, we expect the downstream to do some sort of shell expansion
/// on this string.
///
/// If a command is already expanded and the user does NOT want to do
/// shell expansion (because this usually requires a round trip into
/// /bin/sh or equivalent), specify a `direct:`-prefix. e.g.
/// `direct:nvim a b c`.
///
/// The whitespace before or around the prefix is ignored. For example,
/// `  direct:nvim a b c` and `direct: nvim a b c` are equivalent.
///
/// If the command is not absolute, it'll be looked up via the PATH.
/// For the shell-expansion case, we let the shell do this. For the
/// direct case, we do this directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Execute a command directly, e.g. via `exec`. The format here
    /// is already structured to be ready to passed directly to `exec`
    /// with index zero being the command to execute.
    ///
    /// Index zero is not guaranteed to be an absolute path, and may require
    /// PATH lookup. It is up to the downstream to do this, usually via
    /// delegation to something like `execvp`.
    Direct(Vec<StringZ>),

    /// Execute a command via shell expansion. This provides the command
    /// as a single string that is expected to be expanded in some way
    /// (up to the downstream). Usually `/bin/sh -c`.
    Shell(StringZ),
}

/// The active tag of a [`Command`] (Zig `std.meta.Tag(Command)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Direct,
    Shell,
}

impl ParseCli for Command {
    fn parse_cli_new(input: Option<&str>) -> Result<Self, Error> {
        // Input is required. Whitespace on the edges isn't needed.
        // Commands must be non-empty.
        let input = input.ok_or(Error::ValueRequired)?;
        let trimmed = input.trim_matches(' ');
        if trimmed.is_empty() {
            return Err(Error::ValueRequired);
        }

        // If we have a `:` then we MIGHT have a prefix to specify what
        // tag we should use.
        let (tag, str): (Tag, &str) = 'tag: {
            if let Some(idx) = trimmed.find(':') {
                let prefix = &trimmed[..idx];
                if prefix == "direct" {
                    break 'tag (Tag::Direct, &trimmed[idx + 1..]);
                } else if prefix == "shell" {
                    break 'tag (Tag::Shell, &trimmed[idx + 1..]);
                }
            }

            (Tag::Shell, trimmed)
        };

        match tag {
            // We have a shell command, so we can just dupe it.
            Tag::Shell => Ok(Command::Shell(StringZ::from(str.trim_matches(' ')))),

            // We're not shell expanding, so the arguments are naively
            // split on spaces.
            Tag::Direct => Ok(Command::Direct(
                str.trim_matches(' ').split(' ').map(StringZ::from).collect(),
            )),
        }
    }
}

impl Command {
    /// Creates a command as a single string, joining arguments as
    /// necessary with spaces. Its not guaranteed that this is a valid
    /// command; it is only meant to be human readable.
    pub fn string(&self) -> StringZ {
        match self {
            Command::Shell(v) => v.clone(),
            Command::Direct(v) => StringZ::from(
                v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" "),
            ),
        }
    }

    /// Get an iterator over the arguments array.
    ///
    /// For direct commands, this is very cheap and just iterates over
    /// the array.
    ///
    /// For shell commands, this will use Zig's ArgIteratorGeneral as
    /// a best effort shell string parser. This is not guaranteed to be
    /// 100% accurate, but it works for common cases.
    pub fn arg_iterator(&self) -> ArgIterator<'_> {
        match self {
            Command::Direct(v) => ArgIterator::Direct { i: 0, args: v },
            Command::Shell(v) => ArgIterator::Shell(ArgIteratorGeneral::init(v)),
        }
    }
}

/// Iterates over each argument in the command.
#[derive(Debug, Clone)]
pub enum ArgIterator<'a> {
    Shell(ArgIteratorGeneral),
    Direct { i: usize, args: &'a [StringZ] },
}

impl ArgIterator<'_> {
    /// Return the next argument (always a copy in Rust).
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<String> {
        match self {
            ArgIterator::Shell(v) => v.next(),
            ArgIterator::Direct { i, args } => {
                if *i >= args.len() {
                    return None;
                }
                let result = args[*i].to_string();
                *i += 1;
                Some(result)
            }
        }
    }
}

impl FormatEntry for Command {
    fn format_entry(&self, formatter: &mut EntryFormatter<'_>) -> Result<(), formatter::Error> {
        match self {
            Command::Shell(v) => formatter.format_entry(v),

            Command::Direct(v) => {
                // Zig formats into a fixed 4096 byte buffer.
                const BUF_LEN: usize = 4096;
                let mut buf = String::from("direct:");
                for arg in v {
                    buf.push_str(arg);
                    buf.push(' ');
                }
                if buf.len() > BUF_LEN {
                    return Err(formatter::Error::OutOfMemory);
                }

                formatter.format_entry(&buf[..buf.len() - 1])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::formatter::entry_formatter;

    fn parse(v: &mut Command, input: Option<&str>) -> Result<(), Error> {
        v.parse_cli(input)
    }

    #[test]
    fn command_parse_cli_errors() {
        let mut v = Command::Shell(StringZ::new());
        assert_eq!(Err(Error::ValueRequired), parse(&mut v, None));
        assert_eq!(Err(Error::ValueRequired), parse(&mut v, Some("")));
        assert_eq!(Err(Error::ValueRequired), parse(&mut v, Some(" ")));
    }

    #[test]
    fn command_parse_cli_shell_expanded() {
        let mut v = Command::Shell(StringZ::new());
        parse(&mut v, Some("echo hello")).unwrap();
        assert_eq!(Command::Shell("echo hello".into()), v);

        // Spaces are stripped
        parse(&mut v, Some(" echo hello ")).unwrap();
        assert_eq!(Command::Shell("echo hello".into()), v);
    }

    #[test]
    fn command_parse_cli_direct() {
        let mut v = Command::Shell(StringZ::new());
        parse(&mut v, Some("direct:echo hello")).unwrap();
        let Command::Direct(args) = &v else { panic!("expected direct") };
        assert_eq!(2, args.len());
        assert_eq!("echo", args[0].as_str());
        assert_eq!("hello", args[1].as_str());

        // Spaces around the prefix
        parse(&mut v, Some(" direct:  echo hello")).unwrap();
        let Command::Direct(args) = &v else { panic!("expected direct") };
        assert_eq!(2, args.len());
        assert_eq!("echo", args[0].as_str());
        assert_eq!("hello", args[1].as_str());
    }

    #[test]
    fn command_arg_iterator_shell() {
        let v = Command::Shell("echo hello world".into());
        let mut it = v.arg_iterator();

        assert_eq!("echo", it.next().unwrap());
        assert_eq!("hello", it.next().unwrap());
        assert_eq!("world", it.next().unwrap());
        assert!(it.next().is_none());
    }

    #[test]
    fn command_arg_iterator_direct() {
        let v = Command::Direct(vec!["echo".into(), "hello world".into()]);
        let mut it = v.arg_iterator();

        assert_eq!("echo", it.next().unwrap());
        assert_eq!("hello world", it.next().unwrap());
        assert!(it.next().is_none());
    }

    #[test]
    fn command_string_shell() {
        let v = Command::Shell("echo hello world".into());
        assert_eq!("echo hello world", v.string().as_str());
    }

    #[test]
    fn command_string_direct() {
        let v = Command::Direct(vec!["echo".into(), "hello world".into()]);
        assert_eq!("echo hello world", v.string().as_str());
    }

    #[test]
    fn command_format_config_shell() {
        let mut buf = String::new();

        let mut v = Command::Shell(StringZ::new());
        parse(&mut v, Some("echo hello")).unwrap();
        v.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
        assert_eq!("a = echo hello\n", buf);
    }

    #[test]
    fn command_format_config_direct() {
        let mut buf = String::new();

        let mut v = Command::Shell(StringZ::new());
        parse(&mut v, Some("direct: echo hello")).unwrap();
        v.format_entry(&mut entry_formatter("a", &mut buf)).unwrap();
        assert_eq!("a = direct:echo hello\n", buf);
    }
}
