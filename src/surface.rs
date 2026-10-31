//! Port of `src/Surface.zig`.
//!
//! STATUS: ported
//!
//! Surface represents a single terminal "surface". A terminal surface is
//! a minimal "widget" where the terminal is drawn and responds to events
//! such as keyboard and mouse. Each surface also creates and owns its pty
//! session.
//!
//! The word "surface" is used because it is left to the higher level
//! application runtime to determine if the surface is a window, a tab,
//! a split, a preview pane in a larger window, etc. This struct doesn't care:
//! it just draws and responds to events. The events come from the application
//! runtime so the runtime can determine when and how those are delivered
//! (i.e. with focus, without focus, and so on).
//!
//! Deviations from the Zig code:
//! * Zig initializes the surface in place (`init(self: *Surface, ...)`)
//!   because the renderer, threads and termio point into it. [`Surface::init`]
//!   returns a `Box` whose address is handed out before the fields are
//!   written. The termio is itself boxed ([`Termio::init`]) and
//!   `renderer_state.terminal` points at its terminal. Like Zig, teardown is
//!   the explicit [`Surface::deinit`].
//! * Errors are [`Error`], an anyhow-like wrapper with the two variants
//!   that callers match on (`UnsafePaste`, `UnauthorizedPaste`).
//! * The apprt surface/app are reached through raw pointers like in Zig.
//!   `rt_surface.clipboardRequest` borrows the request (the apprt clones what
//!   it keeps), so [`Surface::complete_clipboard_request`] and
//!   [`Surface::deny_clipboard_request`] take the request by reference and
//!   Kitty request "destroy" calls are drops of the owner's copy.
//! * The key sequence and key table stacks hold raw pointers to binding
//!   sets inside the derived config like Zig. Matched binding actions are
//!   cloned before being performed (they borrow the config).
//! * `std.Io.Timestamp` (awake clock) is [`Instant`]; mutex locking is the
//!   renderer state's [`Mutex`] with [`TinyIo`].
//! * `getProcessInfo` takes the info kind at runtime.

use std::ffi::{c_void, CStr, CString};
use std::io::Write as _;
use std::ptr::{addr_of_mut, NonNull};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::app::{self, App};
use crate::apprt;
use crate::apprt::action::{self as rt_action, Action as RtAction, Target};
use crate::apprt::structs::{Clipboard, ClipboardContent, ClipboardReadResult, ClipboardRequest, KittyRead, KittyWrite};
use crate::config::config::{
    ClipboardAccess, ConfirmCloseSurface, CopyOnSelect, Duration as ConfigDuration, Fullscreen, Keybinds,
    LinkPreviews, MiddleClickAction, MouseScrollMultiplier, MouseShiftCapture as ConfigMouseShiftCapture,
    NonNativeFullscreen, NotifyOnCommandFinish, NotifyOnCommandFinishAction, RepeatableClipboardCodepointMap,
    RightClickAction, ScrollToBottom, WindowPaddingBalance,
};
use crate::config::{conditional::Theme, Config, ConditionalState};
use crate::crash::sentry;
use crate::datastruct::blocking_queue::Timeout as QueueTimeout;
use crate::datastruct::MessageData;
use crate::font;
use crate::font::face::{DesiredSize, DEFAULT_DPI};
use crate::global;
use crate::input;
use crate::input::binding::{
    Action as BindingAction, Flags as BindingFlags, Scope as BindingScope, ScopedSurface, Set as BindingSet,
    Trigger, TriggerKey, Value as BindingValue,
};
use crate::input::{KeyEvent, Mods, MouseButton, MouseButtonState};
use crate::lib::tinyio::{Mutex, TinyIo};
use crate::os as internal_os;
use crate::pkg::oniguruma as oni;
use crate::pty::{ProcessInfo, ProcessInfoValue};
use crate::renderer::{self, Renderer};
use crate::simd;
use crate::surface_mouse::{self, SurfaceMouse};
use crate::terminal;
use crate::terminal::kitty::clipboard as kitty_clipboard;
use crate::terminal::page_list::{PageList, Pin};
use crate::terminal::point;
use crate::terminal::screen_set::Key as ScreenKey;
use crate::terminal::selection::Selection;
use crate::terminal::selection_gesture::{
    AutoscrollTick, Behavior, DeepPress, Drag, DragGeometry, Press, Release, SelectionGesture,
};
use crate::terminal::terminal::{ScrollViewport, Terminal};
use crate::termio::{self, message::WriteReqSmall, MutexState, Termio};

pub use crate::apprt::surface::{Mailbox, Message};

type InspectorKeyEvent = crate::inspector::KeyEvent;
type Inspector = crate::inspector::Inspector;

const LOG: &str = "surface";

/// Minimum window size in cells. This is used to prevent the window from
/// being resized to a size that is too small to be useful. These defaults
/// are chosen to match the default size of Mac's Terminal.app, but is
/// otherwise somewhat arbitrary.
pub const MIN_WINDOW_WIDTH_CELLS: u32 = 10;
pub const MIN_WINDOW_HEIGHT_CELLS: u32 = 4;

/// Minimum size in cells that the running program can resize the window
/// to (CSI 8 t). This is larger than the minimum window size so that a
/// program can't shrink the window to hide its output.
const MIN_RESIZE_WIDTH_CELLS: u32 = 40;
const MIN_RESIZE_HEIGHT_CELLS: u32 = 10;

/// The maximum number of key tables that can be active at any
/// given time. `activate_key_table` calls after this are ignored.
const MAX_ACTIVE_KEY_TABLES: usize = 8;

/// The errors from surface operations (Zig: the inferred error sets).
///
/// Like `anyhow::Error`, any `std::error::Error` converts into it; the
/// variants callers match on are explicit.
#[derive(Debug)]
pub enum Error {
    /// A paste was rejected as unsafe until the user confirms it.
    UnsafePaste,
    /// A clipboard access requires the user's confirmation.
    UnauthorizedPaste,
    /// Any other error.
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::UnsafePaste => f.write_str("UnsafePaste"),
            Error::UnauthorizedPaste => f.write_str("UnauthorizedPaste"),
            Error::Other(e) => std::fmt::Display::fmt(e, f),
        }
    }
}

impl<E: std::error::Error + Send + Sync + 'static> From<E> for Error {
    fn from(e: E) -> Self {
        Error::Other(Box::new(e))
    }
}

impl Error {
    /// An error with a message (for errors Zig names inline).
    fn msg(s: &'static str) -> Error {
        Error::Other(s.into())
    }
}

/// The effect of an input event. This can be used by callers to take
/// the appropriate action after an input event. For example, key
/// input can be forwarded to the OS for further processing if it
/// wasn't handled in any way by Ghostty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEffect {
    /// The input was not handled in any way by Ghostty and should be
    /// forwarded to other subsystems (i.e. the OS) for further
    /// processing.
    Ignored,

    /// The input was handled and consumed by Ghostty.
    Consumed,

    /// The input resulted in a close event for this surface so
    /// the surface, runtime surface, etc. pointers may all be
    /// unsafe to use so exit immediately.
    Closed,
}

/// The search state for the surface.
struct Search {
    state: Box<terminal::search::Thread>,
    thread: Option<JoinHandle<()>>,
}

impl Search {
    fn deinit(mut self) {
        // Notify the thread to stop
        if let Err(err) = self.state.stop.notify() {
            log::error!(target: LOG, "error notifying search thread to stop, may stall err={err}");
        }

        // Wait for the OS thread to quit
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }

        // Now it is safe to deinit the state
        self.state.deinit();
    }
}

/// Mouse state for the surface.

pub struct Mouse {
    /// The last tracked mouse button state by button, indexed by the
    /// button's enum value. `max` is the largest value, not a count, so
    /// the array needs one more slot than that to hold every button.
    pub click_state: [MouseButtonState; MouseButton::MAX + 1],

    /// The last mods state when the last mouse button (whatever it was) was
    /// pressed or release.
    pub mods: Mods,

    /// Gesture state for text selection.
    pub selection_gesture: SelectionGesture,

    /// The last x/y sent for mouse reports.
    pub event_point: Option<point::Coordinate>,

    /// The pressure stage for the mouse. This should always be none if
    /// the mouse is not pressed.
    pub pressure_stage: input::MousePressureStage,

    /// Pending scroll amounts for high-precision scrolls
    pub pending_scroll_x: f64,
    pub pending_scroll_y: f64,

    /// True if the mouse is hidden
    pub hidden: bool,

    /// True if the mouse position is currently over a link.
    pub over_link: bool,

    /// The last x/y in the cursor position for links. We use this to
    /// only process link hover events when the mouse actually moves cells.
    pub link_point: Option<point::Coordinate>,
}

impl Default for Mouse {
    fn default() -> Self {
        Mouse {
            click_state: [MouseButtonState::Release; MouseButton::MAX + 1],
            mods: Mods::default(),
            selection_gesture: SelectionGesture::default(),
            event_point: None,
            pressure_stage: input::MousePressureStage::None,
            pending_scroll_x: 0.0,
            pending_scroll_y: 0.0,
            hidden: false,
            over_link: false,
            link_point: None,
        }
    }
}

impl Mouse {
    /// Return the left-click pin only if it still belongs to the active screen.
    fn active_left_click_pin(&self, screens: &terminal::screen_set::ScreenSet) -> Option<terminal::page_list::PinPtr> {
        self.selection_gesture.validated_left_click_pin(screens)
    }
}

/// An entry of [`Keyboard::table_stack`].
#[derive(Debug, Clone, Copy)]
pub struct KeyTable {
    pub set: *const BindingSet,
    pub once: bool,
}

/// Keyboard state for the surface.
#[derive(Debug, Default)]
pub struct Keyboard {
    /// The currently active key sequence for the surface. If this is null
    /// then we're not currently in a key sequence.
    pub sequence_set: Option<*const BindingSet>,

    /// The queued keys when we're in the middle of a sequenced binding.
    /// These are flushed when the sequence is completed and unconsumed or
    /// invalid.
    ///
    /// This is naturally bounded due to the configuration maximum
    /// length of a sequence.
    pub sequence_queued: Vec<termio::message::WriteReq>,

    /// The stack of tables that is currently active. The first value
    /// in this is the first activated table (NOT the default keybinding set).
    ///
    /// This is bounded by `max_active_key_tables`.
    pub table_stack: Vec<KeyTable>,

    /// The last handled binding. This is used to prevent encoding release
    /// events for handled bindings. We only need to keep track of one because
    /// at least at the time of writing this, its impossible for two keys of
    /// a combination to be handled by different bindings before the release
    /// of the prior (namely since you can't bind modifier-only).
    pub last_trigger: Option<u64>,
}

/// A configured link with its compiled regex (Zig: `DerivedConfig.Link`).
pub struct DerivedLink {
    pub regex: oni::Regex,
    pub action: input::link::Action,
    pub highlight: input::link::Highlight,
}

impl std::fmt::Debug for DerivedLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("action", &self.action)
            .field("highlight", &self.highlight)
            .finish_non_exhaustive()
    }
}

/// The configuration that a surface has, this is copied from the main
/// Config struct usually to prevent sharing a single value.
#[derive(Debug)]
pub struct DerivedConfig {
    /// For docs for these, see the associated config they are derived from.
    pub original_font_size: f32,
    pub keybind: Keybinds,
    pub abnormal_command_exit_runtime_ms: u32,
    pub clipboard_read: ClipboardAccess,
    pub clipboard_write: ClipboardAccess,
    pub clipboard_trim_trailing_spaces: bool,
    pub clipboard_paste_protection: bool,
    pub clipboard_paste_bracketed_safe: bool,
    pub clipboard_codepoint_map: RepeatableClipboardCodepointMap,
    pub copy_on_select: CopyOnSelect,
    pub right_click_action: RightClickAction,
    pub middle_click_action: MiddleClickAction,
    pub confirm_close_surface: ConfirmCloseSurface,
    pub cursor_click_to_move: bool,
    pub desktop_notifications: bool,
    pub font: font::shared_grid_set::DerivedConfig,
    pub mouse_interval: u64,
    pub mouse_hide_while_typing: bool,
    pub mouse_reporting: bool,
    pub mouse_scroll_multiplier: MouseScrollMultiplier,
    pub mouse_shift_capture: ConfigMouseShiftCapture,
    pub fullscreen: Fullscreen,
    pub macos_non_native_fullscreen: NonNativeFullscreen,
    pub macos_option_as_alt: Option<input::OptionAsAlt>,
    pub selection_clear_on_copy: bool,
    pub selection_clear_on_typing: bool,
    pub selection_word_chars: Vec<u32>,
    pub vt_kam_allowed: bool,
    pub wait_after_command: bool,
    pub window_padding_top: u32,
    pub window_padding_bottom: u32,
    pub window_padding_left: u32,
    pub window_padding_right: u32,
    pub window_padding_balance: WindowPaddingBalance,
    pub window_height: u32,
    pub window_width: u32,
    pub title: Option<CString>,
    pub title_report: bool,
    pub vt_window_resize_allowed: bool,
    pub links: Vec<DerivedLink>,
    pub link_osc8: bool,
    pub link_previews: LinkPreviews,
    pub scroll_to_bottom: ScrollToBottom,
    pub notify_on_command_finish: NotifyOnCommandFinish,
    pub notify_on_command_finish_action: NotifyOnCommandFinishAction,
    pub notify_on_command_finish_after: ConfigDuration,
    pub key_remaps: input::KeyRemapSet,
}

impl DerivedConfig {
    pub fn init(config: &Config) -> Result<DerivedConfig, oni::errors::Error> {
        // Build all of our links
        let links = {
            let mut links: Vec<DerivedLink> = Vec::new();
            for link in &config.link.links {
                let regex = link.oni_regex()?;
                links.push(DerivedLink {
                    regex,
                    action: link.action,
                    highlight: link.highlight,
                });
            }
            links
        };

        Ok(DerivedConfig {
            original_font_size: config.font_size,
            keybind: config.keybind.clone(),
            abnormal_command_exit_runtime_ms: config.abnormal_command_exit_runtime,
            clipboard_read: config.clipboard_read,
            clipboard_write: config.clipboard_write,
            clipboard_trim_trailing_spaces: config.clipboard_trim_trailing_spaces,
            clipboard_paste_protection: config.clipboard_paste_protection,
            clipboard_paste_bracketed_safe: config.clipboard_paste_bracketed_safe,
            clipboard_codepoint_map: config.clipboard_codepoint_map.clone(),
            copy_on_select: config.copy_on_select,
            right_click_action: config.right_click_action,
            middle_click_action: config.middle_click_action,
            confirm_close_surface: config.confirm_close_surface,
            cursor_click_to_move: config.cursor_click_to_move,
            desktop_notifications: config.desktop_notifications,
            font: font::shared_grid_set::DerivedConfig::init(config),
            mouse_interval: config.click_repeat_interval as u64 * 1_000_000, // 500ms
            mouse_hide_while_typing: config.mouse_hide_while_typing,
            mouse_reporting: config.mouse_reporting,
            mouse_scroll_multiplier: config.mouse_scroll_multiplier,
            mouse_shift_capture: config.mouse_shift_capture,
            fullscreen: config.fullscreen,
            macos_non_native_fullscreen: config.macos_non_native_fullscreen,
            macos_option_as_alt: config.macos_option_as_alt,
            selection_clear_on_copy: config.selection_clear_on_copy,
            selection_clear_on_typing: config.selection_clear_on_typing,
            selection_word_chars: config.selection_word_chars.codepoints.clone(),
            vt_kam_allowed: config.vt_kam_allowed,
            wait_after_command: config.wait_after_command,
            window_padding_top: config.window_padding_y.top_left,
            window_padding_bottom: config.window_padding_y.bottom_right,
            window_padding_left: config.window_padding_x.top_left,
            window_padding_right: config.window_padding_x.bottom_right,
            window_padding_balance: config.window_padding_balance,
            window_height: config.window_height,
            window_width: config.window_width,
            title: config.title.as_ref().map(|t| t.as_c_str().to_owned()),
            title_report: config.title_report,
            vt_window_resize_allowed: config.vt_window_resize_allowed,
            links,
            link_osc8: config.link_osc8,
            link_previews: config.link_previews,
            scroll_to_bottom: config.scroll_to_bottom,
            notify_on_command_finish: config.notify_on_command_finish,
            notify_on_command_finish_action: config.notify_on_command_finish_action,
            notify_on_command_finish_after: config.notify_on_command_finish_after,
            key_remaps: config.key_remap.clone(),
        })
    }

    pub fn deinit(&mut self) {}

    fn scaled_padding(&self, x_dpi: f32, y_dpi: f32) -> renderer::Padding {
        let padding_top: u32 = (self.window_padding_top as f32 * y_dpi / 72.0).floor() as u32;
        let padding_bottom: u32 = (self.window_padding_bottom as f32 * y_dpi / 72.0).floor() as u32;
        let padding_left: u32 = (self.window_padding_left as f32 * x_dpi / 72.0).floor() as u32;
        let padding_right: u32 = (self.window_padding_right as f32 * x_dpi / 72.0).floor() as u32;

        renderer::Padding {
            top: padding_top,
            bottom: padding_bottom,
            left: padding_left,
            right: padding_right,
        }
    }
}

/// See the module documentation.
pub struct Surface {
    /// Unique ID used to identify this surface for IPC purposes. It is
    /// exposed to the commands running in surfaces as the environment variable
    /// GHOSTTY_SURFACE_ID. It must not be zero as zero is used to incicate a null
    /// value when communicating an ID over DBus as DBus does not allow null/maybe
    /// values.
    pub id: u64,

    /// The app that this surface is attached to.
    pub app: *mut App,

    /// The windowing system surface and app.
    pub rt_app: *mut apprt::App,
    pub rt_surface: *mut apprt::Surface,

    /// The font structures
    pub font_grid_key: font::shared_grid_set::Key,
    pub font_size: DesiredSize,
    pub font_metrics: font::Metrics,

    /// This keeps track of if the font size was ever modified. If it wasn't,
    /// then config reloading will change the font. If it was manually adjusted,
    /// we don't change it on config reload since we assume the user wants
    /// a specific size.
    pub font_size_adjusted: bool,

    /// The renderer for this surface.
    pub renderer: Renderer,

    /// The render state
    pub renderer_state: renderer::State,

    /// The renderer thread manager
    pub renderer_thread: renderer::Thread,

    /// The actual thread
    pub renderer_thr: Option<JoinHandle<()>>,

    /// Mouse state.
    pub mouse: Mouse,

    /// Keyboard input state.
    pub keyboard: Keyboard,

    /// A currently pressed key. This is used so that we can send a keyboard
    /// release event when the surface is unfocused. Note that when the surface
    /// is refocused, a key press event may not be sent again -- this depends
    /// on the apprt (UI framework) in use, but we want to consistently send
    /// a release.
    ///
    /// This is only sent when a keypress event results in a key event being
    /// sent to the pty. If it is consumed by a keybinding or other action,
    /// this is not set.
    ///
    /// Also note the utf8 value is not valid for this event so some unfocused
    /// release events may not send exactly the right data within Kitty keyboard
    /// events. This seems unspecified in the spec so for now I'm okay with
    /// this. Plus, its only for release events where the key text is far
    /// less important.
    pub pressed_key: Option<KeyEvent<'static>>,

    /// The hash value of the last keybinding trigger that we performed. This
    /// is only set if the last key input matched a keybinding, consumed it,
    /// and performed it. This is used to prevent sending release/repeat events
    /// for handled bindings.
    pub last_binding_trigger: u64,

    /// The terminal IO handler.
    pub io: Box<Termio>,
    pub io_thread: termio::Thread,
    pub io_thr: Option<JoinHandle<()>>,

    /// Terminal inspector
    pub inspector: Option<Box<Inspector>>,

    /// All our sizing information.
    pub size: renderer::Size,

    /// The configuration derived from the main config. We "derive" it so that
    /// we don't have a shared pointer hanging around that we need to worry about
    /// the lifetime of. This makes updating config at runtime easier.
    pub config: DerivedConfig,

    /// The conditional state of the configuration. This can affect
    /// how certain configurations take effect such as light/dark mode.
    /// This is managed completely by Ghostty core but an apprt action
    /// is sent whenever this changes.
    pub config_conditional_state: ConditionalState,

    /// This is set to true if our IO thread notifies us our child exited.
    /// This is used to determine if we need to confirm, hold open, etc.
    pub child_exited: bool,

    /// We maintain our focus state and assume we're focused by default.
    /// If we're not initially focused then apprts can call focusCallback
    /// to let us know.
    pub focused: bool,

    /// Whether this surface may be visible. Unknown visibility is considered
    /// visible so reporting remains conservative.
    pub visible: bool,

    /// Used to determine whether to continuously scroll.
    pub selection_scroll_active: bool,

    /// True if the surface is in read-only mode. When read-only, no input
    /// is sent to the PTY but terminal-level operations like selections,
    /// (native) scrolling, and copy keybinds still work. Warn before quit is
    /// always enabled in this state.
    pub readonly: bool,

    /// Used to send notifications that long running commands have finished.
    /// Requires that shell integration be active. Should represent a nanosecond
    /// precision timestamp. It does not necessarily need to correspond to the
    /// actual time, but we must be able to compare two subsequent timestamps to get
    /// the wall clock time that has elapsed between timestamps.
    pub command_timer: Option<Instant>,

    /// Search state
    search: Option<Search>,

    /// Used to rate limit BEL handling.
    pub last_bell_time: Option<Instant>,
}

/// A raw pointer that may be sent to a thread that the sender joins.
struct SendPtr<T>(*mut T);
// SAFETY: the pointee outlives the thread (joined before it is freed).
unsafe impl<T> Send for SendPtr<T> {}
impl<T> SendPtr<T> {
    fn get(&self) -> *mut T {
        self.0
    }
}

/// Holds the renderer state mutex (Zig: `lockUncancelable` + `defer
/// unlock`). It holds a raw pointer so it doesn't borrow the surface.
struct Locked(*const Mutex);

impl Drop for Locked {
    fn drop(&mut self) {
        // SAFETY: the mutex outlives the surface methods holding it.
        unsafe { (*self.0).unlock(TinyIo) };
    }
}

/// Clears the crash thread state (Zig: `defer crash.sentry.thread_state = null`).
struct CrashGuard;

impl Drop for CrashGuard {
    fn drop(&mut self) {
        sentry::set_thread_state(None);
    }
}

/// The amount to scroll. This structure is always normalized so that
/// negative is down, left and positive is up, right. Note that INTERNALLY,
/// vertical scroll on our terminal uses positive for down (right is not
/// supported by our screen since scrollback is only vertical).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ScrollAmount {
    delta: isize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollDirection {
    DownLeft,
    UpRight,
}

impl ScrollAmount {
    fn direction(self) -> ScrollDirection {
        if self.delta < 0 {
            ScrollDirection::DownLeft
        } else {
            ScrollDirection::UpRight
        }
    }

    fn magnitude(self) -> usize {
        self.delta.unsigned_abs()
    }
}

/// `InitialSizeError`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitialSizeError {
    ContentScaleUnavailable,
    AppActionFailed,
}

impl std::fmt::Display for InitialSizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for InitialSizeError {}

/// Represents text read from the terminal and some metadata about it
/// that is often useful to apprts.
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    /// The text that was read from the terminal.
    pub text: String,

    /// The viewport information about this text, if it is visible in
    /// the viewport.
    pub viewport: Option<TextViewport>,
}

/// `Text.Viewport`
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextViewport {
    /// The top-left corner of the selection in pixels within the viewport.
    pub tl_px_x: f64,
    pub tl_px_y: f64,

    /// The linear offset of the start of the selection and the length.
    /// This is "linear" in the sense that it is the offset in the
    /// flattened viewport as a single array of text.
    ///
    /// Note: these values are currently wrong if there is a partially
    /// visible selection in the viewport (i.e. the top-left or
    /// bottom-right of the selection is outside the viewport). But the
    /// apprt usecase we have right now doesn't require these to be
    /// correct so... let's fix this later. The wrong values will always
    /// be within the text bounds so we aren't risking an overflow.
    pub offset_start: u32,
    pub offset_len: u32,
}

impl Text {
    pub fn deinit(self) {}
}

/// A detected link (Zig: `Link`).
#[derive(Debug, Clone, Copy)]
struct Link {
    action: input::link::Action,
    selection: Selection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeySequenceQueued {
    Flush,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeySequenceMemory {
    Retain,
    Free,
}

/// The portion of the screen to write for writeScreenFile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteScreenLoc {
    Screen,    // Full screen
    History,   // History (scrollback)
    Selection, // Selected text
}

impl WriteScreenLoc {
    fn as_str(self) -> &'static str {
        match self {
            WriteScreenLoc::Screen => "screen",
            WriteScreenLoc::History => "history",
            WriteScreenLoc::Selection => "selection",
        }
    }
}

/// The payload for completing a clipboard request with
/// completeClipboardRequest.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompleteClipboard<'a> {
    /// The representations the apprt could serve for the request's MIME
    /// types. These are immediately copied as needed so they only need
    /// to live for the duration of the completion call. Requesters that
    /// only carry text (paste, OSC 52) use the first text-like
    /// representation.
    pub contents: &'a [terminal::clipboard::Content<'a>],

    /// The listing of MIME types available on the clipboard, only
    /// gathered when the request asked for it.
    pub available: &'a [&'a [u8]],

    /// True if any clipboard confirmation prompt was already answered
    /// by the user, skipping further prompts:
    ///
    ///   - For "regular" pasting this means that unsafe pastes are
    ///     allowed. Unsafe data is defined as data that contains
    ///     line terminators (newlines or carriage returns), though
    ///     this definition may change later to detect other scenarios.
    ///
    ///   - For OSC 52 and Kitty clipboard protocol reads and writes no
    ///     prompt is shown to the user when this is true.
    pub confirmed: bool,

    /// True if the user asked to remember their decision. This is only
    /// honored by request types that support session grants (Kitty
    /// clipboard protocol requests carrying a password).
    pub remember: bool,
}

/// Convert a write request into the termio write message.
fn write_req_message(req: termio::message::WriteReq) -> termio::Message {
    match req {
        MessageData::Small(v) => termio::Message::WriteSmall(v),
        MessageData::Stable(v) => termio::Message::WriteStable(v),
        MessageData::Alloc(v) => termio::Message::WriteAlloc(v),
    }
}

/// A CString of `bytes` up to the first NUL (Zig: `dupeZ` of data that
/// never contains NUL in practice).
fn cstring(bytes: &[u8]) -> CString {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    CString::new(&bytes[..end]).expect("no interior NUL")
}

