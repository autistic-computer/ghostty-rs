//! Port of `src/os/open.zig`.
//!
//! STATUS: ported

use std::fmt;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use crate::build_config;

pub use crate::apprt::action::OpenUrlKind;

#[derive(Debug)]
pub enum OpenError {
    /// The URL kind is untrusted and the apprt must check it first.
    UntrustedLink,
    Unimplemented,
    Io(std::io::Error),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::UntrustedLink => f.write_str("UntrustedLink"),
            OpenError::Unimplemented => f.write_str("Unimplemented"),
            OpenError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<std::io::Error> for OpenError {
    fn from(e: std::io::Error) -> Self {
        OpenError::Io(e)
    }
}

/// Open a URL in the default handling application.
///
/// Any output on stderr is logged as a warning in the application logs.
/// Output on stdout is ignored.
///
/// This function is purposely simple for the sake of providing some portable
/// way to open URLs. If you are implementing an apprt for Ghostty, you should
/// consider doing something special-cased for your platform.
///
/// On macOS, untrusted kinds (`Unknown`, `Osc8`) return `UntrustedLink`
/// because the apprt must check them first.
pub fn open(kind: OpenUrlKind, url: &str) -> Result<(), OpenError> {
    // If an embedder declines an untrusted URL, refuse it rather than skip
    // the apprt's safety checks by passing it to `open`.
    if cfg!(target_os = "macos") {
        match kind {
            OpenUrlKind::Unknown | OpenUrlKind::Osc8 => return Err(OpenError::UntrustedLink),
            OpenUrlKind::Text | OpenUrlKind::Html => {}
        }
    }

    let argv: Vec<&str> = if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        vec!["xdg-open", url]
    } else if cfg!(windows) {
        vec!["rundll32", "url.dll,FileProtocolHandler", url]
    } else if cfg!(target_os = "macos") {
        match kind {
            OpenUrlKind::Text => vec!["open", "-t", url],
            OpenUrlKind::Html => vec!["open", url],
            OpenUrlKind::Unknown | OpenUrlKind::Osc8 => unreachable!(),
        }
    } else {
        return Err(OpenError::Unimplemented);
    };

    let mut cmd = Command::new(argv[0]);
    cmd.args(&argv[1..]);
    // Ignore anything from stdout. Pipe stderr so we can log the stderr
    // from the command.
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    if build_config::SNAP {
        // In the snap on Linux the launcher exports LD_LIBRARY_PATH
        // pointing at the snap's bundled libraries. Leaking this into
        // child process can can be problematic, so let's drop it from the
        // env.
        cmd.env_remove("LD_LIBRARY_PATH");
    }

    let exe = cmd.spawn()?;

    std::thread::Builder::new()
        .name("os-open".into())
        .spawn(move || open_thread(exe))?;
    Ok(())
}

fn open_thread(mut exe: std::process::Child) {
    if let Some(stderr) = exe.stderr.take() {
        let mut reader = BufReader::with_capacity(256, stderr);
        let mut line = Vec::new();
        loop {
            // Read inclusively so the delimiter is consumed.
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let text = String::from_utf8_lossy(&line);
            log::warn!(target: "os-open", "open stderr={}", text.trim_end_matches('\n'));
        }
    }
    let _ = exe.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_untrusted_links_have_no_generic_opener_fallback() {
        assert!(matches!(
            open(OpenUrlKind::Osc8, "file:///tmp/payload.command"),
            Err(OpenError::UntrustedLink)
        ));
        assert!(matches!(
            open(OpenUrlKind::Unknown, "file:///tmp/payload.command"),
            Err(OpenError::UntrustedLink)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn open_thread_drains_stderr() {
        let child = Command::new("/bin/sh")
            .args(["-c", "echo one >&2; echo two >&2"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        open_thread(child);
    }
}
