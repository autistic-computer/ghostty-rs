//! Port of libxev `src/backend.zig` and the backend-selection parts of
//! `src/dynamic.zig`.
//!
//! Ghostty uses libxev's *dynamic* API (`xev.Dynamic`), which on Linux
//! chooses between io_uring and epoll at runtime. The Rust port implements a
//! single loop (epoll on Linux, a `poll(2)` emulation elsewhere), so io_uring
//! always reports itself as unavailable and [`detect`] settles on epoll. The
//! selection API is kept so callers (e.g. the GTK apprt's backend
//! preference) port mechanically.

use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

use super::posix::Error;

/// The backend types.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    IoUring = 0,
    Epoll = 1,
    Kqueue = 2,
    WasiPoll = 3,
    Iocp = 4,
}

impl Backend {
    /// All backends, in declaration order.
    pub const ALL: [Backend; 5] = [
        Backend::IoUring,
        Backend::Epoll,
        Backend::Kqueue,
        Backend::WasiPoll,
        Backend::Iocp,
    ];

    /// Returns a recommend default backend from inspecting the system.
    pub const fn default() -> Backend {
        if cfg!(target_os = "linux") {
            if cfg!(target_os = "android") {
                Backend::Epoll
            } else {
                Backend::IoUring
            }
        } else if cfg!(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "visionos",
            target_os = "freebsd"
        )) {
            Backend::Kqueue
        } else if cfg!(target_os = "wasi") {
            Backend::WasiPoll
        } else {
            Backend::Iocp
        }
    }

    /// Candidate backends for this platform in priority order.
    pub const fn candidates() -> &'static [Backend] {
        if cfg!(target_os = "android") {
            &[Backend::Epoll]
        } else if cfg!(target_os = "linux") {
            &[Backend::IoUring, Backend::Epoll]
        } else if cfg!(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "visionos",
            target_os = "freebsd"
        )) {
            &[Backend::Kqueue]
        } else if cfg!(target_os = "wasi") {
            &[Backend::WasiPoll]
        } else {
            &[Backend::Iocp]
        }
    }

    /// A function to test if this API is available on the current system.
    ///
    /// io_uring is never available in the Rust port (it is not
    /// implemented). On non-Linux Unix systems the "kqueue" backend is
    /// served by the `poll(2)` emulation loop.
    pub fn available(self) -> bool {
        match self {
            Backend::IoUring => false,
            Backend::Epoll => super::epoll::available(),
            Backend::Kqueue => cfg!(all(unix, not(target_os = "linux"))),
            Backend::WasiPoll | Backend::Iocp => false,
        }
    }

    /// The Zig tag name (`@tagName`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Backend::IoUring => "io_uring",
            Backend::Epoll => "epoll",
            Backend::Kqueue => "kqueue",
            Backend::WasiPoll => "wasi_poll",
            Backend::Iocp => "iocp",
        }
    }

    fn from_u8(v: u8) -> Backend {
        Backend::ALL[v as usize]
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// This is a flag that can be used to detect that you're using a dynamic
/// API and not the static API (true when there is more than one candidate).
pub const DYNAMIC: bool = Backend::candidates().len() > 1;

/// The backend that is in use. Defaults to the last candidate, like libxev.
static BACKEND: AtomicU8 = AtomicU8::new({
    let c = Backend::candidates();
    c[c.len() - 1] as u8
});

/// The backend that is in use (Zig: `xev.backend`).
pub fn backend() -> Backend {
    Backend::from_u8(BACKEND.load(Ordering::Relaxed))
}

/// Detect the preferred backend based on the system and set it
/// for use. "Preferred" is the first available (API exists) candidate
/// backend in the list.
pub fn detect() -> Result<(), Error> {
    for &be in Backend::candidates() {
        if be.available() {
            BACKEND.store(be as u8, Ordering::Relaxed);
            return Ok(());
        }
    }

    Err(Error::NoAvailableBackends)
}

/// Manually set the backend to use, but if the backend is not
/// available, this will not change the backend in use.
pub fn prefer(be: Backend) -> bool {
    for &candidate in Backend::candidates() {
        if candidate == be {
            if !candidate.available() {
                return false;
            }
            BACKEND.store(candidate as u8, Ordering::Relaxed);
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_selects_available() {
        detect().unwrap();
        assert!(Backend::candidates().contains(&backend()));
        assert!(backend().available());
    }

    #[test]
    fn prefer_unavailable_is_rejected() {
        // Candidates that aren't available are never selected.
        for &be in Backend::candidates() {
            if !be.available() {
                assert!(!prefer(be));
            }
        }
        // Non-candidates are never selected.
        for be in Backend::ALL {
            if !Backend::candidates().contains(&be) {
                assert!(!prefer(be));
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_backends() {
        assert!(DYNAMIC);
        assert_eq!(Backend::candidates(), &[Backend::IoUring, Backend::Epoll]);
        assert!(prefer(Backend::Epoll));
        assert_eq!(backend(), Backend::Epoll);
        assert!(!prefer(Backend::IoUring));
        assert_eq!(backend(), Backend::Epoll);
        assert_eq!(backend().to_string(), "epoll");
    }
}
