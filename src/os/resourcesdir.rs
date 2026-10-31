//! Port of `src/os/resourcesdir.zig`.
//!
//! STATUS: ported

use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourcesDir {
    /// Avoid accessing these directly, use the app() and host() methods
    /// instead.
    pub app_path: Option<String>,
    pub host_path: Option<String>,
}

impl ResourcesDir {
    /// Get the directory to the bundled resources directory accessible
    /// by the application.
    pub fn app(&self) -> Option<&str> {
        self.app_path.as_deref()
    }

    /// Get the directory to the bundled resources directory accessible
    /// by the host environment (i.e. for sandboxed applications). The
    /// returned directory might not be accessible from the application
    /// itself.
    ///
    /// In non-sandboxed environment, this should be the same as app().
    pub fn host(&self) -> Option<&str> {
        self.host_path.as_deref().or(self.app_path.as_deref())
    }
}

/// This is the sentinel value we look for in the path to know
/// we've found the resources directory.
const SENTINELS: &[&str] = if cfg!(windows) {
    &["terminfo/ghostty.terminfo"]
} else if cfg!(target_os = "macos") {
    &["terminfo/78/xterm-ghostty"]
} else if cfg!(target_os = "freebsd") {
    &["site-terminfo/g/ghostty", "site-terminfo/x/xterm-ghostty"]
} else {
    &["terminfo/g/ghostty", "terminfo/x/xterm-ghostty"]
};

/// Returns `GHOSTTY_RESOURCES_DIR` if it is set and non-empty.
fn env_resources_dir() -> Option<String> {
    std::env::var("GHOSTTY_RESOURCES_DIR")
        .ok()
        .filter(|d| !d.is_empty())
}

/// Gets the directory to the bundled resources directory, if it
/// exists (not all platforms or packages have it).
///
/// This is highly Ghostty-specific and can likely be generalized at
/// some point but we can cross that bridge if we ever need to.
pub fn resources_dir() -> Result<ResourcesDir, NoSpaceLeft> {
    // Use the GHOSTTY_RESOURCES_DIR environment variable in release builds.
    //
    // In debug builds we try using terminfo detection first instead, since
    // if debug Ghostty is launched by an older version of Ghostty, it
    // would inherit the old, stale resources of older Ghostty instead of the
    // freshly built ones.
    if !cfg!(debug_assertions) {
        if let Some(dir) = env_resources_dir() {
            return Ok(ResourcesDir {
                app_path: Some(dir),
                host_path: None,
            });
        }
    }

    // Get the path to our running binary
    let Ok(exe) = std::env::current_exe() else {
        return Ok(ResourcesDir::default());
    };
    let Some(exe) = exe.to_str().map(str::to_string) else {
        return Ok(ResourcesDir::default());
    };
    if let Some(found) = search_from_exe(&exe)? {
        return Ok(found);
    }

    // If terminfo detection failed in debug builds (somehow),
    // fallback and use the provided resources dir.
    if cfg!(debug_assertions) {
        if let Some(dir) = env_resources_dir() {
            return Ok(ResourcesDir {
                app_path: Some(dir),
                host_path: None,
            });
        }
    }

    Ok(ResourcesDir::default())
}

/// Climb the tree from the executable path looking for the terminfo
/// bundle as we expect it.
fn search_from_exe(exe: &str) -> Result<Option<ResourcesDir>, NoSpaceLeft> {
    let mut exe = Path::new(exe);
    let mut dir_buf = vec![0u8; 4096];
    while let Some(dir) = exe.parent() {
        if dir.as_os_str().is_empty() {
            break;
        }
        exe = dir;
        let Some(dir_str) = dir.to_str() else {
            return Ok(None);
        };

        // On MacOS, we look for the app bundle path.
        if cfg!(target_vendor = "apple") {
            for sentinel in SENTINELS {
                if let Some(v) = maybe_dir(&mut dir_buf, dir_str, "Contents/Resources", sentinel)? {
                    return Ok(Some(ResourcesDir {
                        app_path: Some(join(v, "ghostty")),
                        host_path: None,
                    }));
                }
            }
        }

        // On all platforms (except BSD), we look for a /usr/share style
        // path. This is valid even on Mac since there is nothing that
        // requires Ghostty to be in an app bundle.
        let share = if cfg!(target_os = "freebsd") {
            "local/share"
        } else {
            "share"
        };
        for sentinel in SENTINELS {
            if let Some(v) = maybe_dir(&mut dir_buf, dir_str, share, sentinel)? {
                return Ok(Some(ResourcesDir {
                    app_path: Some(join(v, "ghostty")),
                    host_path: None,
                }));
            }
        }
    }
    Ok(None)
}

fn join(a: &str, b: &str) -> String {
    Path::new(a).join(b).to_string_lossy().into_owned()
}

/// Zig: `error.NoSpaceLeft` from formatting into a too-small buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoSpaceLeft;

/// Little helper to check if the "base/sub/suffix" directory exists and
/// if so return true. The "suffix" is just used as a way to verify a
/// directory seems roughly right.
///
/// "buf" must be large enough to fit base + sub + suffix. This is generally
/// max_path_bytes so its not a big deal.
pub fn maybe_dir<'a>(
    buf: &'a mut [u8],
    base: &str,
    sub: &str,
    suffix: &str,
) -> Result<Option<&'a str>, NoSpaceLeft> {
    let path = format!("{base}/{sub}/{suffix}");
    if path.len() > buf.len() {
        return Err(NoSpaceLeft);
    }
    buf[..path.len()].copy_from_slice(path.as_bytes());

    if Path::new(&path).try_exists().unwrap_or(false) {
        let len = path.len() - suffix.len() - 1;
        return Ok(Some(std::str::from_utf8(&buf[..len]).unwrap()));
    }

    // Folder doesn't exist. If a different error happens its okay
    // we just ignore it and move on.
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_falls_back_to_app() {
        let rd = ResourcesDir {
            app_path: Some("/app".into()),
            host_path: None,
        };
        assert_eq!(Some("/app"), rd.app());
        assert_eq!(Some("/app"), rd.host());
        let rd = ResourcesDir {
            app_path: Some("/app".into()),
            host_path: Some("/host".into()),
        };
        assert_eq!(Some("/host"), rd.host());
        assert_eq!(None, ResourcesDir::default().host());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn search_finds_share_terminfo() {
        let tmp = crate::os::temp_dir::TempDir::init().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("share/terminfo/g")).unwrap();
        std::fs::write(root.join("share/terminfo/g/ghostty"), b"").unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let exe = root.join("bin/ghostty");
        let found = search_from_exe(exe.to_str().unwrap()).unwrap().unwrap();
        assert_eq!(
            Some(root.join("share/ghostty").to_str().unwrap()),
            found.app()
        );

        let mut buf = [0u8; 8];
        assert_eq!(
            Err(NoSpaceLeft),
            maybe_dir(&mut buf, "/x", "share", "terminfo/g/ghostty")
        );
        resources_dir().unwrap();
    }
}
