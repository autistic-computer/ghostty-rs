//! Port of `src/termio/stream_handler.zig`.
//!
//! STATUS: ported
//!
//! Deviations from the Zig code:
//! * The pointers into the owning [`crate::termio::Termio`] (`size`,
//!   `terminal`, `termio_mailbox`) and to objects owned by the surface
//!   (`renderer_state`, `renderer_mailbox`) are raw pointers like Zig's.
//!   They are only dereferenced while the renderer state mutex is held.
//! * `enquiry_response` is an owned copy (Zig borrows the config's memory).
//! * Allocation failures are not modeled (Rust aborts on OOM), so the
//!   Kitty clipboard paths that turn `error.OutOfMemory` into `EIO` are
//!   unreachable and only the protocol errors remain.
//! * Arena-owned Kitty clipboard requests are plain owned values
//!   (`Box<KittyRead>`/`Box<KittyWrite>`); NUL-terminated strings are
//!   `CString`s truncated at an interior NUL.
//! * `kittyClipboardWriteFinish` takes the in-flight transaction from the
//!   handler instead of a pointer to it (Zig always passes
//!   `self.kitty_clipboard_write.?`).
//! * The OSC 7 host is validated by [`host_name_from_uri`], a port of
//!   `std.Io.net.HostName.fromUri`.
//! * The tmux viewer is taken out of `tmux_viewer` while its actions are
//!   handled (the actions borrow it and `messageWriter` borrows the
//!   handler); the boxed viewer itself does not move.
//! * `surfaceMessageWriter` retries a full app queue with the message handed
//!   back by `push_or_return` (Zig copies the message).

use std::ffi::CString;
use std::io::{self, Cursor, Write as _};
use std::sync::OnceLock;

use crate::apprt;
use crate::apprt::structs::{ClipboardContentBuf, KittyRead, KittyWrite};
use crate::app::Message as AppMessage;
use crate::apprt::surface::Message as SurfaceMessage;
use crate::build_config;
use crate::config::config::OSCColorReportFormat;
use crate::config::ClipboardAccess;
use crate::datastruct::blocking_queue::Timeout as QueueTimeout;
use crate::global::xev;
use crate::lib::tinyio::{Mutex, TinyIo};
use crate::os as internal_os;
use crate::renderer;
use crate::terminal::ansi;
use crate::terminal::apc;
use crate::terminal::charsets;
use crate::terminal::clipboard::Location as ClipboardLocation;
use crate::terminal::color::{self, Dynamic as DynamicColor, Rgb};
use crate::terminal::csi;
use crate::terminal::dcs;
use crate::terminal::device_attributes;
use crate::terminal::device_status;
use crate::terminal::kitty;
use crate::terminal::kitty::clipboard as kitty_clipboard;
use crate::terminal::kitty::clipboard_command::MetadataError;
use crate::terminal::kitty::clipboard_write::{DataError, InvalidError};
use crate::terminal::modes::{self, Mode};
use crate::terminal::mouse;
use crate::terminal::osc;
use crate::terminal::osc::color::{ColoredTarget, Operation as ColorOperationKind, Request as ColorRequest, Target};
use crate::terminal::osc::semantic_prompt::{options as sp_options, Action as SemanticPromptAction};
use crate::terminal::parser::Dcs;
use crate::terminal::screen_set::Key as ScreenKey;
use crate::terminal::sgr;
use crate::terminal::stream::{self, Action};
use crate::terminal::terminal::{DeccolmMode, Resize as TerminalResize, ScrollViewport, SwitchScreenMode};
use crate::terminal::tmux::ControlNotification;
use crate::terminal::xt_checksum;
use crate::terminal::{build_options, Terminal};
use crate::terminfo;
use crate::termio::message::WriteReqSmall;
use crate::termio::{Mailbox, Message};

use crate::terminal::tmux::viewer::{Action as ViewerAction, Input as ViewerInput, Viewer};

const LOG: &str = "io_handler";

/// The stream type for the handler (Zig: `StreamHandler.Stream`).
pub type Stream = stream::Stream<StreamHandler>;

/// True if we have tmux control mode built in.
pub const TMUX_ENABLED: bool = build_options::TMUX_CONTROL_MODE;

/// The errors from handling a VT action (Zig: the inferred error set of
/// `vtFallible`). They are only logged.
pub type VtError = Box<dyn std::error::Error + Send + Sync>;

/// Errors from the Kitty clipboard protocol handlers (Zig:
/// `error{ OutOfMemory, WriteFailed }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyClipboardError {
    OutOfMemory,
    WriteFailed,
}

impl std::fmt::Display for KittyClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for KittyClipboardError {}

impl From<io::Error> for KittyClipboardError {
    fn from(_: io::Error) -> Self {
        KittyClipboardError::WriteFailed
    }
}

/// This is used as the handler for the terminal.Stream type. This is
/// stateful and is expected to live for the entire lifetime of the terminal.
/// It is NOT VALID to stop a stream handler, create a new one, and use that
/// unless all of the member fields are copied.
pub struct StreamHandler {
    pub size: *const renderer::Size,
    pub terminal: *mut Terminal,

    /// Mailbox for data to the termio thread.
    pub termio_mailbox: *const Mailbox,

    /// Mailbox for the surface.
    pub surface_mailbox: apprt::surface::Mailbox,

    /// The shared render state
    pub renderer_state: *mut renderer::State,

    /// The mailbox for notifying the renderer of things.
    pub renderer_mailbox: *const renderer::thread::Mailbox,

    /// A handle to wake up the renderer. This hints to the renderer that
    /// a repaint should happen.
    pub renderer_wakeup: xev::Async,

    /// The response to use for ENQ requests. (Zig: memory owned by whoever
    /// owns StreamHandler; here an owned copy.)
    pub enquiry_response: Vec<u8>,

    /// The color reporting format for OSC requests.
    pub osc_color_report_format: OSCColorReportFormat,

    /// The clipboard write access configuration.
    pub clipboard_write: ClipboardAccess,

    /// Maximum total decoded bytes per Kitty clipboard protocol
    /// (OSC 5522) write transaction; exceeding it aborts with EFBIG.
    pub clipboard_write_limit: usize,

    /// Whether DECRQCRA may report the checksum of an area of the screen,
    /// and XTCHECKSUM may change how it's calculated.
    pub xt_checksum_report: bool,

    //---------------------------------------------------------------
    // Internal state
    /// The APC command handler maintains the APC state. APC is like
    /// CSI or OSC, but it is a private escape sequence that is used
    /// to send commands to the terminal emulator. This is used by
    /// the kitty graphics protocol.
    pub apc: apc::Handler,

    /// The DCS handler maintains DCS state. DCS is like CSI or OSC,
    /// but requires more stateful parsing. This is used by functionality
    /// such as XTGETTCAP.
    pub dcs: dcs::Handler,

    /// The tmux control mode viewer state.
    pub tmux_viewer: Option<Box<Viewer>>,

    /// Session password grants for the Kitty clipboard protocol.
    /// Requests carrying a granted password skip the permission prompt.
    pub kitty_clipboard_grants: kitty_clipboard::Grants,

    /// The in-flight Kitty clipboard protocol (OSC 5522) write
    /// transaction, if any.
    pub kitty_clipboard_write: Option<Box<kitty_clipboard::WriteState>>,

    /// This is set to true when a message was written to the termio
    /// mailbox. This can be used by callers to determine if they need
    /// to wake up the termio thread.
    pub termio_messaged: bool,

    /// This is set to true when we've seen a title escape sequence. We use
    /// this to determine if we need to default the window title.
    pub seen_title: bool,
}

// SAFETY: the raw pointers reference objects that outlive the handler and
// are only used while holding the renderer state mutex (see module docs).
unsafe impl Send for StreamHandler {}

impl std::fmt::Debug for StreamHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHandler")
            .field("termio_messaged", &self.termio_messaged)
            .field("seen_title", &self.seen_title)
            .finish_non_exhaustive()
    }
}

/// The XTGETTCAP response map (Zig: a comptime map).
fn xtgettcap_map() -> &'static terminfo::source::XtgettcapMap {
    static MAP: OnceLock<terminfo::source::XtgettcapMap> = OnceLock::new();
    MAP.get_or_init(|| terminfo::ghostty::GHOSTTY.xtgettcap_map())
}

/// Encode into a small write request (Zig: a fixed writer over
/// `termio.Message.WriteReq.Small.Array`).
fn write_small(f: impl FnOnce(&mut Cursor<&mut [u8]>) -> io::Result<()>) -> io::Result<WriteReqSmall> {
    let mut small = WriteReqSmall::default();
    let len = {
        let mut writer = Cursor::new(&mut small.data[..]);
        f(&mut writer)?;
        writer.position() as usize
    };
    small.len = len as u8;
    Ok(small)
}

/// Encode into a fixed buffer, returning the written bytes (Zig:
/// `std.Io.Writer.fixed` + `buffered`).
fn write_fixed<'b>(
    buf: &'b mut [u8],
    f: impl FnOnce(&mut Cursor<&mut [u8]>) -> io::Result<()>,
) -> io::Result<&'b [u8]> {
    let len = {
        let mut writer = Cursor::new(&mut buf[..]);
        f(&mut writer)?;
        writer.position() as usize
    };
    Ok(&buf[..len])
}

/// A NUL-terminated copy (Zig: `dupeSentinel`). An interior NUL truncates
/// the value since C consumers would stop there anyway.
fn cstring(bytes: &[u8]) -> CString {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    CString::new(&bytes[..end]).expect("no interior NUL")
}

/// The CPR/DECXCPR report position.
struct ReportPos {
    x: usize,
    y: usize,
}

impl stream::Handler for StreamHandler {
    #[inline]
    fn vt(&mut self, action: Action<'_>) {
        let key = action.key();
        if let Err(err) = self.vt_fallible(action) {
            log::warn!(target: LOG, "error handling VT action action={} err={err}", key.as_str());
        }
    }

    fn deinit(&mut self) {
        StreamHandler::deinit(self);
    }
}

