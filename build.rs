//! Cargo build script. Replaces the parts of ghostty's Zig build
//! (`build.zig`, `src/build/*.zig`, `pkg/*/build.zig`) that link C code.

// The GTK apprt's gresource XML generator (`src/apprt/gtk/build/`), shared
// with the crate.
#[allow(dead_code)]
#[path = "src/apprt/gtk/build/info.rs"]
mod info;
#[allow(dead_code, unexpected_cfgs)]
#[path = "src/apprt/gtk/build/gresource.rs"]
mod gresource;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(ghostty_gresource)");
    // `tools/build_lib_vt.sh` builds libghostty-vt (the `ghostty_*` terminal
    // C API, like `zig build -Demit-lib-vt`) with `GHOSTTY_LIB_VT=1`. Like
    // the Zig lib-vt, that library uses none of the app's C libraries
    // (freetype, harfbuzz, fontconfig, oniguruma, EGL, glad, stb), so they
    // are neither compiled nor required then; the script's link step
    // garbage-collects everything the C API does not reach, and fails if
    // the C API ever started to depend on one of them.
    println!("cargo:rerun-if-env-changed=GHOSTTY_LIB_VT");
    println!("cargo:rustc-check-cfg=cfg(ghostty_lib_vt)");
    if std::env::var_os("GHOSTTY_LIB_VT").is_some() {
        lib_vt_options();
    } else {
        pkg_c_libraries();
        stb();
    }
    dcimgui();
    app_version();
    if std::env::var_os("CARGO_FEATURE_GTK").is_some() {
        gtk_resources();
    }
}

/// The libghostty-vt build options (`GhosttyZig.zig`'s `vt_c` module): the
/// crate is compiled with `--cfg ghostty_lib_vt`, which selects
/// `terminal::build_options::Options::lib_vt()`, and the library version
/// (`build.zig` `lib_version`, or `GHOSTTY_LIB_VT_VERSION` like Zig's
/// `-Dlib-version-string`) is passed split into its semantic version parts.
fn lib_vt_options() {
    println!("cargo:rustc-cfg=ghostty_lib_vt");
    println!("cargo:rerun-if-env-changed=GHOSTTY_LIB_VT_VERSION");
    let version = std::env::var("GHOSTTY_LIB_VT_VERSION").unwrap_or_else(|_| "0.1.0-dev".into());
    let (rest, build) = match version.split_once('+') {
        Some((rest, build)) => (rest, build),
        None => (version.as_str(), ""),
    };
    let (core, pre) = match rest.split_once('-') {
        Some((core, pre)) => (core, pre),
        None => (rest, ""),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        panic!("GHOSTTY_LIB_VT_VERSION must be a semantic version: {version}");
    }
    for (name, value) in [
        ("STRING", version.as_str()),
        ("MAJOR", parts[0]),
        ("MINOR", parts[1]),
        ("PATCH", parts[2]),
        ("PRE", pre),
        ("BUILD", build),
    ] {
        println!("cargo:rustc-env=GHOSTTY_LIB_VT_VERSION_{name}={value}");
    }
}

/// The GTK apprt's build steps (`gtkDistResources` in
/// `src/build/SharedDeps.zig`):
///
/// * exports the GTK and libadwaita versions we build against as
///   `GHOSTTY_GTK_VERSION`/`GHOSTTY_ADW_VERSION` (the `GTK_*_VERSION` and
///   `ADW_*_VERSION` header macros in Zig; see `gtk_version.rs` and
///   `adw_version.rs`), and
/// * compiles the GResource bundle (CSS, icons and the GtkBuilder `.ui`
///   files) with `glib-compile-resources` into `$OUT_DIR/ghostty.gresource`,
///   which `gresource::register` embeds. The `.ui` files are generated from
///   the Blueprints ahead of time by `tools/gen_gtk_ui.sh` and committed, so
///   the build never needs `blueprint-compiler`.
fn gtk_resources() {
    use std::path::PathBuf;
    use std::process::Command;

    for (lib, var) in [
        ("gtk4", "GHOSTTY_GTK_VERSION"),
        ("libadwaita-1", "GHOSTTY_ADW_VERSION"),
    ] {
        match pkg_config::Config::new().cargo_metadata(false).probe(lib) {
            Ok(found) => println!("cargo:rustc-env={var}={}", found.version),
            Err(err) => panic!("pkg-config could not find system library {lib}: {err}"),
        }
    }

    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    for path in gresource::file_inputs() {
        println!("cargo:rerun-if-changed={path}");
    }
    for bp in gresource::BLUEPRINTS {
        println!(
            "cargo:rerun-if-changed={}/{}.{}/{}.ui",
            gresource::UI_PATH,
            bp.major,
            bp.minor,
            bp.name
        );
    }

    let xml = gresource::main(&root)
        .unwrap_or_else(|err| panic!("failed to generate the gresource XML: {err}"));
    let xml_path = out_dir.join("ghostty.gresource.xml");
    std::fs::write(&xml_path, xml).expect("failed to write the gresource XML");

    // The source root for the icons and CSS, the `.ui` directory for the
    // compiled blueprints.
    let status = Command::new("glib-compile-resources")
        .arg("--sourcedir")
        .arg(&root)
        .arg("--sourcedir")
        .arg(root.join(gresource::UI_PATH))
        .arg("--target")
        .arg(out_dir.join("ghostty.gresource"))
        .arg(&xml_path)
        .status()
        .unwrap_or_else(|err| panic!("failed to run glib-compile-resources: {err}"));
    if !status.success() {
        panic!("glib-compile-resources failed: {status}");
    }
    println!("cargo:rustc-cfg=ghostty_gresource");
}

