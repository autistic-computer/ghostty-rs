//! Port of `src/terminal/tmux.zig`.
//!
//! STATUS: ported
//!
//! Types and functions related to tmux protocols.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "tmux/control.rs"]
pub mod control;
#[path = "tmux/layout.rs"]
pub mod layout;
#[path = "tmux/output.rs"]
pub mod output;
#[path = "tmux/viewer.rs"]
pub mod viewer;
// @generated-mods-end

pub use control::Notification as ControlNotification;
pub use control::Parser as ControlParser;
pub use layout::Layout;
pub use viewer::Viewer;