impl Surface {
    /// Create a new surface. This must be called from the main thread. The
    /// surface is returned boxed because its address must be stable due to
    /// interfacing with various callbacks.
    pub fn init(
        config_original: &Config,
        app: *mut App,
        rt_app: *mut apprt::App,
        rt_surface: *mut apprt::Surface,
    ) -> Result<Box<Surface>, Error> {
        // SAFETY: the caller passes live app objects for the lifetime of
        // the surface.
        let app_ref = unsafe { &mut *app };
        let rt_surface_ref = unsafe { &*rt_surface };

        // Apply our conditional state. If we fail to apply the conditional state
        // then we log and attempt to move forward with the old config.
        let config_: Option<Config> = match config_original.change_conditional_state(app_ref.config_conditional_state) {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "failed to apply conditional state to config err={err}");
                None
            }
        };

        // We want a config pointer for everything so we get that either
        // based on our conditional state or the original config.
        let mut config_ = config_;
        let config: &Config = match &mut config_ {
            Some(c) => {
                // We want to preserve our original working directory. We
                // don't need to dupe memory here because termio will derive
                // it. We preserve this so directory inheritance works.
                c.working_directory = config_original.working_directory.clone();
                c
            }
            None => config_original,
        };
        Self::init_with_config(config, app, rt_app, rt_surface, app_ref, rt_surface_ref)
    }

    fn init_with_config(
        config: &Config,
        app: *mut App,
        rt_app: *mut apprt::App,
        rt_surface: *mut apprt::Surface,
        app_ref: &mut App,
        rt_surface_ref: &apprt::Surface,
    ) -> Result<Box<Surface>, Error> {
        // Get our configuration
        let derived_config = DerivedConfig::init(config)?;

        // Determine our DPI configurations so we can properly configure
        // font points to pixels and handle other high-DPI scaling factors.
        let content_scale = rt_surface_ref.get_content_scale()?;
        let x_dpi = content_scale.x * DEFAULT_DPI as f32;
        let y_dpi = content_scale.y * DEFAULT_DPI as f32;
        log::debug!(
            target: LOG,
            "xscale={} yscale={} xdpi={} ydpi={}",
            content_scale.x,
            content_scale.y,
            x_dpi,
            y_dpi
        );

        // The font size we desire along with the DPI determined for the surface
        let font_size = DesiredSize {
            points: config.font_size,
            xdpi: x_dpi.round() as u16,
            ydpi: y_dpi.round() as u16,
        };

        // Setup our font group. This will reuse an existing font group if
        // it was already loaded.
        let (font_grid_key, font_grid) = app_ref.font_grid_set.r#ref(&derived_config.font, font_size)?;

        // Everything below this point must release the grid on error
        // (Zig: errdefer). The guard owns the partially initialized parts.
        let mut guard = InitGuard {
            set: &app_ref.font_grid_set,
            font_grid_key: Some(font_grid_key.clone()),
            renderer: None,
            mutex: None,
            render_thread: None,
            io_thread: None,
        };

        // Build our size struct which has all the sizes we need.
        let size: renderer::Size = {
            let surface_size = rt_surface_ref.get_size()?;
            let mut size = renderer::Size {
                screen: renderer::ScreenSize {
                    width: surface_size.width,
                    height: surface_size.height,
                },
                cell: font_grid.cell_size(),
                padding: renderer::Padding::default(),
            };

            let explicit = derived_config.scaled_padding(x_dpi, y_dpi);
            if derived_config.window_padding_balance != WindowPaddingBalance::False {
                size.balance_padding(explicit, derived_config.window_padding_balance);
            } else {
                size.padding = explicit;
            }

            size
        };

        // Initialize the render device if it hasn't been initialized already
        let render_device: *mut renderer::Device = match &mut app_ref.device {
            Some(d) => &mut **d,
            None => {
                let device_options = rt_surface_ref.render_device_options()?;
                let device = renderer::Device::init(device_options)?;
                &mut **app_ref.device.insert(device)
            }
        };
        // The device is owned by the app; re-borrow the grid set.
        guard.set = unsafe { &(*app).font_grid_set };

        // The surface memory. Its address is handed to the renderer, the
        // threads and the termio before the fields are written.
        let mut boxed: Box<std::mem::MaybeUninit<Surface>> = Box::new_uninit();
        let self_ptr: *mut Surface = boxed.as_mut_ptr();

        // Create our terminal grid with the initial size
        let app_mailbox = app::Mailbox {
            rt_app,
            mailbox: unsafe { &(*app).mailbox },
        };
        let surface_mailbox = Mailbox {
            surface: self_ptr,
            app: app_mailbox,
        };
        let renderer_impl = {
            // Renderer.init takes ownership of the config only on success.
            let renderer_config = renderer::generic::DerivedConfig::init(config)?;

            Renderer::init(renderer::Options {
                device: render_device,
                config: renderer_config,
                font_grid: std::sync::Arc::as_ptr(&font_grid) as *mut font::SharedGrid,
                size,
                surface_mailbox,
                rt_surface,
                // SAFETY: only the field address is taken.
                thread: unsafe { addr_of_mut!((*self_ptr).renderer_thread) },
            }).map_err(Error::Other)?
        };
        let renderer_impl = guard.renderer.insert(renderer_impl) as *mut Renderer;

        // The mutex used to protect our renderer state.
        let mutex: *mut Mutex = Box::into_raw(Box::new(Mutex::new()));
        guard.mutex = Some(mutex);

        // Create the renderer thread
        let render_thread = renderer::Thread::init(
            config,
            rt_surface,
            // SAFETY: only the field addresses are taken.
            unsafe { addr_of_mut!((*self_ptr).renderer) },
            unsafe { addr_of_mut!((*self_ptr).renderer_state) },
        )?;
        let render_thread = guard.render_thread.insert(render_thread);
        let render_wakeup = render_thread.wakeup;
        let render_mailbox: *const renderer::thread::Mailbox = &*render_thread.mailbox;

        // Create the IO thread
        let io_thread = termio::Thread::init()?;
        guard.io_thread = Some(io_thread);

        let id: u64 = loop {
            let mut buf = [0u8; 8];
            getrandom::fill(&mut buf).map_err(|_| Error::msg("entropy unavailable"))?;
            let candidate = u64::from_ne_bytes(buf);
            if candidate == 0 {
                continue;
            }
            break candidate;
        };

        // The command we're going to execute
        let command: Option<crate::config::Command> = 'command: {
            if app_ref.first {
                if let Some(command) = &config.initial_command {
                    break 'command Some(command.clone());
                }
            }
            config.command.clone()
        };

        // Start our IO implementation
        let io = {
            // Exec.init takes ownership of the env even if it fails.
            let env = {
                let mut env = match rt_surface_ref.default_termio_env() {
                    Ok(env) => env,
                    Err(err) => {
                        // If an error occurs, we don't want to block surface startup.
                        log::warn!(target: LOG, "error getting env map for surface err={err}");
                        global::environ_map()
                    }
                };

                // don't leak GHOSTTY_LOG to any subprocesses
                env.remove("GHOSTTY_LOG");

                env.insert("GHOSTTY_SURFACE_ID".into(), format!("0x{id:0>16x}"));

                env
            };

            // Initialize our IO backend
            let io_exec = termio::Exec::init(termio::exec::Config {
                command: command.clone(),
                env,
                env_override: config.env.clone(),
                shell_integration: config.shell_integration,
                shell_integration_features: config.shell_integration_features,
                cursor_blink: config.cursor_style_blink,
                working_directory: config
                    .working_directory
                    .as_ref()
                    .and_then(|wd| wd.value())
                    .map(String::from),
                resources_dir: global::resources_dir().host().map(String::from),
                term: config.term.clone(),
                rt_pre_exec_info: crate::command::RtPreExecInfo::init(config),
                rt_post_fork_info: crate::command::RtPostForkInfo::init(config),
            })?;

            // Initialize our IO mailbox
            let io_mailbox = termio::Mailbox::init_spsc()?;

            // Termio.init takes ownership of the config only on success.
            let io_config = termio::DerivedConfig::init(config);

            Termio::init(termio::Options {
                size,
                full_config: config,
                config: io_config,
                backend: termio::Backend::Exec(io_exec),
                mailbox: io_mailbox,
                // SAFETY: only the field address is taken.
                renderer_state: unsafe { addr_of_mut!((*self_ptr).renderer_state) },
                renderer_wakeup: render_wakeup,
                renderer_mailbox: render_mailbox,
                surface_mailbox,
            })?
        };
        let mut io = io;

        // Everything is initialized: move it into the surface.
        let renderer_state = renderer::State::new(
            // SAFETY: allocated above.
            unsafe { NonNull::new_unchecked(mutex) },
            NonNull::from(&mut io.terminal),
        );
        let _ = renderer_impl;
        let renderer_impl = guard.renderer.take().unwrap();
        let render_thread = guard.render_thread.take().unwrap();
        let io_thread = guard.io_thread.take().unwrap();
        guard.mutex = None;
        guard.font_grid_key = None;
        drop(guard);

        boxed.write(Surface {
            id,
            app,
            rt_app,
            rt_surface,
            font_grid_key,
            font_size,
            font_size_adjusted: false,
            font_metrics: font_grid.metrics,
            renderer: renderer_impl,
            renderer_thread: render_thread,
            renderer_state,
            renderer_thr: None,
            mouse: Mouse::default(),
            keyboard: Keyboard::default(),
            pressed_key: None,
            last_binding_trigger: 0,
            io,
            io_thread,
            io_thr: None,
            inspector: None,
            size,
            config: derived_config,

            // Our conditional state is initialized to the app state. This
            // lets us get the most likely correct color theme and so on.
            config_conditional_state: app_ref.config_conditional_state,
            child_exited: false,
            focused: true,
            visible: true,
            selection_scroll_active: false,
            readonly: false,
            command_timer: None,
            search: None,
            last_bell_time: None,
        });
        // SAFETY: fully written above.
        let mut this: Box<Surface> = unsafe { boxed.assume_init() };

        // Outside the block, IO has now taken ownership of our temporary state
        // so on error we deinit the whole surface (Zig: errdefers).
        if let Err(err) = this.init_finish(config, command) {
            this.deinit();
            return Err(err);
        }

        // We are no longer the first surface
        app_ref.first = false;

        Ok(this)
    }

    /// The part of `init` after the surface fields are written.
    fn init_finish(&mut self, config: &Config, command: Option<crate::config::Command>) -> Result<(), Error> {
        let size = self.size;

        // Report initial cell size on surface creation
        self.perform(RtAction::CellSize(rt_action::CellSize {
            width: size.cell.width,
            height: size.cell.height,
        }))?;

        self.perform(RtAction::SizeLimit(rt_action::SizeLimit {
            min_width: size.cell.width * MIN_WINDOW_WIDTH_CELLS,
            min_height: size.cell.height * MIN_WINDOW_HEIGHT_CELLS,
            // No max:
            max_width: 0,
            max_height: 0,
        }))?;

        // Call our size callback which handles all our retina setup
        // Note: this shouldn't be necessary and when we clean up the surface
        // init stuff we should get rid of this. But this is required because
        // sizeCallback does retina-aware stuff we don't do here and don't want
        // to duplicate.
        self.resize(self.size.screen)?;

        // Start our renderer thread
        let thread = SendPtr(&mut self.renderer_thread as *mut renderer::Thread);
        self.renderer_thr = Some(
            std::thread::Builder::new()
                .name("renderer".into())
                .spawn(move || {
                    let thread = thread.get();
                    // SAFETY: the thread is joined in deinit before the
                    // surface is freed.
                    unsafe { (*thread).thread_main() };
                })?,
        );

        // Start our IO thread
        let thread = SendPtr(&mut self.io_thread as *mut termio::Thread);
        let io = SendPtr(&mut *self.io as *mut Termio);
        self.io_thr = Some(
            std::thread::Builder::new()
                .name("io".into())
                .spawn(move || {
                    let thread = thread.get();
                    let io = io.get();
                    // SAFETY: the thread is joined in deinit before the
                    // surface is freed.
                    unsafe { (*thread).thread_main(io) };
                })?,
        );

        // Determine our initial window size if configured. We need to do this
        // quite late in the process because our height/width are in grid dimensions,
        // so we need to know our cell sizes first.
        //
        // Note: it is important to do this after the renderer is setup above.
        // This allows the apprt to fully initialize the surface before we
        // start messing with the window.
        if let Err(err) = self.recompute_initial_size() {
            // We don't treat this as a fatal error because not setting
            // an initial size shouldn't stop our terminal from working.
            log::warn!(target: LOG, "unable to set initial window size: {err}");
        }

        if let Some(title) = &config.title {
            self.perform(RtAction::SetTitle(rt_action::SetTitle { title: title.as_c_str() }))?;
        } else if cfg!(target_os = "linux") && config._xdg_terminal_exec {
            // For xdg-terminal-exec execution we special-case and set the window
            // title to the command being executed. This allows window managers
            // to set custom styling based on the command being executed.
            if let Some(v) = &command {
                let title = v.string();
                self.perform(RtAction::SetTitle(rt_action::SetTitle { title: title.as_c_str() }))?;
            }
        } else if let Some(cmd) = &command {
            match cmd {
                // If a user specifies a command it is appropriate to set the title as argv[0]
                // we know in the case of a direct command it has been supplied by the user
                crate::config::Command::Direct(cmd_str) => {
                    if !cmd_str.is_empty() {
                        self.perform(RtAction::SetTitle(rt_action::SetTitle {
                            title: cmd_str[0].as_c_str(),
                        }))?;
                    }
                }

                // We won't set the title in the case the shell expands the command
                // as that should typically be used to launch a shell which should
                // set its own titles
                crate::config::Command::Shell(_) => {}
            }
        }

        Ok(())
    }

    pub fn deinit(&mut self) {
        // Stop search thread
        if let Some(s) = self.search.take() {
            s.deinit();
        }

        // Stop rendering thread
        if let Some(thr) = self.renderer_thr.take() {
            if let Err(err) = self.renderer_thread.stop.notify() {
                log::error!(target: LOG, "error notifying renderer thread to stop, may stall err={err}");
            }
            let _ = thr.join();
        }

        // Stop our IO thread
        if let Some(thr) = self.io_thr.take() {
            if let Err(err) = self.io_thread.stop.notify() {
                log::error!(target: LOG, "error notifying io thread to stop, may stall err={err}");
            }
            let _ = thr.join();
        }

        // We need to deinit AFTER everything is stopped, since there are
        // shared values between the two threads.
        //
        // The renderer must be deinitialized before the renderer thread:
        // on macOS the renderer's display link callback points at the
        // thread's `draw_now` handle, and the renderer's deinit is what
        // stops the display link for good.
        self.renderer.deinit();
        self.renderer_thread.deinit();
        self.io_thread.deinit();
        self.mouse.selection_gesture.deinit(&mut self.io.terminal);
        self.io.deinit();

        if let Some(v) = self.inspector.take() {
            drop(v);
        }

        // Clean up our keyboard state
        for req in self.keyboard.sequence_queued.drain(..) {
            req.deinit();
        }
        self.keyboard.table_stack = Vec::new();

        // Clean up our font grid
        // SAFETY: the app outlives its surfaces.
        unsafe { (*self.app).font_grid_set.deref(&self.font_grid_key) };

        // Clean up our render state
        self.renderer_state.preedit = None;
        // SAFETY: allocated in init; nothing references it anymore.
        drop(unsafe { Box::from_raw(self.renderer_state.mutex.as_ptr()) });
        self.renderer_state.mutex = NonNull::dangling();
        self.config.deinit();

        log::info!(target: LOG, "surface closed id={:x}", self.id);
    }

    // ---------------------------------------------------------------
    // Small helpers (Zig accesses these through the self pointer).

    pub(crate) fn rt_app(&self) -> &apprt::App {
        // SAFETY: the runtime app outlives the surface.
        unsafe { &*self.rt_app }
    }

    pub(crate) fn rt_surface(&self) -> &apprt::Surface {
        // SAFETY: the runtime surface outlives the core surface.
        unsafe { &*self.rt_surface }
    }

    fn app(&self) -> &mut App {
        // SAFETY: the app outlives the surface and is only used on the
        // main thread.
        unsafe { &mut *self.app }
    }

    /// This surface as an action target.
    pub(crate) fn target(&self) -> Target {
        Target::Surface(self as *const Surface as *mut Surface)
    }

    /// `rt_app.performAction(.{ .surface = self }, ...)`
    fn perform(&self, action: RtAction<'_>) -> Result<bool, apprt::Error> {
        self.rt_app().perform_action(self.target(), action)
    }

    /// Lock the renderer state mutex until the guard drops.
    fn lock(&self) -> Locked {
        let m = self.renderer_state.mutex.as_ptr() as *const Mutex;
        // SAFETY: the mutex lives as long as the surface.
        unsafe { (*m).lock_uncancelable(TinyIo) };
        Locked(m)
    }

    fn crash_guard(&self) -> CrashGuard {
        sentry::set_thread_state(Some(self.crash_thread_state()));
        CrashGuard
    }

    fn renderer_mailbox_push(&self, msg: renderer::Message) {
        self.renderer_thread.mailbox.push(msg, QueueTimeout::Forever);
    }

    /// Close this surface. This will trigger the runtime to start the
    /// close process, which should ultimately deinitialize this surface.
    pub fn close(&mut self) {
        let confirm = self.needs_confirm_quit();
        self.rt_surface().close(confirm);
    }

    /// Returns a mailbox that can be used to send messages to this surface.
    fn surface_mailbox(&self) -> Mailbox {
        Mailbox {
            surface: self as *const Surface as *mut Surface,
            app: app::Mailbox {
                rt_app: self.rt_app,
                // SAFETY: the app outlives the surface.
                mailbox: unsafe { &(*self.app).mailbox },
            },
        }
    }

    /// Queue a message for the IO thread, taking ownership of `msg`.
    ///
    /// We centralize all our logic into this spot so we can intercept
    /// messages for example in readonly mode.
    fn queue_io(&mut self, msg: termio::Message, mutex: MutexState) {
        let Some(msg) = readonly_filter(self.readonly, msg) else {
            return;
        };

        self.io.queue_message(msg, mutex);
    }

    /// Forces the surface to render. This is useful for when the surface
    /// is in the middle of animation (such as a resize, etc.) or when
    /// the render timer is managed manually by the apprt.
    pub fn draw(&mut self) -> Result<(), Error> {
        // Renderers are required to support `drawFrame` being called from
        // the main thread, so that they can update contents during resize.
        self.renderer.draw_frame(true).map_err(Error::Other)?;
        Ok(())
    }

    /// Activate the inspector. This will begin collecting inspection data.
    /// This will not affect the GUI. The GUI must use performAction to
    /// show/hide the inspector UI.
    pub fn activate_inspector(&mut self) -> Result<(), Error> {
        if self.inspector.is_some() {
            return Ok(());
        }

        // Setup the inspector
        let mut ptr = Box::new(Inspector::init());
        let raw = NonNull::from(&mut *ptr);
        self.inspector = Some(ptr);

        // Put the inspector onto the render state
        {
            let _l = self.lock();
            debug_assert!(self.renderer_state.inspector.is_none());
            self.renderer_state.inspector = Some(raw);
        }

        // Notify our components we have an inspector active
        self.renderer_mailbox_push(renderer::Message::Inspector(true));
        self.queue_io(termio::Message::Inspector(true), MutexState::Unlocked);
        Ok(())
    }

    /// Report to the renderer how well the apprt can present the frames
    /// the renderer exports. See `renderer.Health` and the renderer's
    /// `presentation_health` state for the semantics.
    pub fn report_presentation_health(&mut self, health: renderer::Health) {
        self.renderer_mailbox_push(renderer::Message::PresentationHealth(health));
        if let Err(err) = self.renderer_thread.wakeup.notify() {
            log::warn!(target: LOG, "failed to wake up renderer err={err}");
        }
    }

    /// Deactivate the inspector and stop collecting any information.
    pub fn deactivate_inspector(&mut self) {
        if self.inspector.is_none() {
            return;
        }

        // Remove the inspector from the render state
        {
            let _l = self.lock();
            debug_assert!(self.renderer_state.inspector.is_some());
            self.renderer_state.inspector = None;
        }

        // Notify our components we have deactivated inspector
        self.renderer_mailbox_push(renderer::Message::Inspector(false));
        self.queue_io(termio::Message::Inspector(false), MutexState::Unlocked);

        // Deinit the inspector
        self.inspector = None;
    }

    /// True if the surface requires confirmation to quit. This should be called
    /// by apprt to determine if the surface should confirm before quitting.
    pub fn needs_confirm_quit(&self) -> bool {
        // If the surface is in read-only mode, always require confirmation
        if self.readonly {
            return true;
        }

        // If the child has exited, then our process is certainly not alive.
        // We check this first to avoid the locking overhead below.
        if self.child_exited {
            return false;
        }

        // Check the configuration for confirming close behavior.
        match self.config.confirm_close_surface {
            ConfirmCloseSurface::Always => true,
            ConfirmCloseSurface::False => false,
            ConfirmCloseSurface::True => {
                let _l = self.lock();
                !self.io.terminal.cursor_is_at_prompt()
            }
        }
    }

    /// Called from the app thread to handle mailbox messages to our specific
    /// surface.
    pub fn handle_message(&mut self, msg: Message) -> Result<(), Error> {
        match msg {
            // SAFETY: the sender keeps the config alive while it is handled.
            Message::ChangeConfig(config) => self.update_config(unsafe { &*config })?,

            Message::SetTitle(v) => {
                // We ignore the message in case the title was set via config.
                if self.config.title.is_some() {
                    log::debug!(target: LOG, "ignoring title change request since static title is set via config");
                    return Ok(());
                }

                // We know that our title should end in 0.
                let slice = cstring(&v);
                log::debug!(target: LOG, "changing title \"{}\"", slice.to_string_lossy());
                self.perform(RtAction::SetTitle(rt_action::SetTitle { title: &slice }))?;
            }

            Message::ReportTitle(style) => 'report_title: {
                if !self.config.title_report {
                    log::info!(target: LOG, "report_title requested, but disabled via config");
                    break 'report_title;
                }

                let title: Vec<u8> = self
                    .rt_surface()
                    .get_title()
                    .map(|t| t.to_bytes().to_vec())
                    .unwrap_or_default();
                let data = match style {
                    apprt::surface::ReportTitleStyle::Csi21T => {
                        let mut d = b"\x1b]l".to_vec();
                        d.extend_from_slice(&title);
                        d.extend_from_slice(b"\x1b\\");
                        d
                    }
                };

                // We always use an allocating message because we don't know
                // the length of the title and this isn't a performance critical
                // path.
                self.queue_io(termio::Message::WriteAlloc(data.into_boxed_slice()), MutexState::Unlocked);
            }

            Message::ResizeWindow(size) => self.resize_window(size)?,

            Message::ColorChange(change) => 'color_change: {
                use crate::terminal::color::Dynamic as DynamicTarget;
                use crate::terminal::osc::color::Target as ColoredTarget;
                // Notify our apprt, but don't send a mode 2031 DSR report
                // because VT sequences were used to change the color.
                let kind = match change.target {
                    ColoredTarget::Palette(v) => rt_action::ColorKind(v as std::ffi::c_int),
                    ColoredTarget::Dynamic(dyn_) => match dyn_ {
                        DynamicTarget::Foreground => rt_action::ColorKind::FOREGROUND,
                        DynamicTarget::Background => rt_action::ColorKind::BACKGROUND,
                        DynamicTarget::Cursor => rt_action::ColorKind::CURSOR,
                        // Unsupported dynamic color change notification type
                        _ => break 'color_change,
                    },
                    // Special colors aren't supported for change notification
                    ColoredTarget::Special(_) => break 'color_change,
                };
                self.perform(RtAction::ColorChange(rt_action::ColorChange {
                    kind,
                    r: change.color.r,
                    g: change.color.g,
                    b: change.color.b,
                }))?;
            }

            Message::SetMouseShape(shape) => {
                log::debug!(target: LOG, "changing mouse shape: {shape:?}");
                self.perform(RtAction::MouseShape(shape))?;
            }

            Message::ClipboardRead(clipboard) => {
                if self.config.clipboard_read == ClipboardAccess::Deny {
                    log::info!(target: LOG, "application attempted to read clipboard, but 'clipboard-read' is set to deny");
                    return Ok(());
                }

                self.start_clipboard_request(Clipboard::Standard, &ClipboardRequest::Osc52Read(clipboard))?;
            }

            Message::KittyClipboardRead(req) => self.kitty_clipboard_read(req)?,

            Message::KittyClipboardWrite(req) => self.kitty_clipboard_write(req)?,

            Message::ClipboardWrite { clipboard_type, req } => {
                self.clipboard_write(req.slice(), clipboard_type)?;
            }

            Message::PwdChange(w) => {
                let str = cstring(w.slice());
                self.perform(RtAction::Pwd(rt_action::Pwd { pwd: &str }))?;
            }

            Message::Close => self.close(),

            Message::ChildExited(v) => self.child_exited_fn(v),

            Message::DesktopNotification(notification) => {
                if !self.config.desktop_notifications {
                    log::info!(target: LOG, "application attempted to display a desktop notification, but 'desktop-notifications' is disabled");
                    return Ok(());
                }

                let title = cstring(notification.title_slice());
                let body = cstring(notification.body_slice());
                self.show_desktop_notification(&title, &body)?;
            }

            Message::Redraw => self.redraw(),

            Message::RendererHealth(health) => self.update_renderer_health(health),

            Message::Scrollbar(scrollbar) => self.update_scrollbar(scrollbar),

            Message::PresentSurface => self.present_surface()?,

            Message::PasswordInput(v) => self.password_input(v)?,

            Message::RingBell => 'bell: {
                let now = Instant::now();
                if let Some(last) = self.last_bell_time {
                    if now.duration_since(last).as_millis() < 100 {
                        break 'bell;
                    }
                }
                self.last_bell_time = Some(now);
                if let Err(err) = self.perform(RtAction::RingBell) {
                    log::warn!(target: LOG, "apprt failed to ring bell={err}");
                }
            }

            Message::ProgressReport(v) => {
                if let Err(err) = self.perform(RtAction::ProgressReport(v)) {
                    log::warn!(target: LOG, "apprt failed to report progress err={err}");
                }
            }

            Message::SelectionScrollTick(active) => {
                self.selection_scroll_active = active;
                self.selection_scroll_tick()?;
            }

            Message::StartCommand => {
                self.command_timer = Some(Instant::now());
            }

            Message::StopCommand(v) => 'timer: {
                let end = Instant::now();
                let Some(start) = self.command_timer.take() else {
                    break 'timer;
                };
                let duration_raw = end.saturating_duration_since(start).as_nanos();

                let duration = ConfigDuration {
                    duration: duration_raw.min(u64::MAX as u128) as u64,
                };
                log::debug!(target: LOG, "command took {duration}");

                if let Err(err) = self.perform(RtAction::CommandFinished(rt_action::CommandFinished {
                    exit_code: v,
                    duration,
                })) {
                    log::warn!(target: LOG, "apprt failed to notify command finish={err}");
                }
            }

            Message::SearchTotal(v) => {
                self.perform(RtAction::SearchTotal(rt_action::SearchTotal { total: v }))?;
            }

            Message::SearchSelected(v) => {
                self.perform(RtAction::SearchSelected(rt_action::SearchSelected { selected: v }))?;
            }
        }

        Ok(())
    }

    fn selection_scroll_tick(&mut self) -> Result<(), Error> {
        // If we're no longer active then we don't do anything.
        if !self.selection_scroll_active {
            return Ok(());
        }

        // If our gesture doesn't want autoscrolling then disable it.
        let was_autoscrolling = self.mouse.selection_gesture.left_drag_autoscroll
            != terminal::selection_gesture::Autoscroll::None;
        if !was_autoscrolling {
            self.queue_io(termio::Message::SelectionScroll(false), MutexState::Unlocked);
            return Ok(());
        }

        let pos = self.rt_surface().get_cursor_pos()?;
        let pos_vp = self.pos_to_viewport(pos.x as f64, pos.y as f64);

        // We need our locked state for the remainder
        let _l = self.lock();

        let geometry = self.drag_geometry();
        let rectangle = surface_mouse::is_rectangle_select_state(self.mouse.mods);
        let selection = self.mouse.selection_gesture.autoscroll_tick(
            &mut self.io.terminal,
            AutoscrollTick {
                viewport: pos_vp,
                xpos: pos.x as f64,
                ypos: pos.y as f64,
                rectangle,
                word_boundary_codepoints: &self.config.selection_word_chars,
                geometry,
            },
        );

        // If we're no longer autoscrolling for whatever reason, disable it.
        if self.mouse.selection_gesture.left_drag_autoscroll == terminal::selection_gesture::Autoscroll::None {
            self.queue_io(termio::Message::SelectionScroll(false), MutexState::Locked);
        }

        // If our left click was invalidated, ignore the result. This isn't
        // strictly necessary but its a nice to have.
        if self.mouse.selection_gesture.left_click_count == 0 {
            return Ok(());
        }

        // We modified our viewport and selection so we need to queue
        // a render.
        self.set_selection(selection)?;
        self.queue_render()?;
        Ok(())
    }

    fn drag_geometry(&self) -> DragGeometry {
        DragGeometry {
            columns: self.size.grid().columns as u32,
            cell_width: self.size.cell.width,
            padding_left: self.size.padding.left,
            screen_height: self.size.screen.height,
        }
    }

    /// Zig: `childExited` (renamed because `child_exited` is a field).
    fn child_exited_fn(&mut self, info: apprt::surface::ChildExited) {
        // Mark our flag that we exited immediately
        self.child_exited = true;

        // If our runtime was below some threshold then we assume that this
        // was an abnormal exit and we show an error message.
        'runtime: {
            if info.runtime_ms > self.config.abnormal_command_exit_runtime_ms as u64 {
                break 'runtime;
            }

            // On macOS, our exit code detection doesn't work, possibly
            // because of our `login` wrapper. More investigation required.
            if !cfg!(target_os = "macos") {
                // If the exit code is 0 then it was a good exit.
                if info.exit_code == 0 {
                    break 'runtime;
                }
            }

            log::warn!(target: LOG, "abnormal process exit detected, showing error message");

            // Try and show a GUI message. If it returns true, don't do anything else.
            let shown = match self.perform(RtAction::ShowChildExited(info)) {
                Ok(v) => v,
                Err(err) => {
                    log::error!(target: LOG, "error trying to show native child exited GUI err={err}");
                    false
                }
            };
            if shown {
                return;
            }

            // If a native GUI notification was not shown, update our terminal to
            // note the abnormal exit.
            if let Err(err) = self.child_exited_abnormally(info) {
                log::error!(target: LOG, "error handling abnormal child exit err={err}");
                return;
            }

            return;
        }

        // We output a message so that the user knows what's going on and
        // doesn't think their terminal just froze. We show this unconditionally
        // on close even if `wait_after_command` is false and the surface closes
        // immediately because if a user does an `undo` to restore a closed
        // surface then they will see this message and know the process has
        // completed.
        'terminal: {
            // First try and show a native GUI message.
            let shown = match self.perform(RtAction::ShowChildExited(info)) {
                Ok(v) => v,
                Err(err) => {
                    log::error!(target: LOG, "error trying to show native child exited GUI err={err}");
                    false
                }
            };
            if shown {
                break 'terminal;
            }

            // If the native GUI can't be shown, display a text message in the
            // terminal.
            let _l = self.lock();
            let t: &mut Terminal = &mut self.io.terminal;
            t.carriage_return();
            if t.linefeed().is_err() {
                break 'terminal;
            }
            if t.print_string("Process exited. Press any key to close the terminal.").is_err() {
                break 'terminal;
            }
            t.modes.set(terminal::modes::Mode::CursorVisible, false);

            // We also want to ensure that normal keyboard encoding is on
            // so that we can close the terminal. We close the terminal on
            // any key press that encodes a character.
            t.modes.set(terminal::modes::Mode::DisableKeyboard, false);
            t.screens
                .active
                .kitty_keyboard
                .set(terminal::kitty::key::SetMode::Set, terminal::kitty::key::Flags::DISABLED);
        }

        // Waiting after command we stop here. The terminal is updated, our
        // state is updated, and now its up to the user to decide what to do.
        if self.config.wait_after_command {
            return;
        }

        // If we aren't waiting after the command, then we exit immediately
        // with no confirmation.
        self.close();
    }

    /// Called when the child process exited abnormally.
    fn child_exited_abnormally(&mut self, info: apprt::surface::ChildExited) -> Result<(), Error> {
        use crate::terminal::color::Name;
        use crate::terminal::sgr::Attribute;

        // Build up our command for the error message
        let command = match &self.io.backend {
            termio::Backend::Exec(exec) => exec.subprocess.args.join(" "),
        };
        let runtime_str = format!("{} ms", info.runtime_ms);

        let _l = self.lock();
        let t: &mut Terminal = &mut self.io.terminal;

        // No matter what move the cursor back to the column 0.
        t.carriage_return();

        // Reset styles
        t.set_attribute(Attribute::Unset)?;

        // If there is data in the viewport, we want to scroll down
        // a little bit and write a horizontal rule before writing
        // our message. This lets the use see the error message the
        // command may have output.
        let viewport_str = t.plain_string()?;
        if !viewport_str.is_empty() {
            t.linefeed()?;
            for _ in 0..t.cols {
                t.print(0x2501)?;
            }
            t.carriage_return();
            t.linefeed()?;
            t.linefeed()?;
        }

        // Output our error message
        t.set_attribute(Attribute::Fg8(Name::BRIGHT_RED))?;
        t.set_attribute(Attribute::Bold)?;
        t.print_string("Ghostty failed to launch the requested command:")?;
        t.set_attribute(Attribute::Unset)?;

        t.carriage_return();
        t.linefeed()?;
        t.linefeed()?;
        t.print_string(&command)?;
        t.set_attribute(Attribute::Unset)?;

        t.carriage_return();
        t.linefeed()?;
        t.linefeed()?;
        t.print_string("Runtime: ")?;
        t.set_attribute(Attribute::Fg8(Name::RED))?;
        t.print_string(&runtime_str)?;
        t.set_attribute(Attribute::Unset)?;

        // We don't print this on macOS because the exit code is always 0
        // due to the way we launch the process.
        if !cfg!(target_os = "macos") {
            let exit_code_str = format!("{}", info.exit_code);
            t.carriage_return();
            t.linefeed()?;
            t.print_string("Exit Code: ")?;
            t.set_attribute(Attribute::Fg8(Name::RED))?;
            t.print_string(&exit_code_str)?;
            t.set_attribute(Attribute::Unset)?;
        }

        t.carriage_return();
        t.linefeed()?;
        t.linefeed()?;
        t.print_string("Press any key to close the window.")?;

        // Hide the cursor
        t.modes.set(terminal::modes::Mode::CursorVisible, false);
        Ok(())
    }

    /// Called when the terminal detects there is a password input prompt.
    fn password_input(&mut self, v: bool) -> Result<(), Error> {
        {
            let _l = self.lock();

            // If our password input state is unchanged then we don't
            // waste time doing anything more.
            let old = self.io.terminal.flags.password_input;
            if old == v {
                return Ok(());
            }

            self.io.terminal.flags.password_input = v;
        }

        // Notify our apprt so it can do whatever it wants.
        if let Err(err) = self.perform(RtAction::SecureInput(if v {
            rt_action::SecureInput::On
        } else {
            rt_action::SecureInput::Off
        })) {
            // We ignore this error because we don't want to fail this
            // entire operation just because the apprt failed to set
            // the secure input state.
            log::warn!(target: LOG, "apprt failed to set secure input state err={err}");
        }

        self.queue_render()?;
        Ok(())
    }

    fn search_callback(event: terminal::search::thread::Event<'_>, ud: *mut c_void) {
        // IMPORTANT: This function is run on the SEARCH THREAD! It is NOT SAFE
        // to access anything other than values that never change on the surface.
        // The surface is guaranteed to be valid for the lifetime of the search
        // thread.
        let this = ud as *const Surface;
        // SAFETY: see above.
        if let Err(err) = unsafe { (*this).search_callback_(event) } {
            log::warn!(target: LOG, "error in search callback err={err}");
        }
    }

    fn search_callback_(&self, event: terminal::search::thread::Event<'_>) -> Result<(), Error> {
        use terminal::search::thread::Event;
        // NOTE: This runs on the search thread.

        match event {
            Event::ViewportMatches(matches_unowned) => {
                let matches = matches_unowned.to_vec();
                self.renderer_mailbox_push(renderer::Message::SearchViewportMatches(
                    renderer::message::SearchMatches { matches },
                ));
                self.renderer_thread.wakeup.notify()?;
            }

            Event::SelectedMatch(selected_) => {
                if let Some(sel) = selected_ {
                    // Copy the flattened match.
                    let r#match = sel.highlight.clone();
                    self.renderer_mailbox_push(renderer::Message::SearchSelectedMatch(Some(
                        renderer::message::SearchMatch { r#match },
                    )));

                    // Send the selected index to the surface mailbox
                    self.surface_mailbox()
                        .push(Message::SearchSelected(Some(sel.idx)), QueueTimeout::Forever);
                } else {
                    // Reset our selected match
                    self.renderer_mailbox_push(renderer::Message::SearchSelectedMatch(None));

                    // Reset the selected index
                    self.surface_mailbox()
                        .push(Message::SearchSelected(None), QueueTimeout::Forever);
                }

                self.renderer_thread.wakeup.notify()?;
            }

            Event::TotalMatches(total) => {
                self.surface_mailbox()
                    .push(Message::SearchTotal(Some(total)), QueueTimeout::Forever);
            }

            // When we quit, tell our renderer to reset any search state.
            Event::Quit => {
                self.renderer_mailbox_push(renderer::Message::SearchSelectedMatch(None));
                self.renderer_mailbox_push(renderer::Message::SearchViewportMatches(
                    renderer::message::SearchMatches::default(),
                ));
                self.renderer_thread.wakeup.notify()?;

                // Reset search totals in the surface
                self.surface_mailbox()
                    .push(Message::SearchTotal(None), QueueTimeout::Forever);
                self.surface_mailbox()
                    .push(Message::SearchSelected(None), QueueTimeout::Forever);
            }

            // Unhandled, so far.
            Event::Complete => {}
        }

        Ok(())
    }

    /// Call this when modifiers change. This is safe to call even if modifiers
    /// match the previous state.
    ///
    /// This is not publicly exported because modifier changes happen implicitly
    /// on mouse callbacks, key callbacks, etc.
    ///
    /// The renderer state mutex MUST NOT be held.
    fn mods_changed(&mut self, mods: Mods) {
        // The only place we keep track of mods currently is on the mouse.
        if !self.mouse.mods.equal(mods) {
            // The mouse mods only contain binding modifiers since we don't
            // want caps/num lock or sided modifiers to affect the mouse.
            self.mouse.mods = mods.binding();

            // We also need to update the renderer so it knows if it should
            // highlight links. Additionally, mark the screen as dirty so
            // that the highlight state of all links is properly updated.
            {
                let _l = self.lock();
                self.renderer_state.mouse.mods = self.mouse_mods_with_capture(self.mouse.mods);

                // We use the clear screen dirty flag to force a rebuild of all
                // rows because changing mouse mods can affect the highlight state
                // of a link. If there is no link this seems very wasteful but
                // its really only one frame so it's not so bad.
                self.io.terminal.flags.dirty.clear = true;
            }

            if let Err(err) = self.queue_render() {
                // Not a big deal if this fails.
                log::warn!(target: LOG, "failed to notify renderer of mods change err={err}");
            }
        }
    }

    /// Call this whenever the mouse moves or mods changed. The time
    /// at which this is called may matter for the correctness of other
    /// mouse events (see cursorPosCallback) but this is shared logic
    /// for multiple events.
    fn mouse_refresh_links(
        &mut self,
        pos: apprt::CursorPos,
        pos_vp: point::Coordinate,
        over_link: bool,
    ) -> Result<(), Error> {
        // If the position is outside our viewport, do nothing
        if pos.x < 0.0 || pos.y < 0.0 {
            return Ok(());
        }

        // Update the last point that we checked for links so we don't
        // recheck if the mouse moves some pixels to the same point.
        self.mouse.link_point = Some(pos_vp);

        // Get our link at the current position. This returns null if there
        // isn't a link OR if we shouldn't be showing links for some reason
        // (see further comments for cases).
        let (link_, preview): (Option<CString>, bool) = 'link: {
            // If we clicked and our mouse moved cells then we never
            // highlight links until the mouse is unclicked. This follows
            // standard macOS and Linux behavior where a click and drag cancels
            // mouse actions.
            let left_idx = MouseButton::Left as usize;
            if self.mouse.click_state[left_idx] == MouseButtonState::Press {
                'click: {
                    let Some(pin) = self.mouse.active_left_click_pin(&self.io.terminal.screens) else {
                        break 'click;
                    };
                    // SAFETY: validated pins are live.
                    let pin = unsafe { *pin.as_ptr() };
                    let Some(click_pt) = self
                        .io
                        .terminal
                        .screens
                        .active
                        .pages
                        .point_from_pin(point::Tag::Viewport, pin)
                    else {
                        break 'click;
                    };

                    if !click_pt.coord().eql(pos_vp) {
                        log::debug!(target: LOG, "mouse moved while left click held, ignoring link hover");
                        break 'link (None, false);
                    }
                }
            }

            let Some(link) = self.link_at_pos(pos)? else {
                break 'link (None, false);
            };
            match link.action {
                input::link::Action::Open => {
                    let str = self.io.terminal.screens.active.selection_string(
                        terminal::screen::SelectionString {
                            sel: link.selection,
                            trim: false,
                        },
                    );
                    break 'link (Some(cstring(str.as_bytes())), self.config.link_previews == LinkPreviews::True);
                }

                input::link::Action::OpenOsc8 => {
                    // Show the URL in the status bar
                    let pin = link.selection.start();
                    let Some(uri) = self.osc8_uri(pin) else {
                        log::warn!(target: LOG, "failed to get URI for OSC8 hyperlink");
                        break 'link (None, false);
                    };
                    break 'link (Some(cstring(&uri)), self.config.link_previews != LinkPreviews::False);
                }
            }
        };

        // If we found a link, setup our internal state and notify the
        // apprt so it can highlight it.
        if let Some(link) = link_ {
            self.renderer_state.mouse.point = Some(pos_vp);
            self.mouse.over_link = true;
            self.io.terminal.screens.active.dirty.hyperlink_hover = true;
            self.perform(RtAction::MouseShape(terminal::mouse::Shape::Pointer))?;

            if preview {
                self.perform(RtAction::MouseOverLink(rt_action::MouseOverLink { url: &link }))?;
            }

            self.queue_render()?;
            return Ok(());
        }

        // No link, if we're previously over a link then we need to clear
        // the over-link apprt state.
        if over_link {
            self.perform(RtAction::MouseShape(self.io.terminal.mouse_shape))?;
            self.perform(RtAction::MouseOverLink(rt_action::MouseOverLink { url: c"" }))?;
            self.queue_render()?;
            return Ok(());
        }

        Ok(())
    }

    /// Called when our renderer health state changes.
    fn update_renderer_health(&mut self, health: renderer::Health) {
        log::warn!(target: LOG, "renderer health status change status={health:?}");
        if let Err(err) = self.perform(RtAction::RendererHealth(health)) {
            log::warn!(target: LOG, "failed to notify app of renderer health change err={err}");
        }
    }

    /// Called when the scrollbar state changes.
    fn update_scrollbar(&mut self, scrollbar: terminal::page_list::Scrollbar) {
        if let Err(err) = self.perform(RtAction::Scrollbar(scrollbar)) {
            log::warn!(target: LOG, "failed to notify app of scrollbar change err={err}");
        }
    }

    /// Called when the render thread has pushed a new frame.
    /// Notifies the apprt to redraw this surface.
    fn redraw(&mut self) {
        if let Err(err) = self.perform(RtAction::Render) {
            log::warn!(target: LOG, "failed to notify app of frame present err={err}");
        }
    }

    /// This should be called anytime `config_conditional_state` changes
    /// so that the apprt can reload the configuration.
    fn notify_config_conditional_state(&mut self) {
        if let Err(err) = self.perform(RtAction::ReloadConfig(rt_action::ReloadConfig { soft: true })) {
            log::warn!(target: LOG, "failed to notify app of config state change err={err}");
        }
    }

    /// Update our configuration at runtime. This can be called by the apprt
    /// to set a surface-specific configuration that differs from the app
    /// or other surfaces.
    pub fn update_config(&mut self, original: &Config) -> Result<(), Error> {
        // Apply our conditional state. If we fail to apply the conditional state
        // then we log and attempt to move forward with the old config.
        let config_: Option<Config> = match original.change_conditional_state(self.config_conditional_state) {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "failed to apply conditional state to config err={err}");
                None
            }
        };

        // We want a config pointer for everything so we get that either
        // based on our conditional state or the original config.
        let config: &Config = config_.as_ref().unwrap_or(original);

        // Update our new derived config immediately
        let derived = match DerivedConfig::init(config) {
            Ok(v) => v,
            Err(err) => {
                // If the derivation fails then we just log and return. We don't
                // hard fail in this case because we don't want to error the surface
                // when config fails we just want to keep using the old config.
                log::error!(target: LOG, "error updating configuration err={err}");
                return Ok(());
            }
        };
        self.config.deinit();
        self.config = derived;

        // If our mouse is hidden but we disabled mouse hiding, then show it again.
        if !self.config.mouse_hide_while_typing && self.mouse.hidden {
            self.show_mouse();
        }

        // If we are in the middle of a key sequence, clear it.
        self.end_key_sequence(KeySequenceQueued::Drop, KeySequenceMemory::Free);

        // Deactivate all key tables since they may have changed. Importantly,
        // we store pointers into the config as part of our table stack so
        // we can't keep them active across config changes. But this behavior
        // also matches key sequences.
        let _ = self.deactivate_all_key_tables();

        // Before sending any other config changes, we give the renderer a new font
        // grid. We could check to see if there was an actual change to the font,
        // but this is easier and pretty rare so it's not a performance concern.
        //
        // (Calling setFontSize builds and sends a new font grid to the renderer.)
        self.set_font_size({
            // If we have manually adjusted the font size, keep it that way.
            if self.font_size_adjusted {
                log::info!(target: LOG, "font size manually adjusted, preserving previous size on config reload");
                self.font_size
            } else {
                // If we haven't, then we update to the configured font size.
                // This allows config changes to update the font size. We used to
                // never do this but it was a common source of confusion and people
                // assumed that Ghostty was broken! This logic makes more sense.
                let mut size = self.font_size;
                size.points = config.font_size.clamp(1.0, 255.0);
                size
            }
        })?;

        // We need to store our configs in a heap-allocated pointer so that
        // our messages aren't huge.
        let renderer_message = renderer::Message::init_change_config(config)?;
        let termio_config_ptr = Box::new(termio::DerivedConfig::init(config));

        self.renderer_mailbox_push(renderer_message);
        self.queue_io(termio::Message::ChangeConfig(termio_config_ptr), MutexState::Unlocked);

        // With mailbox messages sent, we have to wake them up so they process it.
        if let Err(err) = self.queue_render() {
            log::warn!(target: LOG, "failed to notify renderer of config change err={err}");
        }

        // If we have a title set then we update our window to have the
        // newly configured title.
        if let Some(title) = &config.title {
            self.perform(RtAction::SetTitle(rt_action::SetTitle { title: title.as_c_str() }))?;
        }

        // Notify the window
        self.perform(RtAction::ConfigChange(rt_action::ConfigChange { config }))?;
        Ok(())
    }

    /// Recalculate the initial size of the window based on the
    /// configuration and invoke the apprt `initial_size` action if
    /// necessary.
    fn recompute_initial_size(&mut self) -> Result<(), InitialSizeError> {
        // Both width and height must be set for this to work, as
        // documented on the config options.
        if self.config.window_height == 0 || self.config.window_width == 0 {
            return Ok(());
        }

        let scale = self
            .rt_surface()
            .get_content_scale()
            .map_err(|_| InitialSizeError::ContentScaleUnavailable)?;
        let height = self.config.window_height.max(MIN_WINDOW_HEIGHT_CELLS) * self.size.cell.height;
        let width = self.config.window_width.max(MIN_WINDOW_WIDTH_CELLS) * self.size.cell.width;
        let width_f32 = width as f32;
        let height_f32 = height as f32;

        // The final values are affected by content scale and we need to
        // account for the padding so we get the exact correct grid size.
        let final_width: u32 =
            (width_f32 / scale.x).ceil() as u32 + self.size.padding.left + self.size.padding.right;
        let final_height: u32 =
            (height_f32 / scale.y).ceil() as u32 + self.size.padding.top + self.size.padding.bottom;

        self.perform(RtAction::InitialSize(rt_action::InitialSize {
            width: final_width,
            height: final_height,
        }))
        .map_err(|_| InitialSizeError::AppActionFailed)?;
        Ok(())
    }

    /// Handle a request from the running program (CSI 8 t) to resize the
    /// window to the given grid size.
    fn resize_window(&mut self, size: terminal::stream::ResizeWindow) -> Result<(), Error> {
        if !self.config.vt_window_resize_allowed {
            log::info!(target: LOG, "resize_window requested, but disabled via config");
            return Ok(());
        }

        // Our cell size and padding are in pixels but the apprt expects
        // points. We use the configured padding rather than the current
        // padding since balanced padding depends on the size.
        let scale = self.rt_surface().get_content_scale()?;
        let padding = self
            .config
            .scaled_padding(scale.x * DEFAULT_DPI as f32, scale.y * DEFAULT_DPI as f32);
        let width = ((size.columns as u32).max(MIN_RESIZE_WIDTH_CELLS) * self.size.cell.width
            + padding.left
            + padding.right) as f32;
        let height = ((size.rows as u32).max(MIN_RESIZE_HEIGHT_CELLS) * self.size.cell.height
            + padding.top
            + padding.bottom) as f32;

        // A zero dimension is passed through so the apprt keeps it as is.
        self.perform(RtAction::ResizeWindow(rt_action::ResizeWindow {
            width: if size.columns > 0 { (width / scale.x).ceil() as u32 } else { 0 },
            height: if size.rows > 0 { (height / scale.y).ceil() as u32 } else { 0 },
        }))?;
        Ok(())
    }

    /// Grab the value of text at the given selection point. Note that the
    /// selection structure is used as a way to determine the area of the
    /// screen to read from, it doesn't have to match the user's current
    /// selection state.
    pub fn dump_text(&mut self, sel: Selection) -> Result<Text, Error> {
        let _l = self.lock();
        self.dump_text_locked(sel)
    }

    /// Same as `dump_text` but assumes the renderer state mutex is already
    /// held.
    pub fn dump_text_locked(&mut self, sel: Selection) -> Result<Text, Error> {
        let screen = self.io.terminal.screens.active;

        // Read out the text
        let text = screen.selection_string(terminal::screen::SelectionString { sel, trim: false });

        // Calculate our viewport info if we can.
        let vp: Option<TextViewport> = 'viewport: {
            // If our bottom right pin is before the viewport, then we can't
            // possibly have this text be within the viewport.
            let vp_tl_pin = screen.pages.get_top_left(point::Tag::Viewport);
            let br_pin = sel.bottom_right(&screen);
            if br_pin.before(vp_tl_pin) {
                break 'viewport None;
            }

            // If our top-left pin is after the viewport, then we can't possibly
            // have this text be within the viewport.
            let Some(vp_br_pin) = screen.pages.get_bottom_right(point::Tag::Viewport) else {
                // I don't think this is possible but I don't want to crash on
                // that assertion so let's just break out...
                log::warn!(target: LOG, "viewport bottom-right pin not found, bug?");
                break 'viewport None;
            };
            let tl_pin = sel.top_left(&screen);
            if vp_br_pin.before(tl_pin) {
                break 'viewport None;
            }

            // We established that our top-left somewhere before the viewport
            // bottom-right and that our bottom-right is somewhere after
            // the top-left. This means that at least some portion of our
            // selection is within the viewport.

            // Our top-left point. If it doesn't exist in the viewport it must
            // be before and we can return (0,0).
            let tl_pt: point::Point = screen
                .pages
                .point_from_pin(point::Tag::Viewport, tl_pin)
                .unwrap_or_else(|| {
                    debug_assert!(tl_pin.before(vp_tl_pin));
                    point::Point::Viewport(point::Coordinate::default())
                });

            // Our bottom-right point. If it doesn't exist in the viewport
            // it must be the bottom-right of the viewport.
            let br_pt = screen
                .pages
                .point_from_pin(point::Tag::Viewport, br_pin)
                .unwrap_or_else(|| {
                    debug_assert!(vp_br_pin.before(br_pin));
                    screen.pages.point_from_pin(point::Tag::Viewport, vp_br_pin).unwrap()
                });

            let tl_coord = tl_pt.coord();
            let br_coord = br_pt.coord();

            // Our sizes are all scaled so we need to send the unscaled values back.
            let content_scale = self
                .rt_surface()
                .get_content_scale()
                .unwrap_or(apprt::ContentScale { x: 1.0, y: 1.0 });
            let x: f64 = {
                // Simple x * cell width gives the left
                let mut x = (tl_coord.x as u32 * self.size.cell.width) as f64;

                // Add padding
                x += self.size.padding.left as f64;

                // Scale
                x /= content_scale.x as f64;

                x
            };
            let y: f64 = {
                // Simple y * cell height gives the top
                let mut y = (tl_coord.y * self.size.cell.height) as f64;

                // We want the text baseline
                y += self.size.cell.height as f64;
                y -= self.font_metrics.cell_baseline as f64;

                // Add padding
                y += self.size.padding.top as f64;

                // Scale
                y /= content_scale.y as f64;

                y
            };

            // Utilize viewport sizing to convert to offsets
            let cols = screen.pages.cols as u32;
            let start = tl_coord.y * cols + tl_coord.x as u32;
            let end = br_coord.y * cols + br_coord.x as u32;

            Some(TextViewport {
                tl_px_x: x,
                tl_px_y: y,
                offset_start: start,
                offset_len: end - start,
            })
        };

        Ok(Text { text, viewport: vp })
    }

    /// Returns true if the terminal has a selection.
    pub fn has_selection(&self) -> bool {
        let _l = self.lock();
        self.io.terminal.screens.active.selection.is_some()
    }

    /// Returns the selected text. This is allocated.
    pub fn selection_string(&self) -> Option<String> {
        let _l = self.lock();
        let sel = self.io.terminal.screens.active.selection?;
        Some(
            self.io
                .terminal
                .screens
                .active
                .selection_string(terminal::screen::SelectionString { sel, trim: false }),
        )
    }

    /// Returns the pwd of the terminal, if any. This is always copied because
    /// the pwd can change at any point from termio. If we are calling from the IO
    /// thread you should just check the terminal directly.
    pub fn pwd(&self) -> Option<Vec<u8>> {
        let _l = self.lock();
        let terminal_pwd = self.io.terminal.get_pwd()?;
        Some(terminal_pwd.to_vec())
    }

    /// Resolves a relative file path to an absolute path using the terminal's pwd.
    fn resolve_path_for_opening(&self, path: &str) -> Option<String> {
        if !std::path::Path::new(path).is_absolute() {
            let terminal_pwd = self.io.terminal.get_pwd()?;
            let terminal_pwd = std::str::from_utf8(terminal_pwd).ok()?;

            let resolved = path_resolve(terminal_pwd, path);

            if std::fs::metadata(&resolved).is_err() {
                return None;
            }

            return Some(resolved);
        }

        None
    }

    /// Returns the x/y coordinate of where the IME (Input Method Editor)
    /// keyboard should be rendered.
    pub fn ime_point(&self) -> apprt::IMEPos {
        let content_scale = self
            .rt_surface()
            .get_content_scale()
            .unwrap_or(apprt::ContentScale { x: 1.0, y: 1.0 });
        self.ime_point_scaled(content_scale)
    }

    /// Same as `imePoint`, but divides our sizes by the given scale instead
    /// of the content scale.
    pub fn ime_point_scaled(&self, scale: apprt::ContentScale) -> apprt::IMEPos {
        let (cursor_x, cursor_y, preedit_width) = {
            let _l = self.lock();
            let cursor = &self.io.terminal.screens.active.cursor;
            let preedit_width: usize = self.renderer_state.preedit.as_ref().map(|p| p.width()).unwrap_or(0);
            (cursor.x as u32, cursor.y as u32, preedit_width)
        };

        // TODO: need to handle when scrolling and the cursor is not
        // in the visible portion of the screen.

        // Our sizes are all scaled so we need to send the unscaled values back.

        let x: f64 = {
            // Simple x * cell width gives the top-left corner, then add padding offset
            let mut x = (cursor_x * self.size.cell.width + self.size.padding.left) as f64;

            // We want the midpoint
            x += self.size.cell.width as f64 / 2.0;

            // And scale it
            x /= scale.x as f64;

            x
        };

        let y: f64 = {
            // Simple y * cell height gives the top-left corner, then add padding offset
            let mut y = (cursor_y * self.size.cell.height + self.size.padding.top) as f64;

            // We want the bottom
            y += self.size.cell.height as f64;

            // And scale it
            y /= scale.y as f64;

            y
        };

        // Our height for now is always just the cell height because our preedit
        // rendering only renders in a single line.
        let height: f64 = self.size.cell.height as f64 / scale.y as f64;
        let width: f64 = {
            let mut width = (preedit_width as u32 * self.size.cell.width) as f64;

            // Our max width is the remaining screen width after the cursor.
            // We don't have to deal with wrapping because the preedit doesn't
            // wrap right now.
            let screen_width = self.size.terminal().width as f64;
            let x_offset = ((cursor_x + 1) * self.size.cell.width) as f64;
            let max = screen_width - x_offset;
            width = width.min(max);

            // Note: we don't apply content scale here because it looks like
            // for some reason in macOS its already scaled. I'm not sure why
            // that is so I'm going to just leave this comment here so its known
            // that I left this out on purpose pending more investigation.

            width
        };

        apprt::IMEPos { x, y, width, height }
    }

    fn clipboard_write(&self, data: &[u8], loc: Clipboard) -> Result<(), Error> {
        if self.config.clipboard_write == ClipboardAccess::Deny {
            log::info!(target: LOG, "application attempted to write clipboard, but 'clipboard-write' is set to deny");
            return Ok(());
        }

        // Decode with the SIMD decoder, strict per the Kitty clipboard
        // spec that also governs OSC 52 base64 handling: a request with
        // characters outside the base64 alphabet is discarded entirely
        // (never partially decoded), while a missing-padding tail is
        // tolerated since OSC 52 has no way to report errors.
        let mut buf = vec![0u8; simd::base64::max_len(data) + 1];
        let decoded_len = match simd::base64::decode_strict(data, &mut buf, simd::base64::Padding::Optional) {
            Ok(decoded) => decoded.len(),
            Err(_) => {
                log::info!(target: LOG, "application sent invalid base64 data for OSC 52");
                return Ok(());
            }
        };
        buf[decoded_len] = 0;

        // When clipboard-write is "ask" a prompt is displayed to the user asking
        // them to confirm the clipboard access. Each app runtime handles this
        // differently.
        let confirm = self.config.clipboard_write == ClipboardAccess::Ask;
        if let Err(err) = self.rt_surface().set_clipboard(
            loc,
            &[ClipboardContent {
                mime: b"text/plain",
                data: &buf[..decoded_len],
            }],
            confirm,
        ) {
            log::error!(target: LOG, "error setting clipboard string err={err}");
            return Ok(());
        }
        Ok(())
    }

    fn copy_selection_to_clipboards(
        &self,
        sel: Selection,
        clipboards: &[Clipboard],
        format: input::binding::CopyToClipboard,
    ) -> Result<(), Error> {
        use input::binding::CopyToClipboard;
        use terminal::formatter::{Format, Options as FormatterOptions, ScreenFormatter, ScreenFormatterContent};

        let t = &self.io.terminal;
        let screen = &*t.screens.active;

        // The options we'll use for all formatting. We'll just override the
        // emit format.
        let opts = FormatterOptions {
            emit: Format::Plain, // We'll override this below
            unwrap: true,
            trim: self.config.clipboard_trim_trailing_spaces,
            codepoint_map: Some(self.config.clipboard_codepoint_map.map.list.clone()),
            background: t.colors.background.get(),
            foreground: t.colors.foreground.get(),
            palette: Some(&t.colors.palette.current),
        };

        let format_with = |opts: FormatterOptions<'_>| -> Result<Vec<u8>, Error> {
            let mut out: Vec<u8> = Vec::new();
            let mut formatter = ScreenFormatter::init(screen, opts);
            formatter.content = ScreenFormatterContent::Selection(Some(sel));
            formatter.format(&mut out)?;
            Ok(out)
        };

        let mut contents: Vec<(&'static [u8], Vec<u8>)> = Vec::with_capacity(2);
        match format {
            CopyToClipboard::Plain => {
                contents.push((b"text/plain", format_with(opts)?));
            }

            CopyToClipboard::Vt => {
                // Note: We don't apply codepoint mappings to VT format since it contains
                // escape sequences that should be preserved as-is
                let mut copy = opts;
                copy.emit = Format::Vt;
                contents.push((b"text/plain", format_with(copy)?));
            }

            CopyToClipboard::Html => {
                // Note: We don't apply codepoint mappings to HTML format since HTML
                // has its own character encoding and entity system
                let mut copy = opts;
                copy.emit = Format::Html;
                contents.push((b"text/html", format_with(copy)?));
            }

            CopyToClipboard::Mixed => {
                // First, generate plain text with codepoint mappings applied
                contents.push((b"text/plain", format_with(opts.clone())?));

                // Second, generate HTML without codepoint mappings
                let mut copy = opts;
                copy.emit = Format::Html;

                // We purposely don't emit background/foreground for mixed
                // mode because the HTML contents is often used for rich text
                // input and with trimmed spaces it looks pretty bad.
                copy.background = None;
                copy.foreground = None;

                // Note: We don't apply codepoint mappings to HTML format
                contents.push((b"text/html", format_with(copy)?));
            }
        }

        debug_assert!(!contents.is_empty());
        let contents: Vec<ClipboardContent<'_>> = contents
            .iter()
            .map(|(mime, data)| ClipboardContent { mime, data })
            .collect();
        for &clipboard in clipboards {
            if let Err(err) = self.rt_surface().set_clipboard(clipboard, &contents, false) {
                log::error!(target: LOG, "error setting clipboard string clipboard={clipboard:?} err={err}");
            }
        }
        Ok(())
    }

    /// Set the active selection and notify the apprt on a genuine state
    /// transition. All selection mutations route through here rather than
    /// `screen.select` directly so the notification fires consistently. To
    /// also copy per `copy_on_select`, use `setSelectionAndCopy`.
    ///
    /// This must be called with the renderer mutex held.
    fn set_selection(&mut self, sel_: Option<Selection>) -> Result<(), Error> {
        // Compute the transition before `select` below, which untracks (frees)
        // the previous selection's tracked pins; reading them after would be a
        // use-after-free.
        let mut screen = self.io.terminal.screens.active;
        let prev_ = screen.selection;
        let changed = match (prev_, sel_) {
            (None, s) => s.is_some(),
            (Some(_), None) => true,
            (Some(prev), Some(sel)) => !sel.eql(prev),
        };

        screen.select(sel_)?;

        if changed {
            if let Err(err) = self.perform(RtAction::SelectionChanged) {
                log::warn!(target: LOG, "apprt failed selection_changed notification err={err}");
            }
        }
        Ok(())
    }

    /// Set a selection and, per `copy_on_select`, copy it to the clipboard.
    /// For committing selection gestures (mouse release, select-all binding).
    ///
    /// This must be called with the renderer mutex held.
    fn set_selection_and_copy(&mut self, sel: Selection) -> Result<(), Error> {
        self.set_selection(Some(sel))?;

        use input::binding::CopyToClipboard;
        match self.config.copy_on_select {
            CopyOnSelect::None => {}

            // The selection clipboard is set if supported, otherwise nothing is copied.
            CopyOnSelect::Primary => {
                self.copy_selection_to_clipboards(sel, &[Clipboard::Selection], CopyToClipboard::Mixed)?
            }

            // Only the standard clipboard is set.
            CopyOnSelect::Clipboard => {
                self.copy_selection_to_clipboards(sel, &[Clipboard::Standard], CopyToClipboard::Mixed)?
            }

            // Both standard and selection clipboards are set.
            CopyOnSelect::Both => self.copy_selection_to_clipboards(
                sel,
                &[Clipboard::Standard, Clipboard::Selection],
                CopyToClipboard::Mixed,
            )?,
        }
        Ok(())
    }

    /// Change the cell size for the terminal grid. This can happen as
    /// a result of changing the font size at runtime.
    fn set_cell_size(&mut self, size: renderer::CellSize) -> Result<(), Error> {
        // Update our cell size within our size struct
        self.size.cell = size;
        self.balance_padding_if_needed();

        // Notify the terminal
        self.queue_io(termio::Message::Resize(self.size), MutexState::Unlocked);

        // Update our terminal default size if necessary.
        if let Err(err) = self.recompute_initial_size() {
            // We don't treat this as a fatal error because not setting
            // an initial size shouldn't stop our terminal from working.
            log::warn!(target: LOG, "unable to recompute initial window size: {err}");
        }

        // Notify the window
        self.perform(RtAction::CellSize(rt_action::CellSize {
            width: size.width,
            height: size.height,
        }))?;
        Ok(())
    }

    /// Change the font size.
    ///
    /// This can only be called from the main thread.
    pub fn set_font_size(&mut self, size: DesiredSize) -> Result<(), Error> {
        log::debug!(target: LOG, "set font size size={}", size.points);

        // Update our font size so future changes work
        self.font_size = size;

        // We need to build up a new font stack for this font size.
        let (font_grid_key, font_grid) = self.app().font_grid_set.r#ref(&self.config.font, self.font_size)?;

        // Set our cell size
        if let Err(err) = self.set_cell_size(renderer::CellSize {
            width: font_grid.metrics.cell_width,
            height: font_grid.metrics.cell_height,
        }) {
            self.app().font_grid_set.deref(&font_grid_key);
            return Err(err);
        }

        // Notify our render thread of the new font stack. The renderer
        // MUST accept the new font grid and deref the old.
        let old_key = std::mem::replace(&mut self.font_grid_key, font_grid_key.clone());
        self.renderer_mailbox_push(renderer::Message::FontGrid {
            grid: std::sync::Arc::as_ptr(&font_grid) as *mut font::SharedGrid,
            set: &mut self.app().font_grid_set,
            old_key,
            new_key: font_grid_key,
        });

        // Once we've sent the key we can replace our key
        self.font_metrics = font_grid.metrics;

        // Schedule render which also drains our mailbox
        self.queue_render().expect("wakeup notify");
        Ok(())
    }

    /// This queues a render operation with the renderer thread. The render
    /// isn't guaranteed to happen immediately but it will happen as soon as
    /// practical.
    fn queue_render(&self) -> Result<(), Error> {
        self.renderer_thread.wakeup.notify()?;
        Ok(())
    }

    /// Called by the apprt when the surface's display is realized.
    /// Notifies the renderer so it can begin rendering.
    /// Safe to call from the main thread.
    pub fn display_realized(&mut self) -> Result<(), Error> {
        self.renderer.display_realized().map_err(Error::Other)?;
        Ok(())
    }

    /// Called by the apprt when the surface's display is unrealized (the surface
    /// is being destroyed or reparented). Safe to call from the main thread.
    pub fn display_unrealized(&mut self) {
        self.renderer.display_unrealized();

        // Wake the render thread so it notices `display_realized` is now false
        // and releases GPU resources (swap chain and shaders).
        if let Err(err) = self.renderer_thread.wakeup.notify() {
            log::warn!(target: LOG, "failed to notify renderer thread of unrealize err={err}");
        }
    }

    pub fn size_callback(&mut self, size: apprt::SurfaceSize) -> Result<(), Error> {
        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        let new_screen_size = renderer::ScreenSize {
            width: size.width,
            height: size.height,
        };

        // Update our screen size, but only if it actually changed. And if
        // the screen size didn't change, then our grid size could not have
        // changed, so we just return.
        if self.size.screen.equals(new_screen_size) {
            return Ok(());
        }

        self.resize(new_screen_size)
    }

    fn resize(&mut self, size: renderer::ScreenSize) -> Result<(), Error> {
        // Save our screen size
        self.size.screen = size;
        self.balance_padding_if_needed();

        // Recalculate our grid size. Because Ghostty supports fluid resizing,
        // its possible the grid doesn't change at all even if the screen size changes.
        // We have to update the IO thread no matter what because we send
        // pixel-level sizing to the subprocess.
        let grid_size = self.size.grid();
        if grid_size.columns < 5 && (self.size.padding.left > 0 || self.size.padding.right > 0) {
            log::warn!(target: LOG, "WARNING: very small terminal grid detected with padding set. Is your padding reasonable?");
        }
        if grid_size.rows < 2 && (self.size.padding.top > 0 || self.size.padding.bottom > 0) {
            log::warn!(target: LOG, "WARNING: very small terminal grid detected with padding set. Is your padding reasonable?");
        }

        // Mail the IO thread
        self.queue_io(termio::Message::Resize(self.size), MutexState::Unlocked);

        // Mail the render thread so it updates its padding and screen size.
        self.renderer_mailbox_push(renderer::Message::Resize(self.size));
        if let Err(err) = self.queue_render() {
            log::warn!(target: LOG, "failed to notify renderer of resize err={err}");
        }
        Ok(())
    }

    /// Recalculate the balanced padding if needed.
    fn balance_padding_if_needed(&mut self) {
        if self.config.window_padding_balance == WindowPaddingBalance::False {
            return;
        }
        // (Zig `try`s in a void function, i.e. this cannot fail.)
        let Ok(content_scale) = self.rt_surface().get_content_scale() else {
            return;
        };
        let x_dpi = content_scale.x * DEFAULT_DPI as f32;
        let y_dpi = content_scale.y * DEFAULT_DPI as f32;
        let padding = self.config.scaled_padding(x_dpi, y_dpi);
        self.size.balance_padding(padding, self.config.window_padding_balance);
    }

    /// Called to set the preedit state for character input. Preedit is used
    /// with dead key states, for example, when typing an accent character.
    /// This should be called with null to reset the preedit state.
    ///
    /// The core surface will NOT reset the preedit state on charCallback or
    /// keyCallback and we rely completely on the apprt implementation to track
    /// the preedit state correctly.
    ///
    /// The preedit input must be UTF-8 encoded.
    pub fn preedit_callback(&mut self, preedit_: Option<&[u8]>) -> Result<(), Error> {
        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        let _l = self.lock();

        // We clear our selection when ANY OF:
        // 1. We have an existing preedit
        // 2. We have preedit text
        if self.renderer_state.preedit.is_some() || preedit_.is_some() {
            if self.config.selection_clear_on_typing {
                let _ = self.set_selection(None);
            }
        }

        // We always clear our prior preedit
        self.renderer_state.preedit = None;

        // Mark preedit dirty flag
        self.io.terminal.flags.dirty.preedit = true;

        // If we have no text, we're done. We queue a render in case we cleared
        // a prior preedit (likely).
        let Some(text) = preedit_ else {
            self.queue_render()?;
            return Ok(());
        };

        // We convert the UTF-8 text to codepoints.
        let view = std::str::from_utf8(text)?;

        // Allocate the codepoints slice
        let mut codepoints: Vec<renderer::state::Codepoint> = Vec::new();
        for cp in view.chars() {
            let cp = cp as u32;
            let width = crate::unicode::TABLE.get(cp).width as usize;

            // I've never seen a preedit text with a zero-width character. In
            // theory its possible but we can't really handle it right now.
            // Let's just ignore it.
            if width == 0 {
                continue;
            }

            codepoints.push(renderer::state::Codepoint {
                codepoint: cp,
                wide: width >= 2,
            });
        }

        // If we have no codepoints, then we're done.
        if codepoints.is_empty() {
            self.queue_render()?;
            return Ok(());
        }

        self.renderer_state.preedit = Some(renderer::state::Preedit { codepoints });
        self.queue_render()?;
        Ok(())
    }

    /// Returns true if the given key event would trigger a keybinding
    /// if it were to be processed. This is useful for determining if
    /// a key event should be sent to the terminal or not.
    ///
    /// Note that this function does not check if the binding itself
    /// is performable, only if the key event would trigger a binding.
    /// If a performable binding is found and the event is not performable,
    /// then Ghosty will act as though the binding does not exist.
    pub fn key_event_is_binding(&self, event_orig: KeyEvent<'_>) -> Option<BindingFlags> {
        // Apply key remappings for consistency with keyCallback
        let mut event = event_orig;
        if self.config.key_remaps.is_remapped(event_orig.mods) {
            event.mods = self.config.key_remaps.apply(event_orig.mods);
        }

        match event.action {
            input::Action::Release => return None,
            input::Action::Press | input::Action::Repeat => {}
        }

        // Look up our entry
        let entry = 'entry: {
            // If we're in a sequence, check the sequence set
            if let Some(set) = self.keyboard.sequence_set {
                // SAFETY: sequence sets point into our derived config.
                break 'entry unsafe { &*set }.get_event(&event)?;
            }

            // Check active key tables (inner-most to outer-most)
            for table in self.keyboard.table_stack.iter().rev() {
                // SAFETY: table sets point into our derived config.
                if let Some(entry) = unsafe { &*table.set }.get_event(&event) {
                    break 'entry entry;
                }
            }

            // Check the root set
            self.config.keybind.set.get_event(&event)?
        };

        // Return flags based on the
        Some(match entry.value_ptr {
            BindingValue::Leader(_) => BindingFlags::default(),
            BindingValue::Leaf(v) => v.flags,
            BindingValue::LeafChained(v) => v.flags,
        })
    }

    /// Called for any key events. This handles keybindings, encoding and
    /// sending to the terminal, etc.
    pub fn key_callback(&mut self, event_orig: KeyEvent<'_>) -> Result<InputEffect, Error> {
        // log.warn("text keyCallback event={}", .{event_orig});

        // Apply key remappings to transform modifiers before any processing.
        // This allows users to remap modifier keys at the app level.
        let mut event = event_orig;
        if self.config.key_remaps.is_remapped(event_orig.mods) {
            event.mods = self.config.key_remaps.apply(event_orig.mods);
        }

        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        // Setup our inspector event if we have an inspector.
        let mut insp_ev: Option<InspectorKeyEvent> = if self.inspector.is_some() {
            Some(InspectorKeyEvent::init(&event))
        } else {
            None
        };

        let result = self.key_callback_(event, insp_ev.as_mut());

        // When we're done processing, we always want to add the event to
        // the inspector.
        if let Some(ev) = insp_ev {
            // We have to check for the inspector again because our keybinding
            // might close it. (A closed surface must not be touched.)
            if !matches!(result, Ok(InputEffect::Closed)) {
                if let Some(insp) = &mut self.inspector {
                    match insp.record_key_event(ev) {
                        Ok(()) => {
                            let _ = self.queue_render();
                        }
                        Err(err) => log::warn!(target: LOG, "error adding key event to inspector err={err}"),
                    }
                }
            }
        }

        result
    }

    fn key_callback_(
        &mut self,
        event: KeyEvent<'_>,
        mut insp_ev: Option<&mut InspectorKeyEvent>,
    ) -> Result<InputEffect, Error> {
        // Handle keybindings first. We need to handle this on all events
        // (press, repeat, release) because a press may perform a binding but
        // a release should not encode if we consumed the press.
        if let Some(v) = self.maybe_handle_binding(event, insp_ev.as_deref_mut())? {
            return Ok(v);
        }
        // If we allow KAM and KAM is enabled then we do nothing.
        if self.config.vt_kam_allowed {
            let _l = self.lock();
            if self.io.terminal.modes.get(terminal::modes::Mode::DisableKeyboard) {
                return Ok(InputEffect::Consumed);
            }
        }

        // If this input event has text, then we hide the mouse if configured.
        // We only do this on pressed events to avoid hiding the mouse when we
        // change focus due to a keybinding (i.e. switching tabs).
        if self.config.mouse_hide_while_typing
            && event.action == input::Action::Press
            && !self.mouse.hidden
            && !event.utf8.is_empty()
        {
            self.hide_mouse();
        }

        // If our mouse modifiers change we may need to change our
        // link highlight state.
        if !self.mouse.mods.equal(event.mods) {
            'mouse_mods: {
                // Update our modifiers, this will update mouse mods too
                self.mods_changed(event.mods);

                // We only refresh links if
                // 1. mouse reporting is off
                // OR
                // 2. mouse reporting is on and we are not reporting shift to the terminal
                if self.io.terminal.flags.mouse_event == terminal::mouse::Event::None
                    || (self.mouse.mods.shift && !self.mouse_shift_capture(false))
                {
                    // Refresh our link state
                    let Ok(pos) = self.rt_surface().get_cursor_pos() else {
                        break 'mouse_mods;
                    };
                    let _l = self.lock();
                    let pos_vp = self.pos_to_viewport(pos.x as f64, pos.y as f64);
                    if let Err(err) = self.mouse_refresh_links(pos, pos_vp, self.mouse.over_link) {
                        log::warn!(target: LOG, "failed to refresh links err={err}");
                        break 'mouse_mods;
                    }
                } else if self.io.terminal.flags.mouse_event != terminal::mouse::Event::None && !self.mouse.mods.shift {
                    // If we have mouse reports on and we don't have shift pressed, we reset state
                    self.perform(RtAction::MouseShape(self.io.terminal.mouse_shape))?;
                    self.perform(RtAction::MouseOverLink(rt_action::MouseOverLink { url: c"" }))?;
                    self.queue_render()?;
                }
            }
        }

        // Process the cursor state logic. This will update the cursor shape if
        // needed, depending on the key state.
        if let Some(shape) = (SurfaceMouse {
            physical_key: event.key,
            mouse_event: self.io.terminal.flags.mouse_event,
            mouse_shape: self.io.terminal.mouse_shape,
            mods: self.mouse.mods,
            over_link: self.mouse.over_link,
            hidden: self.mouse.hidden,
        })
        .key_to_mouse_shape()
        {
            self.perform(RtAction::MouseShape(shape))?;
        }

        // We've processed a key event that produced some data so we want to
        // track the last pressed key.
        self.pressed_key = 'event: {
            // We need to unset the allocated fields that will become invalid
            let mut copy: KeyEvent<'static> = static_key_event(&event);

            // If we have a previous pressed key and we're releasing it
            // then we set it to invalid to prevent repeating the release event.
            if event.action == input::Action::Release {
                // if we didn't have a previous event and this is a release
                // event then we just want to set it to null.
                let Some(prev) = self.pressed_key else {
                    break 'event None;
                };
                if prev.key == copy.key {
                    copy.key = input::Key::Unidentified;
                }
            }

            // If our key is invalid and we have no mods, then we're done!
            // This helps catch the state that we naturally released all keys.
            if copy.key == input::Key::Unidentified && copy.mods.empty() {
                break 'event None;
            }

            Some(copy)
        };

        // Encode and send our key. If we didn't encode anything, then we
        // return the effect as ignored.
        if let Some(write_req) = self.encode_key(event, insp_ev)? {
            // If our process is exited and we press a key that results in
            // an encoded value, we close the surface. We want to eventually
            // move this behavior to the apprt probably.
            if self.child_exited {
                write_req.deinit();
                self.close();
                return Ok(InputEffect::Closed);
            }

            self.queue_io(write_req_message(write_req), MutexState::Unlocked);
        } else {
            // No valid request means that we didn't encode anything.
            return Ok(InputEffect::Ignored);
        }

        // If our event is any keypress that isn't a modifier and we generated
        // some data to send to the pty, then we move the viewport down to the
        // bottom. We also clear the selection for any key other then modifiers.
        if !event.key.modifier() {
            let _l = self.lock();

            if self.config.selection_clear_on_typing || event.key == input::Key::Escape {
                self.set_selection(None)?;
            }

            if self.config.scroll_to_bottom.keystroke {
                self.io.terminal.scroll_viewport(ScrollViewport::Bottom);
            }

            self.queue_render()?;
        }

        Ok(InputEffect::Consumed)
    }

    /// Maybe handles a binding for a given event and if so returns the effect.
    /// Returns null if the event is not handled in any way and processing should
    /// continue.
    fn maybe_handle_binding(
        &mut self,
        event: KeyEvent<'_>,
        insp_ev: Option<&mut InspectorKeyEvent>,
    ) -> Result<Option<InputEffect>, Error> {
        match event.action {
            // Release events never trigger a binding but we need to check if
            // we consumed the press event so we don't encode the release.
            input::Action::Release => {
                if let Some(last) = self.keyboard.last_trigger {
                    if last == event.binding_hash() {
                        // We don't reset the last trigger on release because
                        // an apprt may send multiple release events for a single
                        // press event.
                        return Ok(Some(InputEffect::Consumed));
                    }
                }

                return Ok(None);
            }

            // Carry on processing.
            input::Action::Press | input::Action::Repeat => {}
        }

        // The data we need from the matched entry. The entry borrows our
        // config, so copy it out before performing anything.
        enum Matched {
            Leader { set: *const BindingSet, trigger: Trigger },
            Leaf { flags: BindingFlags, actions: Vec<BindingAction> },
        }
        let matched: Matched = {
            // Find an entry in the keybind set that matches our event.
            let entry = 'entry: {
                // Handle key sequences first.
                if let Some(set) = self.keyboard.sequence_set {
                    // Get our entry from the set for the given event.
                    // SAFETY: sequence sets point into our derived config.
                    if let Some(v) = unsafe { &*set }.get_event(&event) {
                        break 'entry v;
                    }

                    // No entry found. We need to encode everything up to this
                    // point and send to the pty since we're in a sequence.

                    // We ignore modifiers so that nested sequences such as
                    // ctrl+a>ctrl+b>c work.
                    if event.key.modifier() {
                        return Ok(None);
                    }

                    // If we have a catch-all of ignore, then we special case our
                    // invalid sequence handling to ignore it.
                    if self.catch_all_is_ignore() {
                        self.end_key_sequence(KeySequenceQueued::Drop, KeySequenceMemory::Retain);
                        return Ok(Some(InputEffect::Ignored));
                    }

                    // Encode everything up to this point
                    self.end_key_sequence(KeySequenceQueued::Flush, KeySequenceMemory::Retain);

                    return Ok(None);
                }

                // No currently active sequence, move on to tables. For tables,
                // we search inner-most table to outer-most. The table stack does
                // NOT include the root set.
                let table_items = self.keyboard.table_stack.clone();
                if !table_items.is_empty() {
                    for (i, table) in table_items.iter().rev().enumerate() {
                        // SAFETY: table sets point into our derived config.
                        if let Some(v) = unsafe { &*table.set }.get_event(&event) {
                            // If this is a one-shot activation AND its the currently
                            // active table, then we deactivate it after this.
                            // Note: we may want to change the semantics here to
                            // remove this table no matter where it is in the stack,
                            // maybe.
                            if table.once && i == 0 {
                                self.perform_binding_action(BindingAction::DeactivateKeyTable)?;
                            }

                            break 'entry v;
                        }
                    }
                }

                // No table, use our default set
                let root: *const BindingSet = &self.config.keybind.set;
                // SAFETY: the root set lives in our derived config.
                match unsafe { &*root }.get_event(&event) {
                    Some(v) => v,
                    None => return Ok(None),
                }
            };

            // Determine if this entry has an action or if its a leader key.
            match entry.value_ptr {
                BindingValue::Leader(set) => Matched::Leader {
                    set: &**set,
                    trigger: *entry.key_ptr,
                },
                BindingValue::Leaf(leaf) => {
                    let leaf = leaf.generic();
                    Matched::Leaf {
                        flags: leaf.flags,
                        actions: leaf.actions_slice().to_vec(),
                    }
                }
                BindingValue::LeafChained(leaf) => {
                    let leaf = leaf.generic();
                    Matched::Leaf {
                        flags: leaf.flags,
                        actions: leaf.actions_slice().to_vec(),
                    }
                }
            }
        };

        let (flags, actions) = match matched {
            Matched::Leader { set, trigger } => {
                // Store this event so that we can drain and encode on invalid.
                // We don't need to cap this because it is naturally capped by
                // the config validation.
                if let Some(req) = self.encode_key(event, insp_ev)? {
                    self.keyboard.sequence_queued.push(req);
                }

                // Setup the next set we'll look at only after all fallible work.
                self.keyboard.sequence_set = Some(set);

                // Start or continue our key sequence
                if let Err(err) = self.perform(RtAction::KeySequence(rt_action::KeySequence::Trigger(trigger))) {
                    log::warn!(target: LOG, "failed to notify app of key sequence err={err}");
                }

                return Ok(Some(InputEffect::Consumed));
            }

            Matched::Leaf { flags, actions } => (flags, actions),
        };

        // consumed determines if the input is consumed or if we continue
        // encoding the key (if we have a key to encode).
        let consumed = {
            // If the consumed flag is explicitly set, then we are consumed.
            // If the global or all flag is set, we always consume.
            flags.consumed || flags.global || flags.all
        };

        // We have an action, so at this point we're handling SOMETHING so
        // we reset the last trigger to null. We only set this if we actually
        // perform an action (below)
        self.keyboard.last_trigger = None;

        // An action also always resets the sequence set.
        self.keyboard.sequence_set = None;

        // Attempt to perform the action
        log::debug!(target: LOG, "key event binding flags={flags:?} action={actions:?}");
        let performed = 'performed: {
            // If this is a global or all action, then we perform it on
            // the app and it applies to every surface.
            if flags.global || flags.all {
                // SAFETY: the runtime app outlives the surface.
                let rt_app = unsafe { &*self.rt_app };
                self.app().perform_all_chained_action(rt_app, &actions);

                // "All" actions are always performed since they are global.
                break 'performed true;
            }

            // Perform each action. We are performed if ANY of the chained
            // actions perform.
            let mut performed = false;
            for action in &actions {
                match self.perform_binding_action(action.clone()) {
                    Ok(v) => performed = performed || v,
                    Err(err) => {
                        log::info!(target: LOG, "key binding action failed action={} err={err}", action.key());
                    }
                }
            }

            performed
        };

        if performed {
            // If we performed an action and it was a closing action,
            // our "self" pointer is not safe to use anymore so we need to
            // just exit immediately.
            for action in &actions {
                if closing_action(action) {
                    log::debug!(target: LOG, "key binding is a closing binding, halting key event processing");
                    return Ok(Some(InputEffect::Closed));
                }
            }

            // If our action was "ignore" then we return the special input
            // effect of "ignored".
            for action in &actions {
                if *action == BindingAction::Ignore {
                    // If we're in a sequence, clear it.
                    self.end_key_sequence(KeySequenceQueued::Drop, KeySequenceMemory::Retain);

                    return Ok(Some(InputEffect::Ignored));
                }
            }
        }

        // If we have the performable flag and the action was not performed,
        // then we act as though a binding didn't exist.
        if flags.performable && !performed {
            // If we're in a sequence, we treat this as if we pressed a key
            // that doesn't exist in the sequence. Reset our sequence and flush
            // any queued events.
            self.end_key_sequence(KeySequenceQueued::Flush, KeySequenceMemory::Retain);

            return Ok(None);
        }

        // If we consume this event, then we are done. If we don't consume
        // it, we processed the action but we still want to process our
        // encodings, too.
        if consumed {
            // If we had queued events, we deinit them since we consumed
            self.end_key_sequence(KeySequenceQueued::Drop, KeySequenceMemory::Retain);

            // Store our last trigger so we don't encode the release event
            self.keyboard.last_trigger = Some(event.binding_hash());

            if let Some(ev) = insp_ev {
                ev.binding = actions;
            }
            return Ok(Some(InputEffect::Consumed));
        }

        // If we didn't perform OR we didn't consume, then we want to
        // encode any queued events for a sequence.
        self.end_key_sequence(KeySequenceQueued::Flush, KeySequenceMemory::Retain);

        Ok(None)
    }

    fn deactivate_all_key_tables(&mut self) -> Result<bool, Error> {
        match self.keyboard.table_stack.len() {
            // No key table active. This does nothing.
            0 => return Ok(false),

            // Clear the entire table stack.
            _ => self.keyboard.table_stack = Vec::new(),
        }

        // Notify the UI.
        if let Err(err) = self.perform(RtAction::KeyTable(rt_action::KeyTable::DeactivateAll)) {
            log::warn!(target: LOG, "failed to notify app of key table err={err}");
        }

        Ok(true)
    }

    /// This checks if the current keybinding sets have a catch_all binding
    /// with `ignore`. This is used to determine some special input cases.
    fn catch_all_is_ignore(&self) -> bool {
        // Get our catch all
        let trigger = Trigger {
            key: TriggerKey::CatchAll,
            ..Default::default()
        };
        let entry = 'entry: {
            for table in self.keyboard.table_stack.iter().rev() {
                // SAFETY: table sets point into our derived config.
                if let Some(entry) = unsafe { &*table.set }.get(trigger) {
                    break 'entry entry;
                }
            }

            match self.config.keybind.set.get(trigger) {
                Some(v) => v,
                None => return false,
            }
        };

        // We have a catch-all entry, see if its an ignore
        match entry.value_ptr {
            BindingValue::Leader(_) => false,
            BindingValue::Leaf(leaf) => leaf.action == BindingAction::Ignore,
            BindingValue::LeafChained(leaf) => leaf.actions.iter().any(|a| *a == BindingAction::Ignore),
        }
    }

    /// End a key sequence. Safe to call if no key sequence is active.
    ///
    /// Action and mem determine the behavior of the queued inputs up to this
    /// point.
    fn end_key_sequence(&mut self, action: KeySequenceQueued, mem: KeySequenceMemory) {
        // Notify apprt key sequence ended
        if let Err(err) = self.perform(RtAction::KeySequence(rt_action::KeySequence::End)) {
            log::warn!(target: LOG, "failed to notify app of key sequence end err={err}");
        }

        // No matter what we clear our current sequence set. This restores
        // the set we look at to the root set.
        self.keyboard.sequence_set = None;

        // If we have no queued data, there is nothing else to do.
        if self.keyboard.sequence_queued.is_empty() {
            return;
        }

        // Run the proper action first
        let queued = std::mem::take(&mut self.keyboard.sequence_queued);
        let capacity = queued.capacity();
        match action {
            KeySequenceQueued::Flush => {
                for write_req in queued {
                    self.queue_io(write_req_message(write_req), MutexState::Unlocked);
                }
            }

            KeySequenceQueued::Drop => {
                for req in queued {
                    req.deinit();
                }
            }
        }

        // Memory handling of the sequence after the action
        match mem {
            KeySequenceMemory::Free => self.keyboard.sequence_queued = Vec::new(),
            KeySequenceMemory::Retain => self.keyboard.sequence_queued = Vec::with_capacity(capacity),
        }
    }

    /// Encodes the key event into a write request. The write request will
    /// always copy or allocate so the caller can safely free the event.
    fn encode_key(
        &mut self,
        event: KeyEvent<'_>,
        insp_ev: Option<&mut InspectorKeyEvent>,
    ) -> Result<Option<termio::message::WriteReq>, Error> {
        let write_req: termio::message::WriteReq = 'req: {
            // Build our encoding options, which requires the lock.
            let encoding_opts = self.encode_key_opts();

            // Try to write the input into a small array. This fits almost
            // every scenario. Larger situations can happen due to long
            // pre-edits.
            let mut data = [0u8; WriteReqSmall::MAX];
            let mut writer = std::io::Cursor::new(&mut data[..]);
            match input::key_encode::encode(&mut writer, event, encoding_opts) {
                Ok(()) => {
                    let written = writer.position() as usize;

                    // Special-case: we did nothing.
                    if written == 0 {
                        return Ok(None);
                    }

                    break 'req MessageData::Small(WriteReqSmall {
                        data,
                        len: written as u8,
                    });
                }

                // Means we need to allocate
                Err(_) => {}
            }

            // We need to allocate. We allocate double the UTF-8 length
            // or double the small array size, whichever is larger. That's
            // a heuristic that should work. The only scenario I know while
            // typing this where we don't have enough space is a long preedit,
            // and in that case the size we need is exactly the UTF-8 length,
            // so the double is being safe.
            let mut alloc_writer: Vec<u8> = Vec::with_capacity((event.utf8.len() * 2).max(data.len() * 2));
            input::key_encode::encode(&mut alloc_writer, event, encoding_opts)?;
            MessageData::Alloc(alloc_writer.into_boxed_slice())
        };

        // Copy the encoded data into the inspector event if we have one.
        // We do this before the mailbox because the IO thread could
        // release the memory before we get a chance to copy it.
        if let Some(ev) = insp_ev {
            ev.pty = write_req.slice().to_vec();
        }

        Ok(Some(write_req))
    }

    fn encode_key_opts(&self) -> input::key_encode::Options {
        let _l = self.lock();
        let t = &self.io.terminal;

        #[allow(unused_mut)]
        let mut opts = input::key_encode::Options::from_terminal(t);
        #[cfg(not(target_os = "macos"))]
        {
            opts
        }

        #[cfg(target_os = "macos")]
        {
            opts.macos_option_as_alt = match self.config.macos_option_as_alt {
                Some(v) => v,
                None => {
                    // If we don't have alt pressed, it doesn't matter what this
                    // config is so we can just say "false" and break out and avoid
                    // more expensive checks below.
                    if !self.mouse.mods.alt {
                        input::OptionAsAlt::False
                    } else {
                        // Alt is pressed, we're on macOS. We break some encapsulation
                        // here and assume libghostty for ease...
                        self.rt_app().keyboard_layout().detect_option_as_alt()
                    }
                }
            };

            opts
        }
    }

    /// Sends text as-is to the terminal without triggering any keyboard
    /// protocol. This will treat the input text as if it was pasted
    /// from the clipboard so the same logic will be applied. Namely,
    /// if bracketed mode is on this will do a bracketed paste. Otherwise,
    /// this will filter newlines to '\r'.
    pub fn text_callback(&mut self, text: &[u8]) -> Result<(), Error> {
        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        self.complete_clipboard_paste(text, true)
    }

    /// Callback for when the surface is fully visible or not, regardless
    /// of focus state. This is used to pause rendering when the surface
    /// is not visible, and also re-render when it becomes visible again.
    pub fn occlusion_callback(&mut self, visible: bool) -> Result<(), Error> {
        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        // Avoid duplicate renderer and visibility reports.
        if self.visible == visible {
            return Ok(());
        }
        self.visible = visible;

        // Update the terminal state for synchronous queries, then notify the IO
        // thread so it can emit a mode 2033 report when enabled.
        let report_visibility = {
            let _l = self.lock();
            self.io.terminal.flags.visible = visible;
            self.io.terminal.modes.get(terminal::modes::Mode::ReportVisibility)
        };
        if report_visibility {
            self.queue_io(
                termio::Message::VisibilityReport { visible, force: false },
                MutexState::Unlocked,
            );
        }

        self.renderer_mailbox_push(renderer::Message::Visible(visible));

        self.queue_render()
    }

    pub fn focus_callback(&mut self, focused: bool) -> Result<(), Error> {
        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        // Always update the app focused surface, otherwise we miss
        // the first surface created.
        if focused {
            let this = self as *mut Surface;
            self.app().focus_surface(this);
        }

        // If our focus state is unchanged we do nothing else.
        if self.focused == focused {
            return Ok(());
        }
        self.focused = focused;

        // Notify our render thread of the new state
        self.renderer_mailbox_push(renderer::Message::Focus(focused));

        if !focused {
            'unfocused: {
                // If we lost focus and we have a keypress, then we want to send a key
                // release event for it. Depending on the apprt, this CAN result in
                // duplicate key release events, but that is better than not sending
                // a key release event at all.
                let Some(mut pressed_key) = self.pressed_key.take() else {
                    break 'unfocused;
                };

                // All our actions will be releases
                pressed_key.action = input::Action::Release;

                // Release the full key first
                if pressed_key.key != input::Key::Unidentified {
                    let effect = self.key_callback(pressed_key).unwrap_or_else(|err| {
                        log::warn!(target: LOG, "error releasing key on focus loss err={err}");
                        InputEffect::Ignored
                    });
                    debug_assert!(effect != InputEffect::Closed);
                }

                // Release any modifiers if set
                if pressed_key.mods.empty() {
                    break 'unfocused;
                }

                // Go through all the modifiers and if they're set, releasing
                // both the left and right sides of the modifier. This may not match
                // the exact input event but it ensures a full reset.
                let original_key = pressed_key.key;
                type ModField = fn(&mut Mods) -> &mut bool;
                let keys: [(ModField, [input::Key; 2]); 4] = [
                    (|m| &mut m.shift, [input::Key::ShiftRight, input::Key::ShiftLeft]),
                    (|m| &mut m.ctrl, [input::Key::ControlRight, input::Key::ControlLeft]),
                    (|m| &mut m.alt, [input::Key::AltRight, input::Key::AltLeft]),
                    (|m| &mut m.super_, [input::Key::MetaRight, input::Key::MetaLeft]),
                ];
                for (field, sides) in keys {
                    if *field(&mut pressed_key.mods) {
                        *field(&mut pressed_key.mods) = false;
                        for side in sides {
                            pressed_key.key = side;
                            if pressed_key.key != original_key {
                                let effect = self.key_callback(pressed_key).unwrap_or_else(|err| {
                                    log::warn!(target: LOG, "error releasing key on focus loss err={err}");
                                    InputEffect::Ignored
                                });
                                debug_assert!(effect != InputEffect::Closed);
                            }
                        }
                    }
                }
            }
        }

        // Schedule render which also drains our mailbox
        self.queue_render()?;

        // Whenever our focus changes we unhide the mouse. The mouse will be
        // hidden again if the user starts typing. This helps alleviate some
        // buggy behavior upstream in macOS with the mouse never becoming visible
        // again when tabbing between programs (see #2525).
        self.show_mouse();

        // Update the focus state and notify the terminal
        {
            {
                let _l = self.lock();
                self.io.terminal.flags.focused = focused;
            }
            self.queue_io(termio::Message::Focused(focused), MutexState::Unlocked);
        }
        Ok(())
    }

    pub fn refresh_callback(&mut self) -> Result<(), Error> {
        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        // The point of this callback is to schedule a render, so do that.
        self.queue_render()
    }

    /// Mouse scroll event. Negative is down, left. Positive is up, right.
    ///
    /// "Natural scrolling" is a macOS term for inverting the scroll direction.
    /// This should be handled by the apprt implementation. At this layer,
    /// negative is always down, left.
    pub fn scroll_callback(&mut self, xoff: f64, yoff: f64, scroll_mods: input::ScrollMods) -> Result<(), Error> {
        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        // Always show the mouse again if it is hidden
        if self.mouse.hidden {
            self.show_mouse();
        }

        let y: ScrollAmount = if yoff == 0.0 {
            ScrollAmount::default()
        } else {
            'y: {
                // We use cell_size to determine if we have accumulated enough to trigger a scroll
                let cell_size = self.size.cell.height as f64;

                // If we have precision scroll, yoff is the number of pixels to scroll. In non-precision
                // scroll, yoff is the number of wheel ticks. Some mice are capable of reporting fractional
                // wheel ticks, which don't necessarily get reported as precision scrolls. We normalize all
                // scroll events to pixels by multiplying the wheel tick value and the cell size. This means
                // that a wheel tick of 1 results in single scroll event.
                let yoff_adjusted: f64 = if scroll_mods.precision {
                    yoff * self.config.mouse_scroll_multiplier.precision
                } else if cfg!(target_os = "macos") {
                    // Round out the yoff to an absolute minimum of 1. macos tries to
                    // simulate precision scrolling with non precision events by
                    // ramping up the magnitude of the offsets as it detects faster
                    // scrolling. Single click (very slow) scrolls are reported with a
                    // magnitude of 0.1 which would normally require a few clicks
                    // before we register an actual scroll event (depending on cell
                    // height and the mouse_scroll_multiplier setting).
                    let yoff_max = if yoff > 0.0 { yoff.max(1.0) } else { yoff.min(-1.0) };

                    yoff_max * cell_size * self.config.mouse_scroll_multiplier.discrete
                } else {
                    yoff * cell_size * self.config.mouse_scroll_multiplier.discrete
                };

                // Add our previously saved pending amount to the offset to get the
                // new offset value. The signs of the pending and yoff should match
                // so that we move further away from zero, but we don't assert
                // this because in theory a user could scroll in the opposite
                // direction and undo a pending scroll.
                let poff = self.mouse.pending_scroll_y + yoff_adjusted;

                // If the new offset is less than a single unit of scroll, we save
                // the new pending value and do not scroll yet.
                if poff.abs() < cell_size {
                    self.mouse.pending_scroll_y = poff;
                    break 'y ScrollAmount::default();
                }

                // We scroll by the number of rows in the offset and save the remainder
                let amount = poff / cell_size;
                debug_assert!(amount.abs() >= 1.0);
                self.mouse.pending_scroll_y = poff - (amount * cell_size);

                // Round towards zero.
                let delta = amount.trunc() as isize;
                debug_assert!(delta.abs() >= 1);

                ScrollAmount { delta }
            }
        };

        // For detailed comments see the y calculation above.
        let x: ScrollAmount = if xoff == 0.0 {
            ScrollAmount::default()
        } else {
            'x: {
                if !scroll_mods.precision {
                    let x_delta_isize = xoff.round() as isize;
                    break 'x ScrollAmount { delta: x_delta_isize };
                }

                let poff = self.mouse.pending_scroll_x + xoff;
                let cell_size = self.size.cell.width as f64;
                if poff.abs() < cell_size {
                    self.mouse.pending_scroll_x = poff;
                    break 'x ScrollAmount::default();
                }

                let amount = poff / cell_size;
                debug_assert!(amount.abs() >= 1.0);
                self.mouse.pending_scroll_x = poff - (amount * cell_size);
                let delta = amount.trunc() as isize;
                debug_assert!(delta.abs() >= 1);
                ScrollAmount { delta }
            }
        };

        // log.info("SCROLL: delta_y={} delta_x={}", .{ y.delta, x.delta });

        {
            let _l = self.lock();

            // If we have an active mouse reporting mode, clear the selection.
            // The selection can occur if the user uses the shift mod key to
            // override mouse grabbing from the window.
            if self.is_mouse_reporting() {
                self.set_selection(None)?;
            }

            // If we're in alternate screen with alternate scroll enabled, then
            // we convert to cursor keys. This only happens if we're:
            // (1) alt screen (2) no explicit mouse reporting and (3) alt
            // scroll mode enabled.
            if self.io.terminal.screens.active_key == ScreenKey::Alternate
                && self.io.terminal.flags.mouse_event == terminal::mouse::Event::None
                && self.io.terminal.modes.get(terminal::modes::Mode::MouseAlternateScroll)
            {
                if y.delta != 0 {
                    // When we send mouse events as cursor keys we always
                    // clear the selection.
                    self.set_selection(None)?;

                    let seq: &'static [u8] = if self.io.terminal.modes.get(terminal::modes::Mode::CursorKeys) {
                        // cursor key: application mode
                        match y.direction() {
                            ScrollDirection::UpRight => b"\x1bOA",
                            ScrollDirection::DownLeft => b"\x1bOB",
                        }
                    } else {
                        // cursor key: normal mode
                        match y.direction() {
                            ScrollDirection::UpRight => b"\x1b[A",
                            ScrollDirection::DownLeft => b"\x1b[B",
                        }
                    };
                    for _ in 0..y.magnitude() {
                        self.queue_io(termio::Message::WriteStable(seq), MutexState::Locked);
                    }
                }

                return Ok(());
            }

            // We have mouse events, are not in an alternate scroll buffer,
            // or have alternate scroll disabled. In this case, we just run
            // the normal logic.

            // If we're scrolling up or down, then send a mouse event.
            if self.is_mouse_reporting() {
                for _ in 0..y.delta.unsigned_abs() {
                    let pos = self.rt_surface().get_cursor_pos()?;
                    self.mouse_report(
                        Some(match y.direction() {
                            ScrollDirection::UpRight => MouseButton::Four,
                            ScrollDirection::DownLeft => MouseButton::Five,
                        }),
                        input::MouseAction::Press,
                        self.mouse.mods,
                        pos,
                    );
                }

                for _ in 0..x.delta.unsigned_abs() {
                    let pos = self.rt_surface().get_cursor_pos()?;
                    self.mouse_report(
                        Some(match x.direction() {
                            ScrollDirection::UpRight => MouseButton::Six,
                            ScrollDirection::DownLeft => MouseButton::Seven,
                        }),
                        input::MouseAction::Press,
                        self.mouse.mods,
                        pos,
                    );
                }

                // If mouse reporting is on, we do not want to scroll the
                // viewport.
                return Ok(());
            }

            if y.delta != 0 {
                // Modify our viewport, this requires a lock since it affects
                // rendering. We have to switch signs here because our delta
                // is negative down but our viewport is positive down.
                self.io.terminal.scroll_viewport(ScrollViewport::Delta(-y.delta));
            }
        }

        self.queue_render()
    }

    /// This is called when the content scale of the surface changes. The surface
    /// can then update any DPI-sensitive state.
    pub fn content_scale_callback(&mut self, content_scale: apprt::ContentScale) -> Result<(), Error> {
        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        // Calculate the new DPI
        let x_dpi = content_scale.x * DEFAULT_DPI as f32;
        let y_dpi = content_scale.y * DEFAULT_DPI as f32;

        // Update our font size which is dependent on the DPI
        let size = {
            let mut size = self.font_size;
            size.xdpi = x_dpi.round() as u16;
            size.ydpi = y_dpi.round() as u16;
            size
        };

        // If our DPI didn't actually change, save a lot of work by doing nothing.
        if size.xdpi == self.font_size.xdpi && size.ydpi == self.font_size.ydpi {
            return Ok(());
        }

        self.set_font_size(size)?;

        // Update our padding which is dependent on DPI. We only do this for
        // unbalanced padding since balanced padding is not dependent on DPI.
        if self.config.window_padding_balance == WindowPaddingBalance::False {
            self.size.padding = self.config.scaled_padding(x_dpi, y_dpi);
        }

        // Force a resize event because the change in padding will affect
        // pixel-level changes to the renderer and viewport.
        self.resize(self.size.screen)
    }

    /// Returns true if mouse reporting is enabled both in the config and
    /// the terminal state.
    fn is_mouse_reporting(&self) -> bool {
        self.config.mouse_reporting && self.io.terminal.flags.mouse_event != terminal::mouse::Event::None
    }

    pub fn mouse_reporting_active(&self) -> bool {
        let _l = self.lock();
        self.is_mouse_reporting()
    }

    fn mouse_report(
        &mut self,
        button: Option<MouseButton>,
        action: input::MouseAction,
        mods: Mods,
        pos: apprt::CursorPos,
    ) {
        // Mouse reporting must be enabled by both config and terminal state
        debug_assert!(self.config.mouse_reporting);
        debug_assert!(self.io.terminal.flags.mouse_event != terminal::mouse::Event::None);

        // Whether any button is pressed at all.
        let any_button_pressed = self.mouse.click_state.iter().any(|s| *s != MouseButtonState::Release);

        let mut data = [0u8; WriteReqSmall::MAX];
        let written = {
            // Build our encoding options.
            let mut opts = input::mouse_encode::Options::from_terminal(&self.io.terminal, self.size);
            opts.any_button_pressed = any_button_pressed;

            // Keep track of our last reported viewport cell for event
            // deduplication.
            opts.last_cell = Some(&mut self.mouse.event_point);

            let mut writer = std::io::Cursor::new(&mut data[..]);
            if let Err(err) = input::mouse_encode::encode(
                &mut writer,
                input::mouse_encode::Event {
                    button,
                    action,
                    mods,
                    pos: input::mouse_encode::Pos { x: pos.x, y: pos.y },
                },
                opts,
            ) {
                // This should never happen since mouse events should never
                // be able to overflow the size of our small array. But if it
                // does, let's log it and return. No need to crash upstreams.
                // In the future we may want to fall back to allocation.
                log::warn!(target: LOG, "failed to encode mouse event err={err}");
                return;
            }
            writer.position() as usize
        };
        if written == 0 {
            return;
        }

        self.queue_io(
            termio::Message::WriteSmall(WriteReqSmall {
                data,
                len: written as u8,
            }),
            MutexState::Locked,
        );
    }

    /// Returns true if the shift modifier is allowed to be captured by modifier
    /// events. It is up to the caller to still verify it is a situation in which
    /// shift capture makes sense (i.e. left button, mouse click, etc.)
    fn mouse_shift_capture(&self, lock: bool) -> bool {
        // Handle our never/always case where we don't need a lock.
        match self.config.mouse_shift_capture {
            ConfigMouseShiftCapture::Never => return false,
            ConfigMouseShiftCapture::Always => return true,
            ConfigMouseShiftCapture::False | ConfigMouseShiftCapture::True => {}
        }

        let _l = if lock { Some(self.lock()) } else { None };

        // If the terminal explicitly requests it then we always allow it
        // since we processed never/always at this point.
        match self.io.terminal.flags.mouse_shift_capture {
            terminal::terminal::MouseShiftCapture::False => return false,
            terminal::terminal::MouseShiftCapture::True => return true,
            terminal::terminal::MouseShiftCapture::Null => {}
        }

        // Otherwise, go with the user's preference
        match self.config.mouse_shift_capture {
            ConfigMouseShiftCapture::False => false,
            ConfigMouseShiftCapture::True => true,
            ConfigMouseShiftCapture::Never | ConfigMouseShiftCapture::Always => unreachable!(), // handled earlier
        }
    }

    /// Returns true if the mouse is currently captured by the terminal
    /// (i.e. reporting events).
    pub fn mouse_captured(&self) -> bool {
        let _l = self.lock();
        self.io.terminal.flags.mouse_event != terminal::mouse::Event::None
    }

    /// Called for mouse button press/release events. This will return true
    /// if the mouse event was consumed in some way (i.e. the program is capturing
    /// mouse events). If the event was not consumed, then false is returned.
    pub fn mouse_button_callback(
        &mut self,
        action: MouseButtonState,
        button: MouseButton,
        mods: Mods,
    ) -> Result<bool, Error> {
        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        // If we have an inspector, we always queue a render
        let result = self.mouse_button_callback_(action, button, mods);
        if self.inspector.is_some() {
            let _ = self.queue_render();
        }
        result
    }

    fn mouse_button_callback_(
        &mut self,
        action: MouseButtonState,
        button: MouseButton,
        mods: Mods,
    ) -> Result<bool, Error> {
        // log.debug("mouse action={} button={} mods={}", .{ action, button, mods });

        // Always record our latest mouse state
        self.mouse.click_state[button as usize] = action;

        // Always show the mouse again if it is hidden
        if self.mouse.hidden {
            self.show_mouse();
        }

        // Update our modifiers if they changed
        self.mods_changed(mods);

        // This is set to true if the terminal is allowed to capture the shift
        // modifier. Note we can do this more efficiently probably with less
        // locking/unlocking but clicking isn't that frequent enough to be a
        // bottleneck.
        let shift_capture = self.mouse_shift_capture(true);

        // Shift-click continues the previous mouse state if we have a selection.
        // cursorPosCallback will also do a mouse report so we don't need to do any
        // of the logic below.
        if button == MouseButton::Left && action == MouseButtonState::Press {
            // We could do all the conditionals in one but I find it more
            // readable as a human to break this one up.
            if mods.shift && self.mouse.selection_gesture.left_click_count > 0 && !shift_capture {
                'extend_selection: {
                    // We split this conditional out on its own because this is the
                    // only one that requires a renderer mutex grab which is VERY
                    // expensive because it could block all our threads.
                    if !self.has_selection() {
                        break 'extend_selection;
                    }

                    // If we are within the interval that the click would register
                    // an increment then we do not extend the selection.
                    let Some(click_time) = self.mouse.selection_gesture.left_click_time else {
                        break 'extend_selection;
                    };
                    let since = click_time.elapsed();
                    if since.as_nanos() <= self.config.mouse_interval as u128 {
                        // Click interval very short, we may be increasing
                        // click counts so we don't extend the selection.
                        break 'extend_selection;
                    }

                    let pos = self.rt_surface().get_cursor_pos()?;
                    self.cursor_pos_callback(pos, None)?;
                    return Ok(true);
                }
            }
        }

        if button == MouseButton::Left && action == MouseButtonState::Release {
            let _l = self.lock();

            // The selection gesture tracks whether a press became a drag by
            // comparing the release cell to the original press cell. Resolve the
            // release position and pin before notifying the gesture so later
            // release handling can query that state.
            let release_pos: Option<apprt::CursorPos> = match self.rt_surface().get_cursor_pos() {
                Ok(pos) => Some(pos),
                Err(err) => {
                    log::warn!(target: LOG, "error reading cursor position for mouse release err={err}");
                    None
                }
            };

            // If we can't map the release position to a cell, pass null so the
            // gesture can conservatively treat the release as having moved away
            // from the pressed cell.
            let release_pin: Option<Pin> = release_pos.and_then(|pos| {
                let release_vp = self.pos_to_viewport(pos.x as f64, pos.y as f64);
                self.io.terminal.screens.active.pages.pin(point::Point::Viewport(release_vp))
            });
            self.mouse
                .selection_gesture
                .release(&mut self.io.terminal, Release { pin: release_pin });

            // Stop selection scrolling when releasing the left mouse button
            // but only when selection scrolling is active.
            if self.selection_scroll_active {
                self.queue_io(termio::Message::SelectionScroll(false), MutexState::Locked);
            }

            // The selection clipboard is only updated for left-click drag when
            // the left button is released. This is to avoid the clipboard
            // being updated on every mouse move which would be noisy.
            if self.config.copy_on_select != CopyOnSelect::None {
                let prev_ = self.io.terminal.screens.active.selection;
                if let Some(prev) = prev_ {
                    self.set_selection_and_copy(Selection::init(prev.start(), prev.end(), prev.rectangle))?;
                }
            }

            // Handle link clicking. We want to do this before we do mouse
            // reporting or any other mouse handling because a successfully
            // clicked link will swallow the event.
            if self.mouse.over_link && !self.mouse.selection_gesture.left_click_dragged {
                // We are holding the renderer lock, but this should just be
                // a cached value.
                let pos = match release_pos {
                    Some(v) => v,
                    None => self.rt_surface().get_cursor_pos()?,
                };
                match self.process_links(pos) {
                    Ok(processed) => {
                        if processed {
                            return Ok(true);
                        }
                    }
                    Err(err) => log::warn!(target: LOG, "error processing links err={err}"),
                }
            }

            // Handle prompt clicking. If we released our mouse on a prompt
            // and we support some kind of click events, then we need to
            // move to it.
            match self.maybe_prompt_click() {
                Ok(handled) => {
                    if handled {
                        return Ok(true);
                    }
                }
                Err(err) => log::warn!(target: LOG, "error processing prompt click err={err}"),
            }
        }

        // Report mouse events if enabled
        {
            let _l = self.lock();
            if self.is_mouse_reporting() {
                'report: {
                    // If we have shift-pressed and we aren't allowed to capture it,
                    // then we do not do a mouse report.
                    if mods.shift && !shift_capture {
                        break 'report;
                    }

                    // In any other mouse button scenario without shift pressed we
                    // clear the selection since the underlying application can handle
                    // that in any way (i.e. "scrolling").
                    self.set_selection(None)?;

                    // We also set the left click count to 0 so that if mouse reporting
                    // is disabled in the middle of press (before release) we don't
                    // suddenly start selecting text.
                    self.mouse.selection_gesture.reset(&mut self.io.terminal);

                    let pos = self.rt_surface().get_cursor_pos()?;

                    let report_action = match action {
                        MouseButtonState::Press => input::MouseAction::Press,
                        MouseButtonState::Release => input::MouseAction::Release,
                    };

                    self.mouse_report(Some(button), report_action, self.mouse.mods, pos);

                    // If we're doing mouse reporting, we do not support any other
                    // selection or highlighting.
                    return Ok(true);
                }
            }
        }

        // For left button clicks we always record some information for
        // selection/highlighting purposes.
        if button == MouseButton::Left && action == MouseButtonState::Press {
            'click: {
                let _l = self.lock();
                let screen = self.io.terminal.screens.active;

                let pos = self.rt_surface().get_cursor_pos()?;
                let pin = {
                    let pt_viewport = self.pos_to_viewport(pos.x as f64, pos.y as f64);
                    match screen.pages.pin(point::Point::Viewport(pt_viewport)) {
                        Some(pin) => pin,
                        None => {
                            // Weird... our viewport x/y that we just converted isn't
                            // found in our pages. This is probably a bug but we don't
                            // want to crash in releases because its harmless. So, we
                            // only assert in debug mode.
                            debug_assert!(false, "viewport pin not found");
                            break 'click;
                        }
                    }
                };

                let behaviors = [
                    Behavior::Cell,
                    Behavior::Word,
                    if mods.ctrl_or_super() { Behavior::Output } else { Behavior::Line },
                ];
                let mut press_selection = self.mouse.selection_gesture.press(
                    &mut self.io.terminal,
                    Press {
                        time: Some(Instant::now()),
                        pin,
                        xpos: pos.x as f64,
                        ypos: pos.y as f64,
                        max_distance: self.size.cell.width as f64,
                        repeat_interval: self.config.mouse_interval,
                        word_boundary_codepoints: &self.config.selection_word_chars,
                        behaviors: &behaviors,
                    },
                )?;

                // The gesture owns the standard single/double/triple-click selection
                // behavior. Surface keeps terminal-surface-specific overrides here.
                match self.mouse.selection_gesture.left_click_count {
                    1 => {}

                    // Double click on a URL selects the entire URL instead of the
                    // standard word selection returned by the gesture.
                    2 => {
                        // Try link detection without requiring modifier keys.
                        if let Ok(Some(result)) = self.link_at_pin(pin, None) {
                            press_selection = Some(result.selection);
                        }
                        // Ignore any errors, likely regex errors.
                    }

                    3 => {}

                    // We should be bounded by 1 to 3
                    _ => unreachable!(),
                }

                // Use `setSelection` (not `setSelectionAndCopy`) here to avoid
                // touching the selection clipboard: for left mouse clicks we only
                // copy on release.
                if let Some(selection) = press_selection {
                    self.set_selection(Some(selection))?;
                    self.queue_render()?;
                } else if self.mouse.selection_gesture.left_click_count == 1
                    && self.io.terminal.screens.active.selection.is_some()
                {
                    self.set_selection(None)?;
                    self.queue_render()?;
                }
            }
        }

        // Middle-click action, either ignore, or paste from clipboard or paste from the selection clipboard if supported.
        if button == MouseButton::Middle && action == MouseButtonState::Press {
            match self.config.middle_click_action {
                MiddleClickAction::Ignore => {}
                MiddleClickAction::ClipboardPaste => {
                    self.start_clipboard_request(Clipboard::Standard, &ClipboardRequest::Paste(Clipboard::Standard))?;
                }
                MiddleClickAction::PrimaryPaste => {
                    if self.rt_surface().supports_clipboard(Clipboard::Selection) {
                        self.start_clipboard_request(
                            Clipboard::Selection,
                            &ClipboardRequest::Paste(Clipboard::Selection),
                        )?;
                    }
                }
            }
        }

        // Right-click down selects word for context menus. If the apprt
        // doesn't implement context menus this can be a bit weird but they
        // are supported by our two main apprts so we always do this. If we
        // want to be careful in the future we can add a function to apprts
        // that let's us know.
        if button == MouseButton::Right && action == MouseButtonState::Press {
            let mut l = Some(self.lock());
            'sel: {
                // Get our viewport pin
                let screen = self.io.terminal.screens.active;
                let pos = self.rt_surface().get_cursor_pos()?;
                let pin = {
                    let pt_viewport = self.pos_to_viewport(pos.x as f64, pos.y as f64);
                    match screen.pages.pin(point::Point::Viewport(pt_viewport)) {
                        Some(pin) => pin,
                        None => {
                            debug_assert!(false, "viewport pin not found");
                            break 'sel;
                        }
                    }
                };

                match self.config.right_click_action {
                    RightClickAction::Ignore => {}
                    RightClickAction::ContextMenu => {
                        // If we already have a selection and the selection contains
                        // where we clicked then we don't want to modify the selection.
                        if let Some(prev_sel) = self.io.terminal.screens.active.selection {
                            if prev_sel.contains(&screen, pin) {
                                break 'sel;
                            }

                            // The selection doesn't contain our pin, so we create a new
                            // word selection where we clicked.
                        }

                        // If there is a link at this position, we want to
                        // select the link. Otherwise, select the word.
                        if let Some(link) = self.link_at_pos(pos)? {
                            self.set_selection_and_copy(link.selection)?;
                        } else {
                            let Some(sel) = screen.select_word(pin, &self.config.selection_word_chars) else {
                                break 'sel;
                            };
                            self.set_selection_and_copy(sel)?;
                        }
                        self.queue_render()?;

                        // Don't consume so that we show the context menu in apprt.
                        return Ok(false);
                    }
                    RightClickAction::Copy => {
                        if let Some(sel) = self.io.terminal.screens.active.selection {
                            self.copy_selection_to_clipboards(
                                sel,
                                &[Clipboard::Standard],
                                input::binding::CopyToClipboard::Mixed,
                            )?;
                        }

                        self.set_selection(None)?;
                        self.queue_render()?;
                    }
                    RightClickAction::CopyOrPaste => {
                        if let Some(sel) = self.io.terminal.screens.active.selection {
                            self.copy_selection_to_clipboards(
                                sel,
                                &[Clipboard::Standard],
                                input::binding::CopyToClipboard::Mixed,
                            )?;
                            self.set_selection(None)?;
                            self.queue_render()?;
                        } else {
                            // Pasting can trigger a lock grab in complete clipboard
                            // request so we need to unlock.
                            drop(l.take());
                            let r = self.start_clipboard_request(
                                Clipboard::Standard,
                                &ClipboardRequest::Paste(Clipboard::Standard),
                            );
                            l = Some(self.lock());
                            r?;

                            // We don't need to clear selection because we didn't have
                            // one to begin with.
                        }
                    }
                    RightClickAction::Paste => {
                        // Before we yield the lock, clear our selection if we have
                        // one.
                        self.set_selection(None)?;
                        self.queue_render()?;

                        // Pasting can trigger a lock grab in complete clipboard
                        // request so we need to unlock.
                        drop(l.take());
                        let r = self.start_clipboard_request(
                            Clipboard::Standard,
                            &ClipboardRequest::Paste(Clipboard::Standard),
                        );
                        l = Some(self.lock());
                        r?;
                    }
                }
            }
            drop(l);

            // Consume the event such that the context menu is not displayed.
            return Ok(true);
        }

        Ok(false)
    }

    /// Requires the renderer state mutex is held.
    fn maybe_prompt_click(&mut self) -> Result<bool, Error> {
        use terminal::osc::parsers::semantic_prompt::ClickEvents;
        use terminal::screen::SemanticClick;

        let screen = self.io.terminal.screens.active;

        // If our screen doesn't handle any prompt clicks, then we never
        // do anything.
        if matches!(screen.semantic_prompt.click, SemanticClick::None) {
            return Ok(false);
        }

        // If cursor-click-to-move is disabled, we don't do any prompt clicking.
        if !self.config.cursor_click_to_move {
            return Ok(false);
        }

        // If our cursor isn't currently at a prompt then we don't handle
        // prompt clicks because we can't move if we're not in a prompt!
        if !self.io.terminal.cursor_is_at_prompt() {
            return Ok(false);
        }

        // A press reported under mouse tracking resets the gesture, and its
        // release must be reported too rather than moving the cursor.
        if self.mouse.selection_gesture.left_click_count == 0 {
            return Ok(false);
        }

        // If the left click moved away from its pressed cell then releasing the
        // mouse completes the drag gesture and we don't do prompt moving.
        if self.mouse.selection_gesture.left_click_dragged {
            return Ok(false);
        }

        // If we have a selection currently, then releasing the mouse completes
        // the selection and we don't do prompt moving.
        if screen.selection.is_some() {
            return Ok(false);
        }

        // Get the pin for our mouse click.
        let pos = self.rt_surface().get_cursor_pos()?;
        let pos_vp = self.pos_to_viewport(pos.x as f64, pos.y as f64);
        let click_pin: Pin = match screen.pages.pin(point::Point::Viewport(pos_vp)) {
            Some(pin) => pin,
            None => {
                // See mouseButtonCallback for explanation
                debug_assert!(false, "viewport pin not found");
                return Ok(false);
            }
        };

        // Get our cursor's most current prompt.
        let prompt_pin: Pin = {
            // SAFETY: the cursor pin is tracked and valid while locked.
            let mut it = unsafe { *screen.cursor.page_pin.as_ptr() }
                .prompt_iterator(terminal::page_list::Direction::LeftUp, None);
            match it.next() {
                Some(v) => v,
                None => {
                    // This shouldn't be possible because we asserted we're at
                    // a prompt above, so we MUST find some prompt in a left_up search.
                    log::warn!(target: LOG, "cursor is at prompt but no prompt found");
                    debug_assert!(false, "no prompt found");
                    return Ok(false);
                }
            }
        };

        // If our mouse click is before the prompt, we don't move.
        // We DO ALLOW clicks AFTER the prompt, specifically with Kitty's
        // click_events=1 since we rely on the shell to validate out of
        // bounds clicks. This matches Kitty's logic as best I can tell.
        if click_pin.before(prompt_pin) {
            return Ok(false);
        }

        // At this point we've established:
        // - Screen supports prompt clicks
        // - Cursor is at a prompt
        // - Click is at or below our prompt
        match screen.semantic_prompt.click {
            // Guarded at the start of this function
            SemanticClick::None => unreachable!(),

            SemanticClick::ClickEvents(v) => {
                // For the event, we always send a left-click press event.
                // This matches what Kitty sends.
                let (key, y): (u8, u32) = match v {
                    ClickEvents::Absolute => (1, pos_vp.y.saturating_add(1)),
                    ClickEvents::Relative => (2, prompt_click_relative_row(&screen.pages, prompt_pin, click_pin)),
                };
                let mut data = [0u8; WriteReqSmall::MAX];
                let resp_len = {
                    let mut w = std::io::Cursor::new(&mut data[..]);
                    write!(w, "\x1B[<0;{};{}M", pos_vp.x as u32 + 1, y)?;
                    w.position() as usize
                };

                // Not that noisy since this only happens on prompt clicks.
                log::debug!(
                    target: LOG,
                    "sending click_events={} event=ESC{}",
                    key,
                    String::from_utf8_lossy(&data[1..resp_len])
                );

                // Ask our IO thread to write the data
                self.queue_io(
                    termio::Message::WriteSmall(WriteReqSmall {
                        data,
                        len: resp_len as u8,
                    }),
                    MutexState::Locked,
                );
            }

            SemanticClick::Cl(_) => {
                let cursor_keys = self.io.terminal.modes.get(terminal::modes::Mode::CursorKeys);
                let left_arrow: &'static [u8] = if cursor_keys { b"\x1bOD" } else { b"\x1b[D" };
                let right_arrow: &'static [u8] = if cursor_keys { b"\x1bOC" } else { b"\x1b[C" };

                let mv = screen.prompt_click_move(click_pin);
                for _ in 0..mv.left {
                    self.queue_io(termio::Message::WriteStable(left_arrow), MutexState::Locked);
                }
                for _ in 0..mv.right {
                    self.queue_io(termio::Message::WriteStable(right_arrow), MutexState::Locked);
                }
            }
        }

        Ok(true)
    }

    /// Returns the link at the given cursor position, if any.
    ///
    /// Requires the renderer mutex is held.
    fn link_at_pos(&mut self, pos: apprt::CursorPos) -> Result<Option<Link>, Error> {
        // Convert our cursor position to a screen point.
        let screen = self.io.terminal.screens.active;
        let mouse_pin: Pin = {
            let point = self.pos_to_viewport(pos.x as f64, pos.y as f64);
            match screen.pages.pin(point::Point::Viewport(point)) {
                Some(pin) => pin,
                None => {
                    log::warn!(target: LOG, "failed to get pin for clicked point");
                    return Ok(None);
                }
            }
        };

        // Get our comparison mods
        let mouse_mods = self.mouse_mods_with_capture(self.mouse.mods);

        // If we have the proper modifiers set then we can check for OSC8 links.
        if self.config.link_osc8 && mouse_mods.equal(input::ctrl_or_super(Mods::default())) {
            'hyperlink: {
                let rac = mouse_pin.row_and_cell();
                // SAFETY: the pin is valid while the mutex is held.
                let cell = unsafe { &*rac.cell };
                if !cell.hyperlink() {
                    break 'hyperlink;
                }
                let sel = Selection::init(mouse_pin, mouse_pin, false);
                return Ok(Some(Link {
                    action: input::link::Action::OpenOsc8,
                    selection: sel,
                }));
            }
        }

        // Fall back to configured links
        self.link_at_pin(mouse_pin, Some(mouse_mods))
    }

    /// Detects if a link is present at the given pin.
    ///
    /// If mouse mods is null then mouse mod requirements are ignored (all
    /// configured links are checked).
    ///
    /// Requires the renderer state mutex is held.
    fn link_at_pin(&mut self, mouse_pin: Pin, mouse_mods: Option<Mods>) -> Result<Option<Link>, Error> {
        if self.config.links.is_empty() {
            return Ok(None);
        }

        let screen = self.io.terminal.screens.active;
        let Some(line) = screen.select_line(terminal::screen::SelectLine {
            pin: mouse_pin,
            whitespace: None,
            // Respect semantic prompt boundaries so link/path matching doesn't
            // merge shell prompt content with the text beside it.
            semantic_prompt_boundary: true,
        }) else {
            return Ok(None);
        };

        let strmap = screen.selection_string_map(terminal::screen::SelectionString { sel: line, trim: false });

        for link in &mut self.config.links {
            // Skip highlight/mods check when mouse_mods is null (double-click mode)
            if let Some(mods) = mouse_mods {
                match link.highlight {
                    input::link::Highlight::Always | input::link::Highlight::Hover => {}
                    input::link::Highlight::AlwaysMods(v) | input::link::Highlight::HoverMods(v) => {
                        if !v.equal(mods) {
                            continue;
                        }
                    }
                }
            }

            let action = link.action;
            let mut it = strmap.search_iterator(&mut link.regex);
            while let Some(m) = it.next()? {
                let sel = m.selection();
                if !sel.contains(&screen, mouse_pin) {
                    continue;
                }
                return Ok(Some(Link { action, selection: sel }));
            }
        }

        Ok(None)
    }

    /// This returns the mouse mods to consider for link highlighting or
    /// other purposes taking into account when shift is pressed for releasing
    /// the mouse from capture.
    ///
    /// The renderer state mutex must be held.
    fn mouse_mods_with_capture(&self, mods: Mods) -> Mods {
        // In any of these scenarios, whatever mods are set (even shift)
        // are preserved.
        if self.io.terminal.flags.mouse_event == terminal::mouse::Event::None {
            return mods;
        }
        if !mods.shift {
            return mods;
        }
        if self.mouse_shift_capture(false) {
            return mods;
        }

        // We have mouse capture, shift set, and we're not allowed to capture
        // shift, so we can clear shift.
        let mut final_ = mods;
        final_.shift = false;
        final_
    }

    /// Attempt to invoke the action of any link that is under the
    /// given position.
    ///
    /// Requires the renderer state mutex is held.
    fn process_links(&mut self, pos: apprt::CursorPos) -> Result<bool, Error> {
        let Some(link) = self.link_at_pos(pos)? else {
            return Ok(false);
        };
        match link.action {
            input::link::Action::Open => {
                let str = self
                    .io
                    .terminal
                    .screens
                    .active
                    .selection_string(terminal::screen::SelectionString {
                        sel: link.selection,
                        trim: false,
                    });

                let resolved_path = self.resolve_path_for_opening(&str);

                let url_to_open = resolved_path.as_deref().unwrap_or(&str);
                self.open_url(rt_action::OpenUrl {
                    kind: rt_action::OpenUrlKind::Unknown,
                    url: url_to_open.as_bytes(),
                })?;
            }

            input::link::Action::OpenOsc8 => {
                let Some(uri) = self.osc8_uri(link.selection.start()) else {
                    log::warn!(target: LOG, "failed to get URI for OSC8 hyperlink");
                    return Ok(false);
                };
                self.open_url(rt_action::OpenUrl {
                    kind: rt_action::OpenUrlKind::Osc8,
                    url: &uri,
                })?;
            }
        }

        Ok(true)
    }

    fn open_url(&self, action: rt_action::OpenUrl<'_>) -> Result<(), Error> {
        // If the apprt handles it then we're done.
        if self.perform(RtAction::OpenUrl(action))? {
            return Ok(());
        }

        // apprt didn't handle it, fallback to our simple cross-platform
        // URL opener. We log a warning because we want well-behaved
        // apprts to handle this themselves.
        log::warn!(target: LOG, "apprt did not handle open URL action, falling back to default opener");
        internal_os::open::open(action.kind, &String::from_utf8_lossy(action.url))?;
        Ok(())
    }

    /// Return the URI for an OSC8 hyperlink at the given position or null
    /// if there is no hyperlink.
    fn osc8_uri(&self, pin: Pin) -> Option<Vec<u8>> {
        // SAFETY: the pin is valid while the renderer mutex is held.
        let page = unsafe { pin.node_mut() }.page();
        let rac = pin.row_and_cell();
        let cell = unsafe { &*rac.cell };
        let link_id = page.lookup_hyperlink(cell)?;
        // SAFETY: the page memory is the base of its hyperlink set.
        let entry = unsafe { *page.hyperlink_set.get(page.memory, link_id) };
        Some(unsafe { entry.uri.slice(page.memory) }.to_vec())
    }

    pub fn mouse_pressure_callback(&mut self, stage: input::MousePressureStage, pressure: f64) -> Result<(), Error> {
        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        // We don't currently use the pressure value for anything. In the
        // future, we could report this to applications using new mouse
        // events or utilize it for some custom UI.
        let _ = pressure;

        // If the pressure stage is the same as what we already have do nothing
        if self.mouse.pressure_stage == stage {
            return Ok(());
        }

        // Update our pressure stage.
        self.mouse.pressure_stage = stage;

        // A deep press is pressure-sensitive pointer input, such as macOS force
        // click / deep click on a trackpad, that occurs while the left mouse
        // button is already down. Treat it as the platform text-selection
        // affordance: select the pressed word, then consume the active gesture so
        // further cursor motion doesn't drag the selection.
        let left_idx = MouseButton::Left as usize;
        if self.mouse.click_state[left_idx] == MouseButtonState::Press && stage == input::MousePressureStage::Deep {
            let _l = self.lock();

            let sel = self.mouse.selection_gesture.deep_press(
                &mut self.io.terminal,
                DeepPress {
                    word_boundary_codepoints: &self.config.selection_word_chars,
                },
            );

            // Deep press consumes the active drag gesture, so stop any pending
            // selection autoscroll timer that may have been started by the drag.
            if self.selection_scroll_active {
                self.queue_io(termio::Message::SelectionScroll(false), MutexState::Locked);
            }

            let Some(sel) = sel else {
                return Ok(());
            };
            self.set_selection(Some(sel))?;
            self.queue_render()?;
        }
        Ok(())
    }

    /// Cursor position callback.
    ///
    /// Send negative x or y values to indicate the cursor is outside the
    /// viewport. The magnitude of the negative values are meaningless;
    /// they are only used to indicate the cursor is outside the viewport.
    /// It's important to do this to ensure hover states are cleared.
    ///
    /// The mods parameter is optional because some apprts do not provide
    /// modifier information on cursor position events. If mods is null then
    /// we'll use the last known mods. This is usually accurate since mod events
    /// will trigger key press events but on some platforms we don't get them.
    /// For example, on macOS, unfocused surfaces don't receive key events but
    /// do receive mouse events so we have to rely on updated mods.
    pub fn cursor_pos_callback(&mut self, pos: apprt::CursorPos, mods: Option<Mods>) -> Result<(), Error> {
        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        // log.debug("cursor pos x={} y={} mods={?}", .{ pos.x, pos.y, mods });

        // If the position is negative, it is outside our viewport and
        // we need to clear any hover states.
        if pos.x < 0.0 || pos.y < 0.0 {
            // Reset our hyperlink state
            self.mouse.link_point = None;
            if self.mouse.over_link {
                self.mouse.over_link = false;
                self.perform(RtAction::MouseShape(self.io.terminal.mouse_shape))?;
                self.perform(RtAction::MouseOverLink(rt_action::MouseOverLink { url: c"" }))?;
                self.queue_render()?;
            }

            let _l = self.lock();

            // No mouse point so we don't highlight links
            self.renderer_state.mouse.point = None;

            // Mark the link's row as dirty, but continue with updating the
            // mouse state below so we can scroll when our position is negative.
            self.io.terminal.screens.active.dirty.hyperlink_hover = true;
        }

        // Always show the mouse again if it is hidden
        if self.mouse.hidden {
            self.show_mouse();
        }

        // Update our modifiers if they changed
        if let Some(v) = mods {
            self.mods_changed(v);
        }

        // The mouse position in the viewport
        let pos_vp = self.pos_to_viewport(pos.x as f64, pos.y as f64);

        // We always reset the over link status because it will be reprocessed
        // below. But we need the old value to know if we need to undo mouse
        // shape changes.
        let over_link = self.mouse.over_link;
        self.mouse.over_link = false;

        // We are reading/writing state for the remainder
        let _l = self.lock();

        // Update our mouse state. We set this to null initially because we only
        // want to set it when we're not selecting or doing any other mouse
        // event.
        self.renderer_state.mouse.point = None;

        // If we have an inspector, we need to always record position information
        if self.inspector.is_some() {
            let last_point = self.io.terminal.screens.active.pages.pin(point::Point::Viewport(pos_vp));
            let insp = self.inspector.as_mut().unwrap();
            insp.mouse.last_xpos = pos.x as f64;
            insp.mouse.last_ypos = pos.y as f64;
            insp.mouse.last_point = last_point;
            self.queue_render()?;
        }

        // Handle link hovering
        // We refresh links when
        // 1. we were previously over a link
        // OR
        // 2. the cursor position has changed (either we have no previous state, or the state has
        //    changed)
        // AND
        // 1. mouse reporting is off
        // OR
        // 2. mouse reporting is on and we are not reporting shift to the terminal
        if (over_link || self.mouse.link_point.is_none_or(|p| !p.eql(pos_vp)))
            && (self.io.terminal.flags.mouse_event == terminal::mouse::Event::None
                || (self.mouse.mods.shift && !self.mouse_shift_capture(false)))
        {
            // If we were previously over a link, we always update. We do this so that if the text
            // changed underneath us, even if the mouse didn't move, we update the URL hints and state
            self.mouse_refresh_links(pos, pos_vp, over_link)?;
        }

        // Do a mouse report
        if self.is_mouse_reporting() {
            'report: {
                // Shift overrides mouse "grabbing" in the window, taken from Kitty.
                // This only applies if there is a mouse button pressed so that
                // movement reports are not affected.
                if self.mouse.mods.shift && !self.mouse_shift_capture(false) {
                    for state in self.mouse.click_state {
                        if state != MouseButtonState::Release {
                            break 'report;
                        }
                    }
                }

                // We use the first mouse button we find pressed in order to report
                // since the spec (afaict) does not say...
                let button: Option<MouseButton> = self
                    .mouse
                    .click_state
                    .iter()
                    .position(|s| *s == MouseButtonState::Press)
                    .map(|i| MouseButton::ALL[i]);

                self.mouse_report(button, input::MouseAction::Motion, self.mouse.mods, pos);

                // If we're doing mouse motion tracking, we do not support text
                // selection.
                return Ok(());
            }
        }

        // Handle cursor position for text selection
        if self.mouse.click_state[MouseButton::Left as usize] == MouseButtonState::Press {
            'select: {
                // Left click pressed but count zero can happen if mouse reporting is on.
                // In this scenario, we mark the click state because we need that to
                // properly make some mouse reports, but we don't keep track of the
                // count because we don't want to handle selection.
                if self.mouse.selection_gesture.left_click_count == 0 {
                    break 'select;
                }

                // If our left-click pin no longer belongs to the active screen then we
                // don't process this. We don't invalidate our pin or mouse state
                // because if the same screen switches back then we can continue our
                // selection.
                if self.mouse.active_left_click_pin(&self.io.terminal.screens).is_none() {
                    break 'select;
                }

                // All roads lead to requiring a re-render at this point.
                self.queue_render()?;

                // Convert to points
                let screen = self.io.terminal.screens.active;
                let Some(pin) = screen.pages.pin(point::Point::Viewport(pos_vp)) else {
                    debug_assert!(false, "viewport pin not found");
                    return Ok(());
                };

                // Perform our drag behavior in our gesture handler.
                let geometry = self.drag_geometry();
                let rectangle = surface_mouse::is_rectangle_select_state(self.mouse.mods);
                let drag_selection = self.mouse.selection_gesture.drag(
                    &mut self.io.terminal,
                    Drag {
                        pin,
                        xpos: pos.x as f64,
                        ypos: pos.y as f64,
                        rectangle,
                        word_boundary_codepoints: &self.config.selection_word_chars,
                        geometry,
                    },
                );

                // Update our autoscroll timer based on the gesture state
                use terminal::selection_gesture::Autoscroll;
                match self.mouse.selection_gesture.left_drag_autoscroll {
                    Autoscroll::None => {
                        if self.selection_scroll_active {
                            self.queue_io(termio::Message::SelectionScroll(false), MutexState::Locked);
                        }
                    }
                    Autoscroll::Up | Autoscroll::Down => {
                        if !self.selection_scroll_active {
                            self.queue_io(termio::Message::SelectionScroll(true), MutexState::Locked);
                        }
                    }
                }

                // Update our selection based on the gesture state
                self.set_selection(drag_selection)?;
            }
        }

        Ok(())
    }

    /// Call to notify Ghostty that the color scheme for the terminal has
    /// changed.
    pub fn color_scheme_callback(&mut self, scheme: apprt::ColorScheme) -> Result<(), Error> {
        // Crash metadata in case we crash in here
        let _crash = self.crash_guard();

        let new_scheme = match scheme {
            apprt::ColorScheme::Light => Theme::Light,
            apprt::ColorScheme::Dark => Theme::Dark,
        };

        // If our scheme didn't change, then we don't do anything.
        if self.config_conditional_state.theme == new_scheme {
            return Ok(());
        }

        // Setup our conditional state which has the current color theme.
        self.config_conditional_state.theme = new_scheme;
        self.notify_config_conditional_state();

        // If mode 2031 is on, then we report the change live.
        self.queue_io(termio::Message::ColorSchemeReport { force: false }, MutexState::Unlocked);
        Ok(())
    }

    pub fn pos_to_viewport(&self, xpos: f64, ypos: f64) -> point::Coordinate {
        // Get our grid cell
        let coord = renderer::Coordinate::Surface(renderer::size::CoordinateSurface { x: xpos, y: ypos });
        let renderer::Coordinate::Grid(grid) = coord.convert(renderer::size::CoordinateTag::Grid, self.size) else {
            unreachable!()
        };
        point::Coordinate {
            x: grid.x,
            y: grid.y as u32,
        }
    }

    /// Scroll to the bottom of the viewport.
    ///
    /// Precondition: the render_state mutex must be held.
    fn scroll_to_bottom(&mut self) -> Result<(), Error> {
        self.io.terminal.scroll_viewport(ScrollViewport::Bottom);
        self.queue_render()
    }

    fn hide_mouse(&mut self) {
        if self.mouse.hidden {
            return;
        }
        self.mouse.hidden = true;
        if let Err(err) = self.perform(RtAction::MouseVisibility(rt_action::MouseVisibility::Hidden)) {
            log::warn!(target: LOG, "apprt failed to set mouse visibility err={err}");
        }
    }

    fn show_mouse(&mut self) {
        if !self.mouse.hidden {
            return;
        }
        self.mouse.hidden = false;
        if let Err(err) = self.perform(RtAction::MouseVisibility(rt_action::MouseVisibility::Visible)) {
            log::warn!(target: LOG, "apprt failed to set mouse visibility err={err}");
        }
    }

    /// Perform a binding action. A binding is a keybinding. This function
    /// must be called from the GUI thread.
    ///
    /// This function returns true if the binding action was performed. This
    /// may return false if the binding action is not supported or if the
    /// binding action would do nothing (i.e. previous tab with no tabs).
    ///
    /// NOTE: At the time of writing this comment, only previous/next tab
    /// will ever return false. We can expand this in the future if it becomes
    /// useful. We did previous/next tab so we could implement #498.
    pub fn perform_binding_action(&mut self, action: BindingAction) -> Result<bool, Error> {
        // Forward app-scoped actions to the app. Some app-scoped actions are
        // special-cased here because they do some special things when performed
        // from the surface.
        if action.scope() == BindingScope::App {
            let app_action = action.scoped_app().unwrap();
            // SAFETY: the runtime app outlives the surface.
            let rt_app = unsafe { &*self.rt_app };
            match app_action {
                input::binding::ScopedApp::NewWindow => {
                    let this = self as *mut Surface;
                    self.app().new_window(rt_app, app::NewWindow { parent: Some(this) })?
                }

                // Undo and redo both support both surface and app targeting.
                // If we are triggering on a surface then we perform the
                // action with the surface target.
                input::binding::ScopedApp::Undo => return Ok(self.perform(RtAction::Undo)?),

                input::binding::ScopedApp::Redo => return Ok(self.perform(RtAction::Redo)?),

                other => self.app().perform_action(rt_app, other)?,
            }
            return Ok(true);
        }

        match action.scoped_surface().unwrap() {
            v @ (ScopedSurface::Csi(_) | ScopedSurface::Esc(_)) => {
                // We need to send the CSI/ESC sequence as a single write request.
                // If you split it across two then the shell can interpret it
                // as two literals.
                let full_data: Vec<u8> = match v {
                    ScopedSurface::Csi(data) => {
                        let mut d = b"\x1b[".to_vec();
                        d.extend_from_slice(&data);
                        d
                    }
                    ScopedSurface::Esc(data) => {
                        let mut d = b"\x1b".to_vec();
                        d.extend_from_slice(&data);
                        d
                    }
                    _ => unreachable!(),
                };
                // (Zig prints into a 128-byte buffer.)
                if full_data.len() > 128 {
                    return Err(Error::msg("NoSpaceLeft"));
                }
                self.queue_io(termio::Message::write_req(&full_data), MutexState::Unlocked);

                // CSI/ESC triggers a scroll.
                {
                    let _l = self.lock();
                    if let Err(err) = self.scroll_to_bottom() {
                        log::warn!(target: LOG, "error scrolling to bottom err={err}");
                    }
                }
            }

            ScopedSurface::Text(data) => {
                let mut buf = vec![0u8; data.len()];
                let text = match crate::config::string::parse(&mut buf, &data) {
                    Ok(v) => v.to_vec(),
                    Err(err) => {
                        log::warn!(
                            target: LOG,
                            "error parsing text binding text={} err={err:?}",
                            String::from_utf8_lossy(&data)
                        );
                        return Ok(true);
                    }
                };
                self.queue_io(termio::Message::write_req(&text), MutexState::Unlocked);

                // Text triggers a scroll.
                {
                    let _l = self.lock();
                    if let Err(err) = self.scroll_to_bottom() {
                        log::warn!(target: LOG, "error scrolling to bottom err={err}");
                    }
                }
            }

            ScopedSurface::CursorKey(ck) => {
                // We send a different sequence depending on if we're
                // in cursor keys mode. We're in "normal" mode if cursor
                // keys mode is NOT set.
                let normal = {
                    let _l = self.lock();

                    // With the lock held, we must scroll to the bottom.
                    // We always scroll to the bottom for these inputs.
                    if let Err(err) = self.scroll_to_bottom() {
                        log::warn!(target: LOG, "error scrolling to bottom err={err}");
                    }

                    !self.io.terminal.modes.get(terminal::modes::Mode::CursorKeys)
                };

                // (Zig writes the config-owned slices as stable writes; the
                // binding was cloned so they are copied here.)
                if normal {
                    self.queue_io(termio::Message::write_req(&ck.normal), MutexState::Unlocked);
                } else {
                    self.queue_io(termio::Message::write_req(&ck.application), MutexState::Unlocked);
                }
            }

            ScopedSurface::Reset => {
                let _l = self.lock();
                self.io.terminal.full_reset();
            }

            ScopedSurface::StartSearch => {
                // To save resources, we don't actually start a search here,
                // we just notify the apprt. The real thread will start when
                // the first needles are set.
                return Ok(self.perform(RtAction::StartSearch(rt_action::StartSearch { needle: c"" }))?);
            }

            ScopedSurface::SearchSelection => {
                let Some(selection) = self.selection_string() else {
                    return Ok(false);
                };
                let needle = cstring(selection.as_bytes());
                return Ok(self.perform(RtAction::StartSearch(rt_action::StartSearch { needle: &needle }))?);
            }

            ScopedSurface::EndSearch => {
                // We only return that this was performed if we actually
                // stopped a search, but we also send the apprt end_search so
                // that GUIs can clean up stale stuff.
                let performed = self.search.is_some();

                if let Some(s) = self.search.take() {
                    s.deinit();
                }

                self.perform(RtAction::EndSearch)?;

                return Ok(performed);
            }

            ScopedSurface::Search(text) => 'search: {
                if self.search.is_none() {
                    // If we're stopping the search and we had no prior search,
                    // then there is nothing to do.
                    if text.is_empty() {
                        return Ok(false);
                    }

                    // We need a stable pointer back to the thread state.
                    let state = Box::new(terminal::search::Thread::init(
                        terminal::page_list::Allocator::GLOBAL,
                        terminal::search::thread::Options {
                            mutex: self.renderer_state.mutex,
                            terminal: self.renderer_state.terminal,
                            event_cb: Some(Self::search_callback),
                            event_userdata: self as *mut Surface as *mut c_void,
                        },
                    )?);
                    let mut s = Search { state, thread: None };

                    let state_ptr = SendPtr(&mut *s.state as *mut terminal::search::Thread);
                    let spawned = std::thread::Builder::new().name("search".into()).spawn(move || {
                        let state = state_ptr.get();
                        // SAFETY: the thread is joined before the state is freed.
                        unsafe { (*state).thread_main() };
                    });
                    match spawned {
                        Ok(t) => s.thread = Some(t),
                        Err(err) => {
                            s.state.deinit();
                            return Err(err.into());
                        }
                    }

                    self.search = Some(s);
                }
                let s = self.search.as_mut().unwrap();

                // Zero-length text means stop searching.
                if text.is_empty() {
                    self.search.take().unwrap().deinit();
                    break 'search;
                }

                s.state.mailbox.push(
                    terminal::search::thread::Message::ChangeNeedle(terminal::search::thread::WriteReq::init(&text)),
                    QueueTimeout::Forever,
                );
                let _ = s.state.wakeup.notify();
            }

            ScopedSurface::NavigateSearch(nav) => {
                let Some(s) = &mut self.search else {
                    return Ok(false);
                };
                s.state.mailbox.push(
                    terminal::search::thread::Message::Select(match nav {
                        input::binding::NavigateSearch::Next => terminal::search::screen::Select::Next,
                        input::binding::NavigateSearch::Previous => terminal::search::screen::Select::Prev,
                    }),
                    QueueTimeout::Forever,
                );
                let _ = s.state.wakeup.notify();
            }

            ScopedSurface::CopyToClipboard(format) => {
                let _l = self.lock();

                if let Some(sel) = self.io.terminal.screens.active.selection {
                    self.copy_selection_to_clipboards(sel, &[Clipboard::Standard], format)?;

                    // Clear the selection if configured to do so.
                    if self.config.selection_clear_on_copy {
                        match self.set_selection(None) {
                            Ok(()) => {
                                if let Err(err) = self.queue_render() {
                                    log::warn!(target: LOG, "failed to queue render after clear selection err={err}");
                                }
                            }
                            Err(err) => log::warn!(target: LOG, "failed to clear selection after copy err={err}"),
                        }
                    }

                    return Ok(true);
                }

                return Ok(false);
            }

            ScopedSurface::CopyUrlToClipboard => {
                // If the mouse isn't over a link, nothing we can do.
                if !self.mouse.over_link {
                    return Ok(false);
                }
                let pos = self.rt_surface().get_cursor_pos()?;

                let _l = self.lock();
                if let Some(link_info) = self.link_at_pos(pos)? {
                    let url_text: Vec<u8> = match link_info.action {
                        // For regex links, get the text from selection
                        input::link::Action::Open => self
                            .io
                            .terminal
                            .screens
                            .active
                            .selection_string(terminal::screen::SelectionString {
                                sel: link_info.selection,
                                trim: self.config.clipboard_trim_trailing_spaces,
                            })
                            .into_bytes(),

                        input::link::Action::OpenOsc8 => {
                            // For OSC8 links, get the URI directly from hyperlink data
                            let Some(uri) = self.osc8_uri(link_info.selection.start()) else {
                                log::warn!(target: LOG, "failed to get URI for OSC8 hyperlink");
                                return Ok(false);
                            };
                            uri
                        }
                    };

                    if let Err(err) = self.rt_surface().set_clipboard(
                        Clipboard::Standard,
                        &[ClipboardContent {
                            mime: b"text/plain",
                            data: &url_text,
                        }],
                        false,
                    ) {
                        log::error!(target: LOG, "error copying url to clipboard err={err}");
                        return Ok(false);
                    }

                    return Ok(true);
                }

                return Ok(false);
            }

            ScopedSurface::CopyTitleToClipboard => return Ok(self.perform(RtAction::CopyTitleToClipboard)?),

            ScopedSurface::PasteFromClipboard => {
                return Ok(self.start_clipboard_request(
                    Clipboard::Standard,
                    &ClipboardRequest::Paste(Clipboard::Standard),
                )? == ClipboardReadResult::Started)
            }

            ScopedSurface::PasteFromSelection => {
                return Ok(self.start_clipboard_request(
                    Clipboard::Selection,
                    &ClipboardRequest::Paste(Clipboard::Selection),
                )? == ClipboardReadResult::Started)
            }

            ScopedSurface::IncreaseFontSize(delta) => {
                // Max delta is somewhat arbitrary.
                let clamped_delta = delta.clamp(0.0, 255.0);

                log::debug!(target: LOG, "increase font size={clamped_delta}");

                // Max point size is somewhat arbitrary.
                let mut size = self.font_size;
                size.points = (size.points + clamped_delta).min(255.0);
                self.set_font_size(size)?;

                // Mark that we manually adjusted the font size
                self.font_size_adjusted = true;
            }

            ScopedSurface::DecreaseFontSize(delta) => {
                // Max delta is somewhat arbitrary.
                let clamped_delta = delta.clamp(0.0, 255.0);

                log::debug!(target: LOG, "decrease font size={clamped_delta}");

                let mut size = self.font_size;
                size.points = (size.points - clamped_delta).max(1.0);
                self.set_font_size(size)?;

                // Mark that we manually adjusted the font size
                self.font_size_adjusted = true;
            }

            ScopedSurface::ResetFontSize => {
                log::debug!(target: LOG, "reset font size");

                let mut size = self.font_size;
                size.points = self.config.original_font_size;
                self.set_font_size(size)?;

                // Reset font size also resets the manual adjustment state
                self.font_size_adjusted = false;
            }

            ScopedSurface::SetFontSize(points) => {
                log::debug!(target: LOG, "set font size={points}");

                let mut size = self.font_size;
                size.points = points.clamp(1.0, 255.0);
                self.set_font_size(size)?;

                // Mark that we manually adjusted the font size
                self.font_size_adjusted = true;
            }

            ScopedSurface::PromptSurfaceTitle => {
                return Ok(self.perform(RtAction::PromptTitle(rt_action::PromptTitle::Surface))?)
            }

            ScopedSurface::PromptTabTitle => return Ok(self.perform(RtAction::PromptTitle(rt_action::PromptTitle::Tab))?),

            ScopedSurface::PromptWindowTitle => {
                return Ok(self.perform(RtAction::PromptTitle(rt_action::PromptTitle::Window))?)
            }

            ScopedSurface::SetSurfaceTitle(v) => {
                let title = cstring(&v);
                return Ok(self.perform(RtAction::SetTitle(rt_action::SetTitle { title: &title }))?);
            }

            ScopedSurface::SetTabTitle(v) => {
                let title = cstring(&v);
                return Ok(self.perform(RtAction::SetTabTitle(rt_action::SetTitle { title: &title }))?);
            }

            ScopedSurface::SetWindowTitle(v) => {
                let title = cstring(&v);
                return Ok(self.perform(RtAction::SetWindowTitle(rt_action::SetTitle { title: &title }))?);
            }

            ScopedSurface::ClearScreen => {
                // This is a duplicate of some of the logic in termio.clearScreen
                // but we need to do this here so we can know the answer before
                // we send the message. If the currently active screen is on the
                // alternate screen then clear screen does nothing so we want to
                // return false so the keybind can be unconsumed.
                {
                    let _l = self.lock();
                    if self.io.terminal.screens.active_key == ScreenKey::Alternate {
                        return Ok(false);
                    }
                }

                self.queue_io(termio::Message::ClearScreen { history: true }, MutexState::Unlocked);
            }

            ScopedSurface::ScrollToTop => {
                self.queue_io(termio::Message::ScrollViewport(ScrollViewport::Top), MutexState::Unlocked);
            }

            ScopedSurface::ScrollToBottom => {
                self.queue_io(termio::Message::ScrollViewport(ScrollViewport::Bottom), MutexState::Unlocked);
            }

            ScopedSurface::ScrollToRow(n) => {
                {
                    let _l = self.lock();
                    self.io.terminal.screens.active.scroll(terminal::screen::Scroll::Row(n));
                }

                self.queue_render()?;
            }

            ScopedSurface::ScrollToSelection => {
                {
                    let _l = self.lock();
                    let mut screen = self.io.terminal.screens.active;
                    let Some(sel) = screen.selection else {
                        return Ok(false);
                    };
                    let tl = sel.top_left(&screen);
                    screen.scroll(terminal::screen::Scroll::Pin(tl));
                }

                self.queue_render()?;
            }

            ScopedSurface::ScrollPageUp => {
                let rows = self.size.grid().rows as isize;
                self.queue_io(termio::Message::ScrollViewport(ScrollViewport::Delta(-rows)), MutexState::Unlocked);
            }

            ScopedSurface::ScrollPageDown => {
                let rows = self.size.grid().rows as isize;
                self.queue_io(termio::Message::ScrollViewport(ScrollViewport::Delta(rows)), MutexState::Unlocked);
            }

            ScopedSurface::ScrollPageFractional(fraction) => {
                let rows = self.size.grid().rows as f32;
                let delta = (fraction * rows).trunc() as isize;
                self.queue_io(termio::Message::ScrollViewport(ScrollViewport::Delta(delta)), MutexState::Unlocked);
            }

            ScopedSurface::ScrollPageLines(lines) => {
                self.queue_io(
                    termio::Message::ScrollViewport(ScrollViewport::Delta(lines as isize)),
                    MutexState::Unlocked,
                );
            }

            ScopedSurface::JumpToPrompt(delta) => {
                self.queue_io(termio::Message::JumpToPrompt(delta as isize), MutexState::Unlocked);
            }

            ScopedSurface::WriteScreenFile(v) => self.write_screen_file(WriteScreenLoc::Screen, v)?,

            ScopedSurface::WriteScrollbackFile(v) => self.write_screen_file(WriteScreenLoc::History, v)?,

            ScopedSurface::WriteSelectionFile(v) => self.write_screen_file(WriteScreenLoc::Selection, v)?,

            ScopedSurface::NewTab => return Ok(self.perform(RtAction::NewTab)?),

            ScopedSurface::CloseTab(v) => {
                return Ok(self.perform(RtAction::CloseTab(match v {
                    input::binding::CloseTabMode::This => rt_action::CloseTabMode::This,
                    input::binding::CloseTabMode::Other => rt_action::CloseTabMode::Other,
                    input::binding::CloseTabMode::Right => rt_action::CloseTabMode::Right,
                }))?)
            }

            ScopedSurface::PreviousTab => return Ok(self.perform(RtAction::GotoTab(rt_action::GotoTab::PREVIOUS))?),
            ScopedSurface::NextTab => return Ok(self.perform(RtAction::GotoTab(rt_action::GotoTab::NEXT))?),
            ScopedSurface::LastTab => return Ok(self.perform(RtAction::GotoTab(rt_action::GotoTab::LAST))?),
            ScopedSurface::GotoTab(v) => {
                return Ok(self.perform(RtAction::GotoTab(rt_action::GotoTab(v as std::ffi::c_int)))?)
            }

            ScopedSurface::MoveTab(position) => {
                return Ok(self.perform(RtAction::MoveTab(rt_action::MoveTab { amount: position }))?)
            }

            ScopedSurface::MoveTabToNewWindow => return Ok(self.perform(RtAction::MoveTabToNewWindow)?),

            ScopedSurface::NewSplit(direction) => {
                use input::binding::SplitDirection as D;
                return Ok(self.perform(RtAction::NewSplit(match direction {
                    D::Right => rt_action::SplitDirection::Right,
                    D::Left => rt_action::SplitDirection::Left,
                    D::Down => rt_action::SplitDirection::Down,
                    D::Up => rt_action::SplitDirection::Up,
                    D::Auto => {
                        if self.size.screen.width > self.size.screen.height {
                            rt_action::SplitDirection::Right
                        } else {
                            rt_action::SplitDirection::Down
                        }
                    }
                }))?);
            }

            ScopedSurface::GotoSplit(direction) => {
                use input::binding::SplitFocusDirection as D;
                return Ok(self.perform(RtAction::GotoSplit(match direction {
                    D::Previous => rt_action::GotoSplit::Previous,
                    D::Next => rt_action::GotoSplit::Next,
                    D::Up => rt_action::GotoSplit::Up,
                    D::Left => rt_action::GotoSplit::Left,
                    D::Down => rt_action::GotoSplit::Down,
                    D::Right => rt_action::GotoSplit::Right,
                }))?);
            }

            ScopedSurface::GotoWindow(direction) => {
                return Ok(self.perform(RtAction::GotoWindow(match direction {
                    input::binding::GotoWindow::Previous => rt_action::GotoWindow::Previous,
                    input::binding::GotoWindow::Next => rt_action::GotoWindow::Next,
                }))?)
            }

            ScopedSurface::ResizeSplit(value) => {
                use input::binding::SplitResizeDirection as D;
                return Ok(self.perform(RtAction::ResizeSplit(rt_action::ResizeSplit {
                    amount: value.1,
                    direction: match value.0 {
                        D::Up => rt_action::ResizeSplitDirection::Up,
                        D::Down => rt_action::ResizeSplitDirection::Down,
                        D::Left => rt_action::ResizeSplitDirection::Left,
                        D::Right => rt_action::ResizeSplitDirection::Right,
                    },
                }))?);
            }

            ScopedSurface::EqualizeSplits => return Ok(self.perform(RtAction::EqualizeSplits)?),

            ScopedSurface::ToggleSplitZoom => return Ok(self.perform(RtAction::ToggleSplitZoom)?),

            ScopedSurface::ToggleReadonly => {
                self.readonly = !self.readonly;
                self.perform(RtAction::Readonly(if self.readonly {
                    rt_action::Readonly::On
                } else {
                    rt_action::Readonly::Off
                }))?;
                return Ok(true);
            }

            ScopedSurface::ResetWindowSize => return Ok(self.perform(RtAction::ResetWindowSize)?),

            ScopedSurface::ToggleMaximize => return Ok(self.perform(RtAction::ToggleMaximize)?),

            ScopedSurface::ToggleFullscreen => {
                return Ok(self.perform(RtAction::ToggleFullscreen(match self.config.macos_non_native_fullscreen {
                    NonNativeFullscreen::False => rt_action::Fullscreen::Native,
                    NonNativeFullscreen::True => rt_action::Fullscreen::MacosNonNative,
                    NonNativeFullscreen::VisibleMenu => rt_action::Fullscreen::MacosNonNativeVisibleMenu,
                    NonNativeFullscreen::PaddedNotch => rt_action::Fullscreen::MacosNonNativePaddedNotch,
                }))?)
            }

            ScopedSurface::ToggleWindowDecorations => return Ok(self.perform(RtAction::ToggleWindowDecorations)?),

            ScopedSurface::ToggleTabOverview => return Ok(self.perform(RtAction::ToggleTabOverview)?),

            ScopedSurface::ToggleWindowFloatOnTop => {
                return Ok(self.perform(RtAction::FloatWindow(rt_action::FloatWindow::Toggle))?)
            }

            ScopedSurface::ToggleSecureInput => {
                return Ok(self.perform(RtAction::SecureInput(rt_action::SecureInput::Toggle))?)
            }

            ScopedSurface::ToggleMouseReporting => {
                self.config.mouse_reporting = !self.config.mouse_reporting;
                log::debug!(target: LOG, "mouse reporting toggled: {}", self.config.mouse_reporting);
            }

            ScopedSurface::ToggleCommandPalette => return Ok(self.perform(RtAction::ToggleCommandPalette)?),

            ScopedSurface::ToggleBackgroundOpacity => return Ok(self.perform(RtAction::ToggleBackgroundOpacity)?),

            ScopedSurface::ShowOnScreenKeyboard => return Ok(self.perform(RtAction::ShowOnScreenKeyboard)?),

            ScopedSurface::SelectAll => {
                let _l = self.lock();

                let sel = self.io.terminal.screens.active.select_all();
                if let Some(s) = sel {
                    self.set_selection_and_copy(s)?;
                    self.queue_render()?;
                }
            }

            ScopedSurface::Inspector(mode) => {
                return Ok(self.perform(RtAction::Inspector(match mode {
                    input::InspectorMode::Toggle => rt_action::Inspector::Toggle,
                    input::InspectorMode::Show => rt_action::Inspector::Show,
                    input::InspectorMode::Hide => rt_action::Inspector::Hide,
                }))?)
            }

            ScopedSurface::CloseSurface => self.close(),

            ScopedSurface::CloseWindow => return Ok(self.perform(RtAction::CloseWindow)?),

            v @ (ScopedSurface::ActivateKeyTable(_) | ScopedSurface::ActivateKeyTableOnce(_)) => {
                let (name, once) = match v {
                    ScopedSurface::ActivateKeyTable(n) => (n, false),
                    ScopedSurface::ActivateKeyTableOnce(n) => (n, true),
                    _ => unreachable!(),
                };
                let name_str = String::from_utf8_lossy(&name);

                // Look up the table in our config
                let Some(set) = self.config.keybind.tables.get(&*name_str) else {
                    log::debug!(target: LOG, "key table not found: {name_str}");
                    return Ok(false);
                };
                let set: *const BindingSet = set;

                // If this is the same table as is currently active, then
                // do nothing.
                if let Some(active) = self.keyboard.table_stack.last() {
                    if std::ptr::eq(active.set, set) {
                        log::debug!(target: LOG, "ignoring duplicate activate table: {name_str}");
                        return Ok(false);
                    }
                }

                // If we're already at the max, ignore it.
                if self.keyboard.table_stack.len() >= MAX_ACTIVE_KEY_TABLES {
                    log::info!(target: LOG, "ignoring activate table, max depth reached: {name_str}");
                    return Ok(false);
                }

                // Add the table to the stack.
                self.keyboard.table_stack.push(KeyTable { set, once });

                // Notify the UI.
                if let Err(err) = self.perform(RtAction::KeyTable(rt_action::KeyTable::Activate(&name))) {
                    log::warn!(target: LOG, "failed to notify app of key table err={err}");
                }

                log::debug!(target: LOG, "key table activated: {name_str}");
            }

            ScopedSurface::DeactivateKeyTable => {
                match self.keyboard.table_stack.len() {
                    // No key table active. This does nothing.
                    0 => return Ok(false),

                    // Final key table active, clear our state.
                    1 => self.keyboard.table_stack = Vec::new(),

                    // Restore the prior key table. We don't free any memory in
                    // this case because we assume it will be freed later when
                    // we finish our key table.
                    _ => {
                        self.keyboard.table_stack.pop();
                    }
                }

                // Notify the UI.
                if let Err(err) = self.perform(RtAction::KeyTable(rt_action::KeyTable::Deactivate)) {
                    log::warn!(target: LOG, "failed to notify app of key table err={err}");
                }
            }

            ScopedSurface::DeactivateAllKeyTables => {
                return self.deactivate_all_key_tables();
            }

            ScopedSurface::EndKeySequence => {
                // End the key sequence and flush queued keys to the terminal,
                // but don't encode the key that triggered this action. This
                // will do that because leaf keys (keys with bindings) aren't
                // in the queued encoding list.
                self.end_key_sequence(KeySequenceQueued::Flush, KeySequenceMemory::Retain);
            }

            ScopedSurface::Crash(location) => match location {
                input::binding::CrashThread::Main => panic!("crash binding action, crashing intentionally"),

                input::binding::CrashThread::Render => {
                    self.renderer_mailbox_push(renderer::Message::Crash);
                    if let Err(err) = self.queue_render() {
                        // Not a big deal if this fails.
                        log::warn!(target: LOG, "failed to notify renderer of crash message err={err}");
                    }
                }

                input::binding::CrashThread::Io => self.queue_io(termio::Message::Crash, MutexState::Unlocked),
            },

            ScopedSurface::AdjustSelection(direction) => {
                use input::binding::AdjustSelection as A;
                use terminal::selection::Adjustment;

                let _l = self.lock();

                let mut screen = self.io.terminal.screens.active;
                let Some(mut sel) = screen.selection else {
                    // If we don't have a selection we do not perform this
                    // action, allowing the keybind to fall through to the
                    // terminal.
                    return Ok(false);
                };
                sel.adjust(
                    &screen,
                    match direction {
                        A::Left => Adjustment::Left,
                        A::Right => Adjustment::Right,
                        A::Up => Adjustment::Up,
                        A::Down => Adjustment::Down,
                        A::PageUp => Adjustment::PageUp,
                        A::PageDown => Adjustment::PageDown,
                        A::Home => Adjustment::Home,
                        A::End => Adjustment::End,
                        A::BeginningOfLine => Adjustment::BeginningOfLine,
                        A::EndOfLine => Adjustment::EndOfLine,
                    },
                );
                // (Zig adjusts the selection in place through a pointer.)
                screen.selection = Some(sel);

                // If the selection endpoint is outside of the current viewpoint,
                // scroll it in to view. Note we always specifically use sel.end
                // because that is what adjust modifies.
                'scroll: {
                    let viewport_tl = screen.pages.get_top_left(point::Tag::Viewport);
                    let viewport_br = screen.pages.get_bottom_right(point::Tag::Viewport).unwrap();
                    if sel.end().is_between(viewport_tl, viewport_br) {
                        break 'scroll;
                    }

                    // Our end point is not within the viewport. If the end
                    // point is after the br then we need to adjust the end so
                    // that it is at the bottom right of the viewport.
                    let target = if sel.end().before(viewport_tl) {
                        sel.end()
                    } else {
                        sel.end().up(screen.pages.rows as usize - 1).unwrap_or(sel.end())
                    };

                    screen.scroll(terminal::screen::Scroll::Pin(target));
                }

                // Queue a render so its shown
                screen.dirty.selection = true;
                self.queue_render()?;
            }
        }

        Ok(true)
    }

    fn write_screen_file(&mut self, loc: WriteScreenLoc, write_screen: input::binding::WriteScreen) -> Result<(), Error> {
        use input::binding::{WriteScreenAction, WriteScreenFormat};
        use terminal::formatter::{Format, Options as FormatterOptions, ScreenFormatter, ScreenFormatterContent};

        // Create a temporary directory to store our scrollback.
        let tmp_dir = internal_os::temp_dir::TempDir::init()?;
        let mut retain_tmp_dir = false;

        let result = (|| -> Result<(), Error> {
            let filename = format!(
                "{}.{}",
                loc.as_str(),
                match write_screen.emit {
                    WriteScreenFormat::Plain | WriteScreenFormat::Vt => "txt",
                    WriteScreenFormat::Html => "html",
                }
            );

            // Open our scrollback file
            let file_path = tmp_dir.path().join(&filename);
            let file = {
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create(true).truncate(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                opts.open(&file_path)?
            };

            // Screen.dumpString writes byte-by-byte, so buffer it
            let mut buf_writer = std::io::BufWriter::with_capacity(4096, file);

            // Write the scrollback contents. This requires a lock.
            {
                let _l = self.lock();

                // We only dump history if we have history. We still keep
                // the file and write the empty file to the pty so that this
                // command always works on the primary screen.
                let t = &self.io.terminal;
                let screen = &*t.screens.active;
                let pages = &screen.pages;
                let sel_: Option<Selection> = match loc {
                    WriteScreenLoc::History => 'history: {
                        // We do not support this for alternate screens
                        // because they don't have scrollback anyways.
                        if t.screens.active_key == ScreenKey::Alternate {
                            break 'history None;
                        }

                        let Some(br) = pages.get_bottom_right(point::Tag::History) else {
                            break 'history None;
                        };
                        Some(Selection::init(pages.get_top_left(point::Tag::History), br, false))
                    }

                    WriteScreenLoc::Screen => 'screen: {
                        let Some(br) = pages.get_bottom_right(point::Tag::Screen) else {
                            break 'screen None;
                        };
                        Some(Selection::init(pages.get_top_left(point::Tag::Screen), br, false))
                    }

                    WriteScreenLoc::Selection => screen.selection,
                };

                let Some(sel) = sel_ else {
                    // If we have no selection we have no data so we do nothing.
                    return Ok(());
                };

                let mut formatter = ScreenFormatter::init(
                    screen,
                    FormatterOptions {
                        emit: match write_screen.emit {
                            WriteScreenFormat::Plain => Format::Plain,
                            WriteScreenFormat::Vt => Format::Vt,
                            WriteScreenFormat::Html => Format::Html,
                        },
                        unwrap: true,
                        trim: false,
                        background: t.colors.background.get(),
                        foreground: t.colors.foreground.get(),
                        palette: Some(&t.colors.palette.current),
                        ..FormatterOptions::new(Format::Plain)
                    },
                );
                formatter.content = ScreenFormatterContent::Selection(Some(
                    sel.ordered(screen, terminal::selection::Order::Forward),
                ));
                formatter.format(&mut buf_writer)?;
            }
            buf_writer.flush()?;
            drop(buf_writer);

            // Get the final path
            let path = std::fs::canonicalize(&file_path)?;
            let path = path.to_string_lossy().into_owned();

            match write_screen.action {
                WriteScreenAction::Copy => {
                    self.rt_surface().set_clipboard(
                        Clipboard::Standard,
                        &[ClipboardContent {
                            mime: b"text/plain",
                            data: path.as_bytes(),
                        }],
                        false,
                    )?;
                }
                WriteScreenAction::Open => self.open_url(rt_action::OpenUrl {
                    kind: match write_screen.emit {
                        WriteScreenFormat::Plain | WriteScreenFormat::Vt => rt_action::OpenUrlKind::Text,
                        WriteScreenFormat::Html => rt_action::OpenUrlKind::Html,
                    },
                    url: path.as_bytes(),
                })?,
                WriteScreenAction::Paste => {
                    self.queue_io(termio::Message::write_req(path.as_bytes()), MutexState::Unlocked)
                }
            }

            // The action accepted the path, so retain the file for its consumer.
            retain_tmp_dir = true;
            Ok(())
        })();

        if retain_tmp_dir {
            tmp_dir.close(internal_os::temp_dir::CloseMode::Retain);
        } else {
            tmp_dir.deinit();
        }
        result
    }

    /// Call this to complete a clipboard request sent to apprt. This should
    /// only be called once for each request.
    ///
    /// If `complete.confirmed` is false then this may return either an
    /// UnsafePaste or UnauthorizedPaste error, depending on the type of
    /// clipboard request. The request state remains alive in that case so
    /// the apprt can run its confirmation flow. (The apprt owns the request;
    /// the Kitty request states are released by the apprt dropping it.)
    pub fn complete_clipboard_request(
        &mut self,
        req: &ClipboardRequest,
        complete: CompleteClipboard<'_>,
    ) -> Result<(), Error> {
        match req {
            ClipboardRequest::Paste(_) => self.complete_clipboard_paste(
                clipboard_text_content(complete.contents).unwrap_or(b""),
                complete.confirmed,
            )?,

            ClipboardRequest::List(clipboard) => {
                if !self.complete_clipboard_paste_event(*clipboard, complete.available)? {
                    log::debug!(target: LOG, "mode 5522 paste event was not written");
                }
            }

            ClipboardRequest::Osc52Read(clipboard) => self.complete_clipboard_read_osc52(
                clipboard_text_content(complete.contents).unwrap_or(b""),
                *clipboard,
                complete.confirmed,
            )?,

            ClipboardRequest::Osc52Write(clipboard) => {
                // The write API wants sentinel-terminated data; the write
                // text round-tripped through the apprt confirmation flow as
                // a plain representation.
                let data = clipboard_text_content(complete.contents).unwrap_or(b"");
                self.rt_surface().set_clipboard(
                    *clipboard,
                    &[ClipboardContent {
                        mime: b"text/plain",
                        data,
                    }],
                    !complete.confirmed,
                )?;
            }

            ClipboardRequest::KittyRead(kitty) => {
                // If we need confirmation we return an error without
                // consuming the request state; the apprt keeps it alive
                // for the confirmation flow. A session grant carried by
                // the request skips the prompt, and a request with no
                // data types is exempt from prompting entirely; see
                // readPromptExempt.
                if self.config.clipboard_read == ClipboardAccess::Ask
                    && !complete.confirmed
                    && !kitty.granted
                    && !kitty_clipboard::read_prompt_exempt(kitty.mimes.len())
                {
                    return Err(Error::UnauthorizedPaste);
                }

                // Record a session grant when the user asked to remember
                // their decision and the request carried a usable
                // password. The grants live with the terminal state on
                // the IO thread.
                if complete.remember && !kitty.pw.is_empty() {
                    let pw = kitty.pw.clone().into_boxed_slice();
                    self.queue_io(
                        termio::Message::KittyClipboardGrantRead(termio::message::KittyClipboardGrant { pw }),
                        MutexState::Unlocked,
                    );
                }

                self.complete_kitty_clipboard_read(kitty, complete.contents, complete.available)?;
            }

            ClipboardRequest::KittyWrite(kitty) => {
                // If we need confirmation we return an error without
                // consuming the request state; the apprt keeps it alive
                // for the confirmation flow. A session grant carried by
                // the request skips the prompt.
                if self.config.clipboard_write == ClipboardAccess::Ask && !complete.confirmed && !kitty.granted {
                    return Err(Error::UnauthorizedPaste);
                }

                // Record a session grant when the user asked to remember
                // their decision and the request carried a usable
                // password. The grants live with the terminal state on
                // the IO thread.
                if complete.remember && !kitty.pw.is_empty() {
                    let pw = kitty.pw.clone().into_boxed_slice();
                    self.queue_io(
                        termio::Message::KittyClipboardGrantWrite(termio::message::KittyClipboardGrant { pw }),
                        MutexState::Unlocked,
                    );
                }

                // Apply the committed representations carried by the
                // request itself; any contents echoed back by the apprt
                // are only what its confirmation prompt displayed. An
                // empty commit clears the clipboard, which the apprt
                // write API expresses as a single empty text entry.
                let contents: Vec<ClipboardContent<'_>> = if !kitty.contents.is_empty() {
                    kitty.contents.iter().map(|c| c.as_content()).collect()
                } else {
                    vec![ClipboardContent {
                        mime: b"text/plain",
                        data: b"",
                    }]
                };
                if let Err(err) = self.rt_surface().set_clipboard(kitty.location, &contents, false) {
                    log::error!(target: LOG, "error setting clipboard err={err}");
                    self.kitty_clipboard_status(
                        kitty_clipboard::Operation::Write,
                        &kitty.id,
                        kitty.terminator,
                        kitty_clipboard::Status::EIO,
                    )?;
                    return Ok(());
                }

                self.kitty_clipboard_status(
                    kitty_clipboard::Operation::Write,
                    &kitty.id,
                    kitty.terminator,
                    kitty_clipboard::Status::DONE,
                )?;
            }
        }
        Ok(())
    }

    /// Deny an in-flight clipboard request. This consumes the request: for
    /// request types whose protocol expects an answer, the denial reply is
    /// written to the pty.
    pub fn deny_clipboard_request(&mut self, req: &ClipboardRequest) {
        match req {
            // A denied paste simply doesn't happen.
            ClipboardRequest::Paste(_) | ClipboardRequest::List(_) => {}

            // OSC 52 has no error responses, but the client is waiting on
            // a reply, so a denied read is answered with empty contents.
            ClipboardRequest::Osc52Read(clipboard) => {
                if let Err(err) = self.complete_clipboard_read_osc52(b"", *clipboard, true) {
                    log::warn!(target: LOG, "error replying to OSC 52 clipboard read err={err}");
                }
            }

            // A denied write simply doesn't happen.
            ClipboardRequest::Osc52Write(_) => {}

            // The Kitty clipboard protocol reports denial explicitly.
            ClipboardRequest::KittyRead(kitty) => {
                if let Err(err) = self.kitty_clipboard_status(
                    kitty_clipboard::Operation::Read,
                    &kitty.id,
                    kitty.terminator,
                    kitty_clipboard::Status::EPERM,
                ) {
                    log::warn!(target: LOG, "error replying to kitty clipboard read err={err}");
                }
            }

            ClipboardRequest::KittyWrite(kitty) => {
                if let Err(err) = self.kitty_clipboard_status(
                    kitty_clipboard::Operation::Write,
                    &kitty.id,
                    kitty.terminator,
                    kitty_clipboard::Status::EPERM,
                ) {
                    log::warn!(target: LOG, "error replying to kitty clipboard write err={err}");
                }
            }
        }
    }

    /// This starts a clipboard request, with some basic validation. For example,
    /// an OSC 52 request is not actually requested if OSC 52 is disabled.
    ///
    /// The result reports whether the request was started; requests that
    /// weren't started never complete. Callers own reacting to that, e.g.
    /// performable paste keybinds pass through and Kitty clipboard reads
    /// answer the program.
    fn start_clipboard_request(&mut self, loc: Clipboard, req: &ClipboardRequest) -> Result<ClipboardReadResult, Error> {
        let list_req;
        let effective_req: &ClipboardRequest = match req {
            ClipboardRequest::Paste(clipboard) => {
                // Snapshot the mode before asking the apprt for clipboard data.
                // Event pastes request only a MIME listing, while ordinary
                // pastes request the text representation as before.
                let event = {
                    let _l = self.lock();
                    self.io.terminal.modes.get(terminal::modes::Mode::KittyPasteEvents)
                };

                if event {
                    list_req = ClipboardRequest::List(*clipboard);
                    &list_req
                } else {
                    req
                }
            }
            _ => req,
        };

        match effective_req {
            ClipboardRequest::Paste(_) | ClipboardRequest::List(_) => {} // always allowed
            ClipboardRequest::Osc52Read(_) => {
                if self.config.clipboard_read == ClipboardAccess::Deny {
                    log::info!(
                        target: LOG,
                        "application attempted to read clipboard, but 'clipboard-read' is set to deny"
                    );
                    return Ok(ClipboardReadResult::Unsupported);
                }
            }

            // The clipboard access policies were already applied by
            // kittyClipboardRead and kittyClipboardWrite, which own
            // replying on denial.
            ClipboardRequest::KittyRead(_) | ClipboardRequest::KittyWrite(_) => {}

            // OSC 52 writes don't travel through this function; they go
            // straight to the apprt setClipboard API.
            ClipboardRequest::Osc52Write(_) => unreachable!(),
        }

        Ok(self.rt_surface().clipboard_request(loc, effective_req)?)
    }

    fn complete_clipboard_paste(&mut self, data: &[u8], allow_unsafe: bool) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }

        let encode_opts: input::paste::Options = {
            let _l = self.lock();
            let opts = input::paste::Options::from_terminal(&self.io.terminal);

            // If we have paste protection enabled, we detect unsafe pastes and return
            // an error. The error approach allows apprt to attempt to complete the paste
            // before falling back to requesting confirmation.
            //
            // We do not do this for bracketed pastes because bracketed pastes are
            // by definition safe since they're framed.
            let unsafe_ = 'unsafe_: {
                // If we've disabled paste protection then we always allow the paste.
                if !self.config.clipboard_paste_protection {
                    break 'unsafe_ false;
                }

                // If we're allowed to paste unsafe data then we always allow the paste.
                // This is set during confirmation usually.
                if allow_unsafe {
                    break 'unsafe_ false;
                }

                if opts.bracketed {
                    // If we're bracketed and the paste contains and ending
                    // bracket then something naughty might be going on and we
                    // never trust it.
                    if memchr::memmem::find(data, b"\x1B[201~").is_some() {
                        break 'unsafe_ true;
                    }

                    // If we are bracketed and configured to trust that then the
                    // paste is not unsafe.
                    if self.config.clipboard_paste_bracketed_safe {
                        break 'unsafe_ false;
                    }
                }

                !input::paste::is_safe(data)
            };

            if unsafe_ {
                log::info!(target: LOG, "potentially unsafe paste detected, rejecting until confirmation");
                return Err(Error::UnsafePaste);
            }

            // With the lock held, we must scroll to the bottom.
            // We always scroll to the bottom for these inputs.
            if let Err(err) = self.scroll_to_bottom() {
                log::warn!(target: LOG, "error scrolling to bottom err={err}");
            }

            opts
        };

        // Encode the data. In most cases this doesn't require any
        // copies, so we optimize for that case.
        let mut data_duped: Vec<u8>;
        let vecs: [&[u8]; 3] = match input::paste::encode(data, encode_opts) {
            Ok(v) => v,
            Err(input::paste::Error::MutableRequired) => {
                // This code path means the data did require a copy and mutation.
                data_duped = data.to_vec();
                input::paste::encode(&mut data_duped[..], encode_opts)
            }
        };

        for vec in vecs {
            if !vec.is_empty() {
                self.queue_io(termio::Message::write_req(vec), MutexState::Unlocked);
            }
        }
        Ok(())
    }

    /// Send a Kitty clipboard-protocol paste event when mode 5522 is enabled.
    /// The event only lists the available MIME types; it does not read any of
    /// their data. The shared terminal paste implementation generates and records
    /// the one-time password used by the program's follow-up OSC 5522 read.
    fn complete_clipboard_paste_event(&mut self, clipboard: Clipboard, available: &[&[u8]]) -> Result<bool, Error> {
        if self.readonly {
            return Ok(false);
        }

        let location = match clipboard {
            Clipboard::Standard => terminal::clipboard::Location::STANDARD,
            Clipboard::Selection => terminal::clipboard::Location::SELECTION,
            Clipboard::Primary => terminal::clipboard::Location::PRIMARY,
        };

        // The protocol implementation caps listings at this size too. Cap here
        // so the temporary Content array stays on the stack.
        let contents_len = available.len().min(kitty_clipboard::MAX_LISTING_MIMES);
        let contents: Vec<terminal::clipboard::Content<'_>> = available[..contents_len]
            .iter()
            .map(|mime| terminal::clipboard::Content { mime, data: b"" })
            .collect();

        let mut aw: Vec<u8> = Vec::new();

        let _l = self.lock();

        let io = &mut *self.io;
        let pasted = terminal::paste::paste(
            terminal::paste::Context {
                terminal: &io.terminal,
                writer: &mut aw,
                kitty_clipboard: Some(terminal::paste::KittyClipboard {
                    grants: &mut io.terminal_stream.handler.kitty_clipboard_grants,
                    io: terminal::sys::os_random_secure,
                }),
            },
            terminal::paste::Request {
                source: terminal::paste::Source::Clipboard(location),
                contents: terminal::paste::Contents::Memory(&contents),
                // A paste event discloses no clipboard data, so unsafe-text
                // confirmation does not apply. If mode 5522 is reset, the empty
                // stand-in representations cause the shared helper to write nothing.
                allow_unsafe: true,
            },
        )?;
        if !pasted {
            return Ok(false);
        }

        self.queue_io(termio::Message::WriteAlloc(aw.into_boxed_slice()), MutexState::Locked);
        Ok(true)
    }

    fn complete_clipboard_read_osc52(
        &mut self,
        data: &[u8],
        clipboard_type: Clipboard,
        confirmed: bool,
    ) -> Result<(), Error> {
        use base64::Engine as _;

        // We should never get here if clipboard-read is set to deny
        debug_assert!(self.config.clipboard_read != ClipboardAccess::Deny);

        // If clipboard-read is set to ask and we haven't confirmed with the user,
        // do that now
        if self.config.clipboard_read == ClipboardAccess::Ask && !confirmed {
            return Err(Error::UnauthorizedPaste);
        }

        // Even if the clipboard data is empty we reply, since presumably
        // the client app is expecting a reply. We first allocate our buffer.
        // This must hold the base64 encoded data PLUS the OSC code surrounding it.
        let kind: u8 = match clipboard_type {
            Clipboard::Standard => b'c',
            Clipboard::Selection => b's',
            Clipboard::Primary => b'p',
        };

        // Wrap our data with the OSC code
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(b"\x1b]52;");
        buf.push(kind);
        buf.push(b';');
        debug_assert!(buf.len() == 7);

        // Do the base64 encoding
        let encoded = base64::engine::general_purpose::STANDARD.encode(data);
        buf.extend_from_slice(encoded.as_bytes());
        buf.extend_from_slice(b"\x1b\\");

        self.queue_io(termio::Message::WriteAlloc(buf.into_boxed_slice()), MutexState::Unlocked);
        Ok(())
    }

    /// Handle a Kitty clipboard protocol (OSC 5522) read request forwarded
    /// by the IO thread. This takes ownership of the request state.
    fn kitty_clipboard_read(&mut self, req: Box<KittyRead>) -> Result<(), Error> {
        // A read denied by policy answers EPERM so clients degrade
        // gracefully instead of waiting on a response that never comes.
        if self.config.clipboard_read == ClipboardAccess::Deny {
            log::info!(target: LOG, "application attempted to read clipboard, but 'clipboard-read' is set to deny");
            self.kitty_clipboard_status(kitty_clipboard::Operation::Read, &req.id, req.terminator, kitty_clipboard::Status::EPERM)?;
            req.destroy();
            return Ok(());
        }

        let location = req.location;
        let request = ClipboardRequest::KittyRead(req);
        let result = match self.start_clipboard_request(location, &request) {
            Ok(v) => v,
            Err(err) => {
                let ClipboardRequest::KittyRead(req) = &request else { unreachable!() };
                let _ = self.kitty_clipboard_status(
                    kitty_clipboard::Operation::Read,
                    &req.id,
                    req.terminator,
                    kitty_clipboard::Status::EIO,
                );
                return Err(err);
            }
        };
        let ClipboardRequest::KittyRead(req) = request else { unreachable!() };

        match result {
            // The request completes asynchronously (the apprt holds its own
            // copy of the request).
            ClipboardReadResult::Started => {}

            // The clipboard has nothing we can serve, which is a
            // successful read that serves no representations. This never
            // prompts even under an ask policy since there are no contents
            // to disclose.
            ClipboardReadResult::Unavailable => {
                self.complete_kitty_clipboard_read(&req, &[], &[])?;
            }

            // The apprt can't serve this clipboard at all, e.g. an
            // unsupported primary selection.
            ClipboardReadResult::Unsupported => {
                self.kitty_clipboard_status(
                    kitty_clipboard::Operation::Read,
                    &req.id,
                    req.terminator,
                    kitty_clipboard::Status::ENOSYS,
                )?;
            }
        }
        req.destroy();
        Ok(())
    }

    /// Handle a committed Kitty clipboard protocol (OSC 5522) write
    /// transaction forwarded by the IO thread. This takes ownership of the
    /// request state.
    fn kitty_clipboard_write(&mut self, req: Box<KittyWrite>) -> Result<(), Error> {
        // A write denied by policy answers EPERM so clients degrade
        // gracefully instead of waiting on a response that never comes.
        // The IO thread already fails transactions that begin under a
        // deny policy, but the policy may have changed mid-transaction.
        if self.config.clipboard_write == ClipboardAccess::Deny {
            log::info!(target: LOG, "application attempted to write clipboard, but 'clipboard-write' is set to deny");
            self.kitty_clipboard_status(kitty_clipboard::Operation::Write, &req.id, req.terminator, kitty_clipboard::Status::EPERM)?;
            req.destroy();
            return Ok(());
        }

        let location = req.location;
        let request = ClipboardRequest::KittyWrite(req);
        let result = match self.start_clipboard_request(location, &request) {
            Ok(v) => v,
            Err(err) => {
                let ClipboardRequest::KittyWrite(req) = &request else { unreachable!() };
                let _ = self.kitty_clipboard_status(
                    kitty_clipboard::Operation::Write,
                    &req.id,
                    req.terminator,
                    kitty_clipboard::Status::EIO,
                );
                return Err(err);
            }
        };
        let ClipboardRequest::KittyWrite(req) = request else { unreachable!() };

        match result {
            // The request completes asynchronously.
            ClipboardReadResult::Started => {}

            // The apprt can't write this clipboard at all, e.g. an
            // unsupported primary selection. Writes carry their own
            // contents so there is no meaningful unavailable state; treat
            // it the same.
            ClipboardReadResult::Unavailable | ClipboardReadResult::Unsupported => {
                self.kitty_clipboard_status(
                    kitty_clipboard::Operation::Write,
                    &req.id,
                    req.terminator,
                    kitty_clipboard::Status::ENOSYS,
                )?;
            }
        }
        req.destroy();
        Ok(())
    }

    /// Reply to a Kitty clipboard request with a single status packet.
    fn kitty_clipboard_status(
        &mut self,
        op: kitty_clipboard::Operation,
        id: &[u8],
        terminator: terminal::osc::Terminator,
        status: kitty_clipboard::Status,
    ) -> Result<(), Error> {
        let mut aw: Vec<u8> = Vec::new();
        kitty_clipboard::Response {
            id,
            terminator,
            ..kitty_clipboard::Response::new(op, status)
        }
        .encode(&mut aw)?;

        self.queue_io(termio::Message::WriteAlloc(aw.into_boxed_slice()), MutexState::Unlocked);
        Ok(())
    }

    /// Complete a Kitty clipboard protocol read with the clipboard
    /// contents.
    fn complete_kitty_clipboard_read(
        &mut self,
        req: &KittyRead,
        contents: &[terminal::clipboard::Content<'_>],
        available: &[&[u8]],
    ) -> Result<(), Error> {
        // Serve the requested representations in request order under their
        // requested names. Text-like MIME aliases all match the canonical
        // text representation, since that is the only name the apprt
        // serves text under. Requested types without a representation are
        // simply never served, which is how the protocol communicates an
        // unavailable representation.
        let mut served: Vec<terminal::clipboard::Content<'_>> = Vec::with_capacity(kitty_clipboard::MAX_READ_MIMES);
        for mime in &req.mimes {
            let mime = mime.to_bytes();
            let data: Option<&[u8]> = contents.iter().find_map(|content| {
                if content.mime == mime {
                    return Some(content.data);
                }
                if terminal::clipboard::is_text_mime(mime) && terminal::clipboard::is_text_mime(content.mime) {
                    return Some(content.data);
                }
                None
            });
            let Some(data) = data else { continue };

            served.push(terminal::clipboard::Content { mime, data });
        }

        // Encode the full success sequence: the OK packet, the targets
        // listing if it was requested, DATA chunks for each served
        // representation, and the final DONE packet.
        let mut aw: Vec<u8> = Vec::new();
        kitty_clipboard::ReadSuccess {
            primary: req.location == Clipboard::Primary,
            id: &req.id,
            pw: None,
            list: req.list,
            available,
            contents: &served,
            terminator: req.terminator,
        }
        .encode(&mut aw)?;

        self.queue_io(termio::Message::WriteAlloc(aw.into_boxed_slice()), MutexState::Unlocked);
        Ok(())
    }

    fn show_desktop_notification(&mut self, title: &CStr, body: &CStr) -> Result<(), Error> {
        // Wyhash is used to hash the contents of the desktop notification to limit
        // how fast identical notifications can be sent sequentially.
        let now = Instant::now();

        // Set a limit of one desktop notification per second so that the OS
        // doesn't kill us when we run out of resources.
        if let Some(last) = self.app().last_notification_time {
            if now.saturating_duration_since(last).as_secs() < 1 {
                log::warn!(target: LOG, "rate limiting desktop notifications");
                return Ok(());
            }
        }

        let new_digest = {
            let mut hash = crate::terminal::hash_map::Wyhash::init(0);
            hash.update(title.to_bytes());
            hash.update(body.to_bytes());
            hash.final_()
        };

        // Set a limit of one notification per five seconds for desktop
        // notifications with identical content.
        if let Some(last) = self.app().last_notification_time {
            if self.app().last_notification_digest == new_digest
                && now.saturating_duration_since(last).as_secs() < 5
            {
                log::warn!(target: LOG, "suppressing identical desktop notification");
                return Ok(());
            }
        }

        self.app().last_notification_time = Some(now);
        self.app().last_notification_digest = new_digest;
        self.perform(RtAction::DesktopNotification(rt_action::DesktopNotification { title, body }))?;
        Ok(())
    }

    fn crash_thread_state(&self) -> sentry::ThreadState {
        sentry::ThreadState {
            r#type: sentry::ThreadType::Main,
            surface: self,
        }
    }

    /// Tell the surface to present itself to the user. This may involve raising the
    /// window and switching tabs.
    fn present_surface(&mut self) -> Result<(), Error> {
        self.perform(RtAction::PresentTerminal)?;
        Ok(())
    }

    /// Get information about the process(es) running within the surface. Returns
    /// `null` if there was an error getting the information or the information is
    /// not available on a particular platform.
    pub fn get_process_info(&mut self, info: ProcessInfo) -> Option<ProcessInfoValue<'_>> {
        self.io.get_process_info(info)
    }
}