impl StreamHandler {
    pub fn deinit(&mut self) {
        self.apc.deinit();
        self.dcs = dcs::Handler::default();
        self.kitty_clipboard_write_abort();
        self.kitty_clipboard_grants = kitty_clipboard::Grants::default();
        if TMUX_ENABLED {
            self.tmux_viewer = None;
        }
    }

    #[inline(always)]
    fn terminal(&mut self) -> &mut Terminal {
        // SAFETY: the terminal outlives the handler and is only accessed
        // while the renderer state mutex is held (module docs).
        unsafe { &mut *self.terminal }
    }

    #[inline(always)]
    fn mutex(&self) -> &Mutex {
        // SAFETY: the render state and its mutex outlive the handler.
        unsafe { (*self.renderer_state).mutex.as_ref() }
    }

    /// This queues a render operation with the renderer thread. The render
    /// isn't guaranteed to happen immediately but it will happen as soon as
    /// practical.
    #[inline]
    pub fn queue_render(&mut self) -> Result<(), xev::Error> {
        self.renderer_wakeup.notify()
    }

    /// Change the configuration for this handler.
    pub fn change_config(&mut self, config: &crate::termio::DerivedConfig) {
        self.osc_color_report_format = config.osc_color_report_format;
        self.clipboard_write = config.clipboard_write;
        self.clipboard_write_limit = config.clipboard_write_limit;
        self.enquiry_response = config.enquiry_response.clone();
        self.xt_checksum_report = config.xt_checksum_report;
        let t = self.terminal();
        t.set_default_cursor_style(config.cursor_style);
        t.set_default_cursor_blink(config.cursor_blink);
        t.set_default_xt_checksum(config.xt_checksum_extension);

        // The config could have changed any of our colors, but the mode
        // 2031 report is sent by Termio.changeConfig. This runs on the io
        // thread, which is the only reader of the termio mailbox, so
        // sending to it here would deadlock if the mailbox were full.
    }

    /// Push a surface message with an instant timeout, handing it back if
    /// the queue is full.
    ///
    /// NOTE(port): mirrors `apprt.surface.Mailbox.push`; Rust moves the
    /// message into the queue, so the retry needs it handed back (Zig copies).
    fn surface_push_instant(&self, msg: SurfaceMessage) -> Result<(), SurfaceMessage> {
        let mb = self.surface_mailbox;
        // SAFETY: the app mailbox outlives every surface mailbox copy.
        let res = unsafe { &*mb.app.mailbox }.push_or_return(
            AppMessage::SurfaceMessage {
                surface: mb.surface,
                message: msg,
            },
            QueueTimeout::Instant,
        );

        // Wake up our app loop
        // SAFETY: as above.
        unsafe { (*mb.app.rt_app).wakeup() };

        match res {
            Ok(n) if n > 0 => Ok(()),
            Ok(_) => unreachable!("instant push reports a size or hands the value back"),
            Err(AppMessage::SurfaceMessage { message, .. }) => Err(message),
            Err(_) => unreachable!(),
        }
    }

    #[inline]
    fn surface_message_writer(&mut self, msg: SurfaceMessage) {
        // See messageWriter which has similar logic and explains why
        // we may have to do this.
        if let Err(msg) = self.surface_push_instant(msg) {
            let io = TinyIo;
            self.mutex().unlock(io);
            let _ = self.surface_mailbox.push(msg, QueueTimeout::Forever);
            self.mutex().lock_uncancelable(io);
        }
    }

    #[inline]
    fn message_writer(&mut self, msg: Message) {
        // SAFETY: the termio mailbox and the render state mutex outlive the
        // handler.
        unsafe { (*self.termio_mailbox).send(msg, Some(self.mutex())) };
        self.termio_messaged = true;
    }

    /// Send a renderer message and unlock the renderer state mutex
    /// if necessary to ensure we don't deadlock.
    ///
    /// This assumes the renderer state mutex is locked.
    #[allow(dead_code)]
    #[inline]
    fn renderer_message_writer(&mut self, msg: renderer::Message) {
        // See termio.Mailbox.send for more details on how this works.

        // SAFETY: the renderer mailbox outlives the handler.
        let mailbox = unsafe { &*self.renderer_mailbox };

        // Try instant first. If it works then we can return.
        let msg = match mailbox.push_or_return(msg, QueueTimeout::Instant) {
            Ok(n) if n > 0 => return,
            Ok(_) => return,
            Err(msg) => msg,
        };

        // Instant would have blocked. Release the renderer mutex,
        // wake up the renderer to allow it to process the message,
        // and then try again.
        let io = TinyIo;
        self.mutex().unlock(io);
        if let Err(err) = self.renderer_wakeup.notify() {
            // This is an EXTREMELY unlikely case. We still don't return
            // and attempt to send the message because its most likely
            // that everything is fine, but log in case a freeze happens.
            log::warn!(target: LOG, "failed to notify renderer, may deadlock err={err:?}");
        }
        let _ = mailbox.push(msg, QueueTimeout::Forever);
        self.mutex().lock_uncancelable(io);
    }

