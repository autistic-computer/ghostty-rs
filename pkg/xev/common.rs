//! Port of libxev `src/loop.zig`: common loop structures. The actual loop
//! implementation is in [`super::epoll`].

use std::ffi::c_void;

use super::epoll::{Completion, Loop, OpResult};

/// Common options across backends. Not all options apply to all backends.
/// Read the doc comment for individual fields to learn what backends they
/// apply to.
///
/// The Zig `thread_pool` option is not ported: Ghostty never passes one and
/// none of the operations of this port need blocking work on a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// The number of queued completions that can be in flight before
    /// requiring interaction with the kernel.
    ///
    /// Backends: io_uring
    pub entries: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self { entries: 256 }
    }
}

/// The loop run mode -- all backends are required to support this in some way.
/// Backends may provide backend-specific APIs that behave slightly differently
/// or in a more configurable way.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Run the event loop once. If there are no blocking operations ready,
    /// return immediately.
    NoWait = 0,

    /// Run the event loop once, waiting for at least one blocking operation
    /// to complete.
    Once = 1,

    /// Run the event loop until it is "done". "Doneness" is defined as
    /// there being no more completions that are active.
    UntilDone = 2,
}

/// The callback of the main Loop operations. Higher level interfaces may
/// use a different callback mechanism.
pub type Callback =
    fn(userdata: *mut c_void, l: &mut Loop, c: &mut Completion, result: OpResult) -> CallbackAction;

/// A callback that does nothing and immediately disarms. This
/// implements xev.Callback and is the default value for completions.
pub fn noop_callback(
    _: *mut c_void,
    _: &mut Loop,
    _: &mut Completion,
    _: OpResult,
) -> CallbackAction {
    CallbackAction::Disarm
}

/// The result type for callbacks. This should be used by all loop
/// implementations and higher level abstractions in order to control
/// what to do after the loop completes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackAction {
    /// The request is complete and is not repeated. For example, a read
    /// callback only fires once and is no longer watched for reads. You
    /// can always free memory associated with the completion prior to
    /// returning this.
    Disarm = 0,

    /// Requeue the same operation request with the same parameters
    /// with the event loop. This makes it easy to repeat a read, timer,
    /// etc. This rearms the request EXACTLY as-is. For example, the
    /// low-level timer interface for io_uring uses an absolute timeout.
    /// If you rearm the timer, it will fire immediately because the absolute
    /// timeout will be in the past.
    ///
    /// The completion is reused so it is not safe to use the same completion
    /// for anything else.
    Rearm = 1,
}

/// The state that a completion can be in.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionState {
    /// The completion is not being used and is ready to be configured
    /// for new work.
    Dead = 0,

    /// The completion is part of an event loop. This may be already waited
    /// on or in the process of being registered.
    Active = 1,
}
