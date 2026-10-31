//! Port of `src/terminal/build_options.zig`.
//!
//! STATUS: ported
//!
//! In Zig, `Options.add` turns an `Options` value into the `terminal_options`
//! build-options module, which the terminal package imports as
//! `@import("terminal_options")`. Rust has no build-options modules, so the
//! values that the **Ghostty application** build passes (see
//! `src/build/Config.zig` `terminalOptions(.ghostty, optimize)`) are exposed
//! here as module-level `pub const`s instead. Code that reads
//! `build_options.kitty_graphics` in Zig reads
//! `crate::terminal::build_options::KITTY_GRAPHICS` in Rust.
//!
//! `Options` and `Features` are kept (with `Features::parse`) so the same
//! decisions can be expressed and tested in Rust; [`Options::ghostty`] is the
//! value every constant below is derived from.

use std::fmt;

/// Options set by Zig build.zig and exposed via `terminal_options`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// The target artifact to build. This will gate some functionality.
    pub artifact: Artifact,

    /// Whether Oniguruma regex support is available. If this isn't
    /// available, some features will be disabled. This may be outdated,
    /// but the specific disabled features are:
    ///
    /// - Kitty graphics protocol
    /// - Tmux control mode
    ///
    pub oniguruma: bool,

    /// Whether to build SIMD-accelerated code paths. This pulls in more
    /// build-time dependencies and adds libc as a runtime dependency,
    /// but results in significant performance improvements.
    pub simd: bool,

    /// True if we should enable the "slow" runtime safety checks. These
    /// are runtime safety checks that are slower than typical and should
    /// generally be disabled in production builds.
    pub slow_runtime_safety: bool,

    /// Force C ABI mode on or off. If not set, then it will be set based on
    /// Options.
    pub c_abi: bool,

    /// Optional feature gates, all enabled by default. See Features.
    pub features: Features,

    /// The version of the application.
    pub version: SemanticVersion,
}

/// The target artifact of a build (`Options.Artifact`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Artifact {
    /// Ghostty application
    Ghostty,

    /// libghostty-vt, Zig module
    Lib,
}