    #[inline]
    fn vt_fallible(&mut self, action: Action<'_>) -> Result<(), VtError> {
        // The branch hints here are based on real world data
        // which indicates that the most common actions are:
        //
        // 1. print
        // 2. set_attribute
        // 3. carriage_return
        // 4. line_feed
        // 5. cursor_pos
        //
        // Together, these 5 actions make up nearly 98% of
        // all actions encountered in real world scenarios.
        //
        // ref: https://github.com/qwerasd205/asciinema-stats
        match action {
            Action::Print(v) => self.terminal().print(v.cp)?,
            Action::PrintSlice(v) => self.terminal().print_slice(v.cps)?,
            Action::PrintRepeat(v) => self.terminal().print_repeat(v)?,
            Action::Bell => self.bell(),
            Action::Backspace => self.terminal().backspace(),
            Action::HorizontalTab(v) => self.horizontal_tab(v),
            Action::HorizontalTabBack(v) => self.horizontal_tab_back(v),
            Action::Linefeed => self.linefeed()?,
            Action::CarriageReturn => self.terminal().carriage_return(),
            Action::Enquiry => self.enquiry(),
            Action::InvokeCharset(v) => self.terminal().invoke_charset(v.bank, v.charset, v.locking),
            Action::CursorUp(v) => self.terminal().cursor_up(v.value as usize),
            Action::CursorDown(v) => self.terminal().cursor_down(v.value as usize),
            Action::CursorLeft(v) => self.terminal().cursor_left(v.value as usize),
            Action::CursorRight(v) => self.terminal().cursor_right(v.value as usize),
            Action::CursorPos(v) => self.terminal().set_cursor_pos(v.row as usize, v.col as usize),
            Action::CursorCol(v) => self.terminal().set_cursor_col(v.value as usize),
            Action::CursorRow(v) => self.terminal().set_cursor_row(v.value as usize),
            Action::CursorColRelative(v) => self.terminal().cursor_col_relative(v.value as usize),
            Action::CursorRowRelative(v) => self.terminal().cursor_row_relative(v.value as usize),
            Action::CursorStyle(v) => self.terminal().set_cursor_style(v),
            Action::EraseDisplayBelow(v) => self.terminal().erase_display(csi::EraseDisplay::Below, v),
            Action::EraseDisplayAbove(v) => self.terminal().erase_display(csi::EraseDisplay::Above, v),
            Action::EraseDisplayComplete(v) => {
                let t = self.terminal();
                t.scroll_viewport(ScrollViewport::Bottom);
                t.erase_display(csi::EraseDisplay::Complete, v);
            }
            Action::EraseDisplayScrollback(v) => {
                self.terminal().erase_display(csi::EraseDisplay::Scrollback, v)
            }
            Action::EraseDisplayScrollComplete(v) => {
                self.terminal().erase_display(csi::EraseDisplay::ScrollComplete, v)
            }
            Action::EraseLineRight(v) => self.terminal().erase_line(csi::EraseLine::RIGHT, v),
            Action::EraseLineLeft(v) => self.terminal().erase_line(csi::EraseLine::LEFT, v),
            Action::EraseLineComplete(v) => self.terminal().erase_line(csi::EraseLine::COMPLETE, v),
            Action::EraseLineRightUnlessPendingWrap(v) => {
                self.terminal().erase_line(csi::EraseLine::RIGHT_UNLESS_PENDING_WRAP, v)
            }
            Action::DeleteChars(v) => self.terminal().delete_chars(v),
            Action::EraseChars(v) => self.terminal().erase_chars(v),
            Action::InsertLines(v) => self.terminal().insert_lines(v),
            Action::InsertBlanks(v) => self.terminal().insert_blanks(v),
            Action::InsertColumns(v) => self.terminal().insert_columns(v),
            Action::DeleteColumns(v) => self.terminal().delete_columns(v),
            Action::DeleteLines(v) => self.terminal().delete_lines(v),
            Action::ScrollUp(v) => self.terminal().scroll_up(v)?,
            Action::ScrollDown(v) => self.terminal().scroll_down(v),
            Action::TabClearCurrent => self.terminal().tab_clear(csi::TabClear::CURRENT),
            Action::TabClearAll => self.terminal().tab_clear(csi::TabClear::ALL),
            Action::TabSet => self.terminal().tab_set(),
            Action::TabReset => self.terminal().tab_reset(),
            Action::Index => self.index()?,
            Action::NextLine => self.next_line()?,
            Action::ReverseIndex => self.reverse_index(),
            Action::BackIndex => self.terminal().back_index(),
            Action::ForwardIndex => self.terminal().forward_index(),
            Action::FullReset => self.full_reset(),
            Action::SoftReset => self.terminal().soft_reset(),
            Action::SetMode(v) => self.set_mode(v.mode, true)?,
            Action::ResetMode(v) => self.set_mode(v.mode, false)?,
            Action::SaveMode(v) => self.terminal().modes.save(v.mode),
            Action::RestoreMode(v) => {
                // For restore mode we have to restore but if we set it, we
                // always have to call setMode because setting some modes have
                // side effects and we want to make sure we process those.
                let enabled = self.terminal().modes.restore(v.mode);
                self.set_mode(v.mode, enabled)?;
            }
            Action::RequestMode(v) => self.request_mode(v.mode),
            Action::RequestModeUnknown(v) => self.request_mode_unknown(v.mode, v.ansi),
            Action::TopAndBottomMargin(v) => self
                .terminal()
                .set_top_and_bottom_margin(v.top_left as usize, v.bottom_right as usize),
            Action::LeftAndRightMargin(v) => self
                .terminal()
                .set_left_and_right_margin(v.top_left as usize, v.bottom_right as usize),
            Action::LeftAndRightMarginAmbiguous => {
                let t = self.terminal();
                if t.modes.get(Mode::EnableLeftAndRightMargin) {
                    t.set_left_and_right_margin(0, 0);
                } else {
                    t.save_cursor();
                }
            }
            Action::SaveCursor => self.save_cursor(),
            Action::RestoreCursor => self.restore_cursor(),
            Action::ModifyKeyFormat(v) => self.set_modify_key_format(v),
            Action::ProtectedModeOff => self.terminal().set_protected_mode(ansi::ProtectedMode::Off),
            Action::ProtectedModeIso => self.terminal().set_protected_mode(ansi::ProtectedMode::Iso),
            Action::ProtectedModeDec => self.terminal().set_protected_mode(ansi::ProtectedMode::Dec),
            Action::MouseShiftCapture(v) => {
                self.terminal().flags.mouse_shift_capture = if v {
                    crate::terminal::terminal::MouseShiftCapture::True
                } else {
                    crate::terminal::terminal::MouseShiftCapture::False
                }
            }
            Action::XtChecksumExtension(v) => {
                if self.xt_checksum_report {
                    self.terminal().flags.xt_checksum = v.flags;
                }
            }
            Action::RequestXtChecksum(v) => self.report_xt_checksum(v),
            Action::SizeReport(v) => self.send_size_report(v),
            Action::ResizeWindow(v) => self.surface_message_writer(SurfaceMessage::ResizeWindow(v)),
            Action::Xtversion => self.report_xtversion()?,
            Action::DeviceAttributes(v) => self.device_attributes(v),
            Action::DeviceStatus(v) => self.device_status_report(v.request)?,
            Action::KittyKeyboardQuery => self.query_kitty_keyboard()?,
            Action::KittyKeyboardPush(v) => {
                log::debug!(target: LOG, "pushing kitty keyboard mode: {:?}", v.flags);
                self.terminal().screens.active.kitty_keyboard.push(v.flags);
            }
            Action::KittyKeyboardPop(v) => {
                log::debug!(target: LOG, "popping kitty keyboard mode n={v}");
                self.terminal().screens.active.kitty_keyboard.pop(v as usize);
            }
            Action::KittyKeyboardSet(v) => {
                log::debug!(target: LOG, "setting kitty keyboard mode: set {:?}", v.flags);
                self.terminal()
                    .screens
                    .active
                    .kitty_keyboard
                    .set(kitty::key::SetMode::Set, v.flags);
            }
            Action::KittyKeyboardSetOr(v) => {
                log::debug!(target: LOG, "setting kitty keyboard mode: or {:?}", v.flags);
                self.terminal()
                    .screens
                    .active
                    .kitty_keyboard
                    .set(kitty::key::SetMode::Or, v.flags);
            }
            Action::KittyKeyboardSetNot(v) => {
                log::debug!(target: LOG, "setting kitty keyboard mode: not {:?}", v.flags);
                self.terminal()
                    .screens
                    .active
                    .kitty_keyboard
                    .set(kitty::key::SetMode::Not, v.flags);
            }
            Action::KittyColorReport(v) => self.kitty_color_report(&v)?,
            Action::ColorOperation(v) => self.color_operation(v.op, &v.requests, v.terminator)?,
            Action::EndHyperlink => self.end_hyperlink(),
            Action::ActiveStatusDisplay(v) => self.terminal().status_display = v,
            Action::Decaln => self.decaln()?,
            Action::WindowTitle(v) => self.window_title(v.title),
            Action::ReportPwd(v) => self.report_pwd(v.url)?,
            Action::ShowDesktopNotification(v) => self.show_desktop_notification(v.title, v.body),
            Action::ProgressReport(v) => self.progress_report(v),
            Action::StartHyperlink(v) => self.start_hyperlink(v.uri, v.id)?,
            Action::ClipboardContents(v) => self.clipboard_contents(v.kind, v.data),
            Action::SemanticPrompt(v) => self.semantic_prompt(&v)?,
            Action::MouseShape(v) => self.set_mouse_shape(v),
            Action::MouseShapeReset => {
                let shape = if self.terminal().flags.mouse_event != mouse::Event::None {
                    mouse::Shape::Default
                } else {
                    mouse::Shape::Text
                };
                self.set_mouse_shape(shape);
            }
            Action::ConfigureCharset(v) => self.configure_charset(v.slot, v.charset),
            Action::SetAttribute(v) => match v {
                sgr::Attribute::Unknown(unk) => {
                    // We optimize for the happy path scenario here, since
                    // unknown/invalid SGRs aren't that common in the wild.
                    log::warn!(target: LOG, "unimplemented or unknown SGR attribute: {unk:?}");
                }
                _ => {
                    if let Err(err) = self.terminal().set_attribute(v.clone()) {
                        log::warn!(target: LOG, "error setting attribute {v:?}: {err:?}");
                    }
                }
            },
            Action::DcsHook(v) => self.dcs_hook(v)?,
            Action::DcsPut(v) => self.dcs_put(v)?,
            Action::DcsUnhook => self.dcs_unhook()?,
            Action::ApcStart => self.apc.start(),
            Action::ApcEnd(_) => self.apc_end()?,
            Action::ApcPut(v) => self.apc.feed(v),
            Action::ApcPutSlice(v) => self.apc.feed_slice(v.bytes),
            Action::KittyClipboard(v) => self.kitty_clipboard(v)?,

            // Unimplemented
            Action::TitlePush(_)
            | Action::TitlePop(_)
            | Action::KittyDnd(_)
            | Action::OscUnknown(_)
            | Action::ProgramStatus(_) => {}
        }

        Ok(())
    }

    #[inline]
    pub fn dcs_hook(&mut self, dcs: Dcs<'_>) -> Result<(), VtError> {
        let Some(mut cmd) = self.dcs.hook(dcs) else {
            return Ok(());
        };
        self.dcs_command(&mut cmd)
    }

    #[inline]
    pub fn dcs_put(&mut self, byte: u8) -> Result<(), VtError> {
        // The command may borrow the DCS handler's buffers, so the handler
        // is moved out while the command runs.
        let mut handler = std::mem::take(&mut self.dcs);
        let res = match handler.put(byte) {
            Some(mut cmd) => self.dcs_command(&mut cmd),
            None => Ok(()),
        };
        self.dcs = handler;
        res
    }

    #[inline]
    pub fn dcs_unhook(&mut self) -> Result<(), VtError> {
        let Some(mut cmd) = self.dcs.unhook() else {
            return Ok(());
        };
        self.dcs_command(&mut cmd)
    }

    fn dcs_command(&mut self, cmd: &mut dcs::Command<'_>) -> Result<(), VtError> {
        // log.warn("DCS command: {}", .{cmd});
        match cmd {
            dcs::Command::Tmux(tmux) => 'tmux: {
                // If tmux control mode is disabled at the build level,
                // then this whole block shouldn't be analyzed.
                if !TMUX_ENABLED {
                    break 'tmux;
                }
                log::info!(target: LOG, "tmux control mode event cmd={tmux:?}");

                match tmux {
                    ControlNotification::Enter => {
                        // Setup our viewer state
                        debug_assert!(self.tmux_viewer.is_none());
                        self.tmux_viewer = Some(Box::new(Viewer::init()));
                        break 'tmux;
                    }

                    ControlNotification::Exit => {
                        // Free our viewer state if we have one
                        self.tmux_viewer = None;

                        // And always break since we assert below
                        // that we're not handling an exit command.
                        break 'tmux;
                    }

                    _ => {}
                }

                debug_assert!(!matches!(tmux, ControlNotification::Enter));
                debug_assert!(!matches!(tmux, ControlNotification::Exit));

                let Some(mut viewer) = self.tmux_viewer.take() else {
                    // This can only really happen if we failed to
                    // initialize the viewer on enter.
                    log::info!(
                        target: LOG,
                        "received tmux control mode command without viewer: {tmux:?}"
                    );
                    break 'tmux;
                };

                for action in viewer.next(ViewerInput::Tmux(*tmux)).iter() {
                    log::info!(target: LOG, "tmux viewer action={action}");
                    match *action {
                        ViewerAction::Exit => {
                            // We ignore this because we will fully exit when
                            // our DCS connection ends. We may want to handle
                            // this in the future to notify our GUI we're
                            // disconnected though.
                        }

                        ViewerAction::Command(command) => {
                            debug_assert!(!command.is_empty());
                            debug_assert!(command[command.len() - 1] == b'\n');
                            self.message_writer(Message::write_req(command));
                        }

                        ViewerAction::Windows(_) => {
                            // TODO
                        }
                    }
                }
                self.tmux_viewer = Some(viewer);
            }

            dcs::Command::Xtgettcap(gettcap) => {
                let map = xtgettcap_map();
                while let Some(key) = gettcap.next() {
                    let Some(response) = map.get(key) else {
                        continue;
                    };
                    self.message_writer(Message::WriteStable(response.as_bytes()));
                }
            }

            dcs::Command::Decrqss(decrqss) => {
                let mut response = [0u8; dcs::Decrqss::MAX_RESPONSE_BYTES];
                let t = self.terminal();
                let encoded = decrqss.encode(t, &mut response)?;
                let msg = Message::write_req(encoded);
                self.message_writer(msg);
            }
        }

