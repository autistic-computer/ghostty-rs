//! Port of `src/inspector/main.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "inspector.rs"]
pub mod inspector;
#[path = "units.rs"]
pub mod units;
#[path = "widgets.rs"]
pub mod widgets;
// @generated-mods-end

pub use self::inspector::Inspector;

pub type KeyEvent = widgets::key::Event;
