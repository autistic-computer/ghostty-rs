//! Port of `src/terminal/osc/parsers.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "parsers/change_window_icon.rs"]
pub mod change_window_icon;
#[path = "parsers/change_window_title.rs"]
pub mod change_window_title;
#[path = "parsers/clipboard_operation.rs"]
pub mod clipboard_operation;
#[path = "parsers/color.rs"]
pub mod color;
#[path = "parsers/hierarchical_context.rs"]
pub mod hierarchical_context;
#[path = "parsers/hyperlink.rs"]
pub mod hyperlink;
#[path = "parsers/iterm2.rs"]
pub mod iterm2;
#[path = "parsers/kitty_clipboard_protocol.rs"]
pub mod kitty_clipboard_protocol;
#[path = "parsers/kitty_color.rs"]
pub mod kitty_color;
#[path = "parsers/kitty_desktop_notification.rs"]
pub mod kitty_desktop_notification;
#[path = "parsers/kitty_dnd_protocol.rs"]
pub mod kitty_dnd_protocol;
#[path = "parsers/kitty_text_sizing.rs"]
pub mod kitty_text_sizing;
#[path = "parsers/mouse_shape.rs"]
pub mod mouse_shape;
#[path = "parsers/osc9.rs"]
pub mod osc9;
#[path = "parsers/program_status.rs"]
pub mod program_status;
#[path = "parsers/report_pwd.rs"]
pub mod report_pwd;
#[path = "parsers/rxvt_extension.rs"]
pub mod rxvt_extension;
#[path = "parsers/semantic_prompt.rs"]
pub mod semantic_prompt;
#[path = "parsers/unknown.rs"]
pub mod unknown;
// @generated-mods-end
