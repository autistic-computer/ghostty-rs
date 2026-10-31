//! Port of `src/apprt/surface.zig`.
//!
//! STATUS: ported
//!
//! Deviations:
//! * `ChildExited.getGObjectType` (GObject boxed registration) is skipped;
//!   the GTK apprt registers its own GObject types.
//! * Pointers that cross threads (`*Surface`, `*const Config`) stay raw
//!   pointers like in Zig; Kitty clipboard request states are owned
//!   `Box`es (the receiver takes ownership, as documented in Zig).
//! * `newConfig` drops the allocator and cannot fail.

use crate::app::{App, Mailbox as AppMailbox, Message as AppMessage};
use crate::surface::Surface as CoreSurface;
use crate::terminal::page_list::Scrollbar;
use crate::config::config::WorkingDirectory;
use crate::config::Config;
use crate::apprt::structs::{Clipboard, KittyRead, KittyWrite};
use crate::datastruct::blocking_queue::{Size as QueueSize, Timeout as QueueTimeout};
use crate::datastruct::MessageData;
use crate::renderer::Health;
use crate::terminal::mouse::Shape as MouseShape;
use crate::terminal::osc::color::ColoredTarget;
use crate::terminal::osc::ProgressReport;

use crate::terminal::stream::ResizeWindow;


/// Represents a write request. Magic number comes from the max size
/// we want this union to be.
pub type WriteReq = MessageData<u8, 255>;

/// The message types that can be sent to a single surface.
#[derive(Debug)]
pub enum Message {
    /// Set the title of the surface.
    /// TODO: we should change this to a "WriteReq" style structure in
    /// the termio message so that we can more efficiently send strings
    /// of any length
    SetTitle([u8; 256]),

    /// Report the window title back to the terminal
    ReportTitle(ReportTitleStyle),

    /// Resize the window to the given grid size, as requested by the
    /// running program (CSI 8 t).
    ResizeWindow(ResizeWindow),

    /// Set the mouse shape.
    SetMouseShape(MouseShape),

    /// Read the clipboard and write to the pty.
    ClipboardRead(Clipboard),

    /// A Kitty clipboard protocol (OSC 5522) read request. The receiver
    /// takes ownership of the request state and must eventually destroy
    /// it.
    KittyClipboardRead(Box<KittyRead>),

    /// A committed Kitty clipboard protocol (OSC 5522) write
    /// transaction. The receiver takes ownership of the request state
    /// and must eventually destroy it.
    KittyClipboardWrite(Box<KittyWrite>),

    /// Write the clipboard contents.
    ClipboardWrite {
        clipboard_type: Clipboard,
        req: WriteReq,
    },

    /// Change the configuration to the given configuration. The pointer is
    /// not valid after receiving this message so any config must be used
    /// and derived immediately.
    ChangeConfig(*const Config),

    /// Close the surface. This will only close the current surface that
    /// receives this, not the full application.
    Close,

    /// The child process running in the surface has exited. This may trigger
    /// a surface close, it may not. Additional details about the child
    /// command are given in the `ChildExited` struct.
    ChildExited(ChildExited),

    /// Show a desktop notification.
    DesktopNotification(DesktopNotification),

    /// Health status change for the renderer.
    RendererHealth(Health),

    /// Tell the surface to present itself to the user. This may require raising
    /// a window and switching tabs.
    PresentSurface,

    /// Notifies the surface that password input has started within
    /// the terminal. This should always be followed by a false value
    /// unless the surface exits.
    PasswordInput(bool),

    /// A terminal color was changed using OSC sequences.
    ColorChange(ColoredTarget),

    /// Notifies the surface that a tick of the timer that is timing
    /// out selection scrolling has occurred. "selection scrolling"
    /// is when the user has clicked and dragged the mouse outside
    /// the viewport of the terminal and the terminal is scrolling
    /// the viewport to follow the mouse cursor.
    SelectionScrollTick(bool),

    /// The terminal has reported a change in the working directory.
    PwdChange(WriteReq),

    /// The terminal encountered a bell character.
    RingBell,

    /// Report the progress of an action using a GUI element
    ProgressReport(ProgressReport),

    /// A command has started in the shell, start a timer.
    StartCommand,

    /// A command has finished in the shell, stop the timer and send out
    /// notifications as appropriate. The optional u8 is the exit code
    /// of the command.
    StopCommand(Option<u8>),

    /// The scrollbar state changed for the surface.
    Scrollbar(Scrollbar),

