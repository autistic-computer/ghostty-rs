//! Port of `src/cli/version.zig`.
//!
//! STATUS: ported
//!
//! Port notes:
//! * The Zig version line reports the Rust compiler instead (from the
//!   `RUSTC_VERSION` build-time environment variable when set).
//! * Zig prints `{}` of enums and types with their fully qualified names
//!   (`builtin.OptimizeMode.ReleaseFast`, `apprt.runtime.Runtime.gtk`, ...);
//!   the same strings are built here.

use std::io::{self, BufWriter, IsTerminal, Write};

use super::args::cli_struct;
use super::ghostty::RunResult;
use crate::build_config;
use crate::global;
use crate::os as internal_os;

cli_struct! {
    #[derive(Debug, Clone)]
    pub struct Options {}
}

/// The help text of this action (Zig `help_strings.Action.version`).
pub const HELP: &str = "The `version` command is used to display information about Ghostty. Recognized as
either `+version` or `--version`.";

/// The `version` command is used to display information about Ghostty. Recognized as
/// either `+version` or `--version`.
pub fn run() -> RunResult {
    let stdout_file = io::stdout();
    let tty = stdout_file.is_terminal();
    let mut stdout = BufWriter::with_capacity(1024, stdout_file.lock());

    let environ_map = global::environ_map();

    if tty {
        if let Some(commit_hash) = build_config::VERSION.build {
            write!(
                stdout,
                "\x1b]8;;https://github.com/ghostty-org/ghostty/commit/{}\x1b\\",
                commit_hash
            )?;
        }
    }
    write!(stdout, "Ghostty {}\n\n", build_config::VERSION_STRING)?;
    if tty {
        write!(stdout, "\x1b]8;;\x1b\\")?;
    }

    writeln!(stdout, "Version")?;
    writeln!(stdout, "  - version: {}", build_config::VERSION_STRING)?;
    writeln!(stdout, "  - channel: {}", build_config::RELEASE_CHANNEL)?;

    writeln!(stdout, "Build Config")?;
    writeln!(
        stdout,
        "  - Rust version  : {}",
        option_env!("RUSTC_VERSION").unwrap_or("unknown")
    )?;
    writeln!(
        stdout,
        "  - build mode    : builtin.OptimizeMode.{}",
        build_config::MODE_STRING
    )?;
    writeln!(
        stdout,
        "  - app runtime   : apprt.runtime.Runtime.{}",
        build_config::APP_RUNTIME
    )?;
    writeln!(
        stdout,
        "  - font engine   : font.backend.Backend.{}",
        build_config::FONT_BACKEND
    )?;
    writeln!(stdout, "  - renderer      : {}", renderer_type_name())?;
    writeln!(stdout, "  - libxev        : {}", crate::pkg::xev::backend())?;
    if build_config::APP_RUNTIME == crate::apprt::runtime::Runtime::Gtk {
        if cfg!(target_os = "linux") {
            let kernel_info = internal_os::get_kernel_info();
            writeln!(
                stdout,
                "  - kernel version: {}",
                kernel_info.as_deref().unwrap_or("Kernel information unavailable")
            )?;
        }
        writeln!(
            stdout,
            "  - desktop env   : {}",
            desktop_environment_tag(internal_os::desktop_environment(&environ_map))
        )?;
        writeln!(stdout, "  - GTK version   :")?;
        writeln!(stdout, "    build         : {}", gtk_version::COMPTIME_VERSION)?;
        writeln!(stdout, "    runtime       : {}", gtk_version::get_runtime_version())?;
        writeln!(stdout, "  - libadwaita    : enabled")?;
        writeln!(stdout, "    build         : {}", adw_version::COMPTIME_VERSION)?;
        writeln!(stdout, "    runtime       : {}", adw_version::get_runtime_version())?;
        if build_config::X11 {
            writeln!(stdout, "  - libX11        : enabled")?;
        } else {
            writeln!(stdout, "  - libX11        : disabled")?;
        }

        // We say `libwayland` since it is possible to build Ghostty without
        // Wayland integration but with Wayland-enabled GTK
        if build_config::WAYLAND {
            writeln!(stdout, "  - libwayland    : enabled")?;
        } else {
            writeln!(stdout, "  - libwayland    : disabled")?;
        }
    }

    // Don't forget to flush!
    stdout.flush()?;
    Ok(0)
}

/// Zig formats the `renderer.Renderer` type with `{}`, which prints its
/// type name.
fn renderer_type_name() -> String {
    use crate::renderer::backend::Backend;
    let name = match build_config::RENDERER {
        Backend::Opengl => "OpenGL",
        Backend::Metal => "Metal",
    };
    format!("renderer.generic.Renderer(renderer.{})", name)
}

/// Zig `{t}` (tag name) of `DesktopEnvironment`.
fn desktop_environment_tag(de: internal_os::DesktopEnvironment) -> &'static str {
    use internal_os::DesktopEnvironment as D;
    match de {
        D::Gnome => "gnome",
        D::Macos => "macos",
        D::Other => "other",
        D::Windows => "windows",
    }
}

/// Loads `major.minor.micro` from the version getters of a shared library
/// at runtime.
#[cfg(not(feature = "gtk"))]
fn runtime_lib_version(libs: &[&str], getters: [&str; 3]) -> String {
    use std::ffi::CString;
    for lib in libs {
        let Ok(lib_c) = CString::new(*lib) else { continue };
        // SAFETY: dlopen/dlsym are called with valid NUL-terminated
        // strings; the getters have the C signature `guint (*)(void)`.
        unsafe {
            let handle = libc::dlopen(lib_c.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
            if handle.is_null() {
                continue;
            }
            let mut parts = [0u32; 3];
            let mut ok = true;
            for (i, getter) in getters.iter().enumerate() {
                let sym = CString::new(*getter).unwrap();
                let f = libc::dlsym(handle, sym.as_ptr());
                if f.is_null() {
                    ok = false;
                    break;
                }
                let f: extern "C" fn() -> u32 = std::mem::transmute(f);
                parts[i] = f();
            }
            libc::dlclose(handle);
            if ok {
                return format!("{}.{}.{}", parts[0], parts[1], parts[2]);
            }
        }
    }
    "unknown".to_owned()
}

#[cfg(feature = "gtk")]
use crate::apprt::gtk::{adw_version, gtk_version};

/// Without the `gtk` feature GTK isn't linked: report the versions of the
/// system libraries found at runtime instead of
/// `crate::apprt::gtk::gtk_version`.
#[cfg(not(feature = "gtk"))]
mod gtk_version {
    /// The GTK version the binary was built against (not linked in the
    /// Rust port's default build).
    pub const COMPTIME_VERSION: &str = "unknown";

    pub fn get_runtime_version() -> String {
        super::runtime_lib_version(
            &["libgtk-4.so.1", "libgtk-4.so"],
            ["gtk_get_major_version", "gtk_get_minor_version", "gtk_get_micro_version"],
        )
    }
}

/// Without the `gtk` feature libadwaita isn't linked (see [`gtk_version`]).
#[cfg(not(feature = "gtk"))]
mod adw_version {
    /// The libadwaita version the binary was built against (not linked in
    /// the Rust port's default build).
    pub const COMPTIME_VERSION: &str = "unknown";

    pub fn get_runtime_version() -> String {
        super::runtime_lib_version(
            &["libadwaita-1.so.0", "libadwaita-1.so"],
            ["adw_get_major_version", "adw_get_minor_version", "adw_get_micro_version"],
        )
    }
}