        Ok(())
    }

    pub fn apc_end(&mut self) -> Result<(), VtError> {
        let Some(result) = self.apc.end() else {
            return Ok(());
        };

        // log.warn("APC command: {}", .{result});
        match result {
            apc::Command::Unknown(_) => {}
            apc::Command::Kitty(kitty_cmd) => {
                if let Some(resp) = self.terminal().kitty_graphics(&kitty_cmd) {
                    let mut buf = [0u8; 1024];
                    let final_ = write_fixed(&mut buf, |w| resp.encode(w))?;
                    if final_.len() > 2 {
                        log::debug!(target: LOG, "kitty graphics response: {final_:x?}");
                        self.message_writer(Message::write_req(final_));
                    }
                }
            }

            apc::Command::Glyph(glyph_req) => {
                let resp = self.terminal().glyph_protocol(&glyph_req);
                match glyph_req {
                    apc::glyph::Request::Register(_) | apc::glyph::Request::Clear(_) => {
                        self.queue_render()?
                    }
                    apc::glyph::Request::Support | apc::glyph::Request::Query(_) => {}
                }

                if let Some(r) = resp {
                    let mut buf = [0u8; apc::glyph::Response::MAX_WIRE_BYTES];
                    let final_ = write_fixed(&mut buf, |w| r.format_wire(w))?;
                    log::debug!(target: LOG, "glyph protocol response: {final_:x?}");
                    self.message_writer(Message::write_req(final_));
                }
            }
        }

        Ok(())
    }

    #[inline]
    fn bell(&mut self) {
        self.surface_message_writer(SurfaceMessage::RingBell);
    }

    #[inline]
    fn horizontal_tab(&mut self, count: u16) {
        let t = self.terminal();
        for _ in 0..count {
            let x = t.screens.active.cursor.x;
            t.horizontal_tab();
            if x == t.screens.active.cursor.x {
                break;
            }
        }
    }

    #[inline]
    fn horizontal_tab_back(&mut self, count: u16) {
        let t = self.terminal();
        for _ in 0..count {
            let x = t.screens.active.cursor.x;
            t.horizontal_tab_back();
            if x == t.screens.active.cursor.x {
                break;
            }
        }
    }

    #[inline]
    fn linefeed(&mut self) -> Result<(), VtError> {
        // Small optimization: call index instead of linefeed because they're
        // identical and this avoids one layer of function call overhead.
        self.terminal().index()?;
        Ok(())
    }

    #[inline]
    pub fn reverse_index(&mut self) {
        self.terminal().reverse_index();
    }

    #[inline]
    pub fn index(&mut self) -> Result<(), VtError> {
        self.terminal().index()?;
        Ok(())
    }

    #[inline]
    pub fn next_line(&mut self) -> Result<(), VtError> {
        let t = self.terminal();
        t.index()?;
        t.carriage_return();
        Ok(())
    }

    pub fn set_modify_key_format(&mut self, format: ansi::ModifyKeyFormat) {
        let t = self.terminal();
        t.flags.modify_other_keys_2 = false;
        if format == ansi::ModifyKeyFormat::OtherKeysNumeric {
            t.flags.modify_other_keys_2 = true;
        }
    }

    fn request_mode(&mut self, mode: Mode) {
        let report = self.terminal().modes.get_report(modes::ReportTag::from_mode(mode));
        self.send_mode_report(report);
    }

    fn request_mode_unknown(&mut self, mode_raw: u16, ansi: bool) {
        let report = self.terminal().modes.get_report(modes::ReportTag {
            value: mode_raw,
            ansi,
        });
        self.send_mode_report(report);
    }

    fn report_xt_checksum(&mut self, req: xt_checksum::Request) {
        if !self.xt_checksum_report {
            return;
        }
        let sum = self.terminal().rect_xt_checksum(req);
        match write_small(|w| xt_checksum::encode(w, req.id, sum)) {
            Ok(small) => self.message_writer(Message::WriteSmall(small)),
            Err(err) => log::error!(target: LOG, "error encoding checksum report err={err}"),
        }
    }

    fn send_mode_report(&mut self, report: modes::Report) {
        match write_small(|w| report.encode(w)) {
            Ok(small) => self.message_writer(Message::WriteSmall(small)),
            Err(err) => log::error!(target: LOG, "error encoding mode report err={err}"),
        }
    }

    pub fn set_mode(&mut self, mode: Mode, enabled: bool) -> Result<(), VtError> {
        // Note: this function doesn't need to grab the render state or
        // terminal locks because it is only called from process() which
        // grabs the lock.

        // If we are setting cursor blinking, we ignore it if we have
        // a default cursor blink setting set. This is a really weird
        // behavior so this comment will go deep into trying to explain it.
        //
        // There are two ways to set cursor blinks: DECSCUSR (CSI _ q)
        // and DEC mode 12. DECSCUSR is the modern approach and has a
        // way to revert to the "default" (as defined by the terminal)
        // cursor style and blink by doing "CSI 0 q". DEC mode 12 controls
        // blinking and is either on or off and has no way to set a
        // default. DEC mode 12 is also the more antiquated approach.
        //
        // The problem is that if the user specifies a desired default
        // cursor blink with `cursor-style-blink`, the moment a running
        // program uses DEC mode 12, the cursor blink can never be reset
        // to the default without an explicit DECSCUSR. But if a program
        // is using mode 12, it is by definition not using DECSCUSR.
        // This makes for somewhat annoying interactions where a poorly
        // (or legacy) behaved program will stop blinking, and it simply
        // never restarts.
        //
        // To get around this, we have a special case where if the user
        // specifies some explicit default cursor blink desire, we ignore
        // DEC mode 12. We allow DECSCUSR to still set the cursor blink
        // because programs using DECSCUSR usually are well behaved and
        // reset the cursor blink to the default when they exit.
        //
        // To be extra safe, users can also add a manual `CSI 0 q` to
        // their shell config when they render prompts to ensure the
        // cursor is exactly as they request.
        if mode == Mode::CursorBlinking && self.terminal().cursor.default_blink.is_some() {
            return Ok(());
        }

        // We first always set the raw mode on our mode state.
        self.terminal().modes.set(mode, enabled);

        // And then some modes require additional processing.
        match mode {
            // Just noting here that autorepeat has no effect on
            // the terminal. xterm ignores this mode and so do we.
            // We know about just so that we don't log that it is
            // an unknown mode.
            Mode::Autorepeat => {}

            // Schedule a render since we changed colors
            Mode::ReverseColors => self.terminal().flags.dirty.reverse_colors = true,

            // Origin resets cursor pos. This is called whether or not
            // we're enabling or disabling origin mode and whether or
            // not the value changed.
            Mode::Origin => self.terminal().set_cursor_pos(1, 1),

            Mode::EnableLeftAndRightMargin => {
                if !enabled {
                    // When we disable left/right margin mode we need to
                    // reset the left/right margins.
                    let t = self.terminal();
                    t.scrolling_region.left = 0;
                    t.scrolling_region.right = t.cols - 1;
                }
            }

            Mode::AltScreenLegacy => {
                self.terminal().switch_screen_mode(SwitchScreenMode::Mode47, enabled)?;
            }

            Mode::AltScreen => {
                self.terminal().switch_screen_mode(SwitchScreenMode::Mode1047, enabled)?;
            }

            Mode::AltScreenSaveCursorClearEnter => {
                self.terminal().switch_screen_mode(SwitchScreenMode::Mode1049, enabled)?;
            }

            // Mode 1048 is xterm's conditional save cursor depending
            // on if alt screen is enabled or not (at the terminal emulator
            // level). Alt screen is always enabled for us so this just
            // does a save/restore cursor.
            Mode::SaveCursor => {
                if enabled {
                    self.terminal().save_cursor();
                } else {
                    self.terminal().restore_cursor();
                }
            }

            // Force resize back to the window size
            Mode::EnableMode3 => {
                // SAFETY: the size lives in the termio that owns this handler.
                let grid_size = unsafe { (*self.size).grid() };
                if let Err(err) = self
                    .terminal()
                    .resize(TerminalResize::new(grid_size.columns, grid_size.rows))
                {
                    log::error!(target: LOG, "error updating terminal size: {err}");
                }
            }

            Mode::Column132 => self.terminal().deccolm(if enabled {
                DeccolmMode::Cols132
            } else {
                DeccolmMode::Cols80
            })?,

            // We need to start a timer to prevent the emulator being hung
            // forever.
            Mode::SynchronizedOutput => {
                if enabled {
                    self.message_writer(Message::StartSynchronizedOutput);
                }
            }

            Mode::Linefeed => {
                self.message_writer(Message::LinefeedMode(enabled));
            }

            Mode::InBandSizeReports => {
                if enabled {
                    self.message_writer(Message::SizeReport(
                        crate::termio::message::SizeReport::Mode2048,
                    ));
                }
            }

            Mode::ReportVisibility => {
                if enabled {
                    let visible = self.terminal().flags.visible;
                    self.message_writer(Message::VisibilityReport {
                        visible,
                        force: true,
                    });
                }
            }

            Mode::FocusEvent => {
                if enabled {
                    let focused = self.terminal().flags.focused;
                    self.message_writer(Message::Focused(focused));
                }
            }

            Mode::MouseEventX10 => self.set_mouse_event(enabled, mouse::Event::X10),
            Mode::MouseEventNormal => self.set_mouse_event(enabled, mouse::Event::Normal),
            Mode::MouseEventButton => self.set_mouse_event(enabled, mouse::Event::Button),
            Mode::MouseEventAny => self.set_mouse_event(enabled, mouse::Event::Any),

            Mode::MouseFormatUtf8 => self.set_mouse_format(enabled, mouse::Format::Utf8),
            Mode::MouseFormatSgr => self.set_mouse_format(enabled, mouse::Format::Sgr),
            Mode::MouseFormatUrxvt => self.set_mouse_format(enabled, mouse::Format::Urxvt),
            Mode::MouseFormatSgrPixels => self.set_mouse_format(enabled, mouse::Format::SgrPixels),

            _ => {}
        }

        Ok(())
    }

    /// The shared body of the `mouse_event_*` cases of [`Self::set_mode`].
    fn set_mouse_event(&mut self, enabled: bool, event: mouse::Event) {
        if enabled {
            self.terminal().flags.mouse_event = event;
            self.set_mouse_shape(mouse::Shape::Default);
        } else {
            self.terminal().flags.mouse_event = mouse::Event::None;
            self.set_mouse_shape(mouse::Shape::Text);
        }
    }

    /// The shared body of the `mouse_format_*` cases of [`Self::set_mode`].
    fn set_mouse_format(&mut self, enabled: bool, format: mouse::Format) {
        self.terminal().flags.mouse_format = if enabled { format } else { mouse::Format::X10 };
    }

    #[inline]
    fn start_hyperlink(&mut self, uri: &[u8], id: Option<&[u8]>) -> Result<(), VtError> {
        self.terminal().screens.active.start_hyperlink(uri, id)?;
        Ok(())
    }

    #[inline]
    pub fn end_hyperlink(&mut self) {
        self.terminal().screens.active.end_hyperlink();
    }

    pub fn device_attributes(&mut self, req: device_attributes::Req) {
        // For the below, we quack as a VT220. We don't quack as
        // a 420 because we don't support DCS sequences.
        match req {
            device_attributes::Req::Primary => {
                // 62 = Level 2 conformance
                // 22 = Color text
                // 52 = Clipboard access
                let resp: &'static [u8] = if self.clipboard_write != ClipboardAccess::Deny {
                    b"\x1B[?62;22;52c"
                } else {
                    b"\x1B[?62;22c"
                };
                self.message_writer(Message::WriteStable(resp));
            }

            device_attributes::Req::Secondary => {
                self.message_writer(Message::WriteStable(b"\x1B[>1;10;0c"));
            }

            _ => log::warn!(target: LOG, "unimplemented device attributes req: {req:?}"),
        }
    }

    /// The cursor position for a CPR/DECXCPR report, honoring origin mode
    /// (relative to the scrolling region when DECOM is set).
    fn cursor_report_pos(&mut self) -> ReportPos {
        let t = self.terminal();
        let cursor = &t.screens.active.cursor;
        if t.modes.get(Mode::Origin) {
            ReportPos {
                x: (cursor.x as usize).saturating_sub(t.scrolling_region.left as usize),
                y: (cursor.y as usize).saturating_sub(t.scrolling_region.top as usize),
            }
        } else {
            ReportPos {
                x: cursor.x as usize,
                y: cursor.y as usize,
            }
        }
    }

    pub fn device_status_report(&mut self, req: device_status::Request) -> Result<(), VtError> {
        match req {
            device_status::Request::OperatingStatus => {
                self.message_writer(Message::WriteStable(b"\x1B[0n"))
            }

            device_status::Request::CursorPosition => {
                let pos = self.cursor_report_pos();

                // Response always is at least 4 chars, so this leaves the
                // remainder for the row/column as base-10 numbers. This
                // will support a very large terminal.
                let small = write_small(|w| write!(w, "\x1B[{};{}R", pos.y + 1, pos.x + 1))?;
                self.message_writer(Message::WriteSmall(small));
            }

            // DECXCPR: extended cursor position adds a page number. Ghostty has
            // no page memory, so the page is always 1.
            device_status::Request::CursorPositionExtended => {
                let pos = self.cursor_report_pos();
                let small = write_small(|w| write!(w, "\x1B[?{};{};1R", pos.y + 1, pos.x + 1))?;
                self.message_writer(Message::WriteSmall(small));
            }

            device_status::Request::ColorScheme => {
                self.message_writer(Message::ColorSchemeReport { force: true })
            }

            device_status::Request::Visibility => {
                let visible = self.terminal().flags.visible;
                self.message_writer(Message::VisibilityReport {
                    visible,
                    force: true,
                });
            }
        }

        Ok(())
    }

    #[inline]
    pub fn decaln(&mut self) -> Result<(), VtError> {
        self.terminal().decaln()?;
        Ok(())
    }

    #[inline]
    pub fn save_cursor(&mut self) {
        self.terminal().save_cursor();
    }

    #[inline]
    pub fn restore_cursor(&mut self) {
        self.terminal().restore_cursor();
    }

    pub fn enquiry(&mut self) {
        log::debug!(
            target: LOG,
            "sending enquiry response={}",
            String::from_utf8_lossy(&self.enquiry_response)
        );
        let msg = Message::write_req(&self.enquiry_response);
        self.message_writer(msg);
    }

    fn configure_charset(&mut self, slot: charsets::Slots, set: charsets::Charset) {
        self.terminal().configure_charset(slot, set);
    }

    pub fn full_reset(&mut self) {
        self.terminal().full_reset();
        self.set_mouse_shape(mouse::Shape::Text);

        // Full reset clears Kitty clipboard session grants.
        self.kitty_clipboard_grants = kitty_clipboard::Grants::default();

        // Reset resets our palette so we report it for mode 2031.
        self.message_writer(Message::ColorSchemeReport { force: false });

        // Clear the progress bar
        self.progress_report(osc::ProgressReport {
            state: osc::ProgressReportState::Remove,
            progress: None,
        });
    }

    /// Record a Kitty clipboard protocol session grant so future
    /// requests with this password skip the permission prompt.
    pub fn kitty_clipboard_grant(&mut self, pw: &[u8], dir: crate::terminal::kitty::clipboard_grants::Direction) {
        self.kitty_clipboard_grants.grant(pw, dir, false);
    }

    pub fn query_kitty_keyboard(&mut self) -> Result<(), VtError> {
        log::debug!(target: LOG, "querying kitty keyboard mode");
        let current = self.terminal().screens.active.kitty_keyboard.current().int();
        let small = write_small(|w| write!(w, "\x1b[?{current}u"))?;
        self.message_writer(Message::WriteSmall(small));
        Ok(())
    }

    pub fn report_xtversion(&mut self) -> Result<(), VtError> {
        log::debug!(target: LOG, "reporting XTVERSION: ghostty {}", build_config::VERSION_STRING);
        let mut buf = [0u8; 288];
        let resp = write_fixed(&mut buf, |w| {
            write!(w, "\x1BP>|{} {}\x1B\\", "ghostty", build_config::VERSION_STRING)
        })?;
        let msg = Message::write_req(resp);
        self.message_writer(msg);
        Ok(())
    }

    //-------------------------------------------------------------------------
    // OSC

    fn window_title(&mut self, title: &[u8]) {
        let mut buf = [0u8; 256];
        if title.len() >= buf.len() {
            log::warn!(target: LOG, "change title requested larger than our buffer size, ignoring");
            return;
        }

        // Set the title on the terminal state. We ignore any errors since
        // we can continue to operate just fine without it.
        if let Err(err) = self.terminal().set_title(title) {
            log::warn!(target: LOG, "error setting title in terminal state: {err}");
        }

        buf[..title.len()].copy_from_slice(title);
        buf[title.len()] = 0;

        // Special handling for the empty title. We treat the empty title
        // as resetting to as if we never saw a title. Other terminals
        // behave this way too (e.g. iTerm2).
        if title.is_empty() {
            // If we have a pwd then we set the title as the pwd else
            // we just set it to blank.
            if let Some(pwd) = self.terminal().get_pwd() {
                if pwd.len() < buf.len() {
                    buf[..pwd.len()].copy_from_slice(pwd);
                    buf[pwd.len()] = 0;
                }
            }

            self.surface_message_writer(SurfaceMessage::SetTitle(buf));
            self.seen_title = false;
            return;
        }

        self.seen_title = true;
        self.surface_message_writer(SurfaceMessage::SetTitle(buf));
    }

    #[inline]
    fn set_mouse_shape(&mut self, shape: mouse::Shape) {
        // Avoid changing the shape if it is already set to avoid excess
        // cross-thread messaging.
        if self.terminal().mouse_shape == shape {
            return;
        }

        self.terminal().mouse_shape = shape;
        self.surface_message_writer(SurfaceMessage::SetMouseShape(shape));
    }

    fn clipboard_contents(&mut self, kind: u8, data: &[u8]) {
        // Note: we ignore the "kind" field and always use the standard clipboard.
        // iTerm also appears to do this but other terminals seem to only allow
        // certain. Let's investigate more.

        let clipboard_type = match kind {
            b'c' => apprt::Clipboard::Standard,
            b's' => apprt::Clipboard::Selection,
            b'p' => apprt::Clipboard::Primary,
            _ => apprt::Clipboard::Standard,
        };

        // Get clipboard contents
        if data.len() == 1 && data[0] == b'?' {
            self.surface_message_writer(SurfaceMessage::ClipboardRead(clipboard_type));
            return;
        }

        // Write clipboard contents
        self.surface_message_writer(SurfaceMessage::ClipboardWrite {
            req: apprt::surface::WriteReq::init(data),
            clipboard_type,
        });
    }

    /// Handle one Kitty clipboard protocol (OSC 5522) packet.
    fn kitty_clipboard(&mut self, v: stream::KittyClipboard<'_>) -> Result<(), KittyClipboardError> {
        // Decode and validate the metadata. Malformed structure drops
        // the packet without a response. Invalid decoded text on a write
        // data or alias packet aborts an in-flight transaction.
        let meta = match kitty_clipboard::Metadata::parse(v.metadata) {
            Ok(Some(meta)) => meta,
            Ok(None) => return Ok(()),
            Err(MetadataError::InvalidValue) => {
                if self.kitty_clipboard_write.is_none() {
                    return Ok(());
                }
                match kitty_clipboard::Metadata::operation(v.metadata) {
                    None => return Ok(()),
                    Some(kitty_clipboard::Operation::Wdata | kitty_clipboard::Operation::Walias) => {
                        self.kitty_clipboard_write_finish(kitty_clipboard::Status::EINVAL, v.terminator)?
                    }
                    Some(kitty_clipboard::Operation::Read | kitty_clipboard::Operation::Write) => {}
                }
                return Ok(());
            }
        };

        match meta.op {
            kitty_clipboard::Operation::Read => {
                self.kitty_clipboard_read(&meta, v.payload.unwrap_or(b""), v.terminator)
            }

            kitty_clipboard::Operation::Write => self.kitty_clipboard_write_begin(&meta, v.terminator),

            kitty_clipboard::Operation::Wdata => {
                self.kitty_clipboard_write_data(&meta, v.payload.unwrap_or(b""), v.terminator)
            }

            kitty_clipboard::Operation::Walias => {
                self.kitty_clipboard_write_alias(&meta, v.payload.unwrap_or(b""), v.terminator)
            }
        }
    }

    fn kitty_clipboard_read(
        &mut self,
        meta: &kitty_clipboard::Metadata,
        payload: &[u8],
        terminator: osc::Terminator,
    ) -> Result<(), KittyClipboardError> {
        // The payload is the requested MIME list. A read request with
        // an undecodable payload is dropped without any response,
        // matching kitty.
        let Ok(decoded) = kitty_clipboard::Payload::init(payload) else {
            return Ok(());
        };
        if !decoded.is_valid_utf8() {
            return Ok(());
        }

        // The targets type ('.') asks for the listing of available
        // types rather than data. Requested types beyond the cap are
        // dropped and simply never served, which is how the protocol
        // reports an unavailable type anyway.
        let mut mimes: Vec<CString> = Vec::with_capacity(kitty_clipboard::MAX_READ_MIMES);
        let mut list = false;
        for mime in decoded.mime_iterator() {
            if mime == kitty_clipboard::TARGETS_MIME {
                list = true;
                continue;
            }
            if mimes.len() == kitty_clipboard::MAX_READ_MIMES {
                continue;
            }
            mimes.push(cstring(mime));
        }

        // Per the spec a password without a name is no password. A
        // stored session grant for it lets the surface skip its
        // permission prompt.
        let pw: &[u8] = if !meta.name.is_empty() { &meta.pw } else { b"" };
        let granted = self.kitty_clipboard_read_granted(pw, mimes.len());

        let req = Box::new(KittyRead {
            location: if meta.loc == ClipboardLocation::PRIMARY {
                apprt::Clipboard::Primary
            } else {
                apprt::Clipboard::Standard
            },
            mimes,
            list,
            id: meta.id.clone(),
            pw: pw.to_vec(),
            name: cstring(&meta.name),
            granted,
            terminator,
        });

        self.surface_message_writer(SurfaceMessage::KittyClipboardRead(req));
        Ok(())
    }

    /// Whether a session grant covers a read request, consuming
    /// one-time grants. A prompt-exempt request never consults the
    /// grants: consuming a one-time paste password on a listing would
    /// burn the grant before the follow-up data read.
    fn kitty_clipboard_read_granted(&mut self, pw: &[u8], mimes_len: usize) -> bool {
        if kitty_clipboard::read_prompt_exempt(mimes_len) {
            return false;
        }
        self.kitty_clipboard_grants
            .r#use(pw, crate::terminal::kitty::clipboard_grants::Direction::Read)
    }

    /// Begin a Kitty clipboard write transaction (type=write).
    fn kitty_clipboard_write_begin(
        &mut self,
        meta: &kitty_clipboard::Metadata,
        terminator: osc::Terminator,
    ) -> Result<(), KittyClipboardError> {
        // A new write silently replaces any in-flight transaction.
        self.kitty_clipboard_write_abort();

        // A write denied by policy can never succeed, so fail the
        // transaction up front instead of spooling data we'd only
        // throw away. Later wdata packets are ignored.
        if self.clipboard_write == ClipboardAccess::Deny {
            log::info!(
                target: LOG,
                "application attempted to write clipboard, but 'clipboard-write' is set to deny"
            );
            self.kitty_clipboard_write_status(kitty_clipboard::Status::EPERM, &meta.id, terminator)?;
            return Ok(());
        }

        let state = kitty_clipboard::WriteState::init(
            meta,
            crate::terminal::kitty::clipboard_write::Options {
                max_size: self.clipboard_write_limit,
            },
        );
        self.kitty_clipboard_write = Some(Box::new(state));
        Ok(())
    }

    /// Accumulate one wdata chunk, or commit the transaction when the
    /// chunk carries no MIME type.
    fn kitty_clipboard_write_data(
        &mut self,
        meta: &kitty_clipboard::Metadata,
        payload: &[u8],
        terminator: osc::Terminator,
    ) -> Result<(), KittyClipboardError> {
        // Data without a transaction is silently ignored, matching
        // kitty.
        let Some(state) = self.kitty_clipboard_write.as_mut() else {
            return Ok(());
        };

        // A wdata packet without a MIME type commits the transaction.
        if meta.mime.is_empty() {
            return self.kitty_clipboard_write_commit(terminator);
        }

        match state.data(meta, payload) {
            Ok(()) => Ok(()),

            // Data over the write limit aborts the transaction and is
            // reported to the client.
            Err(DataError::TooLarge) => {
                self.kitty_clipboard_write_finish(kitty_clipboard::Status::EFBIG, terminator)
            }

            // An invalid base64 payload stream aborts the transaction.
            Err(DataError::Invalid) => {
                self.kitty_clipboard_write_finish(kitty_clipboard::Status::EINVAL, terminator)
            }
        }
    }

    /// Register aliases from a walias packet.
    fn kitty_clipboard_write_alias(
        &mut self,
        meta: &kitty_clipboard::Metadata,
        payload: &[u8],
        terminator: osc::Terminator,
    ) -> Result<(), KittyClipboardError> {
        // Aliases without a transaction are silently ignored. Once a
        // transaction exists, a missing target MIME type is invalid and
        // aborts the transaction.
        let Some(state) = self.kitty_clipboard_write.as_mut() else {
            return Ok(());
        };
        if meta.mime.is_empty() {
            return self.kitty_clipboard_write_finish(kitty_clipboard::Status::EINVAL, terminator);
        }

        match state.alias(meta, payload) {
            Ok(()) => Ok(()),

            // An undecodable alias payload aborts the transaction.
            Err(InvalidError::Invalid) => {
                self.kitty_clipboard_write_finish(kitty_clipboard::Status::EINVAL, terminator)
            }
        }
    }

    /// Commit the transaction: resolve the final contents and forward
    /// them to the surface thread, which owns policy, any permission
    /// prompt, the clipboard write itself, and the final reply.
    fn kitty_clipboard_write_commit(&mut self, terminator: osc::Terminator) -> Result<(), KittyClipboardError> {
        let Some(mut state) = self.kitty_clipboard_write.take() else {
            return Ok(());
        };

        match self.kitty_clipboard_write_send(&mut state, terminator) {
            Ok(()) => {}

            // The last MIME type's payload stream was not correctly
            // padded, which aborts the transaction.
            Err(InvalidError::Invalid) => {
                self.kitty_clipboard_write_status(kitty_clipboard::Status::EINVAL, &state.id, terminator)?;
            }
        }

        // The transaction is complete; the surface owns the reply.
        // (The state was taken above, which is the abort.)
        Ok(())
    }

    fn kitty_clipboard_write_send(
        &mut self,
        state: &mut kitty_clipboard::WriteState,
        terminator: osc::Terminator,
    ) -> Result<(), InvalidError> {
        let committed = state.commit()?;

        // Per the spec a password without a name is no password. A
        // stored session grant for it lets the surface skip its
        // permission prompt.
        let pw: &[u8] = if !committed.name.is_empty() { committed.pw } else { b"" };
        let granted = self
            .kitty_clipboard_grants
            .r#use(pw, crate::terminal::kitty::clipboard_grants::Direction::Write);

        let contents: Vec<ClipboardContentBuf> = committed
            .contents
            .iter()
            .map(|src| ClipboardContentBuf {
                mime: cstring(src.mime),
                data: src.data.to_vec(),
            })
            .collect();
        let req = Box::new(KittyWrite {
            location: if committed.loc == ClipboardLocation::PRIMARY {
                apprt::Clipboard::Primary
            } else {
                apprt::Clipboard::Standard
            },
            contents,
            id: committed.id.to_vec(),
            pw: pw.to_vec(),
            name: cstring(committed.name),
            granted,
            terminator,
        });

        self.surface_message_writer(SurfaceMessage::KittyClipboardWrite(req));
        Ok(())
    }

    /// Answer the write transaction with its final status and drop it.
    /// The id echoed is the one from the transaction's opening write
    /// packet, matching kitty.
    fn kitty_clipboard_write_finish(
        &mut self,
        status: kitty_clipboard::Status,
        terminator: osc::Terminator,
    ) -> Result<(), KittyClipboardError> {
        let Some(state) = self.kitty_clipboard_write.take() else {
            return Ok(());
        };
        self.kitty_clipboard_write_status(status, &state.id, terminator)
    }

    /// Reply to a write transaction with a single status packet.
    fn kitty_clipboard_write_status(
        &mut self,
        status: kitty_clipboard::Status,
        id: &[u8],
        terminator: osc::Terminator,
    ) -> Result<(), KittyClipboardError> {
        let mut stream: Vec<u8> = Vec::new();
        kitty_clipboard::Response {
            id,
            terminator,
            ..kitty_clipboard::Response::new(kitty_clipboard::Operation::Write, status)
        }
        .encode(&mut stream)?;
        self.message_writer(Message::WriteAlloc(stream.into_boxed_slice()));
        Ok(())
    }

    /// Drop any in-flight write transaction without responding.
    fn kitty_clipboard_write_abort(&mut self) {
        self.kitty_clipboard_write = None;
    }

    fn semantic_prompt(&mut self, cmd: &stream::SemanticPrompt<'_>) -> Result<(), VtError> {
        match cmd.action {
            SemanticPromptAction::EndInputStartOutput => {
                self.surface_message_writer(SurfaceMessage::StartCommand);
            }

            SemanticPromptAction::EndCommand => {
                // The specification seems to not specify the type but
                // other terminals accept 32-bits, but exit codes are really
                // bytes, so we just do our best here.
                let code: u8 = {
                    let raw: i32 = cmd.read_option::<sp_options::ExitCode>().unwrap_or(0);
                    u8::try_from(raw).unwrap_or(1)
                };

                self.surface_message_writer(SurfaceMessage::StopCommand(Some(code)));
            }

            // Handled by Terminal, no special handling by us
            SemanticPromptAction::EndPromptStartInput
            | SemanticPromptAction::EndPromptStartInputTerminateEol
            | SemanticPromptAction::FreshLine
            | SemanticPromptAction::FreshLineNewPrompt
            | SemanticPromptAction::NewCommand
            | SemanticPromptAction::PromptStart => {}
        }

        // We do this last so failures are still processed correctly
        // above.
        self.terminal().semantic_prompt(cmd)?;
        Ok(())
    }

    fn report_pwd(&mut self, url: &[u8]) -> Result<(), VtError> {
        // Special handling for the empty URL. We treat the empty URL
        // as resetting the pwd as if we never saw a pwd. I can't find any
        // other terminal that does this but it seems like a reasonable
        // behavior that enables some useful features. For example, the macOS
        // proxy icon can be hidden when a program reports it doesn't know
        // the pwd rather than showing a stale pwd.
        if url.is_empty() {
            // Blank value can never fail because no allocs happen.
            self.terminal().set_pwd(b"").expect("blank pwd never allocates");

            // If we haven't seen a title, we're using the pwd as our title.
            // Set it to blank which will reset our title behavior.
            if !self.seen_title {
                self.window_title(b"");
                debug_assert!(!self.seen_title);
            }

            // Report the change.
            self.surface_message_writer(SurfaceMessage::PwdChange(apprt::surface::WriteReq::Stable(b"")));
            return Ok(());
        }

        if cfg!(windows) {
            log::warn!(target: LOG, "reportPwd unimplemented on windows");
            return Ok(());
        }

        // Attempt to parse this file-style URI using options appropriate
        // for this OSC 7 context (e.g. kitty-shell-cwd expects the full,
        // unencoded path).
        let Ok(url_str) = std::str::from_utf8(url) else {
            log::warn!(target: LOG, "invalid url in OSC 7: InvalidFormat");
            return Ok(());
        };
        let uri = match internal_os::uri::parse(
            url_str,
            internal_os::uri::ParseOptions {
                mac_address: !cfg!(target_os = "macos"),
                raw_path: url.starts_with(b"kitty-shell-cwd://"),
            },
        ) {
            Ok(uri) => uri,
            Err(e) => {
                log::warn!(target: LOG, "invalid url in OSC 7: {e:?}");
                return Ok(());
            }
        };

        if uri.scheme != "file" && uri.scheme != "kitty-shell-cwd" {
            log::warn!(
                target: LOG,
                "OSC 7 scheme must be file or kitty-shell-cwd, got: {}",
                uri.scheme
            );
            return Ok(());
        }

        let mut host_buffer = [0u8; HOST_NAME_MAX_LEN];
        let host = match host_name_from_uri(&uri, &mut host_buffer) {
            Ok(host) => host,
            Err(err @ HostNameError::UriMissingHost) => {
                log::warn!(target: LOG, "OSC 7 uri must contain a hostname: {err:?}");
                return Ok(());
            }
            Err(err @ (HostNameError::NameTooLong | HostNameError::InvalidHostName)) => {
                log::warn!(target: LOG, "OSC 7 uri contains an invalid hostname: {err:?}");
                return Ok(());
            }
        };

        // OSC 7 is a little sketchy because anyone can send any value from
        // any host (such an SSH session). The best practice terminals follow
        // is to valid the hostname to be local.
        let host_valid = match internal_os::hostname::is_local(host) {
            Ok(v) => v,
            Err(err) => {
                log::warn!(target: LOG, "failed to get hostname for OSC 7 validation: {err}");
                return Ok(());
            }
        };
        if !host_valid {
            log::warn!(
                target: LOG,
                "OSC 7 host ({}) must be local",
                String::from_utf8_lossy(host)
            );
            return Ok(());
        }

        // We need the raw path, which might require unescaping.
        let path_len = match uri.path {
            internal_os::uri::Component::Raw(v) | internal_os::uri::Component::PercentEncoded(v) => v.len(),
        };
        let mut path_buf = vec![0u8; path_len];
        let path = uri.path.to_raw(&mut path_buf).map_err(|_| "NoSpaceLeft")?.to_vec();

        log::debug!(target: LOG, "terminal pwd: {}", String::from_utf8_lossy(&path));
        self.terminal().set_pwd(&path)?;

        // Report it to the surface. If creating our write request fails
        // then we just ignore it.
        self.surface_message_writer(SurfaceMessage::PwdChange(apprt::surface::WriteReq::init(&path)));

        // If we haven't seen a title, use our pwd as the title.
        if !self.seen_title {
            self.window_title(&path);
            self.seen_title = false;
        }

        Ok(())
    }

    fn color_operation(
        &mut self,
        op: ColorOperationKind,
        requests: &osc::color::List,
        terminator: osc::Terminator,
    ) -> Result<(), VtError> {
        // We'll need op one day if we ever implement reporting special colors.
        let _ = op;

        // return early if there is nothing to do
        if requests.is_empty() {
            return Ok(());
        }

        let mut response: Vec<u8> = Vec::with_capacity(1024);

        for req in requests {
            match *req {
                ColorRequest::Set(set) => {
                    {
                        let t = self.terminal();
                        match set.target {
                            Target::Palette(i) => {
                                t.flags.dirty.palette = true;
                                t.colors.palette.set(i, set.color);
                            }
                            Target::Dynamic(dynamic) => match dynamic {
                                DynamicColor::Foreground => t.colors.foreground.set(set.color),
                                DynamicColor::Background => t.colors.background.set(set.color),
                                DynamicColor::Cursor => t.colors.cursor.set(set.color),
                                DynamicColor::PointerForeground
                                | DynamicColor::PointerBackground
                                | DynamicColor::TektronixForeground
                                | DynamicColor::TektronixBackground
                                | DynamicColor::HighlightBackground
                                | DynamicColor::TektronixCursor
                                | DynamicColor::HighlightForeground => log::info!(
                                    target: LOG,
                                    "setting dynamic color {dynamic:?} not implemented"
                                ),
                            },
                            Target::Special(_) => {
                                log::info!(target: LOG, "setting special colors not implemented")
                            }
                        }
                    }

                    // Notify the surface of the color change
                    self.surface_message_writer(SurfaceMessage::ColorChange(ColoredTarget {
                        target: set.target,
                        color: set.color,
                    }));
                }

                ColorRequest::Reset(target) => match target {
                    Target::Palette(i) => {
                        let t = self.terminal();
                        t.flags.dirty.palette = true;
                        t.colors.palette.reset(i);
                        let color = t.colors.palette.current[i as usize];

                        self.surface_message_writer(SurfaceMessage::ColorChange(ColoredTarget {
                            target,
                            color,
                        }));
                    }
                    Target::Dynamic(dynamic) => {
                        let t = self.terminal();
                        let dyn_rgb = match dynamic {
                            DynamicColor::Foreground => Some(&mut t.colors.foreground),
                            DynamicColor::Background => Some(&mut t.colors.background),
                            DynamicColor::Cursor => Some(&mut t.colors.cursor),
                            DynamicColor::PointerForeground
                            | DynamicColor::PointerBackground
                            | DynamicColor::TektronixForeground
                            | DynamicColor::TektronixBackground
                            | DynamicColor::HighlightBackground
                            | DynamicColor::TektronixCursor
                            | DynamicColor::HighlightForeground => None,
                        };
                        match dyn_rgb {
                            Some(c) => {
                                c.reset();
                                if let Some(color) = c.default {
                                    self.surface_message_writer(SurfaceMessage::ColorChange(
                                        ColoredTarget { target, color },
                                    ));
                                }
                            }
                            None => log::warn!(
                                target: LOG,
                                "resetting dynamic color {dynamic:?} not implemented"
                            ),
                        }
                    }
                    Target::Special(_) => {
                        log::info!(target: LOG, "resetting special colors not implemented")
                    }
                },

                ColorRequest::ResetPalette => {
                    let mask: Vec<usize> = self.terminal().colors.palette.mask.iter().collect();
                    for i in mask {
                        let t = self.terminal();
                        t.flags.dirty.palette = true;
                        t.colors.palette.reset(i as u8);
                        let color = t.colors.palette.current[i];
                        self.surface_message_writer(SurfaceMessage::ColorChange(ColoredTarget {
                            target: Target::Palette(i as u8),
                            color,
                        }));
                    }
                    self.terminal().colors.palette.mask = color::PaletteMask::EMPTY;
                }

                ColorRequest::ResetSpecial => {
                    log::warn!(target: LOG, "resetting all special colors not implemented")
                }

                ColorRequest::Query(kind) => 'report: {
                    if self.osc_color_report_format == OSCColorReportFormat::None {
                        break 'report;
                    }

                    let t = self.terminal();
                    let color: Rgb = match kind {
                        Target::Palette(i) => t.colors.palette.current[i as usize],
                        Target::Dynamic(dynamic) => match dynamic {
                            DynamicColor::Foreground => t.colors.foreground.get().unwrap(),
                            DynamicColor::Background => t.colors.background.get().unwrap(),
                            DynamicColor::Cursor => t
                                .colors
                                .cursor
                                .get()
                                .unwrap_or_else(|| t.colors.foreground.get().unwrap()),
                            DynamicColor::PointerForeground
                            | DynamicColor::PointerBackground
                            | DynamicColor::TektronixForeground
                            | DynamicColor::TektronixBackground
                            | DynamicColor::HighlightBackground
                            | DynamicColor::TektronixCursor
                            | DynamicColor::HighlightForeground => {
                                log::info!(
                                    target: LOG,
                                    "reporting dynamic color {dynamic:?} not implemented"
                                );
                                break 'report;
                            }
                        },
                        Target::Special(_) => {
                            log::info!(target: LOG, "reporting special colors not implemented");
                            break 'report;
                        }
                    };

                    match self.osc_color_report_format {
                        OSCColorReportFormat::Bit16 => match kind {
                            Target::Palette(i) => write!(
                                response,
                                "\x1b]4;{};rgb:{:04x}/{:04x}/{:04x}",
                                i,
                                color.r as u16 * 257,
                                color.g as u16 * 257,
                                color.b as u16 * 257,
                            )?,
                            Target::Dynamic(dynamic) => write!(
                                response,
                                "\x1b]{};rgb:{:04x}/{:04x}/{:04x}",
                                dynamic as u8,
                                color.r as u16 * 257,
                                color.g as u16 * 257,
                                color.b as u16 * 257,
                            )?,
                            Target::Special(_) => unreachable!(),
                        },

                        OSCColorReportFormat::Bit8 => match kind {
                            Target::Palette(i) => write!(
                                response,
                                "\x1b]4;{};rgb:{:02x}/{:02x}/{:02x}",
                                i, color.r, color.g, color.b,
                            )?,
                            Target::Dynamic(dynamic) => write!(
                                response,
                                "\x1b]{};rgb:{:02x}/{:02x}/{:02x}",
                                dynamic as u8, color.r, color.g, color.b,
                            )?,
                            Target::Special(_) => unreachable!(),
                        },

                        OSCColorReportFormat::None => unreachable!(),
                    }

                    // Zig: the response buffer is a 1024 byte fixed buffer.
                    if response.len() + terminator.string().len() > 1024 {
                        return Err("OutOfMemory".into());
                    }
                    response.extend_from_slice(terminator.string());
                }
            }
        }

        if !response.is_empty() {
            // If any of the operations were reports, finalize the report
            // string and send it to the terminal.
            let msg = Message::write_req(&response);
            self.message_writer(msg);
        }

        Ok(())
    }

    fn show_desktop_notification(&mut self, title: &[u8], body: &[u8]) {
        self.surface_message_writer(SurfaceMessage::DesktopNotification(
            apprt::surface::DesktopNotification::init(title, body),
        ));
    }

    /// Send a report to the pty.
    pub fn send_size_report(&mut self, style: csi::SizeReportStyle) {
        use crate::termio::message::SizeReport;
        match style {
            csi::SizeReportStyle::Csi14T => self.message_writer(Message::SizeReport(SizeReport::Csi14T)),
            csi::SizeReportStyle::Csi16T => self.message_writer(Message::SizeReport(SizeReport::Csi16T)),
            csi::SizeReportStyle::Csi18T => self.message_writer(Message::SizeReport(SizeReport::Csi18T)),
            csi::SizeReportStyle::Csi19T => self.message_writer(Message::SizeReport(SizeReport::Csi19T)),
            csi::SizeReportStyle::Csi21T => self.surface_message_writer(SurfaceMessage::ReportTitle(
                apprt::surface::ReportTitleStyle::Csi21T,
            )),
        }
    }

    fn kitty_color_report(&mut self, request: &kitty::color::Osc) -> Result<(), VtError> {
        use kitty::color::{Kind, Request as KittyRequest, Special};

        let mut stream: Vec<u8> = Vec::new();

        for item in &request.list {
            let t = self.terminal();
            match *item {
                KittyRequest::Query(key) => {
                    // If the writer buffer is empty, we need to write our prefix
                    if stream.is_empty() {
                        stream.extend_from_slice(b"\x1b]21");
                    }

                    let color: Option<Rgb> = match key {
                        Kind::Palette(palette) => Some(t.colors.palette.current[palette as usize]),
                        Kind::Special(special) => match special {
                            Special::Foreground => t.colors.foreground.get(),
                            Special::Background => t.colors.background.get(),
                            Special::Cursor => t.colors.cursor.get(),
                            _ => {
                                log::warn!(target: LOG, "ignoring unsupported kitty color protocol key: {key}");
                                continue;
                            }
                        },
                    };
                    let Some(color) = color else {
                        write!(stream, ";{key}=")?;
                        continue;
                    };

                    write!(stream, ";{key}=rgb:{:02x}/{:02x}/{:02x}", color.r, color.g, color.b)?;
                }
                KittyRequest::Set { key, color } => match key {
                    Kind::Palette(palette) => {
                        t.flags.dirty.palette = true;
                        t.colors.palette.set(palette, color);
                    }

                    Kind::Special(special) => match special {
                        Special::Foreground => t.colors.foreground.set(color),
                        Special::Background => t.colors.background.set(color),
                        Special::Cursor => t.colors.cursor.set(color),
                        _ => {
                            log::warn!(target: LOG, "ignoring unsupported kitty color protocol key: {key}");
                            continue;
                        }
                    },
                },
                KittyRequest::Reset(key) => match key {
                    Kind::Palette(palette) => {
                        t.flags.dirty.palette = true;
                        t.colors.palette.reset(palette);
                    }

                    Kind::Special(special) => match special {
                        Special::Foreground => t.colors.foreground.reset(),
                        Special::Background => t.colors.background.reset(),
                        Special::Cursor => t.colors.cursor.reset(),
                        _ => {
                            log::warn!(target: LOG, "ignoring unsupported kitty color protocol key: {key}");
                            continue;
                        }
                    },
                },
            }
        }

        // If we had any writes to our buffer, we queue them now
        if !stream.is_empty() {
            stream.extend_from_slice(request.terminator.string());
            self.message_writer(Message::WriteAlloc(stream.into_boxed_slice()));
        }

        // Note: we don't have to do a queueRender here because every
        // processed stream will queue a render once it is done processing
        // the read() syscall.
        Ok(())
    }

    /// Display a GUI progress report.
    fn progress_report(&mut self, report: osc::ProgressReport) {
        self.surface_message_writer(SurfaceMessage::ProgressReport(report));
    }
}

