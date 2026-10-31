//! Port of `src/renderer/message.zig`.
//!
//! STATUS: ported
//!
//! Deviations: payloads that Zig allocates (the derived configs, the
//! arena-backed search matches) are owned values (`Box`/`Vec`) freed on
//! drop; `deinit` consumes the message. The font grid pointers stay raw
//! pointers like in Zig (they are reference counted by the grid set).
//!

use crate::config::Config;
use crate::renderer::generic::{DerivedConfig as RendererDerivedConfig, InitError};
use crate::font::shared_grid_set::Key as SharedGridSetKey;
use crate::font::{SharedGrid, SharedGridSet};
use crate::renderer::thread::DerivedConfig as ThreadDerivedConfig;
use crate::renderer::{Health, Size};

pub use crate::terminal::highlight::Flattened;

/// The messages that can be sent to a renderer thread.
#[derive(Debug)]
pub enum Message {
    /// Purposely crash the renderer. This is used for testing and debugging.
    /// See the "crash" binding action.
    Crash,

    /// A change in state in the window focus that this renderer is
    /// rendering within. This is only sent when a change is detected so
    /// the renderer is expected to handle all of these.
    Focus(bool),

    /// A change in the view occlusion state. This can be used to determine
    /// if the window is visible or not. A window can be not visible (occluded)
    /// and still have focus.
    Visible(bool),

    /// Reset the cursor blink by immediately showing the cursor then
    /// restarting the timer.
    ResetCursorBlink,

    /// Change the font grid. This can happen for any number of reasons
    /// including a font size change, family change, etc.
    FontGrid {
        grid: *mut SharedGrid,
        set: *mut SharedGridSet,

        /// The key for the new grid. If adopting the new grid fails for any
        /// reason, the old grid should be kept but the new key should be
        /// dereferenced.
        new_key: SharedGridSetKey,

        /// After accepting the new grid, the old grid must be dereferenced
        /// using the fields below.
        old_key: SharedGridSetKey,
    },

    /// Changes the size. The screen size might change, padding, grid, etc.
    Resize(Size),

    /// The derived configuration to update the renderer with.
    ChangeConfig {
        thread: Box<ThreadDerivedConfig>,
        impl_: Box<RendererDerivedConfig>,
    },

    /// Matches for the current viewport from the search thread. These happen
    /// async so they may be off for a frame or two from the actually rendered
    /// viewport. The renderer must handle this gracefully.
    SearchViewportMatches(SearchMatches),

    /// The selected match from the search thread. May be null to indicate
    /// no match currently.
    SearchSelectedMatch(Option<SearchMatch>),

    /// Activate or deactivate the inspector.
    Inspector(bool),

    /// The macOS display ID has changed for the window.
    MacosDisplayId(u32),

    /// Updates the health of the current presentation state.
    ///
    /// If unhealthy, then somehow the apprt failed to present frames from
    /// the renderer. On GTK, this can be caused by failing to import a DMABUF.
    PresentationHealth(Health),
}

// SAFETY: the font grid pointers are only used by the renderer thread that
// receives the message; the grid set is thread-safe (same as Zig).
unsafe impl Send for Message {}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchMatches {
    pub matches: Vec<Flattened>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchMatch {
    pub r#match: Flattened,
}

impl Message {
    /// Initialize a change_config message.
    pub fn init_change_config(config: &Config) -> Result<Message, InitError> {
        let thread = Box::new(ThreadDerivedConfig::init(config));
        let impl_ = Box::new(RendererDerivedConfig::init(config)?);

        Ok(Message::ChangeConfig { thread, impl_ })
    }

    /// Free resources owned by a message that will not be processed.
    pub fn deinit(self) {
        match self {
            Message::ChangeConfig { thread, mut impl_ } => {
                impl_.deinit();
                drop(impl_);
                drop(thread);
            }

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_config_roundtrip() {
        let config = Config::default();
        let msg = Message::init_change_config(&config).unwrap();
        assert!(matches!(msg, Message::ChangeConfig { .. }));
        msg.deinit();
        Message::Focus(true).deinit();
    }
}