/// The vendored nothings/stb headers (`src/stb`), compiled from
/// `src/stb/stb.c` as `src/build/SharedDeps.zig` does.
fn stb() {
    if cross_checking_macos() {
        return;
    }
    println!("cargo:rerun-if-changed=src/stb");
    cc::Build::new()
        .include("src/stb")
        .file("src/stb/stb.c")
        .warnings(false)
        .compile("stb");
}

/// C libraries wrapped by `pkg/` (freetype, harfbuzz, fontconfig, oniguruma,
/// opengl/glad). The system libraries are linked via pkg-config (the Zig
/// build's "system integration" mode); glad is always compiled statically
/// from `vendor/glad`, as `src/build/SharedDeps.zig` does.
fn pkg_c_libraries() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Cross-checking the macOS target from another host (e.g.
    // `cargo check --target x86_64-apple-darwin` on Linux) can't compile or
    // link C code without an Apple SDK, and a check doesn't need it.
    if target_os == "macos" && !cfg!(target_os = "macos") {
        return;
    }

    for lib in ["freetype2", "harfbuzz", "fontconfig", "oniguruma"] {
        if let Err(err) = pkg_config::Config::new().probe(lib) {
            panic!("pkg-config could not find system library {lib}: {err}");
        }
    }

    // glad (OpenGL loader, always) and the glad EGL loader + libEGL
    // (non-macOS; ghostty only uses EGL with the GTK apprt).
    println!("cargo:rerun-if-changed=vendor/glad");
    let mut glad = cc::Build::new();
    glad.include("vendor/glad/include")
        .file("vendor/glad/src/gl.c")
        .warnings(false);
    if target_os != "macos" {
        glad.file("vendor/glad/src/glad_egl.c");
        if let Err(err) = pkg_config::Config::new().probe("egl") {
            panic!("pkg-config could not find system library egl: {err}");
        }
    }
    glad.compile("glad");

    // pkg/macos: the os_log_with_type macro wrapper.
    if target_os == "macos" {
        println!("cargo:rerun-if-changed=pkg/macos/os/zig_macos.c");
        cc::Build::new()
            .file("pkg/macos/os/zig_macos.c")
            .flag("-std=c99")
            .compile("ghostty_macos");
    }
    if target_os == "linux" {
        // glad's loader uses dlopen.
        println!("cargo:rustc-link-lib=dylib=dl");
    }
}

