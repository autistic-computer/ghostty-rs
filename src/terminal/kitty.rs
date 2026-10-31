//! Port of `src/terminal/kitty.zig`.
//!
//! STATUS: ported
//!
//! Types and functions related to Kitty protocols.
//!
//! `graphics` is only compiled when `build_options.kitty_graphics` is set in
//! Zig; the Rust module is always compiled and consumers check
//! `build_options::KITTY_GRAPHICS`.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "kitty/clipboard.rs"]
pub mod clipboard;
#[path = "kitty/clipboard_command.rs"]
pub mod clipboard_command;
#[path = "kitty/clipboard_grants.rs"]
pub mod clipboard_grants;
#[path = "kitty/clipboard_response.rs"]
pub mod clipboard_response;
#[path = "kitty/clipboard_write.rs"]
pub mod clipboard_write;
#[path = "kitty/color.rs"]
pub mod color;
#[path = "kitty/dnd.rs"]
pub mod dnd;
#[path = "kitty/dnd_command.rs"]
pub mod dnd_command;
#[path = "kitty/dnd_drop.rs"]
pub mod dnd_drop;
#[path = "kitty/dnd_response.rs"]
pub mod dnd_response;
#[path = "kitty/dnd_test.rs"]
pub mod dnd_test;
#[path = "kitty/graphics.rs"]
pub mod graphics;
#[path = "kitty/graphics_animation.rs"]
pub mod graphics_animation;
#[path = "kitty/graphics_command.rs"]
pub mod graphics_command;
#[path = "kitty/graphics_exec.rs"]
pub mod graphics_exec;
#[path = "kitty/graphics_image.rs"]
pub mod graphics_image;
#[path = "kitty/graphics_pixel.rs"]
pub mod graphics_pixel;
#[path = "kitty/graphics_render.rs"]
pub mod graphics_render;
#[path = "kitty/graphics_storage.rs"]
pub mod graphics_storage;
#[path = "kitty/graphics_unicode.rs"]
pub mod graphics_unicode;
#[path = "kitty/key.rs"]
pub mod key;
#[path = "kitty/windows.rs"]
pub mod windows;
// @generated-mods-end

pub use self::key::Flags as KeyFlags;
pub use self::key::FlagStack as KeyFlagStack;
pub use self::key::SetMode as KeySetMode;
