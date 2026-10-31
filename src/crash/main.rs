//! Port of `src/crash/main.zig`.
//!
//! STATUS: ported
//!
//! The crash package contains all the logic around crash handling,
//! whether that's setting up the system to catch crashes (Sentry client),
//! introspecting crash reports, writing crash reports to disk, etc.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "dir.rs"]
pub mod dir;
#[path = "sentry.rs"]
pub mod sentry;
#[path = "sentry_envelope.rs"]
pub mod sentry_envelope;
// @generated-mods-end

pub use dir::{default_dir, Dir, Report, ReportIterator};
pub use sentry_envelope::Envelope;

// The main init/deinit functions for global state.
pub use sentry::{deinit, init};
