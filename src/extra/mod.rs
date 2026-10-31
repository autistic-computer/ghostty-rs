//! Module grouping (no Zig counterpart file; mirrors a directory).
//!
//! The Zig generators in `extra/` run at comptime and inspect the types of
//! `Config`'s fields and of each CLI action's `Options` with `@typeInfo`.
//! Rust has no such reflection, so this module provides the subset they
//! need: [`config_fields`] (from `Config::FIELDS` plus the field types via
//! the [`Value!`](crate::config::key::Value) macro) and [`ACTIONS`] (the
//! CLI actions with their options and help text).

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "bash.rs"]
pub mod bash;
#[path = "fish.rs"]
pub mod fish;
#[path = "pkg_config.rs"]
pub mod pkg_config;
#[path = "sublime.rs"]
pub mod sublime;
#[path = "vim.rs"]
pub mod vim;
#[path = "zsh.rs"]
pub mod zsh;
// @generated-mods-end

use std::marker::PhantomData;
use std::sync::LazyLock;

use crate::cli::args::{CliEnum, Fields, PackedStruct};
use crate::cli::ghostty::Action as CliAction;
use crate::config::path::RepeatablePath;
use crate::config::Config;

/// The parts of Zig's `@typeInfo` of a struct field type that the
/// generators inspect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// `bool`
    Bool,
    /// `?bool`
    OptionalBool,
    /// An `enum` (with its tag names).
    Enum(&'static [&'static str]),
    /// An optional `enum` (with the child's tag names).
    OptionalEnum(&'static [&'static str]),
    /// A `packed struct` of bools (with its field names). Zig only treats
    /// packed structs without a `parseCLI` decl as flags; every Rust
    /// [`PackedStruct`] in `Config` uses the default flags parser.
    PackedStruct(&'static [&'static str]),
    /// `Config.RepeatablePath`
    RepeatablePath,
    /// Any other optional type.
    Optional,
    /// Anything else (ints, floats, strings, other structs, unions, ...).
    Other,
}

impl FieldType {
    /// Zig `T == bool` (`?bool` is `.optional`).
    pub fn is_bool(self) -> bool {
        self == FieldType::Bool
    }

    /// Zig `switch (T) { bool, ?bool => ... }`.
    pub fn is_bool_or_optional_bool(self) -> bool {
        matches!(self, FieldType::Bool | FieldType::OptionalBool)
    }
}

/// A struct field: its (Zig) name and type information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    pub name: &'static str,
    pub ty: FieldType,
}

// Autoref-based specialization to compute a [`FieldType`] from a concrete
// type in a macro (the most specific impl has the most references).
#[doc(hidden)]
pub struct Probe<T>(pub PhantomData<T>);

#[doc(hidden)]
pub trait ProbeConcrete {
    fn field_type(&self) -> FieldType;
}
impl ProbeConcrete for &&&&&&Probe<bool> {
    fn field_type(&self) -> FieldType {
        FieldType::Bool
    }
}
impl ProbeConcrete for &&&&&&Probe<Option<bool>> {
    fn field_type(&self) -> FieldType {
        FieldType::OptionalBool
    }
}
impl ProbeConcrete for &&&&&&Probe<RepeatablePath> {
    fn field_type(&self) -> FieldType {
        FieldType::RepeatablePath
    }
}

#[doc(hidden)]
pub trait ProbeOptionalEnum {
    fn field_type(&self) -> FieldType;
}
impl<E: CliEnum> ProbeOptionalEnum for &&&&&Probe<Option<E>> {
    fn field_type(&self) -> FieldType {
        FieldType::OptionalEnum(E::FIELD_NAMES)
    }
}

#[doc(hidden)]
pub trait ProbeOptional {
    fn field_type(&self) -> FieldType;
}
impl<T> ProbeOptional for &&&&Probe<Option<T>> {
    fn field_type(&self) -> FieldType {
        FieldType::Optional
    }
}

#[doc(hidden)]
pub trait ProbeEnum {
    fn field_type(&self) -> FieldType;
}
impl<E: CliEnum> ProbeEnum for &&&Probe<E> {
    fn field_type(&self) -> FieldType {
        FieldType::Enum(E::FIELD_NAMES)
    }
}

