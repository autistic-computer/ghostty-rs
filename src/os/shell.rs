//! Port of `src/os/shell.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::io;

/// Builder for constructing space-separated shell command strings.
#[derive(Debug, Default, Clone)]
pub struct ShellCommandBuilder {
    buffer: String,
}

impl ShellCommandBuilder {
    pub fn init() -> ShellCommandBuilder {
        ShellCommandBuilder::default()
    }

    /// The command built so far.
    pub fn written(&self) -> &str {
        &self.buffer
    }

    /// Append an argument to the command with automatic space separation.
    pub fn append_arg(&mut self, arg: &str) {
        if arg.is_empty() {
            return;
        }
        if !self.buffer.is_empty() {
            self.buffer.push(' ');
        }
        self.buffer.push_str(arg);
    }

    /// Get the final command string, transferring ownership to caller.
    pub fn to_owned_slice(self) -> String {
        self.buffer
    }
}

/// Writer that escapes characters that shells treat specially to reduce the
/// risk of injection attacks or other such weirdness. Specifically excludes
/// linefeeds so that they can be used to delineate lists of file paths.
///
/// Implements `std::io::Write` when wrapping an `io::Write` and
/// `std::fmt::Write` when wrapping a `fmt::Write`.
#[derive(Debug)]
pub struct ShellEscapeWriter<W> {
    pub child: W,
}

/// Returns the escaped representation of a byte, if it needs escaping.
fn escape(byte: u8) -> Option<[u8; 2]> {
    match byte {
        b'\\' | b'"' | b'\'' | b'$' | b'`' | b'*' | b'?' | b' ' | b'|' | b'(' | b')' => {
            Some([b'\\', byte])
        }
        _ => None,
    }
}

impl<W> ShellEscapeWriter<W> {
    pub fn init(child: W) -> ShellEscapeWriter<W> {
        ShellEscapeWriter { child }
    }

    pub fn into_inner(self) -> W {
        self.child
    }
}

impl<W: io::Write> io::Write for ShellEscapeWriter<W> {
    fn write(&mut self, s: &[u8]) -> io::Result<usize> {
        let mut count = 0;
        for &byte in s {
            match escape(byte) {
                Some(buf) => self.child.write_all(&buf)?,
                None => self.child.write_all(&[byte])?,
            }
            count += 1;
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.child.flush()
    }
}

impl<W: fmt::Write> fmt::Write for ShellEscapeWriter<W> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            if c.is_ascii() {
                if let Some(buf) = escape(c as u8) {
                    self.child.write_char(buf[0] as char)?;
                    self.child.write_char(buf[1] as char)?;
                    continue;
                }
            }
            self.child.write_char(c)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn shell_command_builder() {
        // Empty command
        {
            let cmd = ShellCommandBuilder::init();
            assert_eq!("", cmd.written());
        }

        // Single arg
        {
            let mut cmd = ShellCommandBuilder::init();
            cmd.append_arg("bash");
            assert_eq!("bash", cmd.written());
        }

        // Multiple args
        {
            let mut cmd = ShellCommandBuilder::init();
            cmd.append_arg("bash");
            cmd.append_arg("--posix");
            cmd.append_arg("-l");
            assert_eq!("bash --posix -l", cmd.written());
        }

        // Empty arg
        {
            let mut cmd = ShellCommandBuilder::init();
            cmd.append_arg("bash");
            cmd.append_arg("");
            assert_eq!("bash", cmd.written());
        }

        // toOwnedSlice
        {
            let mut cmd = ShellCommandBuilder::init();
            cmd.append_arg("bash");
            cmd.append_arg("--posix");
            let result = cmd.to_owned_slice();
            assert_eq!("bash --posix", result);
        }
    }

    fn escaped(input: &str) -> String {
        let mut buf: Vec<u8> = Vec::new();
        let mut shell = ShellEscapeWriter::init(&mut buf);
        shell.write_all(input.as_bytes()).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn shell_escape_1() {
        assert_eq!("abc", escaped("abc"));
    }

    #[test]
    fn shell_escape_2() {
        assert_eq!("a\\ c", escaped("a c"));
    }

    #[test]
    fn shell_escape_3() {
        assert_eq!("a\\?c", escaped("a?c"));
    }

    #[test]
    fn shell_escape_4() {
        assert_eq!("a\\\\c", escaped("a\\c"));
    }

    #[test]
    fn shell_escape_5() {
        assert_eq!("a\\|c", escaped("a|c"));
    }

    #[test]
    fn shell_escape_6() {
        assert_eq!("a\\\"c", escaped("a\"c"));
    }

    #[test]
    fn shell_escape_7() {
        assert_eq!("a\\(1\\)", escaped("a(1)"));
    }

    #[test]
    fn shell_escape_fmt() {
        use std::fmt::Write as _;
        let mut s = String::new();
        let mut shell = ShellEscapeWriter::init(&mut s);
        write!(shell, "a b$ü").unwrap();
        assert_eq!("a\\ b\\$ü", s);
    }
}
