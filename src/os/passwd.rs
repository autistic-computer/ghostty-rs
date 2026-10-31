//! Port of `src/os/passwd.zig`.
//!
//! STATUS: ported

#![cfg(not(any(windows, target_arch = "wasm32")))]

use std::ffi::CStr;
use std::fmt;

use crate::build_config::FLATPAK as BUILD_FLATPAK;

/// Entry that is retrieved from the passwd API. This only contains the
/// fields we care about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub shell: Option<String>,
    pub home: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug)]
pub enum Error {
    /// Errors from the flatpak host-command path.
    Flatpak(String),
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Flatpak(e) => write!(f, "flatpak: {e}"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

fn cstr_to_string(ptr: *const libc::c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: passwd fields are NUL-terminated strings in our buffer.
    let source = unsafe { CStr::from_ptr(ptr) };
    Some(String::from_utf8_lossy(source.to_bytes()).into_owned())
}

/// Get the passwd entry for the currently executing user.
pub fn get() -> Result<Entry, Error> {
    let mut buf = [0u8; 1024];
    // SAFETY: passwd is plain old data.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut pw_ptr: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: all pointers are valid for the call.
    let res = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut pw,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut pw_ptr,
        )
    };
    if res != 0 {
        log::warn!(target: "passwd", "error retrieving pw entry code={res}");
        return Ok(Entry::default());
    }

    if pw_ptr.is_null() {
        // Future: let's check if a better shell is available like zsh
        log::warn!(target: "passwd", "no pw entry to detect default shell, will default to 'sh'");
        return Ok(Entry::default());
    }

    let mut result = Entry::default();

    // If we're in flatpak then our entry is always empty so we grab it
    // by shelling out to the host. note that we do HAVE an entry in the
    // sandbox but only the username is correct.
    if super::flatpak::is_flatpak() {
        if !BUILD_FLATPAK {
            log::warn!(target: "passwd", "flatpak detected, but this build doesn't contain flatpak support");
        } else {
            #[cfg(all(target_os = "linux", feature = "gtk"))]
            {
                log::info!(target: "passwd", "flatpak detected, will use host command to get our entry");
                let name = cstr_to_string(pw.pw_name).unwrap_or_default();
                return flatpak_entry(&name);
            }
        }
    }

    result.shell = cstr_to_string(pw.pw_shell);
    result.home = cstr_to_string(pw.pw_dir);
    result.name = cstr_to_string(pw.pw_name);

    Ok(result)
}

/// Get our passwd entry from the host by running `getent passwd` through
/// the Flatpak host-command portal, reading the output through a pty.
#[cfg(all(target_os = "linux", feature = "gtk"))]
fn flatpak_entry(name: &str) -> Result<Entry, Error> {
    use super::flatpak::FlatpakHostCommand;

    // A guard so the pty is deinitialized (master closed) on every path,
    // like Zig's `defer pty.deinit()`. The slave side is closed explicitly
    // below (or here, if we exit early).
    struct PtyGuard {
        pty: crate::pty::Pty,
        slave_open: bool,
    }
    impl Drop for PtyGuard {
        fn drop(&mut self) {
            if self.slave_open {
                crate::lib::compat::fd::close(self.pty.slave);
            }
            self.pty.deinit();
        }
    }
    let mut guard = PtyGuard {
        pty: crate::pty::Pty::open(crate::pty::Winsize::default())
            .map_err(|e| Error::Flatpak(e.to_string()))?,
        slave_open: true,
    };
    let master = guard.pty.master;
    let slave = guard.pty.slave;

    // Note: we wrap our getent call in a /bin/sh login shell because
    // some operating systems (NixOS tested) don't set the PATH for various
    // utilities properly until we get a login shell.
    let cmd = FlatpakHostCommand::new(
        vec![
            "/bin/sh".to_string(),
            "-l".to_string(),
            "-c".to_string(),
            format!("getent passwd {name}"),
        ],
        slave,
        slave,
        slave,
    );
    cmd.spawn().map_err(|e| Error::Flatpak(e.to_string()))?;
    cmd.wait().map_err(|e| Error::Flatpak(e.to_string()))?;

    // Once started, we can close the child side. We do this after
    // wait right now but that is fine too. This lets us read the
    // parent and detect EOF.
    crate::lib::compat::fd::close(slave);
    guard.slave_open = false;

    // Read all of our output
    let mut buf = [0u8; 1024];
    let mut output: Vec<u8> = Vec::new();
    loop {
        // SAFETY: valid fd and buffer.
        let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                // EIO is triggered at the end since we closed our
                // child side. This is just EOF for this.
                Some(libc::EIO) => break,
                Some(libc::EINTR) => continue,
                _ => return Err(Error::Io(err)),
            }
        }
        if n == 0 {
            break;
        }
        output.extend_from_slice(&buf[..n as usize]);

        // Max total size is buf.len. We can do better here by trimming
        // the front and continuing reading but we choose to just exit.
        if output.len() > buf.len() {
            break;
        }
    }

    // Shell and home are the last two entries
    let output = String::from_utf8_lossy(&output);
    let trimmed = output.trim_end_matches([' ', '\r', '\n']);
    let mut it = trimmed.rsplit(':');
    let shell = it.next().map(str::to_string);
    let home = it.next().map(str::to_string);
    Ok(Entry {
        shell,
        home,
        name: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_entry() {
        // We should be able to get an entry
        let entry = get().unwrap();
        assert!(entry.shell.is_some());
        assert!(entry.home.is_some());
    }
}