#[doc(hidden)]
pub trait ProbePacked {
    fn field_type(&self) -> FieldType;
}
impl<P: PackedStruct> ProbePacked for &&Probe<P> {
    fn field_type(&self) -> FieldType {
        FieldType::PackedStruct(P::FIELD_NAMES)
    }
}

#[doc(hidden)]
pub trait ProbeOther {
    fn field_type(&self) -> FieldType;
}
impl<T> ProbeOther for &Probe<T> {
    fn field_type(&self) -> FieldType {
        FieldType::Other
    }
}

/// The [`FieldType`] of a concrete type (Zig `@typeInfo(T)`).
macro_rules! field_type_of {
    ($t:ty) => {{
        #[allow(unused_imports)]
        use $crate::extra::{
            ProbeConcrete as _, ProbeEnum as _, ProbeOptional as _, ProbeOptionalEnum as _,
            ProbeOther as _, ProbePacked as _,
        };
        (&&&&&&&$crate::extra::Probe::<$t>(::std::marker::PhantomData)).field_type()
    }};
}
#[allow(unused_imports)]
pub(crate) use field_type_of;

macro_rules! config_field_types {
    ($($key:tt)*) => {
        vec![$( ($key, field_type_of!($crate::config::key::Value!($key))) ),*]
    };
}

