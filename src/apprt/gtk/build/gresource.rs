//! Port of `src/apprt/gtk/build/gresource.zig`.
//!
//! STATUS: ported
//!
//! This file contains a binary helper that builds our gresource XML
//! file that we can then use with `glib-compile-resources`.
//!
//! This binary is expected to be run from the Ghostty source root.
//! Litmus test: `src/apprt/gtk` should exist relative to the pwd.
//!
//! In the port, `build.rs` includes this file (and `info.rs`) with
//! `#[path]` and calls [`main`] to write the XML before running
//! `glib-compile-resources`; the crate itself uses the constants,
//! [`blueprint`] and [`register`]. The Zig binary resolves files against
//! the working directory; the port takes the source root as a parameter
//! (`build.rs` passes `CARGO_MANIFEST_DIR`). Unlike Zig, the compiled `.ui`
//! files are committed next to their `.blp` sources (see
//! `tools/gen_gtk_ui.sh`), so `build.rs` passes [`UI_PATH`] as the
//! `--sourcedir` for them.
#![allow(dead_code)]

use std::fmt::Write as _;
use std::io;
use std::path::Path;

use super::info as build_info;

/// The path to the Blueprint files. The folder structure is expected to be
/// `{version}/{name}.blp` where `version` is the major and minor
/// minimum adwaita version.
pub const UI_PATH: &str = "src/apprt/gtk/ui";

/// The path to the CSS files.
pub const CSS_PATH: &str = "src/apprt/gtk/css";

/// The possible icon sizes we'll embed into the gresource file.
/// If any size doesn't exist then it will be an error. We could
/// infer this completely from available files but we wouldn't be
/// able to error when they don't exist that way.
pub const ICON_SIZES: &[u32] = &[16, 32, 128, 256, 512, 1024];

/// `struct { major: u16, minor: u16 }`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdwaitaVersion {
    pub major: u16,
    pub minor: u16,
}

/// The oldest libadwaita Ghostty supports. Individual blueprints can
/// require newer than this, never older, so the build checks the headers
/// against whichever of the two is higher.
pub const MINIMUM_ADWAITA: AdwaitaVersion = AdwaitaVersion { major: 1, minor: 7 };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blueprint {
    pub major: u16,
    pub minor: u16,
    pub name: &'static str,
}

const fn bp(major: u16, minor: u16, name: &'static str) -> Blueprint {
    Blueprint { major, minor, name }
}

/// The blueprint files that we will embed into the gresource file.
/// We can't look these up at runtime [easily] because we require the
/// compiled UI files as input. We can refactor this lator to maybe do
/// all of this automatically and ensure we have the right dependencies
/// setup in the build system.
///
/// These will be asserted to exist at runtime.
pub const BLUEPRINTS: &[Blueprint] = &[
    bp(1, 0, "imgui-widget"),
    bp(1, 0, "inspector-widget"),
    bp(1, 0, "key-state-overlay"),
    bp(1, 0, "resize-overlay"),
    bp(1, 0, "search-overlay"),
    bp(1, 0, "split-tree"),
    bp(1, 0, "split-tree-split"),
    bp(1, 0, "surface"),
    bp(1, 0, "surface-scrolled-window"),
    bp(1, 0, "tab"),
    bp(1, 3, "debug-warning"),
    bp(1, 3, "surface-child-exited"),
    bp(1, 4, "inspector-window"),
    bp(1, 4, "window"),
    bp(1, 5, "clipboard-confirmation-dialog"),
    bp(1, 5, "close-confirmation-dialog"),
    bp(1, 5, "command-palette"),
    bp(1, 5, "config-errors-dialog"),
    bp(1, 5, "title-dialog"),
];

/// CSS files in css_path
pub const CSS: &[&str] = &["style.css"];

/// The list of filepaths that we depend on. Used for the build
/// system to have proper caching.
///
/// (A comptime array in Zig.)
pub fn file_inputs() -> Vec<String> {
    let mut deps = Vec::with_capacity(ICON_SIZES.len() * 2 + BLUEPRINTS.len() + CSS.len());
    for size in ICON_SIZES {
        deps.push(format!("images/gnome/{size}.png"));
        deps.push(format!("images/gnome/{}.png", size * 2));
    }
    for bp in BLUEPRINTS {
        deps.push(format!("{UI_PATH}/{}.{}/{}.blp", bp.major, bp.minor, bp.name));
    }
    for name in CSS {
        deps.push(format!("{CSS_PATH}/{name}"));
    }
    deps
}

/// Returns the matching blueprint resource path for the given blueprint
/// definition.
///
/// Zig fails at compile time if the blueprint is not found; the port panics
/// with the same message ("invalid blueprint").
pub fn blueprint(bp: Blueprint) -> String {
    for candidate in BLUEPRINTS {
        if candidate.major == bp.major && candidate.minor == bp.minor && candidate.name == bp.name {
            return format!(
                "{}/ui/{}.{}/{}.ui",
                build_info::RESOURCE_PATH,
                candidate.major,
                candidate.minor,
                candidate.name,
            );
        }
    }

    panic!("invalid blueprint");
}

/// Generates the gresource XML. `root` is the Ghostty source root (the
/// working directory of the Zig binary).
pub fn main(root: &Path) -> io::Result<String> {
    let mut writer = String::new();
    writer.push_str(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <gresources>\n",
    );

    gen_root(root, &mut writer)?;
    gen_icons(root, &mut writer)?;
    gen_ui(root, &mut writer)?;

    writer.push_str("</gresources>\n");
    Ok(writer)
}

/// `std.Io.Dir.access`: errors if the file does not exist.
fn access(root: &Path, source: &str) -> io::Result<()> {
    std::fs::metadata(root.join(source)).map(|_| ()).map_err(|err| {
        io::Error::new(err.kind(), format!("{source}: {err}"))
    })
}

