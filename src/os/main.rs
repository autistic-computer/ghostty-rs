//! Port of `src/os/main.zig`.
//!
//! STATUS: ported
//!
//! The "os" package contains utilities for interfacing with the operating
//! system. These aren't restricted to syscalls or low-level operations, but
//! also OS-specific features and conventions.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "cf_release_thread.rs"]
pub mod cf_release_thread;
#[path = "cgroup.rs"]
pub mod cgroup;
#[path = "dbus.rs"]
pub mod dbus;
#[path = "desktop.rs"]
pub mod desktop;
#[path = "edit.rs"]
pub mod edit;
#[path = "file.rs"]
pub mod file;
#[path = "flatpak.rs"]
pub mod flatpak;
#[path = "homedir.rs"]
pub mod homedir;
#[path = "hostname.rs"]
pub mod hostname;
#[path = "i18n.rs"]
pub mod i18n;
#[path = "i18n_locales.rs"]
pub mod i18n_locales;
#[path = "kernel_info.rs"]
pub mod kernel_info;
#[path = "locale.rs"]
pub mod locale;
#[path = "mach.rs"]
pub mod mach;
#[path = "macos.rs"]
pub mod macos;
#[path = "mouse.rs"]
pub mod mouse;
#[path = "open.rs"]
pub mod open;
#[path = "passwd.rs"]
pub mod passwd;
#[path = "path.rs"]
pub mod path;
#[path = "pipe.rs"]
pub mod pipe;
#[path = "resourcesdir.rs"]
pub mod resourcesdir;
#[path = "shell.rs"]
pub mod shell;
#[path = "stderr.rs"]
pub mod stderr;
#[path = "string_encoding.rs"]
pub mod string_encoding;
#[path = "systemd.rs"]
pub mod systemd;
#[path = "temp_dir.rs"]
pub mod temp_dir;
#[path = "uri.rs"]
pub mod uri;
#[path = "wasm.rs"]
pub mod wasm;
#[path = "windows.rs"]
pub mod windows;
#[path = "xdg.rs"]
pub mod xdg;
// @generated-mods-end

/// An environment variable map (Zig: `std.process.Environ.Map`).
///
/// The Rust port uses UTF-8 `String`s; variables whose name or value is
/// not valid UTF-8 are skipped by [`environ_map`].
pub type EnvironMap = std::collections::HashMap<String, String>;

/// A snapshot of the current process environment (Zig:
/// `global.environMap()` / `testing.environ.createMap`).
pub fn environ_map() -> EnvironMap {
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}

// Functions and types
#[cfg(target_os = "macos")]
pub use self::cf_release_thread::Thread as CFReleaseThread;
pub use self::dbus::launched_by_dbus_activation;
pub use self::desktop::{desktop_environment, launched_from_desktop, DesktopEnvironment};
pub use self::edit::get_config_edit_command;
pub use self::file::{fix_max_files, random_tmp_path, restore_max_files, Rlimit as rlimit};
pub use self::flatpak::is_flatpak;
#[cfg(all(target_os = "linux", feature = "gtk"))]
pub use self::flatpak::FlatpakHostCommand;
pub use self::homedir::{expand_home, home};
pub use self::kernel_info::get_kernel_info;
pub use self::locale::ensure_locale;
pub use self::mouse::click_interval;
pub use self::open::{open, OpenUrlKind as OpenType};
pub use self::pipe::pipe;
pub use self::resourcesdir::{resources_dir, ResourcesDir};
pub use self::shell::ShellEscapeWriter;
pub use self::systemd::launched_by_systemd;
pub use self::temp_dir::TempDir;