    /// Search progress update
    SearchTotal(Option<usize>),

    /// Selected search index change
    SearchSelected(Option<usize>),

    /// Renderer pushed a new frame, redraw this surface.
    Redraw,
}

// SAFETY: the only non-Send member is the `change_config` pointer, which
// (like in Zig) is only dereferenced by the receiver while the sender
// keeps the config alive.
unsafe impl Send for Message {}

/// A fixed-size desktop notification payload sent to the app thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopNotification {
    /// Desktop notification title. (Zig `[63:0]u8`: 63 bytes plus the
    /// sentinel.)
    pub title: [u8; 64],

    /// Desktop notification body. (Zig `[255:0]u8`.)
    pub body: [u8; 256],
}

impl DesktopNotification {
    pub fn init(title: &[u8], body: &[u8]) -> DesktopNotification {
        let mut result = DesktopNotification {
            title: [0; 64],
            body: [0; 256],
        };
        Self::copy_utf8_z(&mut result.title, title);
        Self::copy_utf8_z(&mut result.body, body);
        result
    }

    /// UTF-8 continuation bytes occupy the range 0x80 through 0xBF.
    fn is_utf8_continuation_byte(byte: u8) -> bool {
        matches!(byte, 0x80..=0xBF)
    }

    /// Copy as much of `src` as fits, backing up from a UTF-8 continuation
    /// byte so valid input is never truncated in the middle of a codepoint.
    ///
    /// `dst` is the sentinel-terminated buffer including its sentinel slot
    /// (Zig: `*[capacity:0]u8`), so the capacity is `dst.len() - 1`.
    fn copy_utf8_z(dst: &mut [u8], src: &[u8]) {
        let capacity = dst.len() - 1;
        let mut len = src.len().min(capacity);
        while len > 0 && len < src.len() && Self::is_utf8_continuation_byte(src[len]) {
            len -= 1;
        }

        dst[..len].copy_from_slice(&src[..len]);
        dst[len] = 0;
    }

    /// The title up to the first NUL (Zig: `std.mem.sliceTo(&title, 0)`).
    pub fn title_slice(&self) -> &[u8] {
        slice_to_nul(&self.title)
    }

    /// The body up to the first NUL.
    pub fn body_slice(&self) -> &[u8] {
        slice_to_nul(&self.body)
    }
}

fn slice_to_nul(buf: &[u8]) -> &[u8] {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    &buf[..end]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportTitleStyle {
    Csi21T,
    // This enum is a placeholder for future title styles.
}

/// Note: Zig's `getGObjectType` (a GObject boxed type registration when
/// building the GTK runtime) is not ported here.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChildExited {
    pub exit_code: u32,
    pub runtime_ms: u64,
}

/// A surface mailbox.
#[derive(Debug, Clone, Copy)]
pub struct Mailbox {
    pub surface: *mut CoreSurface,
    pub app: AppMailbox,
}

// SAFETY: like Zig, the surface pointer is only used as an identifier by
// the app thread that owns the surface; the app mailbox is thread-safe.
unsafe impl Send for Mailbox {}
unsafe impl Sync for Mailbox {}

impl Mailbox {
    /// Send a message to the surface.
    pub fn push(&self, msg: Message, timeout: QueueTimeout) -> QueueSize {
        // Surface message sending is actually implemented on the app
        // thread, so we have to rewrap the message with our surface
        // pointer and send it to the app thread.
        self.app.push(
            AppMessage::SurfaceMessage {
                surface: self.surface,
                message: msg,
            },
            timeout,
        )
    }
}

/// Context for new surface creation to determine inheritance behavior
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum NewSurfaceContext {
    Window = 0,
    Tab = 1,
    Split = 2,
}

pub fn should_inherit_working_directory(context: NewSurfaceContext, config: &Config) -> bool {
    match context {
        NewSurfaceContext::Window => config.window_inherit_working_directory,
        NewSurfaceContext::Tab => config.tab_inherit_working_directory,
        NewSurfaceContext::Split => config.split_inherit_working_directory,
    }
}

