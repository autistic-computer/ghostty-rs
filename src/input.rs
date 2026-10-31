//! Port of `src/input.zig`.
//!
//! STATUS: ported
//!
//! The `pub const foo = @import("input/foo.zig")` namespace re-exports are
//! the generated `pub mod` declarations below. `refAllDecls` test dropped.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "input/binding.rs"]
pub mod binding;
#[path = "input/command.rs"]
pub mod command;
#[path = "input/config.rs"]
pub mod config;
#[path = "input/function_keys.rs"]
pub mod function_keys;
#[path = "input/helpgen_actions.rs"]
pub mod helpgen_actions;
#[path = "input/key.rs"]
pub mod key;
#[path = "input/key_encode.rs"]
pub mod key_encode;
#[path = "input/key_mods.rs"]
pub mod key_mods;
#[path = "input/keyboard.rs"]
pub mod keyboard;
#[path = "input/keycodes.rs"]
pub mod keycodes;
#[path = "input/keymap_darwin.rs"]
pub mod keymap_darwin;
#[path = "input/keymap_noop.rs"]
pub mod keymap_noop;
#[path = "input/kitty.rs"]
pub mod kitty;
#[path = "input/link.rs"]
pub mod link;
#[path = "input/mouse.rs"]
pub mod mouse;
#[path = "input/mouse_encode.rs"]
pub mod mouse_encode;
#[path = "input/paste.rs"]
pub mod paste;
// @generated-mods-end

pub use self::key::ctrl_or_super;
pub use self::key::Action;
pub use self::binding::Binding;
pub use self::command::Command;
pub use self::link::Link;
pub use self::key::Key;
pub use self::keyboard::Layout as KeyboardLayout;
pub use self::key::KeyEvent;
pub use self::key_mods::RemapSet as KeyRemapSet;
pub use self::binding::InspectorMode;
pub use self::key_mods::Mods;
pub use self::mouse::Action as MouseAction;
pub use self::mouse::Button as MouseButton;
pub use self::mouse::ButtonState as MouseButtonState;
pub use self::mouse::PressureStage as MousePressureStage;
pub use self::config::OptionAsAlt;
pub use self::mouse::ScrollMods;
pub use self::binding::SplitFocusDirection;
pub use self::binding::SplitResizeDirection;
pub use self::binding::Trigger;

// Keymap is only available on macOS right now. We could implement it
// in theory for XKB too on Linux but we don't need it right now.
#[cfg(target_os = "macos")]
pub use self::keymap_darwin::Keymap;
#[cfg(not(target_os = "macos"))]
pub use self::keymap_noop::KeymapNoop as Keymap;
