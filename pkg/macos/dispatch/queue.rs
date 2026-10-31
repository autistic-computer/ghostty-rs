//! Port of `src/macos/dispatch/queue.zig`.
//!
//! STATUS: ported

use super::c;

/// `dispatch_queue_t`
pub type Queue = *mut std::ffi::c_void;

pub fn get_main() -> Queue {
    c::dispatch_get_main_queue()
}
