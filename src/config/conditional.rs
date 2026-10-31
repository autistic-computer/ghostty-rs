//! Port of `src/config/conditional.zig`.
//!
//! STATUS: ported

use crate::cli::args::{cli_enum, CliEnum};

/// Conditionals in Ghostty configuration are based on a static, typed
/// state of the world instead of a dynamic key-value set. This simplifies
/// the implementation, allows for better type checking, and enables a
/// typed C API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    /// The theme of the underlying OS desktop environment.
    pub theme: Theme,

    /// The target OS of the current build.
    pub os: OsTag,
}

impl Default for State {
    fn default() -> Self {
        State {
            theme: Theme::Light,
            os: OsTag::current(),
        }
    }
}

cli_enum! {
    pub enum Theme {
        Light => "light",
        Dark => "dark",
    }
}

cli_enum! {
    /// Stand-in for Zig's `std.Target.Os.Tag` (the subset of OS tags that
    /// Rust can target); the tag names match Zig's.
    pub enum OsTag {
        Freestanding => "freestanding",
        Other => "other",
        Dragonfly => "dragonfly",
        Freebsd => "freebsd",
        Fuchsia => "fuchsia",
        Haiku => "haiku",
        Hurd => "hurd",
        Illumos => "illumos",
        Linux => "linux",
        Netbsd => "netbsd",
        Openbsd => "openbsd",
        Windows => "windows",
        Uefi => "uefi",
        Ios => "ios",
        Macos => "macos",
        Tvos => "tvos",
        Visionos => "visionos",
        Watchos => "watchos",
        Wasi => "wasi",
        Emscripten => "emscripten",
    }
}

impl OsTag {
    /// The target OS of the current build (Zig `builtin.target.os.tag`).
    pub const fn current() -> OsTag {
        if cfg!(target_os = "linux") || cfg!(target_os = "android") {
            OsTag::Linux
        } else if cfg!(target_os = "macos") {
            OsTag::Macos
        } else if cfg!(target_os = "ios") {
            OsTag::Ios
        } else if cfg!(target_os = "tvos") {
            OsTag::Tvos
        } else if cfg!(target_os = "visionos") {
            OsTag::Visionos
        } else if cfg!(target_os = "watchos") {
            OsTag::Watchos
        } else if cfg!(target_os = "windows") {
            OsTag::Windows
        } else if cfg!(target_os = "freebsd") {
            OsTag::Freebsd
        } else if cfg!(target_os = "netbsd") {
            OsTag::Netbsd
        } else if cfg!(target_os = "openbsd") {
            OsTag::Openbsd
        } else if cfg!(target_os = "dragonfly") {
            OsTag::Dragonfly
        } else if cfg!(target_os = "illumos") {
            OsTag::Illumos
        } else if cfg!(target_os = "haiku") {
            OsTag::Haiku
        } else if cfg!(target_os = "fuchsia") {
            OsTag::Fuchsia
        } else if cfg!(target_os = "hurd") {
            OsTag::Hurd
        } else if cfg!(target_os = "uefi") {
            OsTag::Uefi
        } else if cfg!(target_os = "wasi") {
            OsTag::Wasi
        } else if cfg!(target_os = "emscripten") {
            OsTag::Emscripten
        } else if cfg!(target_os = "none") {
            OsTag::Freestanding
        } else {
            OsTag::Other
        }
    }
}

impl State {
    /// Tests the conditional against the state and returns true if it matches.
    pub fn r#match(&self, cond: &Conditional) -> bool {
        // The raw value of the state field. Since all values are enums
        // currently then we can just use the tag name. If we introduce
        // non-enum state values then this needs to be extended.
        let value: &str = match cond.key {
            Key::Theme => self.theme.tag_name(),
            Key::Os => self.os.tag_name(),
        };

        match cond.op {
            Op::Eq => value == cond.value,
            Op::Ne => value != cond.value,
        }
    }
}

cli_enum! {
    /// An enum of the available conditional configuration keys (one per
    /// field of [`State`]).
    pub enum Key {
        Theme => "theme",
        Os => "os",
    }
}

/// A single conditional that can be true or false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conditional {
    pub key: Key,
    pub op: Op,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    Eq,
    Ne,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditional_enum_match() {
        let state = State {
            theme: Theme::Dark,
            ..Default::default()
        };
        assert!(state.r#match(&Conditional {
            key: Key::Theme,
            op: Op::Eq,
            value: "dark".into(),
        }));
        assert!(!state.r#match(&Conditional {
            key: Key::Theme,
            op: Op::Ne,
            value: "dark".into(),
        }));
        assert!(state.r#match(&Conditional {
            key: Key::Theme,
            op: Op::Ne,
            value: "light".into(),
        }));
    }

    #[test]
    fn conditional_os_match() {
        let state = State::default();
        assert!(state.r#match(&Conditional {
            key: Key::Os,
            op: Op::Eq,
            value: OsTag::current().tag_name().into(),
        }));
    }
}
