//! Port of `src/termio.zig`.
//!
//! STATUS: ported
//!
//! Termio is responsible for "terminal IO." Specifically, this is the
//! reading and writing of bytes for the underlying pty or pty-like device.
//!
//! Termio is constructed of a few components:
//!   - Termio - The main shared struct that has common logic across all
//!     backends and mailboxes (defined below).
//!   - Backend - Responsible for the actual physical IO. For example, one
//!     implementation creates a subprocess, allocates and assigns a pty,
//!     and sets up a read thread on the pty.
//!   - Mailbox - Responsible for storing/dispensing event messages to
//!     the backend. This exists separately from backends because termio
//!     is built to be both single and multi-threaded.
//!
//! Termio supports (and recommends) multi-threaded operation. Multi-threading
//! enables the read/writes to generally happen on separate threads and
//! almost always improves throughput and latency under heavy IO load. To
//! enable threading, use the Thread struct. This wraps a Termio, requires
//! specific backend/mailbox capabilities, and sets up the necessary threads.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "termio/backend.rs"]
pub mod backend;
#[path = "termio/exec.rs"]
pub mod exec;
#[path = "termio/mailbox.rs"]
pub mod mailbox;
#[path = "termio/message.rs"]
pub mod message;
#[path = "termio/options.rs"]
pub mod options;
#[path = "termio/shell_integration.rs"]
pub mod shell_integration;
#[path = "termio/stream_handler.rs"]
pub mod stream_handler;
#[path = "termio/termio.rs"]
pub mod termio;
#[path = "termio/thread.rs"]
pub mod thread;
// @generated-mods-end

pub use self::backend::Backend;
pub use self::exec::Exec;
pub use self::mailbox::Mailbox;
pub use self::message::Message;
pub use self::options::Options;
pub use self::stream_handler::StreamHandler;
pub use self::termio::Termio;
pub use self::termio::{DerivedConfig, Error, MutexState, ThreadData};
pub use self::thread::Thread;