/// The types of all `Config` fields by key, in field order. Checked
/// against `Config::FIELDS` in the tests.
fn config_field_types() -> &'static [(&'static str, FieldType)] {
    static TYPES: LazyLock<Vec<(&'static str, FieldType)>> = LazyLock::new(|| {
        config_field_types!(
            "language"
            "font-family"
            "font-family-bold"
            "font-family-italic"
            "font-family-bold-italic"
            "font-style"
            "font-style-bold"
            "font-style-italic"
            "font-style-bold-italic"
            "font-synthetic-style"
            "font-feature"
            "font-size"
            "font-variation"
            "font-variation-bold"
            "font-variation-italic"
            "font-variation-bold-italic"
            "font-codepoint-map"
            "clipboard-codepoint-map"
            "font-thicken"
            "font-thicken-strength"
            "font-shaping-break"
            "alpha-blending"
            "adjust-cell-width"
            "adjust-cell-height"
            "adjust-font-baseline"
            "adjust-underline-position"
            "adjust-underline-thickness"
            "adjust-strikethrough-position"
            "adjust-strikethrough-thickness"
            "adjust-overline-position"
            "adjust-overline-thickness"
            "adjust-cursor-thickness"
            "adjust-cursor-height"
            "adjust-box-thickness"
            "adjust-icon-height"
            "grapheme-width-method"
            "freetype-load-flags"
            "theme"
            "background"
            "foreground"
            "background-image"
            "background-image-opacity"
            "background-image-position"
            "background-image-fit"
            "background-image-repeat"
            "selection-foreground"
            "selection-background"
            "selection-clear-on-typing"
            "selection-clear-on-copy"
            "selection-word-chars"
            "minimum-contrast"
            "palette"
            "palette-generate"
            "palette-harmonious"
            "cursor-color"
            "cursor-opacity"
            "cursor-style"
            "cursor-style-blink"
            "cursor-text"
            "cursor-click-to-move"
            "mouse-hide-while-typing"
            "scroll-to-bottom"
            "mouse-shift-capture"
            "mouse-reporting"
            "mouse-scroll-multiplier"
            "background-opacity"
            "background-opacity-cells"
            "background-blur"
            "unfocused-split-opacity"
            "unfocused-split-fill"
            "split-divider-color"
            "split-preserve-zoom"
            "search-foreground"
            "search-background"
            "search-selected-foreground"
            "search-selected-background"
            "command"
            "initial-command"
            "notify-on-command-finish"
            "notify-on-command-finish-action"
            "notify-on-command-finish-after"
            "env"
            "input"
            "wait-after-command"
            "abnormal-command-exit-runtime"
            "scrollback-limit-bytes"
            "scrollback-limit-lines"
            "scrollback-compression"
            "scrollbar"
            "link"
            "link-url"
            "link-osc8"
            "link-previews"
            "maximize"
            "fullscreen"
            "title"
            "class"
            "x11-instance-name"
            "working-directory"
            "keybind"
            "key-remap"
            "window-padding-x"
            "window-padding-y"
            "window-padding-balance"
            "window-padding-color"
            "window-vsync"
            "window-inherit-working-directory"
            "tab-inherit-working-directory"
            "split-inherit-working-directory"
            "window-inherit-font-size"
            "window-decoration"
            "window-title-font-family"
            "window-subtitle"
            "window-theme"
            "window-colorspace"
            "window-height"
            "window-width"
            "window-position-x"
            "window-position-y"
            "window-save-state"
            "window-step-resize"
            "window-new-tab-position"
            "window-show-tab-bar"
            "window-titlebar-background"
            "window-titlebar-foreground"
            "drag-handle"
            "resize-overlay"
            "resize-overlay-position"
            "resize-overlay-duration"
            "focus-follows-mouse"
            "clipboard-read"
            "clipboard-write"
            "clipboard-write-limit-bytes"
            "clipboard-trim-trailing-spaces"
            "clipboard-paste-protection"
            "clipboard-paste-bracketed-safe"
            "title-report"
            "image-storage-limit"
            "copy-on-select"
            "right-click-action"
            "middle-click-action"
            "click-repeat-interval"
            "config-file"
            "config-default-files"
            "confirm-close-surface"
            "quit-after-last-window-closed"
            "quit-after-last-window-closed-delay"
            "initial-window"
            "undo-timeout"
            "quick-terminal-position"
            "quick-terminal-size"
            "gtk-quick-terminal-layer"
            "gtk-quick-terminal-namespace"
            "quick-terminal-screen"
            "quick-terminal-animation-duration"
            "quick-terminal-autohide"
            "quick-terminal-space-behavior"
            "quick-terminal-keyboard-interactivity"
            "shell-integration"
            "shell-integration-features"
            "command-palette-entry"
            "osc-color-report-format"
            "vt-kam-allowed"
            "vt-window-resize-allowed"
            "vt-xt-checksum-report"
            "vt-xt-checksum-extension"
            "custom-shader"
            "custom-shader-animation"
            "bell-features"
            "bell-audio-path"
            "bell-audio-volume"
            "app-notifications"
            "macos-non-native-fullscreen"
            "macos-window-buttons"
            "macos-titlebar-style"
            "macos-titlebar-proxy-icon"
            "macos-dock-drop-behavior"
            "macos-option-as-alt"
            "macos-window-shadow"
            "macos-hidden"
            "macos-auto-secure-input"
            "macos-secure-input-indication"
            "macos-applescript"
            "macos-icon"
            "macos-custom-icon"
            "macos-icon-frame"
            "macos-icon-ghost-color"
            "macos-icon-screen-color"
            "macos-shortcuts"
            "linux-cgroup"
            "linux-cgroup-memory-limit"
            "linux-cgroup-processes-limit"
            "linux-cgroup-hard-fail"
            "gtk-opengl-debug"
            "gtk-single-instance"
            "gtk-titlebar"
            "gtk-tabs-location"
            "gtk-tabs-inverted"
            "gtk-titlebar-hide-when-maximized"
            "gtk-toolbar-style"
            "gtk-titlebar-style"
            "gtk-wide-tabs"
            "gtk-horizontal-tab-scroll"
            "gtk-custom-css"
            "desktop-notifications"
            "progress-style"
            "bold-color"
            "faint-opacity"
            "term"
            "enquiry-response"
            "async-backend"
            "auto-update"
            "auto-update-channel"
        )
    });
    &TYPES
}

/// All non-internal `Config` fields (Zig: the fields of `Config` whose name
/// doesn't start with `_`) with their types, in declaration order.
pub fn config_fields() -> &'static [Field] {
    static FIELDS: LazyLock<Vec<Field>> = LazyLock::new(|| {
        let types = config_field_types();
        <Config as Fields>::FIELDS
            .iter()
            .map(|f| Field {
                name: f.name,
                ty: types
                    .iter()
                    .find(|(k, _)| *k == f.name)
                    .map(|(_, t)| *t)
                    .unwrap_or(FieldType::Other),
            })
            .collect()
    });
    &FIELDS
}