/// Zig errdefer state for [`Surface::init`]: releases whatever was
/// initialized when init fails before the surface is written.
struct InitGuard<'a> {
    set: &'a font::SharedGridSet,
    font_grid_key: Option<font::shared_grid_set::Key>,
    renderer: Option<Renderer>,
    mutex: Option<*mut Mutex>,
    render_thread: Option<renderer::Thread>,
    io_thread: Option<termio::Thread>,
}

impl Drop for InitGuard<'_> {
    fn drop(&mut self) {
        if let Some(mut t) = self.io_thread.take() {
            t.deinit();
        }
        if let Some(mut t) = self.render_thread.take() {
            t.deinit();
        }
        if let Some(m) = self.mutex.take() {
            // SAFETY: allocated in init and not referenced by anything live.
            drop(unsafe { Box::from_raw(m) });
        }
        if let Some(mut r) = self.renderer.take() {
            r.deinit();
        }
        if let Some(key) = self.font_grid_key.take() {
            self.set.deref(&key);
        }
    }
}

/// The readonly interception of `queueIo` (split out so it can be tested
/// without a live surface): in readonly mode, we don't allow any writes
/// through to the pty. Dropped writes are freed.
fn readonly_filter(readonly: bool, msg: termio::Message) -> Option<termio::Message> {
    if readonly {
        match msg {
            termio::Message::WriteSmall(_) | termio::Message::WriteStable(_) | termio::Message::WriteAlloc(_) => {
                msg.deinit();
                return None;
            }

            _ => {}
        }
    }

    Some(msg)
}