/// A semantic version (`std.SemanticVersion`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Optional features for lib artifacts. Disabling a feature removes
/// its C API exports and any stream-integrated handling from the
/// build, primarily so size-conscious embedders (e.g. wasm) can trim
/// the binary. The Ghostty application itself always builds with
/// every feature enabled.
///
/// Field names are the accepted tokens for `-Dvt-features`, which
/// acts like `-Dcpu`: comma-separated modifications applied on top
/// of the default (all enabled) set. `+feature` and `feature`
/// enable, `-feature` disables. The special name `all` refers to
/// every feature, so `-Dvt-features=-all,+render-state` builds only
/// the render state API. Hyphens in names are interchangeable with
/// underscores. Fields are emitted verbatim as bool options on the
/// `terminal_options` module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Features {
    /// Terminal state serialization: encode a full terminal (screen
    /// contents, scrollback, cursor, styles, modes, etc.) to a
    /// compact binary format and decode it back into a live
    /// terminal. Used for session persistence, window restoration,
    /// and moving terminal state between processes.
    ///
    /// C API: `ghostty_snapshot_*` and, together with
    /// `grid_introspection`, `ghostty_tracked_grid_ref_snapshot`.
    pub snapshot: bool,

    /// Textual export of terminal contents as plain text, VT
    /// (replayable escape sequences), or HTML, with optional extras
    /// such as cursor position, palette, modes, and tabstops. Used
    /// for copy-to-clipboard, dump-to-file, and debugging.
    ///
    /// C API: `ghostty_formatter_*` and, together with `selection`,
    /// `ghostty_terminal_selection_format_*`.
    pub formatter: bool,

    /// Text selection: select word/line/output/all, selection
    /// ordering, adjustment, and containment math, and the
    /// click/drag gesture state machine (single/double/triple
    /// click behaviors). Selections are expressed in grid
    /// references, so enabling this also keeps the untracked grid
    /// reference constructors even if `grid_introspection` is
    /// disabled.
    ///
    /// C API: `ghostty_terminal_select_*`,
    /// `ghostty_terminal_selection_*`, `ghostty_selection_gesture_*`.
    pub selection: bool,

    /// Terminal search: find matches for a string in the active
    /// area and scrollback (ASCII case-insensitive), with results
    /// that survive primary/alternate screen switches, resize, and
    /// scrollback pruning, plus match selection with wrap-around
    /// and viewport scrolling. Used to implement find bars.
    ///
    /// C API: `ghostty_search_*`.
    pub search: bool,

    /// The render state API: a coherent, update-in-place view of
    /// the visible screen (rows, cells, styles, cursor, colors,
    /// palette) designed to drive a renderer at frame rates. This
    /// is the primary read path for embedders that draw the
    /// terminal.
    ///
    /// C API: `ghostty_render_state_*`.
    pub render_state: bool,

    /// Encoding of host input events into the byte sequences a
    /// terminal application expects: keyboard input (including the
    /// Kitty keyboard protocol), mouse reporting, focus reporting,
    /// and (bracketed) paste. Any interactive embedder needs this;
    /// a read-only viewer does not.
    ///
    /// C API: `ghostty_key_event_*`, `ghostty_key_encoder_*`,
    /// `ghostty_mouse_event_*`, `ghostty_mouse_encoder_*`,
    /// `ghostty_focus_encode`, `ghostty_paste_*`.
    pub input_encode: bool,

    /// Color utilities: luminance, perceived luminance, and
    /// contrast math (e.g. for minimum-contrast rendering), color
    /// string parsing (`#rrggbb`, X11 color names, XParseColor
    /// `rgb:`/`rgbi:` device specs), the default 256-color palette
    /// and palette generation from configurable bases, and the X11
    /// color name table itself.
    ///
    /// C API: `ghostty_color_*`.
    pub color: bool,

    /// Direct inspection of grid contents at a position via grid
    /// references: cell and row data getters (codepoints,
    /// graphemes, style, hyperlink URI, wrap state, ...) plus
    /// tracked grid references that stay valid across terminal
    /// updates. Used for hyperlink hover/open, accessibility, and
    /// tests; deliberately not built for render loops (use
    /// `render_state` for that).
    ///
    /// C API: `ghostty_grid_ref_*`, `ghostty_tracked_grid_ref_*`,
    /// `ghostty_cell_get*`, `ghostty_row_get*`, and (shared with
    /// `selection`) `ghostty_terminal_grid_ref` /
    /// `ghostty_terminal_point_from_grid_ref`.
    pub grid_introspection: bool,

    /// The APC glyph protocol: a Ghostty extension that lets
    /// terminal applications register custom font glyphs (glyf
    /// outlines) for private-use-area codepoints. Embedders that
    /// don't render registered glyphs can disable this; the
    /// sequences are still consumed and safely ignored.
    ///
    /// No C API surface of its own; this gates the stream handling
    /// and glossary storage.
    pub glyph_protocol: bool,

    /// The Kitty graphics protocol: APC command parsing, image
    /// transmission and storage (PNG decoding via the sys
    /// interface), and placement tracking, plus the read APIs a
    /// renderer uses to draw placed images. Disabled sequences are
    /// still consumed and safely ignored.
    ///
    /// This requires the ability to get timestamps from the OS, so
    /// it is always disabled on freestanding targets (e.g.
    /// wasm32-freestanding) regardless of this setting.
    ///
    /// C API: `ghostty_kitty_graphics_*`.
    pub kitty_graphics: bool,
}

/// Error returned by [`Features::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeaturesParseError {
    UnknownFeature,
}

impl fmt::Display for FeaturesParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UnknownFeature")
    }
}

impl std::error::Error for FeaturesParseError {}

impl Default for Features {
    fn default() -> Self {
        Self::ALL
    }
}

impl Features {
    /// Every feature enabled (the Zig field defaults, `.{}`).
    pub const ALL: Features = Features {
        snapshot: true,
        formatter: true,
        selection: true,
        search: true,
        render_state: true,
        input_encode: true,
        color: true,
        grid_introspection: true,
        glyph_protocol: true,
        kitty_graphics: true,
    };

    /// Every feature disabled (a zero backing integer in Zig).
    pub const NONE: Features = Features {
        snapshot: false,
        formatter: false,
        selection: false,
        search: false,
        render_state: false,
        input_encode: false,
        color: false,
        grid_introspection: false,
        glyph_protocol: false,
        kitty_graphics: false,
    };

    /// The field names, in declaration order.
    pub const FIELD_NAMES: [&'static str; 10] = [
        "snapshot",
        "formatter",
        "selection",
        "search",
        "render_state",
        "input_encode",
        "color",
        "grid_introspection",
        "glyph_protocol",
        "kitty_graphics",
    ];