/// `std.Io.net.HostName.max_len`.
const HOST_NAME_MAX_LEN: usize = 254;

use crate::cli::ssh_cache::disk_cache::{host_name_validate, HostNameValidateError};

/// The errors of [`host_name_from_uri`] (Zig: `HostName.fromUri`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostNameError {
    UriMissingHost,
    NameTooLong,
    InvalidHostName,
}

/// Port of `std.Io.net.HostName.fromUri`: the decoded host of `uri`,
/// validated as a host name.
fn host_name_from_uri<'a: 'b, 'b>(
    uri: &internal_os::uri::Uri<'a>,
    buffer: &'b mut [u8; HOST_NAME_MAX_LEN],
) -> Result<&'b [u8], HostNameError> {
    let component = uri.host.as_ref().ok_or(HostNameError::UriMissingHost)?;
    let bytes = component
        .to_raw(&mut buffer[..])
        .map_err(|_| HostNameError::NameTooLong)?;

    host_name_validate(bytes).map_err(|err| match err {
        HostNameValidateError::NameTooLong => HostNameError::NameTooLong,
        HostNameValidateError::InvalidHostName => HostNameError::InvalidHostName,
    })?;

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handler with only the fields the tests use set (Zig: `undefined`
    /// for everything else).
    fn test_handler() -> StreamHandler {
        StreamHandler {
            size: std::ptr::null(),
            terminal: std::ptr::null_mut(),
            termio_mailbox: std::ptr::null(),
            surface_mailbox: apprt::surface::Mailbox {
                surface: std::ptr::null_mut(),
                app: crate::app::Mailbox {
                    rt_app: std::ptr::null(),
                    mailbox: std::ptr::null(),
                },
            },
            renderer_state: std::ptr::null_mut(),
            renderer_mailbox: std::ptr::null(),
            renderer_wakeup: xev::Async::init().unwrap(),
            enquiry_response: Vec::new(),
            osc_color_report_format: OSCColorReportFormat::Bit16,
            clipboard_write: ClipboardAccess::Allow,
            clipboard_write_limit: kitty_clipboard::MAX_WRITE_SIZE,
            xt_checksum_report: false,
            apc: apc::Handler::default(),
            dcs: dcs::Handler::default(),
            tmux_viewer: None,
            kitty_clipboard_grants: kitty_clipboard::Grants::default(),
            kitty_clipboard_write: None,
            termio_messaged: false,
            seen_title: false,
        }
    }

    #[test]
    fn kitty_clipboard_read_targets_only_never_consumes_a_one_time_grant() {
        let mut handler = test_handler();
        handler
            .kitty_clipboard_grants
            .grant(b"otp", crate::terminal::kitty::clipboard_grants::Direction::Read, true);

        // A listing request must not burn the one-time paste password...
        assert!(!handler.kitty_clipboard_read_granted(b"otp", 0));
        // ...so the follow-up data read is still granted, exactly once.
        assert!(handler.kitty_clipboard_read_granted(b"otp", 1));
        assert!(!handler.kitty_clipboard_read_granted(b"otp", 1));
        handler.renderer_wakeup.deinit();
    }

    #[test]
    fn kitty_clipboard_write_oversized_text_replies_efbig() {
        let mut mailbox = Mailbox::init_spsc().unwrap();

        let mut mutex = Mutex::new();
        mutex.lock_uncancelable(TinyIo);

        let mut renderer_state = renderer::State::new(
            std::ptr::NonNull::from(&mut mutex),
            std::ptr::NonNull::dangling(),
        );
        let mut handler = test_handler();
        handler.termio_mailbox = &mailbox;
        handler.renderer_state = &mut renderer_state;
        handler.clipboard_write = ClipboardAccess::Allow;
        handler.clipboard_write_limit = 4;
        handler.kitty_clipboard_write = None;

        let begin = kitty_clipboard::Metadata {
            id: b"macos".to_vec(),
            ..kitty_clipboard::Metadata::new(kitty_clipboard::Operation::Write)
        };
        handler
            .kitty_clipboard_write_begin(&begin, osc::Terminator::St)
            .unwrap();
        let state = handler.kitty_clipboard_write.as_mut().unwrap();
        let wdata = kitty_clipboard::Metadata {
            mime: b"text/plain".to_vec(),
            ..kitty_clipboard::Metadata::new(kitty_clipboard::Operation::Wdata)
        };
        state.data(&wdata, b"SGVsbA==").unwrap(); // "Hell"
        assert_eq!(Err(DataError::TooLarge), state.data(&wdata, b"bw==")); // "o"
        handler
            .kitty_clipboard_write_finish(kitty_clipboard::Status::EFBIG, osc::Terminator::St)
            .unwrap();
        assert!(handler.kitty_clipboard_write.is_none());

        let Mailbox::Spsc { queue, .. } = &mailbox;
        let response = queue.pop();
        assert!(response.is_some());
        let msg = response.unwrap();
        match &msg {
            Message::WriteAlloc(v) => assert_eq!(
                &b"\x1B]5522;type=write:status=EFBIG:id=macos\x1B\\"[..],
                &v[..]
            ),
            other => panic!("unexpected message {other:?}"),
        }
        msg.deinit();

        // Teardown leaves no transaction that could be committed and
        // forwarded to the macOS clipboard path.
        assert!(queue.pop().is_none());

        handler.kitty_clipboard_write_abort();
        handler.renderer_wakeup.deinit();
        mutex.unlock(TinyIo);
        mailbox.deinit();
    }
}
