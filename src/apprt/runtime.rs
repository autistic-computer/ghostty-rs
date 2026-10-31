//! Port of `src/apprt/runtime.zig`.
//!
//! STATUS: ported

use std::fmt;

/// Runtime is the runtime to use for Ghostty. All runtimes do not provide
/// equivalent feature sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Runtime {
    /// Will not produce an executable at all when `zig build` is called.
    /// This is only useful if you're only interested in the lib only (macOS).
    None,

    /// GTK4. Rich windowed application. This uses a full GObject-based
    /// approach to building the application.
    Gtk,
}

impl Runtime {
    /// The default runtime for the current compilation target (Zig:
    /// `default(target)`; the Rust port only ever asks about the target
    /// being compiled).
    pub const fn default() -> Runtime {
        // The Linux and FreeBSD default is GTK because it is a full
        // featured application.
        if cfg!(any(target_os = "linux", target_os = "freebsd")) {
            Runtime::Gtk
        } else {
            // Otherwise, we do NONE so we don't create an exe and we create
            // libghostty. On macOS, Xcode is used to build the app that links
            // to libghostty.
            Runtime::None
        }
    }

    /// The Zig tag name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Runtime::None => "none",
            Runtime::Gtk => "gtk",
        }
    }
}

impl fmt::Display for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime() {
        if cfg!(target_os = "linux") {
            assert_eq!(Runtime::default(), Runtime::Gtk);
        }
        if cfg!(target_os = "macos") {
            assert_eq!(Runtime::default(), Runtime::None);
        }
        assert_eq!(Runtime::Gtk.to_string(), "gtk");
        assert_eq!(Runtime::None.to_string(), "none");
    }
}