/// The help string for a `Config` field (`help_strings.Config.<field>`), or
/// `None` if the field has no doc comment.
pub fn config_help(name: &str) -> Option<String> {
    let field = <Config as Fields>::FIELDS.iter().find(|f| f.name == name)?;
    if !field.has_help() {
        return None;
    }
    Some(field.help())
}

// ---------------------------------------------------------------------------
// CLI actions
// ---------------------------------------------------------------------------

/// A CLI action (`+<name>`) as seen by the generators: the
/// [`crate::cli::ghostty::Action`] plus the kinds of its `Options` fields,
/// which the generators look at with `@typeInfo` in Zig. (The Rust field
/// tables, [`crate::cli::args::FieldInfo`], don't carry the field types.)
#[derive(Debug, Clone, Copy)]
pub struct Action {
    pub action: CliAction,
    /// All fields of the action's Zig `Options` struct, including internal
    /// (`_`-prefixed) ones.
    pub options: &'static [Field],
}

impl Action {
    /// The Zig tag name.
    pub fn name(&self) -> &'static str {
        self.action.tag_name()
    }

    /// `action.file()`: the source file of the action relative to the root
    /// `src/` directory.
    pub fn file(&self) -> String {
        self.action.file()
    }

    /// `help_strings.Action.<name>`: the doc comment of the action's `run`
    /// function with the common leading-space prefix removed.
    pub fn help(&self) -> &'static str {
        self.action.help_string()
    }
}

/// `helpgen.zig` `findCommonPrefix`: the smallest number of leading spaces
/// over all lines (lines of only spaces don't count).
pub fn find_common_prefix(lines: &[&str]) -> usize {
    let mut m = usize::MAX;
    for line in lines {
        let n = line.bytes().position(|c| c != b' ').unwrap_or(usize::MAX);
        m = m.min(n);
    }
    m
}

const fn f(name: &'static str, ty: FieldType) -> Field {
    Field { name, ty }
}

const COLOR_SCHEME: &[&str] = &["all", "dark", "light"];
const FONT_STYLE: &[&str] = &["regular", "bold", "italic", "bold_italic"];
const FONT_PRESENTATION: &[&str] = &["text", "emoji"];

