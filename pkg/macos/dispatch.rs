//! Port of `src/macos/dispatch.zig`.
//!
//! STATUS: ported

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "dispatch/c.rs"]
pub mod c;
#[path = "dispatch/data.rs"]
pub mod data;
#[path = "dispatch/queue.rs"]
pub mod queue;
// @generated-mods-end

use std::ffi::c_void;

pub use data::Data;

extern "C" {
    pub fn dispatch_sync(queue: *mut c_void, block: *mut c_void);
    pub fn dispatch_async(queue: *mut c_void, block: *mut c_void);
}