/// Generate the icon resources. This works by looking up all the icons
/// specified by `icon_sizes` in `images/icons/`. They are asserted to exist
/// by trying to access the file.
fn gen_icons(root: &Path, writer: &mut String) -> io::Result<()> {
    writeln!(writer, "  <gresource prefix=\"{}/icons\">", build_info::RESOURCE_PATH).unwrap();

    for size in ICON_SIZES {
        // 1x
        {
            let alias = format!("{size}x{size}");
            let source = format!("images/gnome/{size}.png");
            access(root, &source)?;
            writeln!(
                writer,
                "    <file alias=\"{alias}/apps/{}.png\">{source}</file>",
                build_info::BASE_APPLICATION_ID,
            )
            .unwrap();
        }

        // 2x
        {
            let alias = format!("{size}x{size}@2");
            let source = format!("images/gnome/{}.png", size * 2);
            access(root, &source)?;
            writeln!(
                writer,
                "    <file alias=\"{alias}/apps/{}.png\">{source}</file>",
                build_info::BASE_APPLICATION_ID,
            )
            .unwrap();
        }
    }

    writer.push_str("  </gresource>\n");
    Ok(())
}

/// Generate the resources at the root prefix.
fn gen_root(root: &Path, writer: &mut String) -> io::Result<()> {
    writeln!(writer, "  <gresource prefix=\"{}\">", build_info::RESOURCE_PATH).unwrap();

    for name in CSS {
        let source = format!("{CSS_PATH}/{name}");
        access(root, &source)?;
        writeln!(
            writer,
            "    <file compressed=\"true\" alias=\"{name}\">{source}</file>",
        )
        .unwrap();
    }

    writer.push_str("  </gresource>\n");
    Ok(())
}

/// Generate all the UI resources.
///
/// Each file is written as a *relative* path, `{major}.{minor}/{name}.ui`,
/// which `glib-compile-resources` resolves against a `--sourcedir`. An
/// absolute path into the Zig cache would change whenever the step that
/// produced the `.ui` did, and this XML with it.
///
/// The compiled `.ui` files are not inputs here for the same reason. We
/// check that the blueprint *sources* exist instead, as `genRoot` and
/// `genIcons` do for the CSS and icons.
fn gen_ui(root: &Path, writer: &mut String) -> io::Result<()> {
    writeln!(writer, "  <gresource prefix=\"{}/ui\">", build_info::RESOURCE_PATH).unwrap();

    for bp in BLUEPRINTS {
        let source = format!("{UI_PATH}/{}.{}/{}.blp", bp.major, bp.minor, bp.name);
        access(root, &source)?;

        let alias = format!("{}.{}/{}.ui", bp.major, bp.minor, bp.name);
        writeln!(
            writer,
            "    <file compressed=\"true\" preprocess=\"xml-stripblanks\" alias=\"{alias}\">{alias}</file>",
        )
        .unwrap();
    }

    writer.push_str("  </gresource>\n");
    Ok(())
}

/// Registers the GResource bundle that `build.rs` compiled from the XML
/// above with GIO, so the resources are reachable under
/// [`build_info::RESOURCE_PATH`]. (In Zig the bundle is a generated C file
/// linked into the binary, which registers itself through a constructor.)
///
/// `build.rs` sets `cfg(ghostty_gresource)` once the bundle is compiled (a
/// `feature` cfg would also apply to `build.rs`, which includes this file).
#[cfg(ghostty_gresource)]
pub fn register() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        gio::resources_register_include!("ghostty.gresource")
            .expect("failed to register the ghostty GResource bundle");
    });
}

#[cfg(all(test, feature = "gtk"))]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn file_inputs_exist() {
        let inputs = file_inputs();
        assert_eq!(inputs.len(), ICON_SIZES.len() * 2 + BLUEPRINTS.len() + CSS.len());
        for path in inputs {
            assert!(root().join(&path).exists(), "{path} missing");
        }
    }

    #[test]
    fn blueprint_resource_path() {
        assert_eq!(
            blueprint(Blueprint { major: 1, minor: 5, name: "title-dialog" }),
            "/com/mitchellh/ghostty/ui/1.5/title-dialog.ui"
        );
    }

    #[test]
    #[should_panic(expected = "invalid blueprint")]
    fn blueprint_invalid() {
        blueprint(Blueprint { major: 1, minor: 0, name: "title-dialog" });
    }

    #[test]
    fn xml_lists_everything() {
        let xml = main(root()).unwrap();
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains(
            "<file compressed=\"true\" alias=\"style.css\">src/apprt/gtk/css/style.css</file>"
        ));
        assert!(xml.contains(
            "<file alias=\"16x16@2/apps/com.mitchellh.ghostty.png\">images/gnome/32.png</file>"
        ));
        assert!(xml.contains("alias=\"1.4/window.ui\">1.4/window.ui</file>"));
    }

    #[test]
    #[cfg(ghostty_gresource)]
    fn registered_resources_are_reachable() {
        register();
        for bp in BLUEPRINTS {
            let path = blueprint(*bp);
            assert!(
                gio::resources_lookup_data(&path, gio::ResourceLookupFlags::NONE).is_ok(),
                "{path} not in the GResource bundle"
            );
        }
        assert!(gio::resources_lookup_data(
            "/com/mitchellh/ghostty/style.css",
            gio::ResourceLookupFlags::NONE
        )
        .is_ok());
        assert!(gio::resources_lookup_data(
            "/com/mitchellh/ghostty/icons/128x128/apps/com.mitchellh.ghostty.png",
            gio::ResourceLookupFlags::NONE
        )
        .is_ok());
    }
}
