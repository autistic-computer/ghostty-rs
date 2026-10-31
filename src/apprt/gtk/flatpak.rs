//! Port of `src/apprt/gtk/flatpak.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

use crate::os::resourcesdir::NoSpaceLeft;
use crate::os::{self as internal_os, ResourcesDir};

use crate::build_config;

pub fn resources_dir() -> Result<ResourcesDir, NoSpaceLeft> {
    if build_config::FLATPAK {
        // Only consult Flatpak runtime data for host case.
        if internal_os::is_flatpak() {
            let mut result = ResourcesDir {
                app_path: Some("/app/share/ghostty".to_owned()),
                host_path: None,
            };

            let keyfile = glib::KeyFile::new();

            if keyfile
                .load_from_file("/.flatpak-info", glib::KeyFileFlags::NONE)
                .is_err()
            {
                return Ok(result);
            }
            let Ok(app_dir) = keyfile.string("Instance", "app-path") else {
                return Ok(result);
            };

            result.host_path = Some(
                std::path::Path::new(app_dir.as_str())
                    .join("share")
                    .join("ghostty")
                    .to_string_lossy()
                    .into_owned(),
            );
            return Ok(result);
        }
    }

    internal_os::resources_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_os_resources_dir_outside_flatpak() {
        assert_eq!(resources_dir().ok(), internal_os::resources_dir().ok());
    }
}
