//! Port of `src/cli/Pager.zig`.
//!
//! STATUS: ported
//!
//! A pager wraps output to an external pager program (like `less`) when
//! stdout is a TTY. The pager command is resolved as:
//!
//!   `$GHOSTTY_PAGER` > `$PAGER` > `less`
//!
//! Setting either env var to an empty string disables paging.
//! If stdout is not a TTY, writes go directly to stdout.

use std::io::{self, BufWriter, IsTerminal, Write};
use std::process::{Child, Command, Stdio};

use crate::global;

/// See the module documentation.
#[derive(Default)]
pub struct Pager {
    /// The pager child process, if one was spawned.
    pub child: Option<Child>,

    /// The buffered file writer used for both the pager pipe and direct
    /// stdout paths.
    file_writer: Option<BufWriter<Box<dyn Write>>>,
}

impl Pager {
    /// Initialize the pager. If stdout is a TTY, this spawns the pager
    /// process. Otherwise, output goes directly to stdout.
    pub fn init() -> Pager {
        Pager {
            child: init_pager(),
            file_writer: None,
        }
    }

    /// Writes to the pager process if available; otherwise, stdout.
    pub fn writer(&mut self) -> &mut BufWriter<Box<dyn Write>> {
        let inner: Box<dyn Write> = match self.child.as_mut().and_then(|c| c.stdin.take()) {
            Some(stdin) => Box::new(stdin),
            None => Box::new(io::stdout()),
        };
        self.file_writer.insert(BufWriter::with_capacity(4096, inner))
    }
}

impl Drop for Pager {
    /// Deinitialize the pager. Waits for the spawned process to exit.
    fn drop(&mut self) {
        // Flush any remaining buffered data, close the pipe so the
        // pager sees EOF, then wait for it to exit.
        if let Some(mut w) = self.file_writer.take() {
            let _ = w.flush();
        }
        if let Some(child) = self.child.as_mut() {
            drop(child.stdin.take());
            let _ = child.wait();
        }
    }
}

fn init_pager() -> Option<Child> {
    if !io::stdout().is_terminal() {
        return None;
    }

    let env = global::environ_map();

    // Resolve the pager command: $GHOSTTY_PAGER > $PAGER > `less`.
    // An empty value for either env var disables paging.
    let cmd: Option<&str> = if let Some(v) = env.get("GHOSTTY_PAGER") {
        if v.is_empty() {
            None
        } else {
            Some(v)
        }
    } else if let Some(v) = env.get("PAGER") {
        if v.is_empty() {
            None
        } else {
            Some(v)
        }
    } else {
        Some("less")
    };

    Command::new(cmd?)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pager_non_tty() {
        // Test output is captured (not a TTY), so no pager is spawned.
        if io::stdout().is_terminal() {
            return;
        }
        let pager = Pager::init();
        assert!(pager.child.is_none());
    }

    #[test]
    fn pager_default_writer() {
        let mut pager = Pager::default();
        assert!(pager.child.is_none());
        let w = pager.writer();
        w.write_all(b"hello").unwrap();
    }
}