/// A copy of a key event without its text (Zig copies the event and sets
/// `utf8` to an empty slice).
fn static_key_event(event: &KeyEvent<'_>) -> KeyEvent<'static> {
    KeyEvent {
        action: event.action,
        key: event.key,
        mods: event.mods,
        consumed_mods: event.consumed_mods,
        composing: event.composing,
        utf8: b"",
        unshifted_codepoint: event.unshifted_codepoint,
    }
}

/// Zig: `std.fs.path.resolve(&.{ base, path })` for a relative `path`:
/// joins and normalizes `.` and `..` components.
fn path_resolve(base: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for comp in base.split('/').chain(path.split('/')) {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
}

/// The first text-like representation of the contents, if any.
fn clipboard_text_content<'a>(contents: &[terminal::clipboard::Content<'a>]) -> Option<&'a [u8]> {
    for content in contents {
        if terminal::clipboard::is_text_mime(content.mime) {
            return Some(content.data);
        }
    }
    None
}

/// Returns true if performing the given action result in closing
/// the surface. This is used to determine if our self pointer is
/// still valid after performing some binding action.
fn closing_action(action: &BindingAction) -> bool {
    matches!(
        action,
        BindingAction::CloseSurface | BindingAction::CloseWindow | BindingAction::CloseTab(_)
    )
}