    /// Mutable access to a field by its declaration index (the Rust
    /// equivalent of `@field(result, field)` in an `inline for`).
    fn field_mut(&mut self, idx: usize) -> &mut bool {
        match idx {
            0 => &mut self.snapshot,
            1 => &mut self.formatter,
            2 => &mut self.selection,
            3 => &mut self.search,
            4 => &mut self.render_state,
            5 => &mut self.input_encode,
            6 => &mut self.color,
            7 => &mut self.grid_introspection,
            8 => &mut self.glyph_protocol,
            9 => &mut self.kitty_graphics,
            _ => unreachable!(),
        }
    }

    pub fn parse(list: &[u8]) -> Result<Features, FeaturesParseError> {
        // Modifications apply on top of the default set.
        let mut result = Features::default();

        for raw in list.split(|&c| c == b',' || c == b' ') {
            if raw.is_empty() {
                continue;
            }

            let mut name = raw;
            let mut enable = true;
            match name[0] {
                b'+' => name = &name[1..],
                b'-' => {
                    enable = false;
                    name = &name[1..];
                }
                _ => {}
            }

            // A dangling sign with no name is malformed.
            if name.is_empty() {
                return Err(FeaturesParseError::UnknownFeature);
            }

            // The special name "all" refers to every feature.
            if name == b"all" {
                for i in 0..Self::FIELD_NAMES.len() {
                    *result.field_mut(i) = enable;
                }
                continue;
            }

            let mut found = false;
            for (i, field) in Self::FIELD_NAMES.iter().enumerate() {
                if eql_name(name, field.as_bytes()) {
                    *result.field_mut(i) = enable;
                    found = true;
                }
            }
            if !found {
                return Err(FeaturesParseError::UnknownFeature);
            }
        }

        Ok(result)
    }
}

/// Feature name equality where '-' in the input matches '_' in
/// the field name.
fn eql_name(input: &[u8], name: &[u8]) -> bool {
    if input.len() != name.len() {
        return false;
    }
    for (&a, &b) in input.iter().zip(name) {
        let norm = if a == b'-' { b'_' } else { a };
        if norm != b {
            return false;
        }
    }
    true
}

impl Options {
    /// The options the Ghostty application build passes for the current
    /// target (`Config.terminalOptions(.ghostty, optimize)`).
    pub const fn ghostty() -> Options {
        Options {
            artifact: Artifact::Ghostty,
            // Config.zig: SIMD is on unless the target is wasm or freestanding.
            simd: !cfg!(target_family = "wasm") && !cfg!(target_os = "none"),
            oniguruma: true,
            c_abi: false,
            // The application requires every feature; only lib artifacts
            // may trim them.
            features: Features::ALL,
            version: APP_VERSION,
            slow_runtime_safety: cfg!(debug_assertions),
        }
    }

    /// The options of libghostty-vt's C library, as `zig build
    /// -Demit-lib-vt` builds it: `Config.terminalOptions(.lib, optimize)`
    /// with the overrides of `GhosttyZig.zig` (`artifact = .lib`,
    /// `oniguruma = false`, and `c_abi = true` for the `vt_c` module). The
    /// version is the libghostty-vt version (`build.zig` `lib_version`).
    /// The crate is built with these options when compiled with
    /// `--cfg ghostty_lib_vt` (`GHOSTTY_LIB_VT=1`, see
    /// `tools/build_lib_vt.sh`).
    pub const fn lib_vt() -> Options {
        Options {
            artifact: Artifact::Lib,
            simd: !cfg!(target_family = "wasm") && !cfg!(target_os = "none"),
            // "We presently don't allow Oniguruma in our Zig module at all."
            oniguruma: false,
            c_abi: true,
            // -Dvt-features (Config.vt_features) defaults to every feature.
            features: Features::ALL,
            version: LIB_VERSION,
            slow_runtime_safety: cfg!(debug_assertions),
        }
    }

    /// Whether the Kitty graphics feature is effectively enabled for
    /// the given target. Kitty graphics requires the ability to get
    /// timestamps and there is no way to do that on freestanding
    /// targets, so it is always disabled there regardless of the
    /// feature setting.
    ///
    /// `target_os` is the Rust `target_os` name; freestanding is `"none"`.
    pub fn kitty_graphics(&self, target_os: &str) -> bool {
        if target_os == "none" {
            return false;
        }
        self.features.kitty_graphics
    }

    /// Zig's `kittyGraphics` evaluated for the target being compiled.
    pub const fn kitty_graphics_current_target(&self) -> bool {
        if cfg!(target_os = "none") {
            return false;
        }
        self.features.kitty_graphics
    }

    // NOTE: Zig's `Options.add(b, m)` registers these values as the
    // `terminal_options` module with the Zig build system. Its Rust
    // equivalent is the set of constants below.
}