/// The CLI actions in `cli/ghostty.zig` `Action` order.
pub const ACTIONS: &[Action] = &[
    Action {
        action: CliAction::Version,
        options: &[],
    },
    Action {
        action: CliAction::Help,
        options: &[f("help", FieldType::Bool)],
    },
    Action {
        action: CliAction::ListFonts,
        options: &[
            f("_arena", FieldType::Optional),
            f("family", FieldType::Optional),
            f("style", FieldType::Optional),
            f("bold", FieldType::Bool),
            f("italic", FieldType::Bool),
        ],
    },
    Action {
        action: CliAction::ListKeybinds,
        options: &[
            f("default", FieldType::Bool),
            f("docs", FieldType::Bool),
            f("plain", FieldType::Bool),
        ],
    },
    Action {
        action: CliAction::ListThemes,
        options: &[
            f("path", FieldType::Bool),
            f("plain", FieldType::Bool),
            f("color", FieldType::Enum(COLOR_SCHEME)),
        ],
    },
    Action {
        action: CliAction::ListColors,
        options: &[f("plain", FieldType::Bool)],
    },
    Action {
        action: CliAction::ListActions,
        options: &[f("docs", FieldType::Bool)],
    },
    Action {
        action: CliAction::Ssh,
        options: &[
            f("_arena", FieldType::Optional),
            f("forward-env", FieldType::Bool),
            f("terminfo", FieldType::Bool),
            f("cache", FieldType::Bool),
            f("ssh", FieldType::Other),
            f("verbose", FieldType::Bool),
            f("_ssh_args", FieldType::Other),
            f("_diagnostics", FieldType::Other),
        ],
    },
    Action {
        action: CliAction::SshCache,
        options: &[
            f("clear", FieldType::Bool),
            f("add", FieldType::Optional),
            f("remove", FieldType::Optional),
            f("prune", FieldType::Optional),
        ],
    },
    Action {
        action: CliAction::EditConfig,
        options: &[],
    },
    Action {
        action: CliAction::ShowConfig,
        options: &[
            f("default", FieldType::Bool),
            f("changes-only", FieldType::Bool),
            f("docs", FieldType::Bool),
            f("no-pager", FieldType::Bool),
        ],
    },
    Action {
        action: CliAction::ExplainConfig,
        options: &[
            f("option", FieldType::Optional),
            f("keybind", FieldType::Optional),
        ],
    },
    Action {
        action: CliAction::ValidateConfig,
        options: &[f("config-file", FieldType::Optional)],
    },
    Action {
        action: CliAction::ShowFace,
        options: &[
            f("_arena", FieldType::Optional),
            f("cp", FieldType::Optional),
            f("string", FieldType::Optional),
            f("style", FieldType::Enum(FONT_STYLE)),
            f("presentation", FieldType::OptionalEnum(FONT_PRESENTATION)),
            f("_diagnostics", FieldType::Other),
        ],
    },
    Action {
        action: CliAction::CrashReport,
        options: &[],
    },
    Action {
        action: CliAction::Boo,
        options: &[],
    },
    Action {
        action: CliAction::NewWindow,
        options: &[
            f("_io", FieldType::Other),
            f("_arena", FieldType::Optional),
            f("class", FieldType::Optional),
            f("_working_directory_seen", FieldType::Bool),
            f("_arguments", FieldType::Other),
            f("_diagnostics", FieldType::Other),
        ],
    },
    Action {
        action: CliAction::NewTab,
        options: &[
            f("_io", FieldType::Other),
            f("_arena", FieldType::Optional),
            f("class", FieldType::Optional),
            f("surface-id", FieldType::Optional),
            f("_working_directory_seen", FieldType::Bool),
            f("_arguments", FieldType::Other),
            f("_diagnostics", FieldType::Other),
        ],
    },
    Action {
        action: CliAction::ToggleQuickTerminal,
        options: &[f("class", FieldType::Optional)],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::key::Key;

    #[test]
    fn config_field_types_match_config() {
        let types = config_field_types();
        let fields = <Config as Fields>::FIELDS;
        assert_eq!(types.len(), fields.len());
        for (t, f) in types.iter().zip(fields) {
            assert_eq!(t.0, f.name);
        }
        assert_eq!(<Key as CliEnum>::FIELD_NAMES.len(), fields.len());
    }

    fn ty(name: &str) -> FieldType {
        config_fields().iter().find(|f| f.name == name).unwrap().ty
    }

    #[test]
    fn config_field_type_kinds() {
        assert_eq!(FieldType::Other, ty("font-family"));
        assert_eq!(FieldType::Optional, ty("language"));
        assert_eq!(FieldType::RepeatablePath, ty("config-file"));
        assert_eq!(FieldType::Bool, ty("focus-follows-mouse"));
        assert_eq!(FieldType::Optional, ty("command"));
        assert!(matches!(
            ty("shell-integration-features"),
            FieldType::PackedStruct(_)
        ));
        assert!(matches!(ty("shell-integration"), FieldType::Enum(_)));
        assert!(matches!(ty("auto-update"), FieldType::OptionalEnum(_)));
    }

    #[test]
    fn actions_match_cli() {
        let names: Vec<_> = CliAction::all().map(|a| a.tag_name()).collect();
        let ours: Vec<_> = ACTIONS.iter().map(|a| a.name()).collect();
        assert_eq!(names, ours);
    }

    #[test]
    fn action_help() {
        assert_eq!(
            "The `version` command is used to display information about Ghostty. Recognized as\n\
             either `+version` or `--version`.",
            ACTIONS[0].help()
        );
        assert_eq!("cli/list_fonts.rs", ACTIONS[2].file());
    }
}
