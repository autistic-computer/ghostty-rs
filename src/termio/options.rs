//! Port of `src/termio/Options.zig`.
//!
//! STATUS: ported
//!
//! The options that are used to configure a terminal IO implementation.
//!
//! Pointers to objects owned elsewhere (the full config during init, the
//! renderer state and mailbox) stay raw pointers like in Zig.

use crate::apprt;
use crate::config::Config;
use crate::global::xev;
use crate::renderer;
use crate::termio;

#[derive(Debug)]
pub struct Options {
    /// All size metrics for the terminal.
    pub size: renderer::Size,

    /// The full app configuration. This is only available during initialization.
    /// The memory it points to is NOT stable after the init call so any values
    /// in here must be copied.
    pub full_config: *const Config,

    /// The derived configuration for this termio implementation.
    pub config: termio::DerivedConfig,

    /// The backend for termio that implements where reads/writes are sourced.
    pub backend: termio::Backend,

    /// The mailbox for the terminal. This is how messages are delivered.
    /// If you're using termio.Thread this MUST be "mailbox".
    pub mailbox: termio::Mailbox,

    /// The render state. The IO implementation can modify anything here. The
    /// surface thread will setup the initial "terminal" pointer but the IO impl
    /// is free to change that if that is useful (i.e. doing some sort of dual
    /// terminal implementation.)
    pub renderer_state: *mut renderer::State,

    /// A handle to wake up the renderer. This hints to the renderer that
    /// a repaint should happen.
    pub renderer_wakeup: xev::Async,

    /// The mailbox for renderer messages.
    pub renderer_mailbox: *const renderer::thread::Mailbox,

    /// The mailbox for sending the surface messages.
    pub surface_mailbox: apprt::surface::Mailbox,
}
