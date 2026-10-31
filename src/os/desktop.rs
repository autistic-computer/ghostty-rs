//! Port of `src/os/desktop.zig`.
//!
//! STATUS: ported

use super::EnvironMap;

#[cfg(target_os = "macos")]
const ARTIFACT_IS_LIB: bool = matches!(
    crate::build_config::ARTIFACT,
    crate::build_config::Artifact::Lib
);

/// Returns true if the program was launched from a desktop environment.
///
/// On macOS, this returns true if the program was launched from Finder.
///
/// On Linux GTK, this returns true if the program was launched using the
/// desktop file. This also includes when `gtk-launch` is used because I
/// can't find a way to distinguish the two scenarios.
///
/// For other platforms and app runtimes, this returns false.
pub fn launched_from_desktop() -> bool {
    // macOS apps launched from finder or `open` always have the init
    // process as their parent.
    #[cfg(target_os = "macos")]
    {
        // This special case is so that if we launch the app via the
        // app bundle (i.e. via open) then we still treat it as if it
        // was launched from the desktop.
        if ARTIFACT_IS_LIB {
            // Source can be "app", "cli", or "zig_run". We assume
            // its the desktop only if its "app". We may want to do
            // "zig_run" but at the moment there's no reason.
            if std::env::var_os("GHOSTTY_MAC_LAUNCH_SOURCE").is_some_and(|s| s == "app") {
                return true;
            }
        }

        // SAFETY: getppid has no preconditions.
        return unsafe { libc::getppid() } == 1;
    }

    // On Linux and BSD, GTK sets GIO_LAUNCHED_DESKTOP_FILE and
    // GIO_LAUNCHED_DESKTOP_FILE_PID. We only check the latter to see if
    // we match the PID and assume that if we do, we were launched from
    // the desktop file. Pid comparing catches the scenario where
    // another terminal was launched from a desktop file and then launches
    // Ghostty and Ghostty inherits the env.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let Some(gio_pid_str) = std::env::var_os("GIO_LAUNCHED_DESKTOP_FILE_PID") else {
            return false;
        };

        // SAFETY: getpid has no preconditions.
        let pid = unsafe { libc::getpid() };
        let Some(gio_pid) = gio_pid_str
            .to_str()
            .and_then(|s| s.parse::<libc::pid_t>().ok())
        else {
            return false;
        };

        return gio_pid == pid;
    }

    // TODO: This should have some logic to detect this.
    #[cfg(windows)]
    return false;

    // iPhone/iPad is always launched from the "desktop"
    #[cfg(target_os = "ios")]
    return true;

    #[allow(unreachable_code)]
    false
}

/// The list of desktop environments that we detect. New Linux desktop
/// environments should only be added to this list if there's a specific
/// reason to differentiate between `gnome` and `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopEnvironment {
    Gnome,
    Macos,
    Other,
    Windows,
}

/// Detect what desktop environment we are running under. This is mainly
/// used on Linux and BSD to enable or disable certain features but there
/// may be more uses in the future.
pub fn desktop_environment(environ_map: &EnvironMap) -> DesktopEnvironment {
    if cfg!(target_os = "macos") {
        return DesktopEnvironment::Macos;
    }
    if cfg!(windows) {
        return DesktopEnvironment::Windows;
    }
    if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        // Use $XDG_SESSION_DESKTOP to determine what DE we are using on Linux
        // https://www.freedesktop.org/software/systemd/man/latest/pam_systemd.html#desktop=
        if let Some(sd) = environ_map.get("XDG_SESSION_DESKTOP") {
            if sd.eq_ignore_ascii_case("gnome") {
                return DesktopEnvironment::Gnome;
            }
            if sd.eq_ignore_ascii_case("gnome-xorg") {
                return DesktopEnvironment::Gnome;
            }
        }

        // If $XDG_SESSION_DESKTOP is not set, or doesn't match any known
        // DE, check $XDG_CURRENT_DESKTOP. $XDG_CURRENT_DESKTOP is a
        // colon-separated list of up to three desktop names, although we
        // only look at the first.
        // https://specifications.freedesktop.org/desktop-entry-spec/latest/recognized-keys.html
        if let Some(cd) = environ_map.get("XDG_CURRENT_DESKTOP") {
            let cd_first = cd.split(':').next().unwrap_or("");
            if cd_first.eq_ignore_ascii_case("gnome") {
                return DesktopEnvironment::Gnome;
            }
        }

        return DesktopEnvironment::Other;
    }
    DesktopEnvironment::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_environment_test() {
        // Always run these tests with a blank environment map so that we
        // don't get any failures from values in the "real" environment
        // leaking in.
        if cfg!(target_os = "macos") {
            assert_eq!(
                DesktopEnvironment::Macos,
                desktop_environment(&EnvironMap::new())
            );
        } else if cfg!(windows) {
            assert_eq!(
                DesktopEnvironment::Windows,
                desktop_environment(&EnvironMap::new())
            );
        } else if cfg!(any(target_os = "linux", target_os = "freebsd")) {
            let with = |k: &str, v: &str| {
                let mut m = EnvironMap::new();
                m.insert(k.to_string(), v.to_string());
                desktop_environment(&m)
            };
            assert_eq!(
                DesktopEnvironment::Gnome,
                with("XDG_SESSION_DESKTOP", "gnome")
            );
            assert_eq!(
                DesktopEnvironment::Gnome,
                with("XDG_SESSION_DESKTOP", "gnome-xorg")
            );
            assert_eq!(
                DesktopEnvironment::Other,
                with("XDG_SESSION_DESKTOP", "foobar")
            );
            assert_eq!(
                DesktopEnvironment::Gnome,
                with("XDG_CURRENT_DESKTOP", "GNOME")
            );
            assert_eq!(
                DesktopEnvironment::Other,
                with("XDG_CURRENT_DESKTOP", "FOOBAR")
            );
            assert_eq!(
                DesktopEnvironment::Other,
                desktop_environment(&EnvironMap::new())
            );
        } else {
            assert_eq!(
                DesktopEnvironment::Other,
                desktop_environment(&EnvironMap::new())
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn launched_from_desktop_matches_env() {
        let expected = std::env::var("GIO_LAUNCHED_DESKTOP_FILE_PID")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            == Some(std::process::id());
        assert_eq!(expected, launched_from_desktop());
    }
}
