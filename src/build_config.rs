//! Port of `src/build_config.zig`.
//!
//! STATUS: ported
//!
//! Build options, available at comptime. Used to configure features. This
//! will reproduce some of the fields from builtin and build_options just
//! so we can limit the amount of imports we need AND give us the ability
//! to shim logic and values into them later.
//!
//! The Zig values come from `build_options` (set by the Zig build); the Rust
//! port has no such step, so these are the values of the **Ghostty
//! application** build on the current target (see PORTING.md): Linux → GTK
//! apprt, OpenGL renderer, fontconfig+FreeType fonts; macOS → no apprt
//! (embedded libghostty), Metal, CoreText. The version comes from Cargo.

use std::fmt;

use crate::apprt::runtime::Runtime;
use crate::font::backend::Backend as FontBackend;
use crate::renderer::backend::Backend as RendererBackend;

/// The release channel for the build (Zig: `build/Config.zig`
/// `ReleaseChannel`, which has no Rust counterpart file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReleaseChannel {
    /// Unstable builds on every commit.
    Tip,

    /// Stable tagged releases.
    Stable,
}

impl ReleaseChannel {
    /// The Zig tag name.
    pub const fn as_str(self) -> &'static str {
        match self {
            ReleaseChannel::Tip => "tip",
            ReleaseChannel::Stable => "stable",
        }
    }
}

impl fmt::Display for ReleaseChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The entrypoint of the executable (Zig: `build/Config.zig`
/// `ExeEntrypoint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExeEntrypoint {
    Ghostty,
    Helpgen,
    MdgenGhostty1,
    MdgenGhostty5,
    WebgenConfig,
    WebgenActions,
    WebgenCommands,
}

/// A semantic version (Zig: `std.SemanticVersion`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SemanticVersion {
    pub major: usize,
    pub minor: usize,
    pub patch: usize,
    pub pre: Option<&'static str>,
    pub build: Option<&'static str>,
}

impl fmt::Display for SemanticVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = self.pre {
            write!(f, "-{pre}")?;
        }
        if let Some(build) = self.build {
            write!(f, "+{build}")?;
        }
        Ok(())
    }
}

/// Parse a decimal number at compile time.
const fn parse_usize(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut v = 0usize;
    while i < bytes.len() {
        assert!(bytes[i].is_ascii_digit());
        v = v * 10 + (bytes[i] - b'0') as usize;
        i += 1;
    }
    v
}

