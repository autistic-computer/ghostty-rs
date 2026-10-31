//! Rust implementation of the subset of [libxev](https://github.com/mitchellh/libxev)
//! (revision `819ffa180ca20e31c7eded74e2e1aceb1eab557f`, the version pinned by
//! Ghostty's `build.zig.zon`) that Ghostty uses. libxev is an external Zig
//! dependency, so this module has no Zig counterpart inside the Ghostty tree.
//!
//! STATUS: ported — the subset Ghostty uses: `Loop`, `Completion`,
//! `CallbackAction`, `Async`, `Timer`, `Process`, `Stream` (read, write,
//! queued writes) and dynamic backend selection. Not ported: io_uring,
//! kqueue, IOCP and WASI backends (non-Linux Unix uses a `poll(2)`
//! emulation of the epoll loop), the thread pool, `File`, `TCP`, `UDP`,
//! the socket operations, stream close/poll and the C API.
//!
//! # API shape
//!
//! The API mirrors libxev's so that Ghostty's threads (termio, renderer,
//! search, ...) port mechanically:
//!
//! * Completions are caller-owned memory. Arming one stores a raw pointer
//!   to it in the [`Loop`]; it must not move or be dropped until it is dead
//!   again. Every arming function is therefore an `unsafe fn` documenting
//!   that contract (see [`epoll`]).
//! * Callbacks are plain `fn` pointers taking
//!   `(userdata: *mut U, loop: &mut Loop, completion: &mut Completion, result)`
//!   and returning a [`CallbackAction`]. Zig's `?*Userdata` is a raw
//!   (possibly null) `*mut U`.
//! * [`Async::notify`] may be called from any thread.

// Module declarations are hand written (this directory is not generated
// by tools/scaffold.py).
#[path = "backend.rs"]
pub mod backend;
#[path = "common.rs"]
pub mod common;
#[path = "epoll.rs"]
pub mod epoll;
#[path = "poller.rs"]
pub mod poller;
#[path = "posix.rs"]
pub mod posix;
#[path = "watcher/mod.rs"]
pub mod watcher;

pub use backend::{backend, detect, prefer, Backend, DYNAMIC};
pub use common::{noop_callback, Callback, CallbackAction, CompletionState, Options, RunMode};
pub use epoll::{
    Completion, Loop, OpResult, Operation, OperationType, ReadBuffer, TimerError, TimerTrigger,
    WriteBuffer,
};
pub use posix::{CancelError, CloseError, Error, PollError, ReadError, WriteError};
pub use watcher::process::Process;
pub use watcher::r#async::Async;
pub use watcher::stream::{Stream, WriteQueue, WriteRequest};
pub use watcher::timer::Timer;

/// This is used to detect a static vs dynamic API (Zig: `xev.dynamic`).
pub const fn dynamic() -> bool {
    DYNAMIC
}

pub use watcher::process;
/// Zig: `Async.WaitError` and friends, namespaced like the Zig decls
/// (`xev.Async.WaitError` is `xev::r#async::WaitError`).
pub use watcher::r#async;
pub use watcher::stream;
pub use watcher::timer;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_is_zero_able() {
        let c = Completion::default();
        assert_eq!(c.state(), CompletionState::Dead);
    }

    #[test]
    fn loop_basics() {
        detect().unwrap();
        let mut l = Loop::init(Options::default()).unwrap();
        l.run(RunMode::UntilDone).unwrap();
        l.stop();
        assert!(l.stopped());
    }

    #[test]
    fn loop_deinit_is_idempotent() {
        let mut l = Loop::init(Options::default()).unwrap();
        l.deinit();
        l.deinit();
    }
}