/// Returns the one-based SGR row for a click at or below the prompt.
fn prompt_click_relative_row(pages: &PageList, prompt_pin: Pin, click_pin: Pin) -> u32 {
    let prompt_y = pages.point_from_pin(point::Tag::Screen, prompt_pin).unwrap().coord().y;
    let click_y = pages.point_from_pin(point::Tag::Screen, click_pin).unwrap().coord().y;
    click_y - prompt_y + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_io_frees_allocated_writes_in_readonly_mode() {
        // queueIo must free allocated writes in read-only mode.
        let data: Box<[u8]> = b"\x1b]lGhostty\x1b\\".to_vec().into_boxed_slice();
        assert!(readonly_filter(true, termio::Message::WriteAlloc(data.clone())).is_none());
        assert!(readonly_filter(true, termio::Message::WriteStable(b"x")).is_none());
        // Non-write messages and non-readonly writes pass through.
        assert!(readonly_filter(true, termio::Message::Focused(true)).is_some());
        assert!(readonly_filter(false, termio::Message::WriteAlloc(data)).is_some());
    }

    #[test]
    fn path_resolve_normalizes() {
        assert_eq!("/home/u/x.txt", path_resolve("/home/u", "x.txt"));
        assert_eq!("/home/x.txt", path_resolve("/home/u/", "../x.txt"));
        assert_eq!("/home/u/a/b", path_resolve("/home/u", "./a//b"));
    }

    #[test]
    fn mouse_click_state_holds_every_button() {
        let mut mouse = Mouse::default();
        for value in MouseButton::ALL {
            mouse.click_state[value as usize] = MouseButtonState::Press;
        }
        for state in mouse.click_state {
            assert_eq!(MouseButtonState::Press, state);
        }
    }

    #[test]
    fn prompt_click_relative_row_cases() {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Layout {
            NoHistory,
            Offset,
            CrossPage,
            PromptAboveViewport,
        }
        struct Case {
            layout: Layout,
            click_y: u32,
            expected: u32,
        }
        let cases = [
            Case { layout: Layout::NoHistory, click_y: 1, expected: 1 },
            Case { layout: Layout::Offset, click_y: 3, expected: 3 },
            Case { layout: Layout::CrossPage, click_y: 3, expected: 3 },
            Case { layout: Layout::PromptAboveViewport, click_y: 0, expected: 3 },
        ];
        for case in cases {
            let mut pages = PageList::init(terminal::page_list::Options {
                cols: 80,
                rows: 5,
                ..Default::default()
            })
            .unwrap();
            let scroll_rows: u32 = match case.layout {
                Layout::NoHistory => 0,
                Layout::Offset | Layout::PromptAboveViewport => 10,
                // SAFETY: the first node is live.
                Layout::CrossPage => unsafe { pages.pages.first.unwrap().as_ref() }.capacity().rows as u32 - 2,
            };
            for _ in 0..scroll_rows {
                let _ = pages.grow().unwrap();
            }

            let prompt_y = if case.layout == Layout::PromptAboveViewport {
                scroll_rows - 2
            } else {
                scroll_rows + 1
            };
            let prompt = pages
                .pin(point::Point::Screen(point::Coordinate { x: 0, y: prompt_y }))
                .unwrap();
            let click = pages
                .pin(point::Point::Viewport(point::Coordinate { x: 7, y: case.click_y }))
                .unwrap();
            assert_eq!(case.layout == Layout::CrossPage, prompt.node != click.node);
            assert_eq!(
                case.layout == Layout::PromptAboveViewport,
                pages.point_from_pin(point::Tag::Viewport, prompt).is_none()
            );
            assert_eq!(case.expected, prompt_click_relative_row(&pages, prompt, click));
        }
    }
}