const fn non_empty(s: &'static str) -> Option<&'static str> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// The semantic version of this build.
pub const VERSION: SemanticVersion = SemanticVersion {
    major: parse_usize(env!("CARGO_PKG_VERSION_MAJOR")),
    minor: parse_usize(env!("CARGO_PKG_VERSION_MINOR")),
    patch: parse_usize(env!("CARGO_PKG_VERSION_PATCH")),
    pre: non_empty(env!("GHOSTTY_VERSION_PRE")),
    build: non_empty(env!("GHOSTTY_VERSION_BUILD")),
};

/// `VERSION` formatted (Zig: `std.fmt.comptimePrint("{f}", .{version})`).
pub const VERSION_STRING: &str = {
    const CORE: &str = concat!(
        env!("CARGO_PKG_VERSION_MAJOR"),
        ".",
        env!("CARGO_PKG_VERSION_MINOR"),
        ".",
        env!("CARGO_PKG_VERSION_PATCH")
    );
    match (VERSION.pre, VERSION.build) {
        (None, None) => CORE,
        (Some(_), None) => concat!(
            env!("CARGO_PKG_VERSION_MAJOR"),
            ".",
            env!("CARGO_PKG_VERSION_MINOR"),
            ".",
            env!("CARGO_PKG_VERSION_PATCH"),
            "-",
            env!("GHOSTTY_VERSION_PRE")
        ),
        (None, Some(_)) => concat!(
            env!("CARGO_PKG_VERSION_MAJOR"),
            ".",
            env!("CARGO_PKG_VERSION_MINOR"),
            ".",
            env!("CARGO_PKG_VERSION_PATCH"),
            "+",
            env!("GHOSTTY_VERSION_BUILD")
        ),
        (Some(_), Some(_)) => concat!(
            env!("CARGO_PKG_VERSION_MAJOR"),
            ".",
            env!("CARGO_PKG_VERSION_MINOR"),
            ".",
            env!("CARGO_PKG_VERSION_PATCH"),
            "-",
            env!("GHOSTTY_VERSION_PRE"),
            "+",
            env!("GHOSTTY_VERSION_BUILD")
        ),
    }
};

/// The release channel for this build.
pub const RELEASE_CHANNEL: ReleaseChannel = match VERSION.pre {
    Some(_) => ReleaseChannel::Tip,
    None => ReleaseChannel::Stable,
};

/// The optimization mode as a string (Zig: the tag name of
/// `builtin.mode`; Cargo's dev profile maps to `Debug`, release to
/// `ReleaseFast`).
pub const MODE_STRING: &str = if cfg!(debug_assertions) {
    "Debug"
} else {
    "ReleaseFast"
};

/// The artifact we're producing. This can be used to determine if we're
/// building a standalone exe, an embedded lib, etc.
pub const ARTIFACT: Artifact = Artifact::detect();

/// Our build configuration. We re-export a lot of these back at the
/// top-level so its a bit cleaner to use throughout the code.
pub const EXE_ENTRYPOINT: ExeEntrypoint = ExeEntrypoint::Ghostty;
/// Whether this is a Flatpak build (Zig build option `-Dflatpak`, default
/// false).
pub const FLATPAK: bool = false;
/// Whether this is a Snap build (Zig build option `-Dsnap`, default false).
pub const SNAP: bool = false;
pub const APP_RUNTIME: Runtime = Runtime::default();
/// Whether the GTK apprt is built with X11 support (Zig build option
/// `build_options.x11`, `-Dgtk-x11`, on by default for GTK builds on Linux).
pub const X11: bool = matches!(APP_RUNTIME, Runtime::Gtk);
/// Whether the GTK apprt is built with Wayland support (Zig build option
/// `build_options.wayland`, `-Dgtk-wayland`, on by default for GTK builds
/// on Linux).
pub const WAYLAND: bool = matches!(APP_RUNTIME, Runtime::Gtk);
pub const FONT_BACKEND: FontBackend = FontBackend::default(
    crate::font::backend::Target::current(),
    crate::font::backend::WasmTarget::Browser,
);
pub const RENDERER: RendererBackend = RendererBackend::default();
/// Enabled by default only for macOS, and other Unix-like systems like
/// Linux and FreeBSD when using glibc.
pub const I18N: bool = cfg!(any(
    target_os = "macos",
    target_os = "ios",
    all(
        any(target_os = "linux", target_os = "freebsd"),
        target_env = "gnu"
    )
));

/// The bundle ID for the app. This is used in many places and is currently
/// hardcoded here. We could make this configurable in the future if there
/// is a reason to do so.
///
/// On macOS, this must match the App bundle ID. We can get that dynamically
/// via an API but I don't want to pay the cost of that at runtime.
///
/// On GTK, this should match the various folders with resources.
///
/// There are many places that don't use this variable so simply swapping
/// this variable is NOT ENOUGH to change the bundle ID. I just wanted to
/// avoid it in Zig coe as much as possible.
pub const BUNDLE_ID: &str = "com.mitchellh.ghostty";

/// True if we should have "slow" runtime safety checks. The initial motivation
/// for this was terminal page/pagelist integrity checks. These were VERY
/// slow but very thorough. But they made it so slow that the terminal couldn't
/// be used for real work. We'd love to have an option to run a build with
/// safety checks that could be used for real work. This lets us do that.
pub const SLOW_RUNTIME_SAFETY: bool = cfg!(debug_assertions);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Artifact {
    /// Standalone executable
    Exe,

    /// Embeddable library
    Lib,

    /// The WASM-targeted module.
    WasmModule,
}

impl Artifact {
    /// Zig inspects `builtin.output_mode`. A Rust crate is compiled once
    /// for both its lib and bin targets, so the port reports the Ghostty
    /// application build: an exe, except on wasm.
    pub const fn detect() -> Artifact {
        if cfg!(target_arch = "wasm32") {
            return Artifact::WasmModule;
        }

        Artifact::Exe
    }
}

/// True if runtime safety checks are enabled.
pub const IS_DEBUG: bool = cfg!(debug_assertions);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_cargo() {
        assert_eq!(VERSION.to_string(), VERSION_STRING);
        assert_eq!(
            VERSION.pre.is_some(),
            RELEASE_CHANNEL == ReleaseChannel::Tip
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_app_build() {
        assert_eq!(APP_RUNTIME, Runtime::Gtk);
        assert_eq!(RENDERER, RendererBackend::Opengl);
        assert_eq!(FONT_BACKEND, FontBackend::FontconfigFreetype);
        assert!(FONT_BACKEND.has_fontconfig());
        assert!(FONT_BACKEND.has_harfbuzz());
        assert!(FONT_BACKEND.has_freetype());
        assert!(!FONT_BACKEND.has_coretext());
        assert_eq!(ARTIFACT, Artifact::Exe);
        assert_eq!(EXE_ENTRYPOINT, ExeEntrypoint::Ghostty);
    }
}