/// Returns a new config for a surface for the given app that should be
/// used for any new surfaces. The resulting config should be deinitialized
/// after the surface is initialized.
pub fn new_config(app: &App, config: &Config, context: NewSurfaceContext) -> Config {
    // Create a shallow clone
    let mut copy = config.shallow_clone();

    // Get our previously focused surface for some inherited values.
    let prev = app.focused_surface();
    if let Some(p) = prev {
        if should_inherit_working_directory(context, config) {
            // SAFETY: focused_surface only returns live surfaces.
            if let Some(pwd) = unsafe { (*p).pwd() } {
                copy.working_directory = Some(WorkingDirectory::Path(
                    String::from_utf8_lossy(&pwd).into_owned(),
                ));
            }
        }
    }

    copy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_notification_init() {
        let notification = DesktopNotification::init(b"Title", b"Body");

        assert_eq!(b"Title", notification.title_slice());
        assert_eq!(b"Body", notification.body_slice());
    }

    #[test]
    fn copy_utf8_z_handles_len_at_the_final_byte_of_every_utf8_sequence_length() {
        let mut dst_1_byte = [0u8; 2];
        let src_ending_in_1_byte_codepoint = "ab".as_bytes();
        assert!(!DesktopNotification::is_utf8_continuation_byte(
            src_ending_in_1_byte_codepoint[dst_1_byte.len() - 1]
        ));
        DesktopNotification::copy_utf8_z(&mut dst_1_byte, src_ending_in_1_byte_codepoint);
        assert_eq!(b"a", slice_to_nul(&dst_1_byte));

        let mut dst_2_bytes = [0u8; 3];
        let src_ending_in_2_byte_codepoint = "aЯ".as_bytes();
        assert!(DesktopNotification::is_utf8_continuation_byte(
            src_ending_in_2_byte_codepoint[dst_2_bytes.len() - 1]
        ));
        DesktopNotification::copy_utf8_z(&mut dst_2_bytes, src_ending_in_2_byte_codepoint);
        assert_eq!(b"a", slice_to_nul(&dst_2_bytes));

        let mut dst_3_bytes = [0u8; 4];
        let src_ending_in_3_byte_codepoint = "a€".as_bytes();
        assert!(DesktopNotification::is_utf8_continuation_byte(
            src_ending_in_3_byte_codepoint[dst_3_bytes.len() - 1]
        ));
        DesktopNotification::copy_utf8_z(&mut dst_3_bytes, src_ending_in_3_byte_codepoint);
        assert_eq!(b"a", slice_to_nul(&dst_3_bytes));

        let mut dst_4_bytes = [0u8; 5];
        let src_ending_in_4_byte_codepoint = "a😀".as_bytes();
        assert!(DesktopNotification::is_utf8_continuation_byte(
            src_ending_in_4_byte_codepoint[dst_4_bytes.len() - 1]
        ));
        DesktopNotification::copy_utf8_z(&mut dst_4_bytes, src_ending_in_4_byte_codepoint);
        assert_eq!(b"a", slice_to_nul(&dst_4_bytes));
    }

    #[test]
    fn copy_utf8_z_keeps_a_complete_codepoint_at_the_truncation_boundary() {
        let mut dst = [0u8; 6];

        DesktopNotification::copy_utf8_z(&mut dst, "abcЯz".as_bytes());

        assert_eq!("abcЯ".as_bytes(), slice_to_nul(&dst));
    }

    #[test]
    fn copy_utf8_z_preserves_utf8_that_fits() {
        let mut dst = [0u8; 6];

        DesktopNotification::copy_utf8_z(&mut dst, "abcЯ".as_bytes());

        assert_eq!("abcЯ".as_bytes(), slice_to_nul(&dst));
    }

    // Extra (not in Zig): mailbox rewrapping. (New config working
    // directory inheritance needs a live core surface and is covered by
    // the integration test in `crate::surface`.)
    #[test]
    fn mailbox_push_wraps_surface_message() {
        let rt_app = crate::apprt::App::default();
        let queue = crate::app::MailboxQueue::new();
        // The surface pointer is only an identifier here.
        let core = std::ptr::NonNull::<CoreSurface>::dangling().as_ptr();
        let mb = Mailbox {
            surface: core,
            app: AppMailbox {
                rt_app: &rt_app,
                mailbox: &queue,
            },
        };
        assert_eq!(1, mb.push(Message::RingBell, QueueTimeout::Instant));
        match queue.pop() {
            Some(AppMessage::SurfaceMessage { surface, message }) => {
                assert_eq!(core, surface);
                assert!(matches!(message, Message::RingBell));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(1, rt_app.wakeups());
    }

    #[test]
    fn new_config_without_focused_surface() {
        let app = App::create().unwrap();
        let mut config = Config::default();
        config.window_inherit_working_directory = true;
        let c = new_config(&app, &config, NewSurfaceContext::Window);
        assert_eq!(None, c.working_directory);
        app.destroy();
    }
}