/// Not a Zig test: an end-to-end run of a core [`App`] and [`Surface`]
/// with the stand-in apprt surface, the real OpenGL renderer (on a
/// surfaceless EGL context) and renderer/IO threads, and a real child
/// process on a pty.
#[cfg(all(test, unix, not(target_os = "macos")))]
mod integration_tests {
    use std::time::Duration;

    use super::*;
    use crate::apprt::action::ActionKey;
    use crate::config::Command;
    use crate::renderer::opengl::device::test_context::egl_lock;

    fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        let start = Instant::now();
        while !f() {
            assert!(start.elapsed() < Duration::from_secs(20), "timeout waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn screen_text(surface: &Surface) -> String {
        let _l = surface.lock();
        surface.io.terminal.plain_string().unwrap()
    }

    #[test]
    fn app_surface_echo_hi() {
        let _guard = egl_lock();

        let mut app = App::create().unwrap();
        let mut rt_app = Box::new(apprt::App::default());
        let mut env = crate::command::EnvMap::new();
        env.insert("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into());
        env.insert("GHOSTTY_LOG".into(), "leaked".into());
        let mut rt_surface = Box::new(apprt::Surface {
            app: &mut *rt_app,
            env: Some(env),
            ..Default::default()
        });

        let config = Config {
            command: Some(Command::Direct(
                ["/bin/sh", "-c", "echo hi; printf \"$GHOSTTY_LOG\""]
                    .iter()
                    .map(|a| (*a).into())
                    .collect(),
            )),
            shell_integration: crate::config::config::ShellIntegration::None,
            term: "xterm-256color".into(),
            // The config is not finalized (gtk-single-instance stays
            // `detect`), so keep the GTK runtime's cgroup hooks out of it.
            linux_cgroup: crate::config::config::LinuxCgroup::Never,
            ..Config::default()
        };

        let mut surface = Surface::init(&config, &mut *app, &mut *rt_app, &mut *rt_surface).unwrap();
        rt_surface.core_surface = &mut *surface;
        app.add_surface(&mut *rt_surface);
        assert!(!app.first);
        assert_ne!(0, surface.id);
        assert_eq!(Some(&mut *surface as *mut Surface), app.find_surface_by_id(surface.id));

        // The surface reported its sizes and the command as the title.
        let actions = rt_app.take_actions();
        assert!(actions.contains(&ActionKey::CellSize));
        assert!(actions.contains(&ActionKey::SizeLimit));
        assert!(actions.contains(&ActionKey::QuitTimer));
        assert_eq!(Some(c"/bin/sh".to_owned()), rt_app.titles.lock().unwrap().first().cloned());

        // The child output reaches the terminal.
        wait_until("hi", || screen_text(&surface).starts_with("hi"));

        // Process the app mailbox until the child exit closes the surface.
        wait_until("close", || {
            app.tick(&rt_app).unwrap();
            rt_surface.closed.lock().unwrap().is_some()
        });
        assert!(surface.child_exited);
        // The child has exited, so no confirmation is needed.
        assert_eq!(Some(false), *rt_surface.closed.lock().unwrap());
        assert!(!app.needs_confirm_quit());
        let text = screen_text(&surface);
        assert!(text.starts_with("hi\n"), "{text:?}");
        // GHOSTTY_LOG is not leaked to the child.
        assert!(!text.contains("leaked"), "{text:?}");
        assert!(text.contains("Process exited. Press any key to close the terminal."), "{text:?}");
        assert!(rt_app.take_actions().contains(&ActionKey::ShowChildExited));

        // Binding actions: select all and copy to the clipboard.
        assert!(surface.perform_binding_action(BindingAction::SelectAll).unwrap());
        assert!(surface.has_selection());
        assert!(surface
            .perform_binding_action(BindingAction::CopyToClipboard(input::binding::CopyToClipboard::Plain))
            .unwrap());
        let copied = rt_surface.clipboard_data(Clipboard::Standard).unwrap();
        assert!(copied.starts_with(b"hi\n"), "{:?}", String::from_utf8_lossy(&copied));
        assert!(rt_app.take_actions().contains(&ActionKey::SelectionChanged));

        // App-scoped actions go through the app.
        assert!(surface.perform_binding_action(BindingAction::NewWindow).unwrap());
        assert_eq!(vec![ActionKey::NewWindow], rt_app.take_actions());

        // Font size changes resize the cells.
        let cell = surface.size.cell;
        assert!(surface.perform_binding_action(BindingAction::IncreaseFontSize(4.0)).unwrap());
        assert!(surface.font_size_adjusted);
        assert!(surface.size.cell.height > cell.height);
        assert!(surface.perform_binding_action(BindingAction::ResetFontSize).unwrap());
        assert!(!surface.font_size_adjusted);
        assert_eq!(cell, surface.size.cell);

        // Focus makes the surface the app's focused surface, which new
        // surface configs inherit the working directory from.
        surface.focus_callback(true).unwrap();
        assert_eq!(Some(&mut *surface as *mut Surface), app.focused_surface());
        {
            let _l = surface.lock();
            surface.io.terminal.set_pwd(b"/tmp").unwrap();
        }
        assert_eq!(Some(b"/tmp".to_vec()), surface.pwd());
        let mut inherit = Config::default();
        inherit.window_inherit_working_directory = true;
        let c = apprt::surface::new_config(&app, &inherit, apprt::surface::NewSurfaceContext::Window);
        assert_eq!(
            Some(crate::config::config::WorkingDirectory::Path("/tmp".into())),
            c.working_directory
        );

        // Preedit and IME position.
        surface.preedit_callback(Some("é".as_bytes())).unwrap();
        assert_eq!(1, surface.renderer_state.preedit.as_ref().unwrap().codepoints.len());
        assert!(surface.ime_point().width > 0.0);
        surface.preedit_callback(None).unwrap();
        assert!(surface.renderer_state.preedit.is_none());

        // Tear down.
        app.delete_surface(&mut *rt_surface);
        assert!(app.focused_surface().is_none());
        assert!(rt_app.take_actions().contains(&ActionKey::QuitTimer));
        surface.deinit();
        drop(surface);
        app.destroy();
    }
}
