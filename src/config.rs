//! Port of `src/config.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "config/c_api.rs"]
pub mod c_api;
#[path = "config/c_get.rs"]
pub mod c_get;
#[path = "config/clipboard_codepoint_map.rs"]
pub mod clipboard_codepoint_map;
#[path = "config/command.rs"]
pub mod command;
#[path = "config/conditional.rs"]
pub mod conditional;
#[path = "config/config.rs"]
pub mod config;
#[path = "config/edit.rs"]
pub mod edit;
#[path = "config/error_list.rs"]
pub mod error_list;
#[path = "config/file_load.rs"]
pub mod file_load;
#[path = "config/formatter.rs"]
pub mod formatter;
#[path = "config/formatter_file.rs"]
pub mod formatter_file;
#[path = "config/io.rs"]
pub mod io;
#[path = "config/key.rs"]
pub mod key;
#[path = "config/limit.rs"]
pub mod limit;
#[path = "config/path.rs"]
pub mod path;
#[path = "config/repeatable_string_map.rs"]
pub mod repeatable_string_map;
#[path = "config/string.rs"]
pub mod string;
#[path = "config/theme.rs"]
pub mod theme;
#[path = "config/url.rs"]
pub mod url;
#[path = "config/wasm.rs"]
pub mod wasm;
// @generated-mods-end

pub use self::config::Config;

pub use self::conditional::State as ConditionalState;
pub use self::file_load::preferred_default_file_path;
pub use self::formatter::{entry_formatter, format_entry};
pub use self::formatter_file::FileFormatter;

// Field types
pub use self::config::BackgroundImageFit;
pub use self::config::BackgroundImagePosition;
pub use self::config::BoldColor;
pub use self::config::ClipboardAccess;
pub use self::config::Command;
pub use self::config::ConfirmCloseSurface;
pub use self::config::CopyOnSelect;
pub use self::config::CustomShaderAnimation;
pub use self::config::FontShapingBreak;
pub use self::config::FontStyle;
pub use self::config::FontSyntheticStyle;
pub use self::config::FreetypeLoadFlags;
pub use self::config::Fullscreen;
pub use self::config::Keybinds;
pub use self::config::LinkPreviews;
pub use self::config::MiddleClickAction;
pub use self::config::MouseScrollMultiplier;
pub use self::config::MouseShiftCapture;
pub use self::config::NonNativeFullscreen;
pub use self::config::Path;
pub use self::config::RepeatableCodepointMap;
pub use self::config::RepeatableFontVariation;
pub use self::config::RepeatablePath;
pub use self::config::RepeatableString;
pub use self::config::RightClickAction;
pub use self::config::ShellIntegrationFeatures;
pub use self::config::WindowDecoration;
pub use self::config::WindowPaddingColor;
pub use self::config::WorkingDirectory;
pub use self::repeatable_string_map::RepeatableStringMap;

// Alternate APIs
pub use self::c_api as capi;
#[cfg(target_arch = "wasm32")]
pub use self::wasm as wasm_api;
