//! Port of `src/macos/os.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "os/c.rs"]
pub mod c;
#[path = "os/log.rs"]
pub mod log;
#[path = "os/signpost.rs"]
pub mod signpost;
// @generated-mods-end

pub use self::log::{Log, LogType};