/// Dear ImGui + the Dear Bindings C API (`pkg/dcimgui/build.zig`), only with
/// the `inspector` feature. Sources are vendored in `vendor/dcimgui`.
fn dcimgui() {
    if std::env::var_os("CARGO_FEATURE_INSPECTOR").is_none() {
        return;
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_vendor = std::env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default();
    if target_os == "macos" && !cfg!(target_os = "macos") {
        // Cross-checking from another host; see pkg_c_libraries.
        return;
    }
    let apple = target_vendor == "apple";

    println!("cargo:rerun-if-changed=vendor/dcimgui");
    println!("cargo:rerun-if-changed=pkg/dcimgui/ext.cpp");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .warnings(false)
        .include("vendor/dcimgui/imgui")
        .include("vendor/dcimgui/bindings")
        .define("IMGUI_HAS_DOCK", "1")
        .define("IMGUI_USE_WCHAR32", "1")
        .define("IMGUI_DISABLE_OBSOLETE_FUNCTIONS", "1")
        .define("IMGUI_ENABLE_FREETYPE", "1")
        .define("IMGUI_IMPL_API", "extern \"C\"");

    // Freetype is linked by pkg_c_libraries; only the headers are needed.
    let freetype = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("freetype2")
        .expect("pkg-config could not find freetype2");
    for path in &freetype.include_paths {
        build.include(path);
    }

    for file in [
        "imgui_demo.cpp",
        "imgui_draw.cpp",
        "imgui_tables.cpp",
        "imgui_widgets.cpp",
        "imgui.cpp",
        "misc/freetype/imgui_freetype.cpp",
    ] {
        build.file(format!("vendor/dcimgui/imgui/{file}"));
    }
    build
        .file("vendor/dcimgui/bindings/dcimgui.cpp")
        .file("vendor/dcimgui/bindings/dcimgui_internal.cpp")
        .file("pkg/dcimgui/ext.cpp");

    if apple {
        // Metal (and on macOS, OSX) backends.
        build
            .file("vendor/dcimgui/imgui/backends/imgui_impl_metal.mm")
            .flag("-fobjc-arc");
        if target_os == "macos" {
            build.file("vendor/dcimgui/imgui/backends/imgui_impl_osx.mm");
            println!("cargo:rustc-link-lib=framework=GameController");
        }
        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=QuartzCore");
        println!("cargo:rustc-link-lib=framework=AppKit");
    } else {
        // OpenGL3 backend should only be built on non-Apple targets.
        build
            .define("ZIGPKG_IMGUI_ENABLE_OPENGL3", "1")
            .file("vendor/dcimgui/imgui/backends/imgui_impl_opengl3.cpp");
    }

    build.compile("dcimgui");
}

/// True when type-checking for macOS from a non-macOS host: C sources can't
/// be compiled for that target here, and nothing is linked anyway.
fn cross_checking_macos() -> bool {
    std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "macos" && !cfg!(target_os = "macos")
}

/// The app version (`src/build/Config.zig` + `GitVersion.zig`): the
/// `Cargo.toml` version (Zig: `appVersion`), with git's branch as the
/// pre-release and its short hash as build metadata on untagged builds.
/// `GHOSTTY_VERSION_STRING` overrides it (Zig: `-Dversion-string`). Sets
/// `GHOSTTY_VERSION_PRE` and `GHOSTTY_VERSION_BUILD` for `build_config`.
fn app_version() {
    println!("cargo:rerun-if-env-changed=GHOSTTY_VERSION_STRING");
    let (pre, build) = match std::env::var("GHOSTTY_VERSION_STRING") {
        // If an explicit version is given, we always use it.
        Ok(v) => {
            let (rest, build) = match v.split_once('+') {
                Some((rest, build)) => (rest.to_string(), build.to_string()),
                None => (v.clone(), String::new()),
            };
            let pre = rest.split_once('-').map(|(_, pre)| pre.to_string()).unwrap_or_default();
            let core = rest.split('-').next().unwrap_or_default();
            assert_eq!(
                core,
                format!(
                    "{}.{}.{}",
                    std::env::var("CARGO_PKG_VERSION_MAJOR").unwrap(),
                    std::env::var("CARGO_PKG_VERSION_MINOR").unwrap(),
                    std::env::var("CARGO_PKG_VERSION_PATCH").unwrap()
                ),
                "GHOSTTY_VERSION_STRING must match the Cargo.toml version"
            );
            (pre, build)
        }
        Err(_) => git_version(),
    };
    println!("cargo:rustc-env=GHOSTTY_VERSION_PRE={pre}");
    println!("cargo:rustc-env=GHOSTTY_VERSION_BUILD={build}");
}

/// `GitVersion.detect` reduced to the version Zig builds from it: `(pre,
/// build)`. Without git or a repository this is an unknown dev version.
fn git_version() -> (String, String) {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git").args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let unknown = ("dev".to_string(), "0000000".to_string());

    let Some(git_dir) = git(&["rev-parse", "--git-dir"]) else {
        return unknown;
    };
    // Rebuild when HEAD moves (a commit or checkout).
    println!("cargo:rerun-if-changed={git_dir}/HEAD");
    if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(&["rev-parse", "--git-path", &head_ref]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    let Some(branch) = git(&["rev-parse", "--abbrev-ref", "HEAD"]) else {
        return unknown;
    };
    let branch: String = branch
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let short_hash = git(&["-c", "log.showSignature=false", "log", "--pretty=format:%h", "-n", "1"])
        .unwrap_or_default();

    if let Some(tag) = git(&["describe", "--exact-match", "--tags"]) {
        // Tip releases behave just like any other pre-release so we skip.
        if tag != "tip" {
            let expected = format!(
                "v{}.{}.{}",
                std::env::var("CARGO_PKG_VERSION_MAJOR").unwrap(),
                std::env::var("CARGO_PKG_VERSION_MINOR").unwrap(),
                std::env::var("CARGO_PKG_VERSION_PATCH").unwrap()
            );
            assert_eq!(tag, expected, "tagged releases must be in vX.Y.Z format matching Cargo.toml");
            return (String::new(), String::new());
        }
    }

    (branch, short_hash)
}
