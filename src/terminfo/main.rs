//! Port of `src/terminfo/main.zig`.
//!
//! STATUS: ported
//!
//! Package terminfo provides functionality related to terminfo/termcap files.
//!
//! At the time of writing this comment, the focus is on generating terminfo
//! files so that we can maintain our terminfo in Zig instead of hand-writing
//! the archaic (imo) terminfo format by hand. But eventually we may want to
//! extract this into a more full-featured library on its own.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "ghostty.rs"]
pub mod ghostty;
#[path = "source.rs"]
pub mod source;
// @generated-mods-end

pub use ghostty::{version, GHOSTTY};
pub use source::Source;