/// Parse a decimal `usize` at compile time (for Cargo version env vars).
const fn parse_usize(s: &str) -> usize {
    let b = s.as_bytes();
    let mut i = 0;
    let mut v = 0usize;
    while i < b.len() {
        v = v * 10 + (b[i] - b'0') as usize;
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

/// The application version (`build_config::VERSION`: the Cargo package
/// version plus git's branch and hash, as the Zig build derives it from
/// `build.zig.zon` and git).
const APP_VERSION: SemanticVersion = SemanticVersion {
    major: parse_usize(env!("CARGO_PKG_VERSION_MAJOR")),
    minor: parse_usize(env!("CARGO_PKG_VERSION_MINOR")),
    patch: parse_usize(env!("CARGO_PKG_VERSION_PATCH")),
    pre: non_empty(env!("GHOSTTY_VERSION_PRE")),
    build: non_empty(env!("GHOSTTY_VERSION_BUILD")),
};

const fn env_or(value: Option<&'static str>, default: &'static str) -> &'static str {
    match value {
        Some(v) => v,
        None => default,
    }
}

/// The libghostty-vt version: `build.zig`'s `lib_version` (`0.1.0-dev`),
/// or the `GHOSTTY_LIB_VT_VERSION` given to `tools/build_lib_vt.sh` (Zig:
/// `-Dlib-version-string`), which `build.rs` splits into its parts.
const LIB_VERSION: SemanticVersion = SemanticVersion {
    major: parse_usize(env_or(option_env!("GHOSTTY_LIB_VT_VERSION_MAJOR"), "0")),
    minor: parse_usize(env_or(option_env!("GHOSTTY_LIB_VT_VERSION_MINOR"), "1")),
    patch: parse_usize(env_or(option_env!("GHOSTTY_LIB_VT_VERSION_PATCH"), "0")),
    pre: non_empty(env_or(option_env!("GHOSTTY_LIB_VT_VERSION_PRE"), "dev")),
    build: non_empty(env_or(option_env!("GHOSTTY_LIB_VT_VERSION_BUILD"), "")),
};

/// The libghostty-vt version as a string (`LIB_VERSION` formatted).
const LIB_VERSION_STRING: &str = env_or(option_env!("GHOSTTY_LIB_VT_VERSION_STRING"), "0.1.0-dev");

// ---------------------------------------------------------------------------
// The `terminal_options` module values (what `Options.add` emits) for the
// Ghostty application build, or for libghostty-vt's C library when the
// crate is compiled with `--cfg ghostty_lib_vt`.
// ---------------------------------------------------------------------------

/// The options this crate is built with.
pub const OPTIONS: Options = if cfg!(ghostty_lib_vt) {
    Options::lib_vt()
} else {
    Options::ghostty()
};

/// `terminal_options.artifact`: the target artifact. The Ghostty
/// application, or `Lib` for libghostty-vt (`--cfg ghostty_lib_vt`).
pub const ARTIFACT: Artifact = OPTIONS.artifact;

/// `terminal_options.c_abi`: whether the terminal package exports a C ABI.
/// False for the application build, true for libghostty-vt.
pub const C_ABI: bool = OPTIONS.c_abi;

/// `terminal_options.oniguruma`: whether Oniguruma regex support is available.
pub const ONIGURUMA: bool = OPTIONS.oniguruma;

/// `terminal_options.simd`: whether SIMD-accelerated code paths are built.
pub const SIMD: bool = OPTIONS.simd;

/// `terminal_options.slow_runtime_safety`: enable slow runtime safety checks.
/// Matches Zig's `optimize == .debug`, i.e. Rust debug assertions.
pub const SLOW_RUNTIME_SAFETY: bool = OPTIONS.slow_runtime_safety;

/// `terminal_options.tmux_control_mode`: synthesized from `oniguruma`.
pub const TMUX_CONTROL_MODE: bool = OPTIONS.oniguruma;

/// `terminal_options.snapshot`: see [`Features::snapshot`].
pub const SNAPSHOT: bool = OPTIONS.features.snapshot;
/// `terminal_options.formatter`: see [`Features::formatter`].
pub const FORMATTER: bool = OPTIONS.features.formatter;
/// `terminal_options.selection`: see [`Features::selection`].
pub const SELECTION: bool = OPTIONS.features.selection;
/// `terminal_options.search`: see [`Features::search`].
pub const SEARCH: bool = OPTIONS.features.search;
/// `terminal_options.render_state`: see [`Features::render_state`].
pub const RENDER_STATE: bool = OPTIONS.features.render_state;
/// `terminal_options.input_encode`: see [`Features::input_encode`].
pub const INPUT_ENCODE: bool = OPTIONS.features.input_encode;
/// `terminal_options.color`: see [`Features::color`].
pub const COLOR: bool = OPTIONS.features.color;
/// `terminal_options.grid_introspection`: see [`Features::grid_introspection`].
pub const GRID_INTROSPECTION: bool = OPTIONS.features.grid_introspection;
/// `terminal_options.glyph_protocol`: see [`Features::glyph_protocol`].
pub const GLYPH_PROTOCOL: bool = OPTIONS.features.glyph_protocol;
/// `terminal_options.kitty_graphics`: see [`Features::kitty_graphics`]
/// (force-disabled on freestanding targets, see [`Options::kitty_graphics`]).
pub const KITTY_GRAPHICS: bool = OPTIONS.kitty_graphics_current_target();

/// `terminal_options.version_string`: the full version string.
pub const VERSION_STRING: &str = if cfg!(ghostty_lib_vt) {
    LIB_VERSION_STRING
} else {
    crate::build_config::VERSION_STRING
};
/// `terminal_options.version_major`.
pub const VERSION_MAJOR: usize = OPTIONS.version.major;
/// `terminal_options.version_minor`.
pub const VERSION_MINOR: usize = OPTIONS.version.minor;
/// `terminal_options.version_patch`.
pub const VERSION_PATCH: usize = OPTIONS.version.patch;
/// `terminal_options.version_pre`.
pub const VERSION_PRE: Option<&str> = OPTIONS.version.pre;
/// `terminal_options.version_build`.
pub const VERSION_BUILD: Option<&str> = OPTIONS.version.build;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_parse() {
        let none = Features::NONE;

        // Empty input keeps the defaults.
        assert_eq!(Features::default(), Features::parse(b"").unwrap());

        // All three token forms, comma separated.
        assert_eq!(
            Features {
                snapshot: false,
                ..Default::default()
            },
            Features::parse(b"-snapshot").unwrap()
        );
        assert_eq!(
            Features {
                snapshot: false,
                formatter: false,
                ..Default::default()
            },
            Features::parse(b"-snapshot,+render_state,-formatter,color").unwrap()
        );

        // The special "all" name refers to every feature.
        assert_eq!(none, Features::parse(b"-all").unwrap());
        assert_eq!(Features::default(), Features::parse(b"-all,+all").unwrap());
        let mut render_only = none;
        render_only.render_state = true;
        assert_eq!(render_only, Features::parse(b"-all,+render-state").unwrap());

        // Hyphens are interchangeable with underscores.
        assert_eq!(
            Features {
                input_encode: false,
                grid_introspection: false,
                ..Default::default()
            },
            Features::parse(b"-input-encode,-grid_introspection").unwrap()
        );

        // Later tokens win.
        assert_eq!(
            Features::default(),
            Features::parse(b"-snapshot,+snapshot").unwrap()
        );

        // Unknown names and dangling signs are errors.
        assert_eq!(
            Err(FeaturesParseError::UnknownFeature),
            Features::parse(b"bogus")
        );
        assert_eq!(
            Err(FeaturesParseError::UnknownFeature),
            Features::parse(b"-snapshot,-")
        );
    }

    #[test]
    fn lib_vt_options() {
        let opts = Options::lib_vt();
        assert_eq!(opts.artifact, Artifact::Lib);
        assert!(opts.c_abi);
        assert!(!opts.oniguruma);
        assert_eq!(opts.features, Features::ALL);
        assert_eq!(opts.slow_runtime_safety, cfg!(debug_assertions));
        assert_eq!(opts.version, LIB_VERSION);
        assert_eq!(LIB_VERSION.to_string(), LIB_VERSION_STRING);
    }

    #[test]
    #[cfg(not(ghostty_lib_vt))]
    fn ghostty_app_options() {
        assert_eq!(ARTIFACT, Artifact::Ghostty);
        assert!(!C_ABI);
        assert!(ONIGURUMA);
        assert!(TMUX_CONTROL_MODE);
        assert_eq!(SLOW_RUNTIME_SAFETY, cfg!(debug_assertions));
        assert_eq!(OPTIONS.features, Features::ALL);
        assert!(OPTIONS.kitty_graphics("linux"));
        assert!(!OPTIONS.kitty_graphics("none"));
        assert_eq!(OPTIONS.version.to_string(), VERSION_STRING);
    }
}
