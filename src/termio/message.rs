//! Port of `src/termio/message.zig`.
//!
//! STATUS: ported
//!
//! Deviations: allocated payloads (`change_config`, `write_alloc`, the
//! Kitty clipboard grant password) are owned `Box`es freed on drop, so the
//! allocator fields are gone and `deinit` consumes the message. The
//! `write_*` variants hold the [`MessageData`] payload types directly.

use crate::datastruct::message_data::Small;
use crate::datastruct::MessageData;
use crate::renderer;
use crate::termio::DerivedConfig;

/// Zig: `terminal.Terminal.ScrollViewport`.
pub use crate::terminal::terminal::ScrollViewport;

/// Represents a write request. Magic number comes from the largest
/// other union value. It can be upped if we add a larger union member
/// in the future.
pub type WriteReq = MessageData<u8, 38>;

/// `WriteReq.Small`
pub type WriteReqSmall = Small<u8, 38>;

/// The types of size reports that we support.
pub type SizeReport = crate::terminal::size_report::Style;

/// The messages that can be sent to an IO thread.
///
/// This is not a tiny structure (~40 bytes at the time of writing this comment),
/// but the number of messages we send to the IO thread is also very small.
/// At the current size we can queue 26,000 messages before consuming a MB of RAM.
#[derive(Debug)]
pub enum Message {
    /// Request a color scheme report is sent to the PTY.
    ColorSchemeReport {
        /// Force write the current color scheme
        force: bool,
    },

    /// Request a visibility report is sent to the PTY.
    VisibilityReport {
        /// The visibility state to report.
        visible: bool,

        /// Send the report even when mode 2033 is disabled.
        force: bool,
    },

    /// Purposely crash the renderer. This is used for testing and debugging.
    /// See the "crash" binding action.
    Crash,

    /// The derived configuration to update the implementation with. This
    /// is allocated and is expected to be freed when done.
    ChangeConfig(Box<DerivedConfig>),

    /// Activate or deactivate the inspector.
    Inspector(bool),

    /// Resize the window.
    Resize(renderer::Size),

    /// Request a size report is sent to the PTY ([in-band
    /// size report, mode 2048](https://gist.github.com/rockorager/e695fb2924d36b2bcf1fff4a3704bd83) and
    /// [XTWINOPS](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h4-Functions-using-CSI-_-ordered-by-the-final-character-lparen-s-rparen:CSI-Ps;Ps;Ps-t.1EB0)).
    SizeReport(SizeReport),

    /// Clear the screen.
    ClearScreen {
        /// Include clearing the history
        history: bool,
    },

    /// Scroll the viewport
    ScrollViewport(ScrollViewport),

    /// Selection scrolling. If this is set to true then the termio
    /// thread starts a timer that will trigger a `selection_scroll_tick`
    /// message back to the surface. This ping/pong is because the
    /// surface thread doesn't have access to an event loop from libghostty.
    SelectionScroll(bool),

    /// Jump forward/backward n prompts.
    JumpToPrompt(isize),

    /// Send this when a synchronized output mode is started. This will
    /// start the timer so that the output mode is disabled after a
    /// period of time so that a bad actor can't hang the terminal.
    StartSynchronizedOutput,

    /// Enable or disable linefeed mode (mode 20).
    LinefeedMode(bool),

    /// The surface gained or lost focus.
    Focused(bool),

    /// Record a Kitty clipboard protocol session grant for a password
    /// so future requests carrying it skip the permission prompt, for
    /// reads and writes respectively.
    KittyClipboardGrantRead(KittyClipboardGrant),
    KittyClipboardGrantWrite(KittyClipboardGrant),

    /// Write where the data fits in the union.
    WriteSmall(WriteReqSmall),

    /// Write where the data pointer is stable.
    WriteStable(&'static [u8]),

    /// Write where the data is allocated and must be freed.
    WriteAlloc(Box<[u8]>),
}

/// The payload of the kitty_clipboard_grant_* messages. The
/// password is allocated and must be freed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyClipboardGrant {
    pub pw: Box<[u8]>,
}

impl Message {
    /// Return a write request for the given data. This will use
    /// write_small if it fits or write_alloc otherwise. This should NOT
    /// be used for stable pointers which can be manually set to write_stable.
    pub fn write_req(data: &[u8]) -> Message {
        match WriteReq::init(data) {
            MessageData::Stable(_) => unreachable!(),
            MessageData::Small(v) => Message::WriteSmall(v),
            MessageData::Alloc(v) => Message::WriteAlloc(v),
        }
    }

    /// Free resources owned by a message that will not be processed.
    /// The message is invalid after this call.
    pub fn deinit(self) {
        match self {
            Message::ChangeConfig(mut v) => {
                v.deinit();
                drop(v);
            }
            Message::WriteAlloc(v) => drop(v),
            Message::KittyClipboardGrantRead(v) | Message::KittyClipboardGrantWrite(v) => drop(v),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_size() {
        // Ensure we don't grow our IO message size without explicitly wanting to.
        assert_eq!(40usize, std::mem::size_of::<Message>());
    }

    #[test]
    fn write_req() {
        match Message::write_req(b"hello") {
            Message::WriteSmall(v) => {
                assert_eq!(5, v.len);
                assert_eq!(b"hello", &v.data[..5]);
            }
            other => panic!("unexpected {other:?}"),
        }

        let big = [b'x'; 64];
        let msg = Message::write_req(&big);
        match &msg {
            Message::WriteAlloc(v) => assert_eq!(&big[..], &v[..]),
            other => panic!("unexpected {other:?}"),
        }
        msg.deinit();
        assert_eq!(38, WriteReqSmall::MAX);
    }
}
