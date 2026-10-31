//! Port of `src/terminal/c/terminal.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - The wrapper owns the native terminal through a raw pointer
//!   (`Box::into_raw`) and the persistent stream's handler holds the only
//!   `&'static mut` to it (Zig: both hold `*Terminal`). Every access to
//!   the native terminal goes through the handler
//!   ([`TerminalWrapper::terminal`]/[`TerminalWrapper::terminal_mut`]).
//! - Effect trampolines recover the wrapper from the handler with an
//!   offset computation, like Zig's `@fieldParentPtr`.
//! - Enum arguments (`option`, `data`, `mode`, point tags) cross the ABI as
//!   `c_int` and are validated (Zig checks them with `std.enums.fromInt`
//!   in safe builds). Callback options take the function pointer itself as
//!   the value, like Zig.
//! - C enums that mirror core enums with a different representation
//!   (`TerminalScreen`, `ProgressState`, `ProgramStatusState`,
//!   `SemanticPromptKind`, ...) are declared here with `c_api_enum!` and
//!   converted at the boundary.
//! - Allocation failure is not recoverable in Rust; the Zig paths that
//!   report `out_of_memory` for internal allocations keep their result
//!   codes where a Rust error exists (`setTitle`, `select`, ...).
//!   `continuation_alloc_io` returns a `Vec<u8>` (Zig: a slice owned by a
//!   caller-provided allocator).

use std::ffi::{c_int, c_void};
use std::mem::ManuallyDrop;

use super::color_scheme as color_scheme_c;
use super::grid_ref as grid_ref_c;
use super::grid_ref_tracked as grid_ref_tracked_c;
use super::io as c_io;
use super::kitty_graphics as kitty_gfx_c;
use super::result::Result;
use super::search as search_c;
use super::selection as selection_c;
use super::style as style_c;
use super::types::HandleAllocator;
use crate::lib::allocator::{self as lib_alloc, Allocator as CAllocator, ZigAllocator};
use crate::lib::String as LibString;
use crate::lib::TinyIo;
use crate::terminal::build_options;
use crate::terminal::clipboard;
use crate::terminal::color;
use crate::terminal::cursor::Style as CursorStyle;
use crate::terminal::device_attributes;
use crate::terminal::device_status;
use crate::terminal::kitty::clipboard as kitty_clipboard;
use crate::terminal::kitty::graphics_image::TemporaryFile;
use crate::terminal::mem as terminal_mem;
use crate::terminal::modes::{self, Mode, ModeTag, ModeTagBacking};
use crate::terminal::mouse;
use crate::terminal::osc;
use crate::terminal::osc::program_status;
use crate::terminal::osc::semantic_prompt as osc_sp;
use crate::terminal::page_list::{ScrollbarC, PageList};
use crate::terminal::point;
use crate::terminal::screen_set::Key as ScreenKey;
use crate::terminal::size::CellCountInt;
use crate::terminal::size_report;
use crate::terminal::snapshot::snapshot as snapshot_core;
use crate::terminal::stream::{self, Boundary as StreamBoundary, BoundarySet, WriteContinuationError};
use crate::terminal::stream_continuation::Tracker;
use crate::terminal::stream_terminal::{self, Handler};
use crate::terminal::terminal::{
    self as zig_terminal, CellSizePx, Resize, ResizeError, ScrollViewport as ZigScrollViewport,
    ScrollViewportC, ScrollViewportTag, Terminal as ZigTerminal,
};

const LOG_TARGET: &str = "terminal_c";

/// The persistent stream type of a C terminal.
type Stream = stream_terminal::Stream<'static>;

/// `std.Io.Dir.max_path_bytes` (Linux `PATH_MAX`).
const MAX_PATH_BYTES: usize = 4096;

/// C terminals do not retain replay bytes unless the embedding application
/// opts in through `GHOSTTY_TERMINAL_OPT_CONTINUATION_MAX_BYTES`.
pub const DEFAULT_CONTINUATION_MAX_BYTES: usize = 0;

/// Owns the `std.Io` implementation retained by every C terminal.
///
/// Snapshot decoding creates this before the native terminal exists and
/// transfers it into the final C wrapper after READY.
///
/// This is TinyIo on every target: it is stateless and supports exactly
/// the operations the terminal needs at a fraction of the code size of
/// `std.Io.Threaded` (see lib/TinyIo.zig), on POSIX and Windows alike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Io {
    pub impl_: TinyIo,
}

impl Io {
    pub const INIT: Io = Io { impl_: TinyIo::INIT };

    /// Return the value passed to native terminal construction and decoding.
    pub fn io(self) -> TinyIo {
        self.impl_.io()
    }
}

/// Wrapper around ZigTerminal that tracks additional state for C API usage,
/// such as the persistent VT stream needed to handle escape sequences split
/// across multiple vt_write calls.
pub struct TerminalWrapper {
    /// The heap-allocated native terminal (`Box::into_raw`). Only used to
    /// free it; every access goes through `stream.handler.terminal`.
    terminal_ptr: *mut ZigTerminal,
    /// C construction has no I/O argument, so the wrapper retains the owner
    /// created by `new` or transferred from snapshot decoding until `free`.
    pub io: Io,
    /// Owned copy of the temporary directory path for some operations
    /// (e.g. kitty graphics). This is only allocated once the embedder sets
    /// the option.
    tmp_dir_path: std::option::Option<Vec<u8>>,
    /// The terminfo name reported for XTGETTCAP "TN". The stream handler holds
    /// a slice into this.
    terminfo_name_buf: [u8; Handler::MAX_TERMINFO_NAME_BYTES],
    pub stream: ManuallyDrop<Stream>,
    pub effects: Effects,
    pub tracked_grid_refs: Vec<*mut grid_ref_tracked_c::TrackedGridRef>,
    pub searches: Vec<*mut search_c::SearchWrapper>,
    alloc: HandleAllocator,
}

impl TerminalWrapper {
    /// The native terminal.
    pub fn terminal(&self) -> &ZigTerminal {
        self.stream.handler.terminal
    }

    /// The native terminal (mutable).
    pub fn terminal_mut(&mut self) -> &mut ZigTerminal {
        self.stream.handler.terminal
    }

    /// The allocator this terminal was created with (Zig: `t.gpa()`).
    pub fn alloc(&self) -> HandleAllocator {
        self.alloc
    }

    /// Fetches a `TerminalWrapper` pointer from a `Handler`
    /// (Zig: `@fieldParentPtr`).
    fn from_handler(handler: &mut Handler<'_>) -> *mut TerminalWrapper {
        let offset = std::mem::offset_of!(TerminalWrapper, stream) + std::mem::offset_of!(Stream, handler);
        (handler as *mut Handler<'_> as *mut u8).wrapping_sub(offset) as *mut TerminalWrapper
    }
}

impl Drop for TerminalWrapper {
    fn drop(&mut self) {
        self.stream.deinit();
        // SAFETY: the stream (whose handler borrows the terminal) is
        // dropped before the terminal it borrows, exactly once.
        unsafe {
            ManuallyDrop::drop(&mut self.stream);
            drop(Box::from_raw(self.terminal_ptr));
        }
    }
}

/// A single MIME representation in a clipboard write.
///
/// C: GhosttyClipboardContent
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ClipboardContent {
    pub mime: LibString,
    pub data: LibString,
}

impl ClipboardContent {
    const EMPTY: ClipboardContent = ClipboardContent {
        mime: LibString {
            ptr: std::ptr::null(),
            len: 0,
        },
        data: LibString {
            ptr: std::ptr::null(),
            len: 0,
        },
    };
}

/// A protocol-neutral request to replace or clear clipboard contents.
/// The embedder answers by calling `reply` with the request before the
/// callback returns.
///
/// C: GhosttyClipboardWrite
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ClipboardWrite {
    pub size: usize,
    pub location: clipboard::Location,
    pub contents: *const ClipboardContent,
    pub contents_len: usize,
    pub name: LibString,
    pub granted: bool,
    pub can_remember: bool,
    /// Terminal-owned reply state; opaque to the embedder.
    pub ctx: *const c_void,
    pub reply: ClipboardWriteReplyFn,
}

/// The reply to a clipboard write request.
///
/// C: GhosttyClipboardWriteReply
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ClipboardWriteReply {
    pub size: usize,
    pub result: clipboard::WriteStatus,
    pub remember: bool,
}

/// C function pointer type for replying to a clipboard write.
///
/// C: GhosttyClipboardWriteReplyFn
pub type ClipboardWriteReplyFn = unsafe extern "C" fn(*const ClipboardWrite, *const ClipboardWriteReply);

/// The reply to a clipboard read request.
///
/// C: GhosttyClipboardReadReply
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ClipboardReadReply {
    pub size: usize,
    pub result: clipboard::ReadStatus,
    pub contents: *const ClipboardContent,
    pub contents_len: usize,
    pub available: *const LibString,
    pub available_len: usize,
    pub remember: bool,
}

/// A synchronous request to read clipboard contents. The embedder answers
/// by calling `reply` with the request before the callback returns.
///
/// C: GhosttyClipboardRead
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ClipboardRead {
    pub size: usize,
    pub location: clipboard::Location,
    pub mimes: *const LibString,
    pub mimes_len: usize,
    pub list: bool,
    pub name: LibString,
    pub granted: bool,
    pub can_remember: bool,
    /// Terminal-owned reply state; opaque to the embedder.
    pub ctx: *const c_void,
    pub reply: ClipboardReadReplyFn,
}

/// C function pointer type for replying to a clipboard read.
///
/// C: GhosttyClipboardReadReplyFn
pub type ClipboardReadReplyFn = unsafe extern "C" fn(*const ClipboardRead, *const ClipboardReadReply);

/// A request to show a desktop notification.
///
/// C: GhosttyTerminalDesktopNotification
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DesktopNotification {
    pub size: usize,
    pub title: LibString,
    pub body: LibString,
}

crate::c_api_enum! {
    /// C: GhosttyTerminalProgressState
    /// (Zig: `osc.Command.ProgressReport.State`.)
    pub enum ProgressState {
        Remove = 0,
        Set = 1,
        Error = 2,
        Indeterminate = 3,
        Pause = 4,
    }
}

impl ProgressState {
    fn from_zig(s: osc::ProgressReportState) -> ProgressState {
        match s {
            osc::ProgressReportState::Remove => ProgressState::Remove,
            osc::ProgressReportState::Set => ProgressState::Set,
            osc::ProgressReportState::Error => ProgressState::Error,
            osc::ProgressReportState::Indeterminate => ProgressState::Indeterminate,
            osc::ProgressReportState::Pause => ProgressState::Pause,
        }
    }
}

/// A progress report emitted by the running program.
///
/// C: GhosttyTerminalProgressReport
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProgressReport {
    pub size: usize,
    pub state: ProgressState,
    pub progress: i8,
}

crate::c_api_enum! {
    /// C: GhosttyProgramStatusState
    /// (Zig: `osc.Command.ProgramStatus.State`.)
    pub enum ProgramStatusState {
        Idle = 0,
        Working = 1,
        Done = 2,
        Blocked = 3,
        Error = 4,
        Clear = 5,
    }
}

impl ProgramStatusState {
    fn from_zig(s: program_status::State) -> ProgramStatusState {
        match s {
            program_status::State::Idle => ProgramStatusState::Idle,
            program_status::State::Working => ProgramStatusState::Working,
            program_status::State::Done => ProgramStatusState::Done,
            program_status::State::Blocked => ProgramStatusState::Blocked,
            program_status::State::Error => ProgramStatusState::Error,
            program_status::State::Clear => ProgramStatusState::Clear,
        }
    }
}

crate::c_api_enum! {
    /// What a blocked program needs from the user. This matches
    /// `osc.Command.ProgramStatus.Kind` but adds `none` first, because C has
    /// no optional enums and a zeroed value must mean "no kind".
    ///
    /// C: GhosttyProgramStatusKind
    pub enum ProgramStatusKind {
        None = 0,
        Permission = 1,
        Question = 2,
        Auth = 3,
    }
}

/// A program status report (OSC 7501). Absent text is an empty string,
/// an absent kind is `none`, and absent progress is -1. See the header
/// for the meaning of each field.
///
/// C: GhosttyTerminalProgramStatus
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProgramStatus {
    pub size: usize,
    pub state: ProgramStatusState,
    pub kind: ProgramStatusKind,
    pub progress: i8,
    pub id: LibString,
    pub app: LibString,
    pub title: LibString,
    pub message: LibString,
}

crate::c_api_enum! {
    /// C: GhosttySemanticPromptKind
    /// (Zig: `Handler.SemanticPrompt.Kind`.)
    pub enum SemanticPromptKind {
        Invalid = 0,
        PromptStart = 1,
        InputStart = 2,
        OutputStart = 3,
        CommandEnd = 4,
    }
}

impl SemanticPromptKind {
    fn from_zig(k: stream_terminal::SemanticPromptKind) -> SemanticPromptKind {
        use stream_terminal::SemanticPromptKind as K;
        match k {
            K::Invalid => SemanticPromptKind::Invalid,
            K::PromptStart => SemanticPromptKind::PromptStart,
            K::InputStart => SemanticPromptKind::InputStart,
            K::OutputStart => SemanticPromptKind::OutputStart,
            K::CommandEnd => SemanticPromptKind::CommandEnd,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttySemanticPromptPromptKind
    /// (Zig: `Handler.SemanticPrompt.PromptKind`.)
    pub enum SemanticPromptPromptKind {
        Primary = 0,
        Right = 1,
        Continuation = 2,
        Secondary = 3,
    }
}

impl SemanticPromptPromptKind {
    fn from_zig(k: stream_terminal::SemanticPromptPromptKind) -> SemanticPromptPromptKind {
        use stream_terminal::SemanticPromptPromptKind as K;
        match k {
            K::Primary => SemanticPromptPromptKind::Primary,
            K::Right => SemanticPromptPromptKind::Right,
            K::Continuation => SemanticPromptPromptKind::Continuation,
            K::Secondary => SemanticPromptPromptKind::Secondary,
        }
    }
}

/// A shell integration event. See `Handler.SemanticPrompt`.
///
/// C: GhosttyTerminalSemanticPrompt
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SemanticPrompt {
    pub size: usize,
    pub kind: SemanticPromptKind,
    pub prompt_kind: SemanticPromptPromptKind,
    pub has_exit_code: bool,
    pub exit_code: i32,
    pub command: LibString,
    pub error: LibString,
}

/// A request to resize the window's text area to a size in rows and columns.
///
/// C: GhosttyTerminalResizeWindowGrid
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResizeWindowGrid {
    pub rows: u16,
    pub columns: u16,
}

crate::c_api_enum! {
    /// C: GhosttyTerminalResizeWindowTag
    pub enum ResizeWindowTag {
        Grid = 0,
    }
}

/// C: GhosttyTerminalResizeWindowValue
///
/// Padding: the largest variant is 4 bytes. Use [2]u64 (16 bytes)
/// so that other kinds of resize requests can be added without an
/// ABI break.
#[repr(C)]
#[derive(Clone, Copy)]
pub union ResizeWindowValue {
    pub grid: ResizeWindowGrid,
    pub _padding: [u64; 2],
}

/// A request from the running program to resize the window.
///
/// C: GhosttyTerminalResizeWindow (Zig: `ResizeWindow.C`)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ResizeWindow {
    pub tag: ResizeWindowTag,
    pub value: ResizeWindowValue,
}

impl ResizeWindow {
    /// `lib.TaggedUnion.cFieldRename`.
    pub fn c_field_rename(tag: ResizeWindowTag) -> std::option::Option<&'static str> {
        match tag {
            ResizeWindowTag::Grid => Some("grid"),
        }
    }
}

/// A borrowed unsupported string sequence.
///
/// C: GhosttyTerminalUnknownStringSequence
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UnknownStringSequence {
    pub truncated: bool,
    pub content: LibString,
}

/// An OSC sequence whose number is not implemented.
///
/// C: GhosttyTerminalUnknownOscSequence
pub type UnknownOscSequence = osc::UnknownC;

crate::c_api_enum! {
    /// C: GhosttyTerminalUnknownSequenceTag
    pub enum UnknownSequenceTag {
        Apc = 0,
        Osc = 1,
    }
}

/// C: GhosttyTerminalUnknownSequenceValue
///
/// A future borrowed CSI payload may need parameter, separator, and
/// intermediate arrays. Reserve 128 bytes so that representation and
/// other structured sequence types can be added without an ABI break.
#[repr(C)]
#[derive(Clone, Copy)]
pub union UnknownSequenceValue {
    pub apc: UnknownStringSequence,
    pub osc: UnknownOscSequence,
    pub _padding: [u64; 16],
}

/// An unsupported terminal sequence reported to the C callback.
///
/// C: GhosttyTerminalUnknownSequence (Zig: `UnknownSequence.C`)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UnknownSequence {
    pub tag: UnknownSequenceTag,
    pub value: UnknownSequenceValue,
}

impl UnknownSequence {
    /// `lib.TaggedUnion.cFieldRename`.
    pub fn c_field_rename(tag: UnknownSequenceTag) -> std::option::Option<&'static str> {
        match tag {
            UnknownSequenceTag::Apc => Some("apc"),
            UnknownSequenceTag::Osc => Some("osc"),
        }
    }
}

/// A terminal mode and boolean value used for mode configuration.
///
/// C: GhosttyTerminalModeConfig
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeConfig {
    pub mode: ModeTagBacking,
    pub value: bool,
}

impl ModeConfig {
    fn to_mode(self) -> std::option::Option<Mode> {
        let tag = ModeTag::from_bits(self.mode);
        modes::mode_from_int(tag.value, tag.ansi)
    }
}

/// C function pointer type for the write_pty callback.
pub type WritePtyFn = unsafe extern "C" fn(Terminal, *mut c_void, *const u8, usize);

/// C function pointer type for the bell callback.
pub type BellFn = unsafe extern "C" fn(Terminal, *mut c_void);

/// C function pointer type for the color_scheme callback.
/// Returns true and fills out_scheme if a color scheme is available,
/// or returns false to silently ignore the query.
pub type ColorSchemeFn = unsafe extern "C" fn(Terminal, *mut c_void, *mut c_int) -> bool;

/// C function pointer type for the enquiry callback.
/// Returns the response bytes. The memory must remain valid
/// until the callback returns.
pub type EnquiryFn = unsafe extern "C" fn(Terminal, *mut c_void) -> LibString;

/// C function pointer type for the xtversion callback.
/// Returns the version string (e.g. "ghostty 1.2.3"). The memory
/// must remain valid until the callback returns. An empty string
/// (len=0) causes the default "libghostty" to be reported.
pub type XtversionFn = unsafe extern "C" fn(Terminal, *mut c_void) -> LibString;

/// C function pointer type for the clipboard_write callback. The request
/// is borrowed for the callback duration and must be answered through
/// its reply function before the callback returns.
pub type ClipboardWriteFn = unsafe extern "C" fn(Terminal, *mut c_void, *const ClipboardWrite);

/// C function pointer type for the clipboard_read callback. The request
/// is borrowed for the callback duration and must be answered through
/// its reply function before the callback returns.
pub type ClipboardReadFn = unsafe extern "C" fn(Terminal, *mut c_void, *const ClipboardRead);

/// C function pointer type for the desktop_notification callback. The
/// request and its strings are borrowed for the callback duration.
pub type DesktopNotificationFn = unsafe extern "C" fn(Terminal, *mut c_void, *const DesktopNotification);

/// C function pointer type for the title_changed callback.
pub type TitleChangedFn = unsafe extern "C" fn(Terminal, *mut c_void);

/// C function pointer type for the render_hold callback.
pub type RenderHoldFn = unsafe extern "C" fn(Terminal, *mut c_void, bool);

/// C function pointer type for the resize_window callback.
pub type ResizeWindowFn = unsafe extern "C" fn(Terminal, *mut c_void, *const ResizeWindow);

/// C function pointer type for the pwd_changed callback.
pub type PwdChangedFn = unsafe extern "C" fn(Terminal, *mut c_void);

/// C function pointer type for the progress_report callback.
pub type ProgressReportFn = unsafe extern "C" fn(Terminal, *mut c_void, *const ProgressReport);

/// C function pointer type for the program_status callback. The report
/// and its strings are borrowed for the callback duration.
pub type ProgramStatusFn = unsafe extern "C" fn(Terminal, *mut c_void, *const ProgramStatus);

/// C function pointer type for the semantic_prompt callback. The event
/// and its strings are borrowed for the callback duration.
pub type SemanticPromptFn = unsafe extern "C" fn(Terminal, *mut c_void, *const SemanticPrompt);

/// C function pointer type for the reset callback.
pub type ResetFn = unsafe extern "C" fn(Terminal, *mut c_void);

/// C function pointer type for the unknown_sequence callback. The request
/// and its content are borrowed for the callback duration.
pub type UnknownSequenceFn = unsafe extern "C" fn(Terminal, *mut c_void, *const UnknownSequence);

/// C function pointer type for the size callback. Used by XTWINOPS queries
/// and VT-driven mode 2048 enable reports. Returns true and fills out_size
/// if size is available, or false to suppress the XTWINOPS response or
/// mode 2048 report.
pub type SizeFn = unsafe extern "C" fn(Terminal, *mut c_void, *mut size_report::Size) -> bool;

/// C function pointer type for the device_attributes callback.
/// Returns true and fills out_attrs if attributes are available,
/// or returns false to silently ignore the query.
pub type DeviceAttributesFn = unsafe extern "C" fn(Terminal, *mut c_void, *mut DeviceAttributes) -> bool;

/// C-compatible device attributes struct.
/// C: GhosttyDeviceAttributes (Zig: `Effects.CDeviceAttributes`)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceAttributes {
    pub primary: DeviceAttributesPrimary,
    pub secondary: DeviceAttributesSecondary,
    pub tertiary: DeviceAttributesTertiary,
}

/// C: GhosttyDeviceAttributesPrimary
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceAttributesPrimary {
    pub conformance_level: u16,
    pub features: [u16; 64],
    pub num_features: usize,
}

/// C: GhosttyDeviceAttributesSecondary
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceAttributesSecondary {
    pub device_type: u16,
    pub firmware_version: u16,
    pub rom_cartridge: u16,
}

/// C: GhosttyDeviceAttributesTertiary
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceAttributesTertiary {
    pub unit_id: u32,
}

/// C callback state for terminal effects.
///
/// Most trampolines are always installed on the stream handler. They check
/// these fields and do nothing when the C callback is null.
///
/// A few trampolines are only installed while their C callback is set,
/// because the stream handler behaves differently when the Zig effect is
/// null:
///
/// - `unknown_sequence`: unknown sequences are skipped without being
///   captured.
/// - `clipboard_write`: Kitty clipboard writes fail up front instead of
///   collecting data for a write that can never finish.
/// - `clipboard_read`: clipboard reads are denied.
/// - `program_status`: the OSC 7501 support query gets no reply, so
///   programs know not to send reports.
pub struct Effects {
    pub userdata: *mut c_void,
    pub write_pty: std::option::Option<WritePtyFn>,
    pub bell: std::option::Option<BellFn>,
    pub color_scheme: std::option::Option<ColorSchemeFn>,
    pub desktop_notification: std::option::Option<DesktopNotificationFn>,
    pub device_attributes_cb: std::option::Option<DeviceAttributesFn>,
    pub enquiry: std::option::Option<EnquiryFn>,
    pub xtversion: std::option::Option<XtversionFn>,
    pub title_changed: std::option::Option<TitleChangedFn>,
    pub pwd_changed: std::option::Option<PwdChangedFn>,
    pub progress_report: std::option::Option<ProgressReportFn>,
    pub program_status: std::option::Option<ProgramStatusFn>,
    pub semantic_prompt: std::option::Option<SemanticPromptFn>,
    pub reset: std::option::Option<ResetFn>,
    pub size_cb: std::option::Option<SizeFn>,
    pub clipboard_write: std::option::Option<ClipboardWriteFn>,
    pub clipboard_read: std::option::Option<ClipboardReadFn>,
    pub unknown_sequence: std::option::Option<UnknownSequenceFn>,
    pub render_hold: std::option::Option<RenderHoldFn>,
    pub resize_window: std::option::Option<ResizeWindowFn>,

    /// Scratch buffer for DA1 feature codes. The device attributes
    /// trampoline converts C feature codes into this buffer and returns
    /// a slice pointing into it. Storing it here ensures the slice
    /// remains valid after the trampoline returns, since the caller
    /// (`reportDeviceAttributes`) reads it before any re-entrant call.
    da_features_buf: [device_attributes::Feature; 64],
}

impl Default for Effects {
    fn default() -> Self {
        Effects {
            userdata: std::ptr::null_mut(),
            write_pty: None,
            bell: None,
            color_scheme: None,
            desktop_notification: None,
            device_attributes_cb: None,
            enquiry: None,
            xtversion: None,
            title_changed: None,
            pwd_changed: None,
            progress_report: None,
            program_status: None,
            semantic_prompt: None,
            reset: None,
            size_cb: None,
            clipboard_write: None,
            clipboard_read: None,
            unknown_sequence: None,
            render_hold: None,
            resize_window: None,
            da_features_buf: [device_attributes::Feature(0); 64],
        }
    }
}

/// Opaque context behind ClipboardWrite.ctx for the reply trampoline.
struct ClipboardWriteCtx<'a> {
    write: clipboard::Write<'a>,
}

/// Opaque context behind ClipboardRead.ctx for the reply trampoline.
struct ClipboardReadCtx<'a> {
    read: clipboard::Read<'a>,
}

impl Effects {
    /// The wrapper behind `handler` and its C callback state.
    ///
    /// SAFETY (all trampolines): the handler is the one embedded in a live
    /// `TerminalWrapper`; only the `effects` field is read through the
    /// recovered pointer.
    #[inline]
    fn wrapper(handler: &mut Handler<'_>) -> (*mut TerminalWrapper, &'static Effects) {
        let w = TerminalWrapper::from_handler(handler);
        // SAFETY: see above.
        (w, unsafe { &*std::ptr::addr_of!((*w).effects) })
    }

    fn write_pty_trampoline(handler: &mut Handler<'_>, data: &[u8]) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.write_pty else {
            return;
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata, data.as_ptr(), data.len()) };
    }

    fn bell_trampoline(handler: &mut Handler<'_>) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.bell else {
            return;
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata) };
    }

    fn clipboard_write_trampoline(handler: &mut Handler<'_>, write: clipboard::Write<'_>) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.clipboard_write else {
            return write.reply(clipboard::WriteResult::Unsupported);
        };

        // Most protocols currently produce one representation, so keep that
        // path allocation-free while supporting arbitrary multi-MIME writes.
        let mut stack_contents = [ClipboardContent::EMPTY; 4];
        let mut heap_contents: Vec<ClipboardContent>;
        let contents: &mut [ClipboardContent] = if write.contents.len() <= stack_contents.len() {
            &mut stack_contents[..write.contents.len()]
        } else {
            heap_contents = vec![ClipboardContent::EMPTY; write.contents.len()];
            &mut heap_contents
        };

        for (c_content, content) in contents.iter_mut().zip(write.contents) {
            *c_content = ClipboardContent {
                mime: LibString::init(content.mime),
                data: LibString::init(content.data),
            };
        }

        let ctx = ClipboardWriteCtx { write };
        let request = ClipboardWrite {
            size: std::mem::size_of::<ClipboardWrite>(),
            location: write.location,
            contents: if !contents.is_empty() {
                contents.as_ptr()
            } else {
                std::ptr::null()
            },
            contents_len: contents.len(),
            name: LibString::init(write.name),
            granted: write.granted,
            can_remember: write.can_remember,
            ctx: &ctx as *const ClipboardWriteCtx<'_> as *const c_void,
            reply: Self::clipboard_write_reply_trampoline,
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata, &request) };
    }

    unsafe extern "C" fn clipboard_write_reply_trampoline(
        request: *const ClipboardWrite,
        reply: *const ClipboardWriteReply,
    ) {
        let ctx = &*((*request).ctx as *const ClipboardWriteCtx<'_>);
        let write = ctx.write;
        let reply = &*reply;
        match reply.result {
            clipboard::WriteStatus::SUCCESS => write.reply(clipboard::WriteResult::Success(clipboard::WriteSuccess {
                remember: reply.remember,
            })),
            clipboard::WriteStatus::DENIED => write.reply(clipboard::WriteResult::Denied),
            clipboard::WriteStatus::BUSY => write.reply(clipboard::WriteResult::Busy),
            clipboard::WriteStatus::INVALID_DATA => write.reply(clipboard::WriteResult::InvalidData),
            clipboard::WriteStatus::IO_ERROR => write.reply(clipboard::WriteResult::IoError),
            _ => write.reply(clipboard::WriteResult::Unsupported),
        }
    }

    fn clipboard_read_trampoline(handler: &mut Handler<'_>, read: clipboard::Read<'_>) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.clipboard_read else {
            return;
        };

        let mimes: Vec<LibString> = read.mimes.iter().map(|m| LibString::init(m)).collect();

        let ctx = ClipboardReadCtx { read };
        let request = ClipboardRead {
            size: std::mem::size_of::<ClipboardRead>(),
            location: read.location,
            mimes: if !mimes.is_empty() {
                mimes.as_ptr()
            } else {
                std::ptr::null()
            },
            mimes_len: mimes.len(),
            list: read.list,
            name: LibString::init(read.name),
            granted: read.granted,
            can_remember: read.can_remember,
            ctx: &ctx as *const ClipboardReadCtx<'_> as *const c_void,
            reply: Self::clipboard_read_reply_trampoline,
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata, &request) };
    }

    unsafe extern "C" fn clipboard_read_reply_trampoline(
        request: *const ClipboardRead,
        reply: *const ClipboardReadReply,
    ) {
        let ctx = &*((*request).ctx as *const ClipboardReadCtx<'_>);
        let read = ctx.read;
        let reply = &*reply;
        match reply.result {
            clipboard::ReadStatus::SUCCESS => {}
            clipboard::ReadStatus::DENIED => return read.reply(clipboard::ReadResult::Denied),
            clipboard::ReadStatus::BUSY => return read.reply(clipboard::ReadResult::Busy),
            clipboard::ReadStatus::IO_ERROR => return read.reply(clipboard::ReadResult::IoError),
            _ => return read.reply(clipboard::ReadResult::Unsupported),
        }

        let c_contents: &[ClipboardContent] = super::types::slice_or_empty(reply.contents, reply.contents_len);
        let c_available: &[LibString] = super::types::slice_or_empty(reply.available, reply.available_len);

        let contents: Vec<clipboard::Content<'_>> = c_contents
            .iter()
            .map(|c| clipboard::Content {
                mime: lib_string_slice(c.mime),
                data: lib_string_slice(c.data),
            })
            .collect();
        let available: Vec<&[u8]> = c_available.iter().map(|m| lib_string_slice(*m)).collect();

        read.reply(clipboard::ReadResult::Success(clipboard::ReadSuccess {
            contents: &contents,
            available: &available,
            remember: reply.remember,
        }));
    }

    fn desktop_notification_trampoline(handler: &mut Handler<'_>, notification: stream::ShowDesktopNotification<'_>) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.desktop_notification else {
            return;
        };
        let request = DesktopNotification {
            size: std::mem::size_of::<DesktopNotification>(),
            title: LibString::init(notification.title),
            body: LibString::init(notification.body),
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata, &request) };
    }

    fn color_scheme_trampoline(handler: &mut Handler<'_>) -> std::option::Option<device_status::ColorScheme> {
        let (w, effects) = Self::wrapper(handler);
        let func = effects.color_scheme?;
        let mut scheme: c_int = 0;
        // SAFETY: C callback contract.
        if unsafe { func(w, effects.userdata, &mut scheme) } {
            return color_scheme_c::ColorScheme::from_int(scheme).map(|s| s.to_zig());
        }
        None
    }

    fn device_attributes_trampoline(handler: &mut Handler<'_>) -> device_attributes::Attributes {
        let w = TerminalWrapper::from_handler(handler);
        // SAFETY: see `wrapper`; the features buffer lives in the wrapper.
        let effects = unsafe { &mut *std::ptr::addr_of_mut!((*w).effects) };
        let Some(func) = effects.device_attributes_cb else {
            return device_attributes::Attributes::default();
        };

        // Get our attributes from the callback.
        let mut c_attrs = DeviceAttributes {
            primary: DeviceAttributesPrimary {
                conformance_level: 0,
                features: [0; 64],
                num_features: 0,
            },
            secondary: DeviceAttributesSecondary::default(),
            tertiary: DeviceAttributesTertiary::default(),
        };
        // SAFETY: C callback contract.
        if !unsafe { func(w, effects.userdata, &mut c_attrs) } {
            return device_attributes::Attributes::default();
        }

        // All our types are non-exhaustive newtypes, so any value is valid.
        let n = c_attrs.primary.num_features.min(64);
        for i in 0..n {
            effects.da_features_buf[i] = device_attributes::Feature(c_attrs.primary.features[i]);
        }

        // SAFETY: the buffer lives in the wrapper and is read by the caller
        // before any re-entrant call (see `da_features_buf`).
        let features: &'static [device_attributes::Feature] =
            unsafe { std::slice::from_raw_parts(effects.da_features_buf.as_ptr(), n) };
        device_attributes::Attributes {
            primary: device_attributes::Primary {
                conformance_level: device_attributes::ConformanceLevel(c_attrs.primary.conformance_level),
                features,
            },
            secondary: device_attributes::Secondary {
                device_type: device_attributes::DeviceType(c_attrs.secondary.device_type),
                firmware_version: c_attrs.secondary.firmware_version,
                rom_cartridge: c_attrs.secondary.rom_cartridge,
            },
            tertiary: device_attributes::Tertiary {
                unit_id: c_attrs.tertiary.unit_id,
            },
        }
    }

    fn enquiry_trampoline<'h>(handler: &'h mut Handler<'_>) -> &'h [u8] {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.enquiry else {
            return b"";
        };
        // SAFETY: C callback contract.
        let result = unsafe { func(w, effects.userdata) };
        if result.len == 0 {
            return b"";
        }
        // SAFETY: the memory remains valid until the callback returns.
        unsafe { std::slice::from_raw_parts(result.ptr, result.len) }
    }

    fn xtversion_trampoline<'h>(handler: &'h mut Handler<'_>) -> &'h [u8] {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.xtversion else {
            return b"";
        };
        // SAFETY: C callback contract.
        let result = unsafe { func(w, effects.userdata) };
        if result.len == 0 {
            return b"";
        }
        // SAFETY: the memory remains valid until the callback returns.
        unsafe { std::slice::from_raw_parts(result.ptr, result.len) }
    }

    fn title_changed_trampoline(handler: &mut Handler<'_>) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.title_changed else {
            return;
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata) };
    }

    fn render_hold_trampoline(handler: &mut Handler<'_>, held: bool) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.render_hold else {
            return;
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata, held) };
    }

    fn pwd_changed_trampoline(handler: &mut Handler<'_>) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.pwd_changed else {
            return;
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata) };
    }

    fn progress_report_trampoline(handler: &mut Handler<'_>, report: osc::ProgressReport) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.progress_report else {
            return;
        };
        let c_report = ProgressReport {
            size: std::mem::size_of::<ProgressReport>(),
            state: ProgressState::from_zig(report.state),
            progress: report.progress.map_or(-1, |v| v as i8),
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata, &c_report) };
    }

    fn program_status_trampoline(handler: &mut Handler<'_>, report: program_status::Report<'_>) {
        use program_status::options;

        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.program_status else {
            return;
        };

        // Both buffers hold the longest text the parser accepts, so
        // writing to them can't fail.
        let mut title_buf = [0u8; program_status::MAX_TITLE_BYTES];
        let mut title: &mut [u8] = &mut title_buf;
        report.write_text::<options::Title, _>(&mut title).expect("title fits");
        let title_len = program_status::MAX_TITLE_BYTES - title.len();
        let mut message_buf = [0u8; program_status::MAX_MSG_BYTES];
        let mut message: &mut [u8] = &mut message_buf;
        report.write_text::<options::Msg, _>(&mut message).expect("message fits");
        let message_len = program_status::MAX_MSG_BYTES - message.len();

        let c_report = ProgramStatus {
            size: std::mem::size_of::<ProgramStatus>(),
            state: ProgramStatusState::from_zig(report.state),
            kind: match report.read_option::<options::Kind>() {
                Some(program_status::Kind::Permission) => ProgramStatusKind::Permission,
                Some(program_status::Kind::Question) => ProgramStatusKind::Question,
                Some(program_status::Kind::Auth) => ProgramStatusKind::Auth,
                None => ProgramStatusKind::None,
            },
            progress: report.read_option::<options::Progress>().map_or(-1, |v| v as i8),
            id: LibString::init(report.read_option::<options::Id>().unwrap_or(b"")),
            app: LibString::init(report.read_option::<options::App>().unwrap_or(b"")),
            title: LibString::init(&title_buf[..title_len]),
            message: LibString::init(&message_buf[..message_len]),
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata, &c_report) };
    }

    fn semantic_prompt_trampoline(handler: &mut Handler<'_>, event: stream_terminal::SemanticPrompt<'_>) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.semantic_prompt else {
            return;
        };
        let c_event = SemanticPrompt {
            size: std::mem::size_of::<SemanticPrompt>(),
            kind: SemanticPromptKind::from_zig(event.kind),
            prompt_kind: SemanticPromptPromptKind::from_zig(event.prompt_kind),
            has_exit_code: event.exit_code.is_some(),
            exit_code: event.exit_code.unwrap_or(0),
            command: LibString::init(event.command),
            error: LibString::init(event.err),
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata, &c_event) };
    }

    fn reset_trampoline(handler: &mut Handler<'_>) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.reset else {
            return;
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata) };
    }

    fn resize_window_trampoline(handler: &mut Handler<'_>, request: stream_terminal::ResizeWindow) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.resize_window else {
            return;
        };
        let value = match request {
            stream_terminal::ResizeWindow::Grid(grid) => ResizeWindow {
                tag: ResizeWindowTag::Grid,
                value: ResizeWindowValue {
                    grid: ResizeWindowGrid {
                        rows: grid.rows,
                        columns: grid.columns,
                    },
                },
            },
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata, &value) };
    }

    fn unknown_sequence_trampoline(handler: &mut Handler<'_>, sequence: stream_terminal::UnknownSequence<'_>) {
        let (w, effects) = Self::wrapper(handler);
        let Some(func) = effects.unknown_sequence else {
            return;
        };
        let value = match sequence {
            stream_terminal::UnknownSequence::Apc(apc_value) => UnknownSequence {
                tag: UnknownSequenceTag::Apc,
                value: UnknownSequenceValue {
                    apc: UnknownStringSequence {
                        truncated: apc_value.truncated,
                        content: LibString::init(apc_value.content),
                    },
                },
            },
            stream_terminal::UnknownSequence::Osc(osc_value) => UnknownSequence {
                tag: UnknownSequenceTag::Osc,
                value: UnknownSequenceValue { osc: osc_value.cval() },
            },
        };
        // SAFETY: C callback contract.
        unsafe { func(w, effects.userdata, &value) };
    }

    fn size_trampoline(handler: &mut Handler<'_>) -> std::option::Option<size_report::Size> {
        let (w, effects) = Self::wrapper(handler);
        let func = effects.size_cb?;
        let mut s = size_report::Size::default();
        // SAFETY: C callback contract.
        if unsafe { func(w, effects.userdata, &mut s) } {
            return Some(s);
        }
        None
    }
}

/// A `GhosttyString` as a slice.
fn lib_string_slice<'a>(s: LibString) -> &'a [u8] {
    // SAFETY: C API contract: `ptr` is valid for `len` bytes.
    unsafe { super::types::slice_or_empty(s.ptr, s.len) }
}

/// C: GhosttyTerminal
pub type Terminal = *mut TerminalWrapper;

crate::c_api_enum! {
    /// C: GhosttyTerminalCompressionMode
    /// (Zig: `ZigTerminal.CompressionMode`.)
    pub enum CompressionMode {
        Incremental = 0,
        Full = 1,
    }
}

impl CompressionMode {
    fn to_zig(self) -> zig_terminal::CompressionMode {
        match self {
            CompressionMode::Incremental => zig_terminal::CompressionMode::Incremental,
            CompressionMode::Full => zig_terminal::CompressionMode::Full,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttyTerminalCompressionResult
    /// (Zig: `ZigTerminal.CompressionResult`.)
    pub enum CompressionResult {
        Unsupported = 0,
        Pending = 1,
        Complete = 2,
    }
}

impl CompressionResult {
    fn from_zig(r: zig_terminal::CompressionResult) -> CompressionResult {
        match r {
            zig_terminal::CompressionResult::Unsupported => CompressionResult::Unsupported,
            zig_terminal::CompressionResult::Pending => CompressionResult::Pending,
            zig_terminal::CompressionResult::Complete => CompressionResult::Complete,
        }
    }
}

/// The native terminal of a C terminal handle.
///
/// # Safety
/// `terminal_` must be NULL or a live terminal handle.
pub unsafe fn zig_terminal<'a>(terminal_: Terminal) -> std::option::Option<&'a mut ZigTerminal> {
    Some(terminal_.as_mut()?.terminal_mut())
}

/// Convert a C `GhosttyPointTag`.
pub fn point_tag_from_int(v: c_int) -> std::option::Option<point::Tag> {
    [point::Tag::Active, point::Tag::Viewport, point::Tag::Screen, point::Tag::History]
        .into_iter()
        .find(|t| *t as c_int == v)
}

/// Attach the persistent C stream and I/O owner to a heap-stable terminal.
/// The terminal is dropped if wrapper allocation fails.
fn wrap(
    alloc: HandleAllocator,
    t: Box<ZigTerminal>,
    io: Io,
    continuation_max_bytes: usize,
) -> std::result::Result<Terminal, OutOfMemory> {
    let terminal_ptr = Box::into_raw(t);
    // SAFETY: the terminal is heap-stable and owned by the wrapper, which
    // drops the stream (holding this borrow) before the terminal.
    let t: &'static mut ZigTerminal = unsafe { &mut *terminal_ptr };

    // Trampolines are always installed so setting C callbacks later takes
    // effect immediately.
    let mut handler: Handler<'static> = t.vt_handler();
    handler.effects = stream_terminal::Effects {
        write_pty: Some(Effects::write_pty_trampoline),
        bell: Some(Effects::bell_trampoline),
        color_scheme: Some(Effects::color_scheme_trampoline),
        desktop_notification: Some(Effects::desktop_notification_trampoline),
        drag_and_drop: None,
        device_attributes: Some(Effects::device_attributes_trampoline),
        enquiry: Some(Effects::enquiry_trampoline),
        xtversion: Some(Effects::xtversion_trampoline),
        title_changed: Some(Effects::title_changed_trampoline),
        pwd_changed: Some(Effects::pwd_changed_trampoline),
        progress_report: Some(Effects::progress_report_trampoline),
        semantic_prompt: Some(Effects::semantic_prompt_trampoline),
        reset: Some(Effects::reset_trampoline),
        size: Some(Effects::size_trampoline),
        render_hold: Some(Effects::render_hold_trampoline),
        resize_window: Some(Effects::resize_window_trampoline),

        // Installed dynamically when the callback is set; see Effects.
        clipboard_write: None,
        clipboard_read: None,
        program_status: None,
    };

    let wrapper = TerminalWrapper {
        terminal_ptr,
        io,
        tmp_dir_path: None,
        terminfo_name_buf: [0; Handler::MAX_TERMINFO_NAME_BYTES],
        stream: ManuallyDrop::new(Stream::init(stream::Options {
            allocator: Some(osc::Allocator::GLOBAL),
            continuation_max_bytes: Some(continuation_max_bytes),
            ..stream::Options::new(handler)
        })),
        effects: Effects::default(),
        tracked_grid_refs: Vec::new(),
        searches: Vec::new(),
        alloc,
    };
    let ptr = alloc.create(wrapper);
    if ptr.is_null() {
        return Err(OutOfMemory);
    }
    Ok(ptr)
}

/// `error.OutOfMemory`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfMemory;

/// (Zig: `RestoreContinuationError`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreContinuationError {
    OutOfMemory,
    ContinuationDisabled,
    ContinuationUnavailable,
    InvalidContinuation,
}

/// Replay a decoded continuation exactly once into a newly created C terminal
/// and verify that the persistent stream exports the identical canonical
/// bytes. The terminal remains valid on error and may be freed normally.
///
/// # Safety
/// `terminal_` must be NULL or a live terminal handle.
pub unsafe fn restore_continuation(
    terminal_: Terminal,
    continuation: &[u8],
) -> std::result::Result<(), RestoreContinuationError> {
    let Some(wrapper) = terminal_.as_mut() else {
        return Err(RestoreContinuationError::InvalidContinuation);
    };
    if !continuation.is_empty() {
        wrapper.stream.next_slice(continuation);
    }

    let mut exported: Vec<u8> = Vec::new();
    wrapper.stream.write_continuation(&mut exported).map_err(|err| match err {
        WriteContinuationError::WriteFailed => RestoreContinuationError::OutOfMemory,
        WriteContinuationError::ContinuationDisabled => RestoreContinuationError::ContinuationDisabled,
        WriteContinuationError::ContinuationUnavailable => RestoreContinuationError::ContinuationUnavailable,
    })?;
    if continuation != exported.as_slice() {
        return Err(RestoreContinuationError::InvalidContinuation);
    }
    Ok(())
}

/// (Zig: `FromDecodedError`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FromDecodedError {
    OutOfMemory,
    InvalidContinuation,
}

/// Transfer a core snapshot result into a caller-owned C terminal.
///
/// `io` is the owner the decoded terminal was built with and is retained
/// by the returned wrapper. The decoded terminal is transferred only
/// after its final heap address has been allocated; its
/// continuation remains in `decoded` and is replayed before returning.
/// `continuation_max_bytes` selects the returned terminal's tracking policy:
/// zero uses a temporary exact-size tracker and restores the ordinary C
/// default before returning, while a nonzero value leaves tracking enabled
/// with that limit.
pub fn from_decoded(
    alloc: HandleAllocator,
    io: Io,
    decoded: &mut snapshot_core::Decoded,
    continuation_max_bytes: usize,
) -> std::result::Result<Terminal, FromDecodedError> {
    let native = Box::new(decoded.to_owned());

    let continuation = decoded.continuation_bytes();
    debug_assert!(continuation_max_bytes == 0 || continuation.len() <= continuation_max_bytes);

    // Without opt-in retention, non-ground state needs tracking only long
    // enough to verify that replay reconstructed the exact canonical
    // continuation. With retention, even ground state needs a tracker so its
    // continuation can be exported successfully as an empty slice.
    let tracker_max_bytes = if continuation_max_bytes > 0 {
        continuation_max_bytes
    } else {
        continuation.len()
    };
    let terminal = wrap(alloc, native, io, tracker_max_bytes).map_err(|_| FromDecodedError::OutOfMemory)?;

    if !continuation.is_empty() {
        // SAFETY: freshly created handle.
        if let Err(err) = unsafe { restore_continuation(terminal, continuation) } {
            // SAFETY: the handle is not used afterwards.
            unsafe { free(terminal) };
            return Err(match err {
                // The decoded bytes fit this fresh tracker's cap, so losing them
                // while replaying can only be an allocation failure.
                RestoreContinuationError::OutOfMemory | RestoreContinuationError::ContinuationUnavailable => {
                    FromDecodedError::OutOfMemory
                }
                RestoreContinuationError::ContinuationDisabled | RestoreContinuationError::InvalidContinuation => {
                    FromDecodedError::InvalidContinuation
                }
            });
        }
    }

    // Unless the decoder opted into retention, restore the same disabled
    // tracking default used by newly created C terminals. Disabling tracking
    // does not alter the parser or UTF-8 state reconstructed above.
    if continuation_max_bytes == 0 {
        // SAFETY: freshly created handle.
        set_continuation_max_bytes(unsafe { &mut *terminal }, DEFAULT_CONTINUATION_MAX_BYTES);
    }
    Ok(terminal)
}

/// `error{InvalidValue, OutOfMemory}`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NewError {
    InvalidValue,
    OutOfMemory,
}

/// C: ghostty_terminal_new
#[export_name = "ghostty_terminal_new"]
pub extern "C" fn new(
    alloc_: std::option::Option<&CAllocator>,
    result: &mut Terminal,
    cols: CellCountInt,
    rows: CellCountInt,
) -> Result {
    match new_(alloc_, cols, rows) {
        Ok(t) => {
            *result = t;
            Result::Success
        }
        Err(err) => {
            *result = std::ptr::null_mut();
            match err {
                NewError::InvalidValue => Result::InvalidValue,
                NewError::OutOfMemory => Result::OutOfMemory,
            }
        }
    }
}

fn new_(alloc_: std::option::Option<&CAllocator>, cols: CellCountInt, rows: CellCountInt) -> std::result::Result<Terminal, NewError> {
    if cols == 0 || rows == 0 {
        return Err(NewError::InvalidValue);
    }

    let alloc = HandleAllocator::new(alloc_);
    let io = Io::INIT;

    // Setup our terminal
    let mut t = ZigTerminal::init(zig_terminal::Options {
        cols,
        rows,
        ..Default::default()
    })
    .map_err(|e| match e {
        zig_terminal::InitError::InvalidValue => NewError::InvalidValue,
        zig_terminal::InitError::OutOfMemory => NewError::OutOfMemory,
    })?;

    // libghostty-vt embedders don't necessarily install Ghostty's shell
    // integration, so don't assume OSC 133 prompts can be redrawn on resize.
    // Shells can still opt in with OSC 133;A;redraw=1.
    t.flags.shell_redraws_prompt = osc_sp::Redraw::False;

    wrap(alloc, Box::new(t), io, DEFAULT_CONTINUATION_MAX_BYTES).map_err(|_| NewError::OutOfMemory)
}

/// C: ghostty_terminal_vt_write
///
/// # Safety
/// `terminal_` must be NULL or live; `ptr` must be valid for `len` reads.
#[export_name = "ghostty_terminal_vt_write"]
pub unsafe extern "C" fn vt_write(terminal_: Terminal, ptr: *const u8, len: usize) {
    let Some(wrapper) = terminal_.as_mut() else {
        return;
    };
    wrapper.stream.next_slice(super::types::slice_or_empty(ptr, len));
}

crate::c_api_enum! {
    /// A boundary as a C bit flag. The C API takes and reports boundaries as a
    /// bitmask, so each value is the bit for the stream boundary of the same
    /// name.
    ///
    /// C: GhosttyTerminalBoundary
    pub enum Boundary {
        Ground = 1 << 0,
        ApcStart = 1 << 1,
        ApcEnd = 1 << 2,
        DcsStart = 1 << 3,
        DcsEnd = 1 << 4,
        OscStart = 1 << 5,
        OscEnd = 1 << 6,
    }
}

/// Every stream boundary in declaration order.
const STREAM_BOUNDARIES: [StreamBoundary; 7] = [
    StreamBoundary::Ground,
    StreamBoundary::ApcStart,
    StreamBoundary::ApcEnd,
    StreamBoundary::DcsStart,
    StreamBoundary::DcsEnd,
    StreamBoundary::OscStart,
    StreamBoundary::OscEnd,
];

// Each bit must match the stream boundary's position in the enum,
// so the conversions below can shift instead of switching.
const _: () = {
    assert!(Boundary::Ground as c_int == 1 << StreamBoundary::Ground as u8);
    assert!(Boundary::ApcStart as c_int == 1 << StreamBoundary::ApcStart as u8);
    assert!(Boundary::ApcEnd as c_int == 1 << StreamBoundary::ApcEnd as u8);
    assert!(Boundary::DcsStart as c_int == 1 << StreamBoundary::DcsStart as u8);
    assert!(Boundary::DcsEnd as c_int == 1 << StreamBoundary::DcsEnd as u8);
    assert!(Boundary::OscStart as c_int == 1 << StreamBoundary::OscStart as u8);
    assert!(Boundary::OscEnd as c_int == 1 << StreamBoundary::OscEnd as u8);
};

impl Boundary {
    const ALL: u32 = (1 << STREAM_BOUNDARIES.len()) - 1;

    /// Convert a bitmask from C to a set. Returns null if the mask is
    /// empty or has bits that aren't boundaries.
    fn set_from_bits(bits: u32) -> std::option::Option<BoundarySet> {
        if bits == 0 || bits & !Self::ALL != 0 {
            return None;
        }
        let mut result = BoundarySet::EMPTY;
        for b in STREAM_BOUNDARIES {
            if bits & Self::bit(b) != 0 {
                result.insert(b);
            }
        }
        Some(result)
    }

    /// Convert a set to a bitmask for C.
    fn bits_from_set(set_: BoundarySet) -> u32 {
        let mut bits: u32 = 0;
        for b in STREAM_BOUNDARIES {
            if set_.contains(b) {
                bits |= Self::bit(b);
            }
        }
        bits
    }

    fn bit(b: StreamBoundary) -> u32 {
        1u32 << (b as u8)
    }
}

crate::c_api_enum! {
    /// C: GhosttyTerminalStringKind (Zig: `StreamBoundary.StringKind`)
    pub enum StringKind {
        Apc = 0,
        Dcs = 1,
        Osc = 2,
    }
}

impl StringKind {
    fn from_zig(k: stream::StringKind) -> StringKind {
        match k {
            stream::StringKind::Apc => StringKind::Apc,
            stream::StringKind::Dcs => StringKind::Dcs,
            stream::StringKind::Osc => StringKind::Osc,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttyTerminalStringIntroducer (Zig: `StreamBoundary.Introducer`)
    pub enum StringIntroducer {
        Esc = 0,
        C1 = 1,
    }
}

impl StringIntroducer {
    fn from_zig(i: stream::Introducer) -> StringIntroducer {
        match i {
            stream::Introducer::Esc => StringIntroducer::Esc,
            stream::Introducer::C1 => StringIntroducer::C1,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttyTerminalStringEnd (Zig: `StreamBoundary.EndReason`)
    pub enum StringEnd {
        StEsc = 0,
        StC1 = 1 => "st_c1",
        Cancelled = 2,
        Aborted = 3,
        Bel = 4,
    }
}

impl StringEnd {
    fn from_zig(e: stream::EndReason) -> StringEnd {
        match e {
            stream::EndReason::StEsc => StringEnd::StEsc,
            stream::EndReason::StC1 => StringEnd::StC1,
            stream::EndReason::Cancelled => StringEnd::Cancelled,
            stream::EndReason::Aborted => StringEnd::Aborted,
            stream::EndReason::Bel => StringEnd::Bel,
        }
    }
}

/// Where `vt_write_until` stopped. This flattens `StreamBoundary.Stop`
/// for C. Fields that don't apply to a stop are zero, and `reached` tells
/// the caller which fields apply.
///
/// A stop can be both a string end and a string start, when an 8-bit
/// introducer of another kind arrives inside an APC or a DCS header. The
/// Zig stop carries a kind for each. This struct has one `kind` field, so
/// it holds the kind of the string that ended, and the start flag in
/// `reached` names the kind that started.
///
/// C: GhosttyTerminalWriteStop
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteStop {
    pub size: usize,
    pub consumed: usize,
    pub reached: u32,
    pub kind: StringKind,
    pub introducer: StringIntroducer,
    pub end: StringEnd,
    pub byte: u8,
}

impl Default for WriteStop {
    fn default() -> Self {
        WriteStop {
            size: std::mem::size_of::<WriteStop>(),
            consumed: 0,
            reached: 0,
            kind: StringKind::Apc,
            introducer: StringIntroducer::Esc,
            end: StringEnd::StEsc,
            byte: 0,
        }
    }
}

/// C: ghostty_terminal_vt_write_until
///
/// # Safety
/// `terminal_` must be NULL or live; a non-null `ptr_` must be valid for
/// `len` reads; a non-null `out_` must be valid for `out_.size` bytes.
#[export_name = "ghostty_terminal_vt_write_until"]
pub unsafe extern "C" fn vt_write_until(
    terminal_: Terminal,
    ptr_: *const u8,
    len: usize,
    boundaries: u32,
    out_: *mut WriteStop,
) -> Result {
    let Some(out) = out_.as_mut() else {
        return Result::InvalidValue;
    };
    if out.size < std::mem::size_of::<WriteStop>() {
        return Result::InvalidValue;
    }

    // Reset every field so a reused struct never carries a previous stop.
    *out = WriteStop::default();

    let Some(wrapper) = terminal_.as_mut() else {
        return Result::InvalidValue;
    };
    let input: &[u8] = if !ptr_.is_null() {
        super::types::slice_or_empty(ptr_, len)
    } else if len == 0 {
        b""
    } else {
        return Result::InvalidValue;
    };
    let Some(requested) = Boundary::set_from_bits(boundaries) else {
        return Result::InvalidValue;
    };

    let Some(stop) = wrapper.stream.next_slice_until(input, requested) else {
        out.consumed = len;
        return Result::NoValue;
    };

    out.consumed = stop.consumed;
    out.reached = Boundary::bits_from_set(stop.reached);
    if let Some(start) = stop.start {
        out.kind = StringKind::from_zig(start.kind);
        out.introducer = StringIntroducer::from_zig(start.introducer);
        out.byte = start.byte;
    }
    // When a byte both ends and starts a string, the end's kind wins;
    // see WriteStop.
    if let Some(end) = stop.end {
        out.kind = StringKind::from_zig(end.kind);
        out.end = StringEnd::from_zig(end.reason);
        out.byte = end.byte;
    }

    Result::Success
}

/// (Zig: `ContinuationWriteError`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationWriteError {
    InvalidValue,
    WriteFailed,
    ContinuationDisabled,
    ContinuationUnavailable,
}

impl From<WriteContinuationError> for ContinuationWriteError {
    fn from(err: WriteContinuationError) -> Self {
        match err {
            WriteContinuationError::WriteFailed => ContinuationWriteError::WriteFailed,
            WriteContinuationError::ContinuationDisabled => ContinuationWriteError::ContinuationDisabled,
            WriteContinuationError::ContinuationUnavailable => ContinuationWriteError::ContinuationUnavailable,
        }
    }
}

/// Write the exact replay-safe continuation for a C terminal to a writer.
/// Snapshot encoding uses this helper so it can preflight continuation state
/// without converting its allocator or writer through the C ABI.
///
/// # Safety
/// `terminal_` must be NULL or live.
pub unsafe fn continuation_write_io(
    terminal_: Terminal,
    writer: &mut dyn std::io::Write,
) -> std::result::Result<(), ContinuationWriteError> {
    // Keep handle validation here so the three public output forms and the
    // snapshot encoder all use exactly the same continuation preflight.
    let Some(wrapper) = terminal_.as_ref() else {
        return Err(ContinuationWriteError::InvalidValue);
    };

    // Stream owns the tracker and distinguishes disabled tracking from a
    // tracker that lost bytes after allocation failure or limit overflow.
    wrapper.stream.write_continuation(writer)?;
    Ok(())
}

/// (Zig: `ContinuationAllocError`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationAllocError {
    InvalidValue,
    OutOfMemory,
    ContinuationDisabled,
    ContinuationUnavailable,
}

/// Return an owned copy of the terminal's replay-safe continuation.
///
/// If `allow_untracked_ground` is true, disabled tracking is accepted only
/// when the stream is provably at ground and an empty buffer is returned.
///
/// # Safety
/// `terminal_` must be NULL or live.
pub unsafe fn continuation_alloc_io(
    terminal_: Terminal,
    allow_untracked_ground: bool,
) -> std::result::Result<Vec<u8>, ContinuationAllocError> {
    let Some(wrapper) = terminal_.as_ref() else {
        return Err(ContinuationAllocError::InvalidValue);
    };
    if allow_untracked_ground && wrapper.stream.continuation.is_none() && wrapper.stream.ground() {
        return Ok(Vec::new());
    }

    // The allocating writer gives snapshot encoding and the public allocation
    // API one common way to obtain an exact owned continuation.
    let mut aw: Vec<u8> = Vec::new();

    // A write failure from an allocating writer can only be allocation failure;
    // no external callback participates in this path.
    continuation_write_io(terminal_, &mut aw).map_err(|err| match err {
        ContinuationWriteError::InvalidValue => ContinuationAllocError::InvalidValue,
        ContinuationWriteError::WriteFailed => ContinuationAllocError::OutOfMemory,
        ContinuationWriteError::ContinuationDisabled => ContinuationAllocError::ContinuationDisabled,
        ContinuationWriteError::ContinuationUnavailable => ContinuationAllocError::ContinuationUnavailable,
    })?;
    Ok(aw)
}

/// Map errors that are intrinsic to continuation export. Public callback
/// failures receive finer classification in `continuation_write` below.
fn continuation_error_result(err: ContinuationWriteError) -> Result {
    match err {
        ContinuationWriteError::InvalidValue => Result::InvalidValue,
        ContinuationWriteError::WriteFailed => Result::IoError,
        ContinuationWriteError::ContinuationDisabled => Result::InvalidValue,
        ContinuationWriteError::ContinuationUnavailable => Result::InvalidValue,
    }
}

/// C: ghostty_terminal_continuation_write
///
/// # Safety
/// `terminal_` must be NULL or live; `writer` must follow the
/// `GhosttyWriter` contract.
#[export_name = "ghostty_terminal_continuation_write"]
pub unsafe extern "C" fn continuation_write(terminal_: Terminal, writer: c_io::Writer) -> Result {
    // Reject the missing callback before invoking the common helper so this is
    // classified as a bad argument rather than a write failure.
    if writer.write.is_none() {
        return Result::InvalidValue;
    }

    // The callback was validated above, so invalid_write can only mean output
    // accounting overflow. Keep that distinct from callback rejection.
    let mut buffer = [0u8; c_io::WriterAdapter::RECOMMENDED_BUFFER_LEN];
    let mut adapter = c_io::WriterAdapter::init_buffered(writer, &mut buffer);
    'write: {
        match continuation_write_io(terminal_, &mut adapter) {
            Ok(()) => {}
            Err(ContinuationWriteError::WriteFailed) => break 'write,
            Err(err) => return continuation_error_result(err),
        }
        if adapter.flush().is_err() {
            break 'write;
        }
        return Result::Success;
    }

    if adapter.invalid_write {
        return Result::LimitExceeded;
    }
    if adapter.callback_failed {
        return Result::IoError;
    }
    continuation_error_result(ContinuationWriteError::WriteFailed)
}

/// A writer that only counts bytes (Zig: `std.Io.Writer.Discarding`).
#[derive(Default)]
struct Discarding {
    count: usize,
}

impl std::io::Write for Discarding {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.count += data.len();
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// C: ghostty_terminal_continuation_buf
///
/// # Safety
/// `terminal_` must be NULL or live; a non-null `out_` must be valid for
/// `out_len` writes; a non-null `out_written_` must be writable.
#[export_name = "ghostty_terminal_continuation_buf"]
pub unsafe extern "C" fn continuation_buf(
    terminal_: Terminal,
    out_: *mut u8,
    out_len: usize,
    out_written_: *mut usize,
) -> Result {
    let Some(out_written) = out_written_.as_mut() else {
        return Result::InvalidValue;
    };
    // All failure paths leave deterministic output metadata.
    *out_written = 0;
    if out_.is_null() && out_len != 0 {
        return Result::InvalidValue;
    }

    if out_.is_null() {
        // A null/zero destination is the explicit size-query form. Discarding
        // runs the real exporter, so disabled or unavailable tracking is still
        // detected before a required length is reported.
        let mut discarding = Discarding::default();
        if let Err(err) = continuation_write_io(terminal_, &mut discarding) {
            return continuation_error_result(err);
        }
        *out_written = discarding.count;
        return Result::OutOfSpace;
    }

    // Fixed writers report WriteFailed when capacity is exhausted. The stream
    // exporter itself remains all-or-nothing from the API's perspective.
    let buf = std::slice::from_raw_parts_mut(out_, out_len);
    let mut writer: &mut [u8] = buf;
    match continuation_write_io(terminal_, &mut writer) {
        Ok(()) => {}
        Err(ContinuationWriteError::WriteFailed) => {
            // Re-run against a counter to return the full required capacity,
            // not merely the prefix that fit in the caller's buffer.
            let mut discarding = Discarding::default();
            if let Err(count_err) = continuation_write_io(terminal_, &mut discarding) {
                return continuation_error_result(count_err);
            }
            *out_written = discarding.count;
            return Result::OutOfSpace;
        }
        Err(err) => return continuation_error_result(err),
    }

    // The initialized prefix of the fixed destination.
    *out_written = out_len - writer.len();
    Result::Success
}

/// C: ghostty_terminal_continuation_alloc
///
/// # Safety
/// `terminal_` must be NULL or live; non-null output pointers must be
/// writable.
#[export_name = "ghostty_terminal_continuation_alloc"]
pub unsafe extern "C" fn continuation_alloc(
    terminal_: Terminal,
    alloc_: std::option::Option<&CAllocator>,
    out_ptr_: *mut *mut u8,
    out_len_: *mut usize,
) -> Result {
    let Some(out_ptr) = out_ptr_.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(out_len) = out_len_.as_mut() else {
        return Result::InvalidValue;
    };
    // Make ownership unambiguous even if validation or allocation fails.
    *out_ptr = std::ptr::null_mut();
    *out_len = 0;

    let bytes = match continuation_alloc_io(terminal_, false) {
        Ok(bytes) => bytes,
        Err(err) => {
            return match err {
                ContinuationAllocError::InvalidValue => Result::InvalidValue,
                ContinuationAllocError::OutOfMemory => Result::OutOfMemory,
                ContinuationAllocError::ContinuationDisabled => Result::InvalidValue,
                ContinuationAllocError::ContinuationUnavailable => Result::InvalidValue,
            }
        }
    };

    // An idle parser has no continuation. Never export an empty slice's
    // dangling pointer to a foreign runtime (and never allocate for it).
    if bytes.is_empty() {
        return Result::Success;
    }

    // Ownership crosses the ABI here; callers release this exact pointer and
    // length with ghostty_free and the same allocator selection.
    let alloc = lib_alloc::default(alloc_);
    let ptr = alloc.alloc(bytes.len(), 0, 0);
    if ptr.is_null() {
        return Result::OutOfMemory;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
    *out_ptr = ptr;
    *out_len = bytes.len();
    Result::Success
}

fn continuation_max_bytes(wrapper: &TerminalWrapper) -> usize {
    // Absence of a tracker is the public representation of disabled tracking.
    wrapper.stream.continuation.as_ref().map_or(0, |tracker| tracker.max_bytes)
}

/// Change continuation tracking policy without disturbing normal VT parser
/// state. Bytes which were not retained while disabled or after exceeding an
/// earlier cap cannot be reconstructed: export remains unavailable until a
/// later feed reaches ground or contains a new replay start.
fn set_continuation_max_bytes(wrapper: &mut TerminalWrapper, max_bytes: usize) {
    if max_bytes == 0 {
        // Disabling releases retained bytes immediately. Parser and UTF-8 state
        // continue normally; only future replay/export information is lost.
        if let Some(tracker) = &mut wrapper.stream.continuation {
            tracker.deinit();
        }
        wrapper.stream.continuation = None;
        return;
    }

    if let Some(tracker) = &mut wrapper.stream.continuation {
        // Changing a live cap preserves retained bytes when they still fit.
        tracker.max_bytes = max_bytes;
        if tracker.bytes.len() > max_bytes {
            // Once a retained prefix is discarded it cannot be reconstructed
            // from parser state alone. Mark it broken until Stream observes a
            // ground state or a new replay-safe sequence start.
            tracker.bytes.clear();
            tracker.broken = true;
        }
        return;
    }

    // Enabling from zero starts an empty tracker. It can immediately track
    // only if no earlier bytes are needed.
    wrapper.stream.continuation = Some(Tracker::init(osc::Allocator::GLOBAL, max_bytes));
    if !wrapper.stream.ground() {
        // The parser was already mid-sequence while tracking was disabled, so
        // exporting now would omit an unknown prefix.
        wrapper.stream.continuation.as_mut().unwrap().broken = true;
    }
}

/// C: ghostty_terminal_compression_activity
///
/// # Safety
/// `terminal_` must be NULL or live; a non-null `out_activity_` must be
/// writable.
#[export_name = "ghostty_terminal_compression_activity"]
pub unsafe extern "C" fn compression_activity(terminal_: Terminal, out_activity_: *mut u64) -> Result {
    let Some(t) = zig_terminal(terminal_) else {
        return Result::InvalidValue;
    };
    let Some(out_activity) = out_activity_.as_mut() else {
        return Result::InvalidValue;
    };
    *out_activity = t.compression_activity();
    Result::Success
}

/// C: ghostty_terminal_compress
///
/// # Safety
/// `terminal_` must be NULL or live; a non-null `out_result_` must be
/// writable.
#[export_name = "ghostty_terminal_compress"]
pub unsafe extern "C" fn compress(terminal_: Terminal, mode_: c_int, out_result_: *mut CompressionResult) -> Result {
    let Some(t) = zig_terminal(terminal_) else {
        return Result::InvalidValue;
    };
    let Some(out_result) = out_result_.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(mode) = CompressionMode::from_int(mode_) else {
        return Result::InvalidValue;
    };
    *out_result = CompressionResult::from_zig(t.compress(mode.to_zig()));
    Result::Success
}

crate::c_api_enum! {
    /// C: GhosttyTerminalOption
    ///
    /// The input type of each option is documented in the header
    /// (Zig: `Option.InType`).
    pub enum Option {
        Userdata = 0,
        WritePty = 1,
        Bell = 2,
        Enquiry = 3,
        Xtversion = 4,
        TitleChanged = 5,
        Size = 6 => "size_cb",
        ColorScheme = 7,
        DeviceAttributes = 8,
        Title = 9,
        Pwd = 10,
        ColorForeground = 11,
        ColorBackground = 12,
        ColorCursor = 13,
        ColorPalette = 14,
        KittyImageStorageLimit = 15,
        KittyImageMediumFile = 16,
        KittyImageMediumTempFile = 17,
        KittyImageMediumSharedMem = 18,
        ApcMaxBytes = 19,
        ApcMaxBytesKitty = 20,
        Selection = 21,
        DefaultCursorStyle = 22,
        DefaultCursorBlink = 23,
        GlyphProtocol = 24,
        PwdChanged = 25,
        ClipboardWrite = 26,
        ScrollbackMaxBytes = 27,
        ScrollbackMaxLines = 28,
        DesktopNotification = 29,
        ProgressReport = 30,
        ContinuationMaxBytes = 31,
        TitleReport = 32,
        ModeDefault = 33,
        Mode = 34,
        UnknownSequence = 35,
        UnknownMaxBytes = 36,
        TerminfoName = 37,
        ClipboardRead = 38,
        ClipboardWriteMaxBytes = 39,
        ResizePullScrollback = 40,
        RenderHold = 41,
        SemanticPrompt = 42,
        Reset = 43,
        XtChecksumReport = 44,
        XtChecksumExtension = 45,
        ProgramStatus = 46,
        ResizeWindow = 47,
    }
}

/// C: ghostty_terminal_set
///
/// # Safety
/// `terminal_` must be NULL or live; `value` must be NULL or point to the
/// option's input type (for callback options, `value` is the function
/// pointer itself).
#[export_name = "ghostty_terminal_set"]
pub unsafe extern "C" fn set(terminal_: Terminal, option: c_int, value: *const c_void) -> Result {
    let Some(option) = Option::from_int(option) else {
        log::warn!(target: LOG_TARGET, "terminal_set invalid option value={option}");
        return Result::InvalidValue;
    };

    let Some(wrapper) = terminal_.as_mut() else {
        return Result::InvalidValue;
    };

    set_typed(wrapper, option, value)
}

/// Reinterpret `value` as an optional C function pointer.
unsafe fn func<F: Copy>(value: *const c_void) -> std::option::Option<F> {
    debug_assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*const c_void>());
    if value.is_null() {
        None
    } else {
        Some(std::mem::transmute_copy::<*const c_void, F>(&value))
    }
}

/// Read `value` as an optional `*const T`.
unsafe fn val<'a, T>(value: *const c_void) -> std::option::Option<&'a T> {
    (value as *const T).as_ref()
}

unsafe fn set_typed(wrapper: &mut TerminalWrapper, option: Option, value: *const c_void) -> Result {
    match option {
        Option::Userdata => wrapper.effects.userdata = value as *mut c_void,
        Option::WritePty => wrapper.effects.write_pty = func(value),
        Option::Bell => wrapper.effects.bell = func(value),
        Option::ColorScheme => wrapper.effects.color_scheme = func(value),
        Option::DesktopNotification => wrapper.effects.desktop_notification = func(value),
        Option::DeviceAttributes => wrapper.effects.device_attributes_cb = func(value),
        Option::Enquiry => wrapper.effects.enquiry = func(value),
        Option::Xtversion => wrapper.effects.xtversion = func(value),
        Option::TitleChanged => wrapper.effects.title_changed = func(value),
        Option::PwdChanged => wrapper.effects.pwd_changed = func(value),
        Option::ProgressReport => wrapper.effects.progress_report = func(value),
        Option::ResizeWindow => wrapper.effects.resize_window = func(value),
        Option::Size => wrapper.effects.size_cb = func(value),
        Option::RenderHold => wrapper.effects.render_hold = func(value),
        Option::SemanticPrompt => wrapper.effects.semantic_prompt = func(value),
        Option::Reset => wrapper.effects.reset = func(value),
        Option::ClipboardWrite => {
            wrapper.effects.clipboard_write = func(value);
            wrapper.stream.handler.effects.clipboard_write = if !value.is_null() {
                Some(Effects::clipboard_write_trampoline)
            } else {
                None
            };
        }
        Option::ClipboardRead => {
            wrapper.effects.clipboard_read = func(value);
            wrapper.stream.handler.effects.clipboard_read = if !value.is_null() {
                Some(Effects::clipboard_read_trampoline)
            } else {
                None
            };
        }
        Option::ProgramStatus => {
            // Installed dynamically because the terminal only answers the
            // OSC 7501 support query when something consumes reports.
            wrapper.effects.program_status = func(value);
            wrapper.stream.handler.effects.program_status = if !value.is_null() {
                Some(Effects::program_status_trampoline)
            } else {
                None
            };
        }
        Option::UnknownSequence => {
            wrapper.effects.unknown_sequence = func(value);
            wrapper.stream.handler.unknown_sequence = if !value.is_null() {
                Some(Effects::unknown_sequence_trampoline)
            } else {
                None
            };
        }
        Option::TitleReport => {
            wrapper.stream.handler.title_report = val::<bool>(value).copied().unwrap_or(false);
        }
        Option::XtChecksumReport => {
            wrapper.stream.handler.xt_checksum_report = val::<bool>(value).copied().unwrap_or(false);
        }
        Option::Title => {
            let str_ = val::<LibString>(value).map_or(&b""[..], |v| lib_string_slice(*v));
            if wrapper.terminal_mut().set_title(str_).is_err() {
                return Result::OutOfMemory;
            }
        }
        Option::Pwd => {
            let str_ = val::<LibString>(value).map_or(&b""[..], |v| lib_string_slice(*v));
            if wrapper.terminal_mut().set_pwd(str_).is_err() {
                return Result::OutOfMemory;
            }
        }
        Option::TerminfoName => {
            let str_ = val::<LibString>(value).map_or(&b""[..], |v| lib_string_slice(*v));
            if str_.len() > wrapper.terminfo_name_buf.len() {
                return Result::InvalidValue;
            }
            wrapper.terminfo_name_buf[..str_.len()].copy_from_slice(str_);
            wrapper.stream.handler.terminfo_name = if !str_.is_empty() {
                // SAFETY: the buffer lives in the heap-stable wrapper, which
                // outlives its stream.
                Some(std::slice::from_raw_parts(wrapper.terminfo_name_buf.as_ptr(), str_.len()))
            } else {
                None
            };
        }
        Option::ColorForeground => {
            let t = wrapper.terminal_mut();
            t.colors.foreground.default = val::<color::RgbC>(value).map(|v| color::Rgb::from_c(*v));
            t.flags.dirty.palette = true;
        }
        Option::ColorBackground => {
            let t = wrapper.terminal_mut();
            t.colors.background.default = val::<color::RgbC>(value).map(|v| color::Rgb::from_c(*v));
            t.flags.dirty.palette = true;
        }
        Option::ColorCursor => {
            let t = wrapper.terminal_mut();
            t.colors.cursor.default = val::<color::RgbC>(value).map(|v| color::Rgb::from_c(*v));
            t.flags.dirty.palette = true;
        }
        Option::ColorPalette => {
            let t = wrapper.terminal_mut();
            t.colors.palette.change_default(match val::<color::PaletteC>(value) {
                Some(v) => color::palette_zval(v),
                None => color::DEFAULT,
            });
            t.flags.dirty.palette = true;
        }
        Option::KittyImageStorageLimit => {
            if !build_options::KITTY_GRAPHICS {
                return Result::Success;
            }
            let limit: usize = val::<u64>(value).map_or(0, |v| *v as usize);
            let t = wrapper.terminal_mut();
            for (_, mut screen) in t.screens.iter() {
                let screen = &mut *screen;
                screen.kitty_images.set_limit(&mut screen.pages, limit);
            }
        }
        Option::KittyImageMediumFile | Option::KittyImageMediumSharedMem => {
            if !build_options::KITTY_GRAPHICS {
                return Result::Success;
            }
            let Some(&v) = val::<bool>(value) else {
                return Result::Success;
            };
            let t = wrapper.terminal_mut();
            for (_, mut screen) in t.screens.iter() {
                match option {
                    Option::KittyImageMediumFile => screen.kitty_images.image_limits.file = v,
                    Option::KittyImageMediumSharedMem => screen.kitty_images.image_limits.shared_memory = v,
                    _ => unreachable!(),
                }
            }
        }
        Option::KittyImageMediumTempFile => {
            if !build_options::KITTY_GRAPHICS {
                return Result::Success;
            }
            if let Some(v) = val::<LibString>(value) {
                if v.len > MAX_PATH_BYTES {
                    return Result::OutOfMemory;
                }
                let path = lib_string_slice(*v).to_vec();
                let t = wrapper.terminal_mut();
                for (_, mut screen) in t.screens.iter() {
                    screen.kitty_images.image_limits.temporary_file = TemporaryFile::Enabled {
                        directory: path.clone(),
                    };
                }

                // Every screen points at the new copy now so the previous
                // one can be released.
                wrapper.tmp_dir_path = Some(path);
            } else {
                let t = wrapper.terminal_mut();
                for (_, mut screen) in t.screens.iter() {
                    screen.kitty_images.image_limits.temporary_file = TemporaryFile::Disabled;
                }
                wrapper.tmp_dir_path = None;
            }
        }
        Option::ApcMaxBytes => {
            wrapper.stream.handler.apc_handler.max_bytes = match val::<usize>(value) {
                Some(&v) => crate::terminal::apc::MaxBytes::init_full(v),
                None => crate::terminal::apc::MaxBytes::init(None, None),
            };
        }
        Option::ApcMaxBytesKitty => {
            let max_bytes = &mut wrapper.stream.handler.apc_handler.max_bytes;
            match val::<usize>(value) {
                Some(&v) => max_bytes.put(crate::terminal::apc::Protocol::Kitty, v),
                None => max_bytes.remove(crate::terminal::apc::Protocol::Kitty),
            }
        }
        Option::GlyphProtocol => {
            let Some(&enabled) = val::<bool>(value) else {
                return Result::Success;
            };
            wrapper
                .stream
                .handler
                .apc_handler
                .enable(crate::terminal::apc::Protocol::Glyph, enabled);
            if !enabled {
                wrapper.terminal_mut().glyph_glossary.clear_and_free();
            }
        }
        Option::Selection => {
            let t = wrapper.terminal_mut();
            if let Some(ptr) = val::<selection_c::CSelection>(value) {
                let Some(sel) = ptr.to_zig() else {
                    return Result::InvalidValue;
                };
                if t.screens.active.select(Some(sel)).is_err() {
                    return Result::OutOfMemory;
                }
            } else {
                t.screens.active.clear_selection();
            }
        }
        Option::DefaultCursorStyle => {
            let raw = val::<c_int>(value).copied().unwrap_or(TerminalCursorStyle::Block as c_int);
            let Some(style) = TerminalCursorStyle::from_int(raw).map(TerminalCursorStyle::to_zig) else {
                return Result::InvalidValue;
            };
            wrapper.terminal_mut().set_default_cursor_style(style);
        }
        Option::DefaultCursorBlink => {
            let blink = val::<bool>(value).copied().unwrap_or(false);
            wrapper.terminal_mut().set_default_cursor_blink(Some(blink));
        }
        Option::ScrollbackMaxBytes => {
            wrapper
                .terminal_mut()
                .set_scrollback_max_bytes(val::<usize>(value).copied());
        }
        Option::ScrollbackMaxLines => {
            wrapper
                .terminal_mut()
                .set_scrollback_max_lines(val::<usize>(value).copied());
        }
        Option::ContinuationMaxBytes => {
            set_continuation_max_bytes(
                wrapper,
                val::<usize>(value).copied().unwrap_or(DEFAULT_CONTINUATION_MAX_BYTES),
            );
        }
        Option::UnknownMaxBytes => {
            // One limit applies to every unknown sequence type.
            let max_bytes = val::<usize>(value).copied().unwrap_or(0);
            wrapper.stream.handler.apc_handler.unknown_max_bytes = max_bytes;
            wrapper.stream.parser.osc_parser.unknown_max_bytes = max_bytes;
        }
        Option::ClipboardWriteMaxBytes => {
            wrapper.stream.handler.kitty_clipboard_write_max_bytes =
                val::<usize>(value).copied().unwrap_or(kitty_clipboard::MAX_WRITE_SIZE);
        }
        Option::ResizePullScrollback => {
            wrapper.terminal_mut().flags.resize_pull_scrollback = val::<bool>(value).copied().unwrap_or(true);
        }
        Option::XtChecksumExtension => {
            let bits = val::<u8>(value).copied().unwrap_or(0);
            // `std.math.cast(u5, bits)`
            if bits > 0x1F {
                return Result::InvalidValue;
            }
            wrapper
                .terminal_mut()
                .set_default_xt_checksum(crate::terminal::xt_checksum::Flags::from_bits(bits));
        }
        Option::Mode | Option::ModeDefault => {
            let Some(&config) = val::<ModeConfig>(value) else {
                return Result::InvalidValue;
            };
            let Some(mode) = config.to_mode() else {
                return Result::InvalidValue;
            };
            let t = wrapper.terminal_mut();
            match option {
                Option::Mode => t.modes.set(mode, config.value),
                Option::ModeDefault => {
                    if !modes::default_configurable(mode) {
                        return Result::InvalidValue;
                    }
                    t.modes.set_default(mode, config.value);
                }
                _ => unreachable!(),
            }
        }
    }
    Result::Success
}

crate::c_api_enum! {
    /// C: GhosttyTerminalCursorStyle
    pub enum TerminalCursorStyle {
        Bar = 0,
        Block = 1,
        Underline = 2,
        BlockHollow = 3,
    }
}

impl TerminalCursorStyle {
    fn to_zig(self) -> CursorStyle {
        match self {
            TerminalCursorStyle::Bar => CursorStyle::Bar,
            TerminalCursorStyle::Block => CursorStyle::Block,
            TerminalCursorStyle::Underline => CursorStyle::Underline,
            TerminalCursorStyle::BlockHollow => CursorStyle::BlockHollow,
        }
    }
}

/// C: GhosttyTerminalScrollViewport
pub type ScrollViewport = ScrollViewportC;

/// C: ghostty_terminal_scroll_viewport
///
/// # Safety
/// `terminal_` must be NULL or live; `behavior.tag` must be a valid tag.
#[export_name = "ghostty_terminal_scroll_viewport"]
pub unsafe extern "C" fn scroll_viewport(terminal_: Terminal, behavior: ScrollViewport) {
    let Some(t) = zig_terminal(terminal_) else {
        return;
    };
    t.scroll_viewport(match behavior.tag {
        ScrollViewportTag::Top => ZigScrollViewport::Top,
        ScrollViewportTag::Bottom => ZigScrollViewport::Bottom,
        ScrollViewportTag::Delta => ZigScrollViewport::Delta(behavior.value.delta),
        ScrollViewportTag::Row => ZigScrollViewport::Row(behavior.value.row),
        ScrollViewportTag::DeltaPrompt => ZigScrollViewport::DeltaPrompt(behavior.value.delta_prompt),
    });
}

/// C: ghostty_terminal_resize
///
/// # Safety
/// `terminal_` must be NULL or live.
#[export_name = "ghostty_terminal_resize"]
pub unsafe extern "C" fn resize(
    terminal_: Terminal,
    cols: CellCountInt,
    rows: CellCountInt,
    cell_width_px: u32,
    cell_height_px: u32,
) -> Result {
    let Some(wrapper) = terminal_.as_mut() else {
        return Result::InvalidValue;
    };
    match wrapper.stream.handler.resize(Resize {
        cols,
        rows,
        cell_size_px: Some(CellSizePx {
            width: cell_width_px,
            height: cell_height_px,
        }),
    }) {
        Ok(()) => Result::Success,
        Err(ResizeError::InvalidValue) => Result::InvalidValue,
        Err(ResizeError::OutOfMemory) => Result::OutOfMemory,
    }
}

/// C: ghostty_terminal_reset
///
/// # Safety
/// `terminal_` must be NULL or live.
#[export_name = "ghostty_terminal_reset"]
pub unsafe extern "C" fn reset(terminal_: Terminal) {
    let Some(wrapper) = terminal_.as_mut() else {
        return;
    };
    let t = wrapper.terminal_mut();

    // A reset always turns off synchronized output, ending its hold.
    let sync = t.modes.get(Mode::SynchronizedOutput);
    t.full_reset();
    if sync {
        Effects::render_hold_trampoline(&mut wrapper.stream.handler, false);
    }
}

/// C: GhosttyKittyGraphics
pub type KittyGraphics = kitty_gfx_c::KittyGraphics;

crate::c_api_enum! {
    /// C: GhosttyTerminalScreen (Zig: `ScreenSet.Key`)
    pub enum TerminalScreen {
        Primary = 0,
        Alternate = 1,
    }
}

impl TerminalScreen {
    pub fn from_zig(k: ScreenKey) -> TerminalScreen {
        match k {
            ScreenKey::Primary => TerminalScreen::Primary,
            ScreenKey::Alternate => TerminalScreen::Alternate,
        }
    }

    pub fn to_zig(self) -> ScreenKey {
        match self {
            TerminalScreen::Primary => ScreenKey::Primary,
            TerminalScreen::Alternate => ScreenKey::Alternate,
        }
    }
}

/// C: GhosttyTerminalScrollbar
pub type TerminalScrollbar = ScrollbarC;

/// C: GhosttyTerminalMemoryUsage
///
/// This is a sized struct, so new fields may only be added to the end.
/// The figures for each screen are separate fields rather than a nested
/// struct per screen, because only the outermost struct can grow. Add a
/// new per-screen field as a primary and alternate pair.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalMemoryUsage {
    pub size: usize,
    pub compression_supported: bool,
    pub primary_pages: u64,
    pub primary_virtual_bytes: u64,
    pub primary_resident_bytes: u64,
    pub primary_compressed_pages: u64,
    pub primary_compressed_bytes: u64,
    pub primary_image_bytes: u64,
    pub alternate_pages: u64,
    pub alternate_virtual_bytes: u64,
    pub alternate_resident_bytes: u64,
    pub alternate_compressed_pages: u64,
    pub alternate_compressed_bytes: u64,
    pub alternate_image_bytes: u64,
}

impl Default for TerminalMemoryUsage {
    fn default() -> Self {
        TerminalMemoryUsage {
            size: std::mem::size_of::<TerminalMemoryUsage>(),
            compression_supported: false,
            primary_pages: 0,
            primary_virtual_bytes: 0,
            primary_resident_bytes: 0,
            primary_compressed_pages: 0,
            primary_compressed_bytes: 0,
            primary_image_bytes: 0,
            alternate_pages: 0,
            alternate_virtual_bytes: 0,
            alternate_resident_bytes: 0,
            alternate_compressed_pages: 0,
            alternate_compressed_bytes: 0,
            alternate_image_bytes: 0,
        }
    }
}

impl TerminalMemoryUsage {
    /// Gather the memory usage of `t`. The size field is set to
    /// `caller_size` instead of our own size, so that copying the result
    /// into the caller's struct keeps the size they passed in.
    fn init(t: &ZigTerminal, caller_size: usize) -> TerminalMemoryUsage {
        let mut result = TerminalMemoryUsage {
            size: caller_size,
            compression_supported: terminal_mem::can_reclaim(terminal_mem::DecommitMode::Strict),
            ..Default::default()
        };

        let primary = t.screens.get(ScreenKey::Primary).unwrap().memory_usage();
        result.primary_pages = primary.pages.pages as u64;
        result.primary_virtual_bytes = primary.pages.virtual_bytes as u64;
        result.primary_resident_bytes = primary.pages.resident_bytes as u64;
        result.primary_compressed_pages = primary.pages.compressed_pages as u64;
        result.primary_compressed_bytes = primary.pages.compressed_bytes as u64;
        result.primary_image_bytes = primary.image_bytes as u64;

        // The alternate screen is created on first use. Until then its
        // fields stay zero.
        if let Some(screen) = t.screens.get(ScreenKey::Alternate) {
            let alternate = screen.memory_usage();
            result.alternate_pages = alternate.pages.pages as u64;
            result.alternate_virtual_bytes = alternate.pages.virtual_bytes as u64;
            result.alternate_resident_bytes = alternate.pages.resident_bytes as u64;
            result.alternate_compressed_pages = alternate.pages.compressed_pages as u64;
            result.alternate_compressed_bytes = alternate.pages.compressed_bytes as u64;
            result.alternate_image_bytes = alternate.image_bytes as u64;
        }

        result
    }
}

crate::c_api_enum! {
    /// C: GhosttyTerminalData
    ///
    /// The output type of each kind is documented in the header (Zig:
    /// `TerminalData.OutType`).
    pub enum TerminalData {
        Invalid = 0,
        Cols = 1,
        Rows = 2,
        CursorX = 3,
        CursorY = 4,
        CursorPendingWrap = 5,
        ActiveScreen = 6,
        CursorVisible = 7,
        KittyKeyboardFlags = 8,
        Scrollbar = 9,
        CursorStyle = 10,
        MouseTracking = 11,
        Title = 12,
        Pwd = 13,
        TotalRows = 14,
        ScrollbackRows = 15,
        WidthPx = 16,
        HeightPx = 17,
        ColorForeground = 18,
        ColorBackground = 19,
        ColorCursor = 20,
        ColorPalette = 21,
        ColorForegroundDefault = 22,
        ColorBackgroundDefault = 23,
        ColorCursorDefault = 24,
        ColorPaletteDefault = 25,
        KittyImageStorageLimit = 26,
        KittyImageMediumFile = 27,
        KittyImageMediumTempFile = 28,
        KittyImageMediumSharedMem = 29,
        KittyGraphics = 30,
        Selection = 31,
        ViewportActive = 32,
        VtProcessingError = 33,
        ScrollbackMaxBytes = 34,
        ScrollbackMaxLines = 35,
        ContinuationMaxBytes = 36,
        Mode = 37,
        VtGround = 38,
        CursorAtPrompt = 39,
        ClipboardWriteMaxBytes = 40,
        MouseShape = 41,
        MemoryUsage = 42,
    }
}

/// C: ghostty_terminal_get
///
/// # Safety
/// `terminal_` must be NULL or live; `out` must be NULL or point to the
/// kind's output type.
#[export_name = "ghostty_terminal_get"]
pub unsafe extern "C" fn get(terminal_: Terminal, data: c_int, out: *mut c_void) -> Result {
    let Some(data) = TerminalData::from_int(data) else {
        log::warn!(target: LOG_TARGET, "terminal_get invalid data value={data}");
        return Result::InvalidValue;
    };

    if out.is_null() {
        return Result::InvalidValue;
    }

    match data {
        TerminalData::Invalid => Result::InvalidValue,
        _ => get_typed(terminal_, data, out),
    }
}

/// C: ghostty_terminal_get_multi
///
/// # Safety
/// `keys` and `values` must be NULL or valid for `count` entries; see
/// [`get`].
#[export_name = "ghostty_terminal_get_multi"]
pub unsafe extern "C" fn get_multi(
    terminal_: Terminal,
    count: usize,
    keys: *const c_int,
    values: *const *mut c_void,
    out_written: *mut usize,
) -> Result {
    if keys.is_null() || values.is_null() {
        return Result::InvalidValue;
    }

    for i in 0..count {
        let result = get(terminal_, *keys.add(i), *values.add(i));
        if result != Result::Success {
            if let Some(w) = out_written.as_mut() {
                *w = i;
            }
            return result;
        }
    }
    if let Some(w) = out_written.as_mut() {
        *w = count;
    }
    Result::Success
}

unsafe fn get_typed(terminal_: Terminal, data: TerminalData, out: *mut c_void) -> Result {
    let Some(wrapper) = terminal_.as_mut() else {
        return Result::InvalidValue;
    };
    let ground = wrapper.stream.ground();
    let semantic_failure = wrapper.stream.handler.semantic_failure;
    let continuation_max = continuation_max_bytes(wrapper);
    let clipboard_write_max_bytes = wrapper.stream.handler.kitty_clipboard_write_max_bytes;
    let t = wrapper.terminal_mut();

    macro_rules! write {
        ($ty:ty, $v:expr) => {
            *(out as *mut $ty) = $v
        };
    }

    match data {
        TerminalData::Invalid => return Result::InvalidValue,
        TerminalData::Cols => write!(CellCountInt, t.cols),
        TerminalData::Rows => write!(CellCountInt, t.rows),
        TerminalData::CursorX => write!(CellCountInt, t.screens.active.cursor.x),
        TerminalData::CursorY => write!(CellCountInt, t.screens.active.cursor.y),
        TerminalData::CursorPendingWrap => write!(bool, t.screens.active.cursor.pending_wrap),
        TerminalData::ActiveScreen => write!(TerminalScreen, TerminalScreen::from_zig(t.screens.active_key)),
        TerminalData::CursorVisible => write!(bool, t.modes.get(Mode::CursorVisible)),
        TerminalData::KittyKeyboardFlags => write!(u8, t.screens.active.kitty_keyboard.current().int()),
        TerminalData::Scrollbar => write!(TerminalScrollbar, t.screens.active.pages.scrollbar().cval()),
        TerminalData::CursorStyle => write!(style_c::Style, style_c::Style::from_style(&t.screens.active.cursor.style)),
        TerminalData::MouseTracking => write!(
            bool,
            t.modes.get(Mode::MouseEventX10)
                || t.modes.get(Mode::MouseEventNormal)
                || t.modes.get(Mode::MouseEventButton)
                || t.modes.get(Mode::MouseEventAny)
        ),
        TerminalData::MouseShape => write!(c_int, t.mouse_shape as c_int),
        TerminalData::Title => write!(LibString, {
            let title = t.get_title().unwrap_or(b"");
            LibString {
                ptr: title.as_ptr(),
                len: title.len(),
            }
        }),
        TerminalData::Pwd => write!(LibString, {
            let pwd = t.get_pwd().unwrap_or(b"");
            LibString {
                ptr: pwd.as_ptr(),
                len: pwd.len(),
            }
        }),
        TerminalData::TotalRows => write!(usize, t.screens.active.pages.total_rows),
        TerminalData::ScrollbackRows => write!(usize, t.screens.active.pages.total_rows - t.rows as usize),
        TerminalData::WidthPx => write!(u32, t.width_px),
        TerminalData::HeightPx => write!(u32, t.height_px),
        TerminalData::ColorForeground => {
            let Some(c) = t.colors.foreground.get() else {
                return Result::NoValue;
            };
            write!(color::RgbC, c.cval())
        }
        TerminalData::ColorBackground => {
            let Some(c) = t.colors.background.get() else {
                return Result::NoValue;
            };
            write!(color::RgbC, c.cval())
        }
        TerminalData::ColorCursor => {
            let Some(c) = t.colors.cursor.get() else {
                return Result::NoValue;
            };
            write!(color::RgbC, c.cval())
        }
        TerminalData::ColorForegroundDefault => {
            let Some(c) = t.colors.foreground.default else {
                return Result::NoValue;
            };
            write!(color::RgbC, c.cval())
        }
        TerminalData::ColorBackgroundDefault => {
            let Some(c) = t.colors.background.default else {
                return Result::NoValue;
            };
            write!(color::RgbC, c.cval())
        }
        TerminalData::ColorCursorDefault => {
            let Some(c) = t.colors.cursor.default else {
                return Result::NoValue;
            };
            write!(color::RgbC, c.cval())
        }
        TerminalData::ColorPalette => write!(color::PaletteC, color::palette_cval(&t.colors.palette.current)),
        TerminalData::ColorPaletteDefault => write!(color::PaletteC, color::palette_cval(t.colors.palette.original())),
        TerminalData::KittyImageStorageLimit => {
            if !build_options::KITTY_GRAPHICS {
                return Result::NoValue;
            }
            write!(u64, t.screens.active.kitty_images.total_limit as u64)
        }
        TerminalData::KittyImageMediumFile => {
            if !build_options::KITTY_GRAPHICS {
                return Result::NoValue;
            }
            write!(bool, t.screens.active.kitty_images.image_limits.file)
        }
        TerminalData::KittyImageMediumTempFile => {
            if !build_options::KITTY_GRAPHICS {
                return Result::NoValue;
            }
            let screen = &*t.screens.active;
            let dir: &[u8] = match &screen.kitty_images.image_limits.temporary_file {
                TemporaryFile::Enabled { directory } => directory,
                TemporaryFile::Disabled => b"",
            };
            write!(LibString, LibString::init(dir))
        }
        TerminalData::KittyImageMediumSharedMem => {
            if !build_options::KITTY_GRAPHICS {
                return Result::NoValue;
            }
            write!(bool, t.screens.active.kitty_images.image_limits.shared_memory)
        }
        TerminalData::KittyGraphics => {
            if !build_options::KITTY_GRAPHICS {
                return Result::NoValue;
            }
            let screen = t.screens.active.as_ptr();
            write!(KittyGraphics, std::ptr::addr_of_mut!((*screen).kitty_images))
        }
        TerminalData::Selection => {
            let Some(sel) = t.screens.active.selection else {
                return Result::NoValue;
            };
            write!(selection_c::CSelection, selection_c::CSelection::from_zig(sel))
        }
        TerminalData::ViewportActive => write!(
            bool,
            t.screens.active.pages.viewport == crate::terminal::page_list::Viewport::Active
        ),
        TerminalData::VtProcessingError => write!(bool, semantic_failure),
        TerminalData::VtGround => write!(bool, ground),
        TerminalData::ScrollbackMaxBytes => {
            let max = t.screens.get(ScreenKey::Primary).unwrap().pages.limits.bytes.explicit;
            if max == usize::MAX {
                return Result::NoValue;
            }
            write!(usize, max)
        }
        TerminalData::ScrollbackMaxLines => {
            let max = t.screens.get(ScreenKey::Primary).unwrap().pages.limits.lines.explicit;
            if max == usize::MAX {
                return Result::NoValue;
            }
            write!(usize, max)
        }
        TerminalData::ContinuationMaxBytes => write!(usize, continuation_max),
        TerminalData::ClipboardWriteMaxBytes => write!(usize, clipboard_write_max_bytes),
        TerminalData::Mode => {
            let out = &mut *(out as *mut ModeConfig);
            let Some(mode) = out.to_mode() else {
                return Result::InvalidValue;
            };
            out.value = t.modes.get(mode);
        }
        TerminalData::CursorAtPrompt => write!(bool, t.cursor_is_at_prompt()),
        TerminalData::MemoryUsage => {
            // A smaller size means the caller doesn't have every field of
            // the first version of this struct, so reject it. A larger size
            // means the caller was built against a newer version with more
            // fields. We write only the fields we know and leave the rest
            // alone.
            //
            // When fields are added later, compare against the size of the
            // first version here, and copy only min(out.size, current size)
            // bytes.
            let out = out as *mut TerminalMemoryUsage;
            let size = (*out).size;
            if size < std::mem::size_of::<TerminalMemoryUsage>() {
                return Result::InvalidValue;
            }
            *out = TerminalMemoryUsage::init(t, size);
        }
    }

    Result::Success
}

/// C: ghostty_terminal_grid_ref
///
/// # Safety
/// `terminal_` must be NULL or live; a non-null `out_ref` must be writable.
#[export_name = "ghostty_terminal_grid_ref"]
pub unsafe extern "C" fn grid_ref(terminal_: Terminal, pt: point::PointC, out_ref: *mut grid_ref_c::CGridRef) -> Result {
    let Some(t) = zig_terminal(terminal_) else {
        return Result::InvalidValue;
    };
    let zig_pt = point::Point::from_c(pt);
    let Some(p) = t.screens.active.pages.pin(zig_pt) else {
        return Result::InvalidValue;
    };
    if let Some(out) = out_ref.as_mut() {
        *out = grid_ref_c::CGridRef::from_pin(p);
    }
    Result::Success
}

/// C: ghostty_terminal_grid_ref_track
///
/// # Safety
/// `terminal_` must be NULL or live; a non-null `out_ref` must be writable.
#[export_name = "ghostty_terminal_grid_ref_track"]
pub unsafe extern "C" fn grid_ref_track(
    terminal_: Terminal,
    pt: point::PointC,
    out_ref: *mut grid_ref_tracked_c::CTrackedGridRef,
) -> Result {
    let Some(wrapper) = terminal_.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(out) = out_ref.as_mut() else {
        return Result::InvalidValue;
    };
    *out = std::ptr::null_mut();

    let alloc = wrapper.alloc;
    let t = wrapper.terminal_mut();
    let mut screen = t.screens.active;
    let list: &mut PageList = &mut screen.pages;
    let Some(p) = list.pin(point::Point::from_c(pt)) else {
        return Result::InvalidValue;
    };
    let Ok(tracked_pin) = list.track_pin(p) else {
        return Result::OutOfMemory;
    };

    let screen_key = TerminalScreen::from_zig(t.screens.active_key);
    let screen_generation = t.screens.generation(t.screens.active_key);
    let r = alloc.create(grid_ref_tracked_c::TrackedGridRef {
        alloc,
        terminal: terminal_,
        screen_key,
        screen_generation,
        pin: tracked_pin,
    });
    if r.is_null() {
        list.untrack_pin(tracked_pin);
        return Result::OutOfMemory;
    }

    // Store the tracked ref in the terminal so that when we free
    // the terminal the tracked ref can be detached safely.
    wrapper.tracked_grid_refs.push(r);

    *out = r;
    Result::Success
}

/// C: ghostty_terminal_point_from_grid_ref
///
/// # Safety
/// `terminal_` must be NULL or live; `ref_` must be valid and its node live;
/// a non-null `out` must be writable.
#[export_name = "ghostty_terminal_point_from_grid_ref"]
pub unsafe extern "C" fn point_from_grid_ref(
    terminal_: Terminal,
    ref_: &grid_ref_c::CGridRef,
    tag: c_int,
    out: *mut point::Coordinate,
) -> Result {
    let Some(t) = zig_terminal(terminal_) else {
        return Result::InvalidValue;
    };
    let Some(tag) = point_tag_from_int(tag) else {
        return Result::InvalidValue;
    };
    let Some(p) = ref_.to_pin() else {
        return Result::InvalidValue;
    };
    let Some(pt) = t.screens.active.pages.point_from_pin(tag, p) else {
        return Result::NoValue;
    };
    if let Some(o) = out.as_mut() {
        *o = pt.coord();
    }
    Result::Success
}

/// C: ghostty_terminal_free
///
/// # Safety
/// `terminal_` must be NULL or a terminal from [`new`] not used afterwards.
#[export_name = "ghostty_terminal_free"]
pub unsafe extern "C" fn free(terminal_: Terminal) {
    let Some(wrapper) = terminal_.as_mut() else {
        return;
    };

    for &r in &wrapper.tracked_grid_refs {
        (*r).terminal = std::ptr::null_mut();
    }
    wrapper.tracked_grid_refs = Vec::new();
    for &search in &wrapper.searches {
        (*search).terminal = std::ptr::null_mut();
    }
    wrapper.searches = Vec::new();
    let alloc = wrapper.alloc;
    alloc.destroy(terminal_);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Zig's `std.mem.Allocator.failing`: every allocation fails.
    struct FailingZig;

    impl ZigAllocator for FailingZig {
        fn alloc(&self, _len: usize, _alignment: u8, _ra: usize) -> *mut u8 {
            std::ptr::null_mut()
        }

        fn resize(&self, _m: *mut u8, _old: usize, _a: u8, _new: usize, _ra: usize) -> bool {
            false
        }

        fn remap(&self, _m: *mut u8, _old: usize, _a: u8, _new: usize, _ra: usize) -> *mut u8 {
            std::ptr::null_mut()
        }

        fn free(&self, _m: *mut u8, _len: usize, _a: u8, _ra: usize) {}
    }

    static FAILING_ZIG: FailingZig = FailingZig;

    struct StaticDyn(&'static dyn ZigAllocator);
    // SAFETY: wraps a stateless allocator.
    unsafe impl Sync for StaticDyn {}
    static FAILING_DYN: StaticDyn = StaticDyn(&FAILING_ZIG);

    /// `CAllocator.fromZig(&std.mem.Allocator.failing)`.
    pub(crate) static FAILING_C_ALLOCATOR: CAllocator = CAllocator::from_zig(&FAILING_DYN.0);

    // -----------------------------------------------------------------
    // Rust test helpers (Zig tests spell these out inline).

    use crate::lib::allocator::C_ALLOCATOR;
    use crate::terminal::c::allocator as allocator_c;
    use std::cell::{Cell, RefCell};

    fn new_t(cols: CellCountInt, rows: CellCountInt) -> Terminal {
        let mut t: Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut t, cols, rows));
        assert!(!t.is_null());
        t
    }

    fn p<T>(v: &T) -> *const c_void {
        v as *const T as *const c_void
    }

    fn o<T>(v: &mut T) -> *mut c_void {
        v as *mut T as *mut c_void
    }

    fn vw(t: Terminal, data: &[u8]) {
        unsafe { vt_write(t, data.as_ptr(), data.len()) }
    }

    fn zt<'a>(t: Terminal) -> &'a mut ZigTerminal {
        unsafe { zig_terminal(t).unwrap() }
    }

    fn wr<'a>(t: Terminal) -> &'a mut TerminalWrapper {
        unsafe { &mut *t }
    }

    fn plain(t: Terminal) -> String {
        zt(t).plain_string().unwrap()
    }

    fn sopt(t: Terminal, option: Option, value: *const c_void) -> Result {
        unsafe { set(t, option as c_int, value) }
    }

    fn gdat(t: Terminal, data: TerminalData, out: *mut c_void) -> Result {
        unsafe { get(t, data as c_int, out) }
    }

    fn mode_tag(value: u16, ansi: bool) -> ModeTagBacking {
        ModeTag::new(value, ansi).to_bits()
    }

    fn enable_continuation(t: Terminal) {
        let limit: usize = 1024;
        assert_eq!(Result::Success, sopt(t, Option::ContinuationMaxBytes, p(&limit)));
    }

    fn cont_buf(t: Terminal, buf: Option2<&mut [u8]>, written: &mut usize) -> Result {
        unsafe {
            match buf {
                Some(b) => continuation_buf(t, b.as_mut_ptr(), b.len(), written),
                None => continuation_buf(t, std::ptr::null_mut(), 0, written),
            }
        }
    }

    type Option2<T> = std::option::Option<T>;

    fn sv(t: Terminal, v: ZigScrollViewport) {
        unsafe { scroll_viewport(t, v.cval()) }
    }

    fn scrollbar(t: Terminal) -> TerminalScrollbar {
        let mut s = TerminalScrollbar { total: 0, offset: 0, len: 0 };
        assert_eq!(Result::Success, gdat(t, TerminalData::Scrollbar, o(&mut s)));
        s
    }

    fn viewport_active(t: Terminal) -> bool {
        let mut v = false;
        assert_eq!(Result::Success, gdat(t, TerminalData::ViewportActive, o(&mut v)));
        v
    }

    // A shared write_pty capture (Zig: each test's `S.writePty`). Tests
    // run on their own threads, so thread-locals are per test.
    thread_local! {
        static LAST: RefCell<Option2<Vec<u8>>> = const { RefCell::new(None) };
        static ALL: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        static CALLS: Cell<usize> = const { Cell::new(0) };
        static LAST_UD: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
    }

    unsafe extern "C" fn write_pty_capture(_: Terminal, ud: *mut c_void, ptr: *const u8, len: usize) {
        let data = std::slice::from_raw_parts(ptr, len);
        LAST.with(|l| *l.borrow_mut() = Some(data.to_vec()));
        ALL.with(|a| a.borrow_mut().extend_from_slice(data));
        CALLS.set(CALLS.get() + 1);
        LAST_UD.set(ud);
    }

    fn last() -> Option2<Vec<u8>> {
        LAST.with(|l| l.borrow().clone())
    }

    fn clear_last() {
        LAST.with(|l| *l.borrow_mut() = None);
    }

    fn all_written() -> Vec<u8> {
        ALL.with(|a| a.borrow().clone())
    }

    fn clear_all() {
        ALL.with(|a| a.borrow_mut().clear());
    }

    fn set_write_pty(t: Terminal) {
        assert_eq!(
            Result::Success,
            sopt(t, Option::WritePty, write_pty_capture as WritePtyFn as *const c_void)
        );
    }

    fn cs(s: LibString) -> Vec<u8> {
        lib_string_slice(s).to_vec()
    }

    fn hex_upper(s: &[u8]) -> String {
        s.iter().map(|b| format!("{b:02X}")).collect()
    }

    // -----------------------------------------------------------------

    #[test]
    fn new_free() {
        let t = new_t(80, 24);
        unsafe { free(t) };
    }

    #[test]
    fn new_invalid_value() {
        let mut t: Terminal = std::ptr::null_mut();
        assert_eq!(Result::InvalidValue, new(Some(&C_ALLOCATOR), &mut t, 0, 24));
        assert!(t.is_null());

        assert_eq!(Result::InvalidValue, new(Some(&C_ALLOCATOR), &mut t, 80, 0));
        assert!(t.is_null());
    }

    #[test]
    fn continuation_option_and_data() {
        let t = new_t(80, 24);

        let mut value: usize = 0;
        assert_eq!(Result::Success, gdat(t, TerminalData::ContinuationMaxBytes, o(&mut value)));
        assert_eq!(DEFAULT_CONTINUATION_MAX_BYTES, value);

        let custom: usize = 1234;
        assert_eq!(Result::Success, sopt(t, Option::ContinuationMaxBytes, p(&custom)));
        assert_eq!(Result::Success, gdat(t, TerminalData::ContinuationMaxBytes, o(&mut value)));
        assert_eq!(custom, value);

        assert_eq!(Result::Success, sopt(t, Option::ContinuationMaxBytes, std::ptr::null()));
        assert_eq!(Result::Success, gdat(t, TerminalData::ContinuationMaxBytes, o(&mut value)));
        assert_eq!(DEFAULT_CONTINUATION_MAX_BYTES, value);

        let disabled: usize = 0;
        assert_eq!(Result::Success, sopt(t, Option::ContinuationMaxBytes, p(&disabled)));
        assert_eq!(Result::Success, gdat(t, TerminalData::ContinuationMaxBytes, o(&mut value)));
        assert_eq!(0, value);

        let mut written: usize = 99;
        assert_eq!(Result::InvalidValue, cont_buf(t, None, &mut written));
        assert_eq!(0, written);
        unsafe { free(t) };
    }

    #[test]
    fn continuation_buffer_and_allocator_export_exact_suffix() {
        let t = new_t(80, 24);
        enable_continuation(t);

        vw(t, b"A\x1b[31");

        let mut required: usize = 0;
        assert_eq!(Result::OutOfSpace, cont_buf(t, None, &mut required));
        assert_eq!(4, required);

        let mut short = [0u8; 2];
        assert_eq!(Result::OutOfSpace, cont_buf(t, Some(&mut short), &mut required));
        assert_eq!(4, required);

        let mut buf = [0u8; 8];
        let mut written: usize = 0;
        assert_eq!(Result::Success, cont_buf(t, Some(&mut buf), &mut written));
        assert_eq!(b"\x1b[31", &buf[..written]);

        unsafe {
            let mut out_ptr: *mut u8 = std::ptr::null_mut();
            let mut out_len: usize = 0;
            assert_eq!(
                Result::Success,
                continuation_alloc(t, Some(&C_ALLOCATOR), &mut out_ptr, &mut out_len)
            );
            assert!(!out_ptr.is_null());
            assert_eq!(b"\x1b[31", std::slice::from_raw_parts(out_ptr, out_len));
            allocator_c::free(Some(&C_ALLOCATOR), out_ptr, out_len);

            vw(t, b"m");
            assert_eq!(Result::OutOfSpace, cont_buf(t, None, &mut required));
            assert_eq!(0, required);

            // Completing the sequence must return an empty, allocation-free result.
            let failing = &FAILING_C_ALLOCATOR;
            assert_eq!(
                Result::Success,
                continuation_alloc(t, Some(failing), &mut out_ptr, &mut out_len)
            );
            assert!(out_ptr.is_null());
            assert_eq!(0, out_len);
            allocator_c::free(Some(failing), out_ptr, out_len);
            free(t);
        }
    }

    #[test]
    fn continuation_export_tracks_split_utf8() {
        let t = new_t(80, 24);
        enable_continuation(t);

        let prefix = [0xF0u8, 0x9F];
        vw(t, &prefix);

        let mut buf = [0u8; 4];
        let mut written: usize = 0;
        assert_eq!(Result::Success, cont_buf(t, Some(&mut buf), &mut written));
        assert_eq!(&prefix[..], &buf[..written]);

        let suffix = [0x98u8, 0x80];
        vw(t, &suffix);
        assert_eq!(Result::OutOfSpace, cont_buf(t, None, &mut written));
        assert_eq!(0, written);
        unsafe { free(t) };
    }

    #[test]
    fn continuation_callback_writer_reports_success_and_failure() {
        struct Sink {
            bytes: [u8; 8],
            len: usize,
        }

        unsafe extern "C" fn sink_write(userdata: *mut c_void, data: *const u8, len: usize) -> bool {
            let s = &mut *(userdata as *mut Sink);
            if len > s.bytes.len() - s.len {
                return false;
            }
            s.bytes[s.len..s.len + len].copy_from_slice(std::slice::from_raw_parts(data, len));
            s.len += len;
            true
        }

        unsafe extern "C" fn sink_fail(_: *mut c_void, _: *const u8, _: usize) -> bool {
            false
        }

        let t = new_t(80, 24);
        enable_continuation(t);
        vw(t, b"\x1b[31");

        unsafe {
            let mut sink = Sink { bytes: [0; 8], len: 0 };
            assert_eq!(
                Result::Success,
                continuation_write(
                    t,
                    c_io::Writer {
                        write: Some(sink_write),
                        userdata: o(&mut sink),
                    }
                )
            );
            assert_eq!(b"\x1b[31", &sink.bytes[..sink.len]);

            let fail = c_io::Writer {
                write: Some(sink_fail),
                userdata: std::ptr::null_mut(),
            };
            assert_eq!(Result::IoError, continuation_write(t, fail));
            assert_eq!(
                Result::InvalidValue,
                continuation_write(
                    t,
                    c_io::Writer {
                        write: None,
                        userdata: std::ptr::null_mut(),
                    }
                )
            );
            assert_eq!(Result::InvalidValue, continuation_write(std::ptr::null_mut(), fail));
            free(t);
        }
    }

    #[test]
    fn continuation_runtime_reconfiguration_recovers_safely() {
        let t = new_t(80, 24);

        let enough: usize = 8;
        assert_eq!(Result::Success, sopt(t, Option::ContinuationMaxBytes, p(&enough)));
        vw(t, b"\x1b[123");
        let too_small: usize = 2;
        assert_eq!(Result::Success, sopt(t, Option::ContinuationMaxBytes, p(&too_small)));

        let mut written: usize = 99;
        assert_eq!(Result::InvalidValue, cont_buf(t, None, &mut written));
        assert_eq!(0, written);

        // Raising the limit cannot reconstruct discarded bytes, but a new ESC is
        // a complete replay start and repairs tracking without first grounding.
        assert_eq!(Result::Success, sopt(t, Option::ContinuationMaxBytes, p(&enough)));
        vw(t, b"\x1b[");
        let mut buf = [0u8; 8];
        assert_eq!(Result::Success, cont_buf(t, Some(&mut buf), &mut written));
        assert_eq!(b"\x1b[", &buf[..written]);

        let disabled: usize = 0;
        assert_eq!(Result::Success, sopt(t, Option::ContinuationMaxBytes, p(&disabled)));
        assert_eq!(Result::Success, sopt(t, Option::ContinuationMaxBytes, p(&enough)));
        assert_eq!(Result::InvalidValue, cont_buf(t, None, &mut written));

        // Completing the sequence reaches ground and resets the broken marker.
        vw(t, b"m");
        assert_eq!(Result::OutOfSpace, cont_buf(t, None, &mut written));
        assert_eq!(0, written);
        unsafe { free(t) };
    }

    #[test]
    fn continuation_internal_allocation_and_restoration() {
        let source = new_t(80, 24);
        enable_continuation(source);
        vw(source, b"\x1b[31");

        unsafe {
            let bytes = continuation_alloc_io(source, false).unwrap();
            assert_eq!(b"\x1b[31", bytes.as_slice());

            let restored = new_t(80, 24);
            enable_continuation(restored);
            restore_continuation(restored, &bytes).unwrap();

            let reexported = continuation_alloc_io(restored, false).unwrap();
            assert_eq!(bytes, reexported);

            let invalid = new_t(80, 24);
            enable_continuation(invalid);
            assert_eq!(
                Err(RestoreContinuationError::InvalidContinuation),
                restore_continuation(invalid, b"A")
            );

            free(invalid);
            free(restored);
            free(source);
        }
    }

    #[test]
    fn set_scrollback_limits() {
        let t = new_t(80, 3);

        let primary = zt(t).screens.get(ScreenKey::Primary).unwrap();

        let max_bytes: usize = 0;
        assert_eq!(Result::Success, sopt(t, Option::ScrollbackMaxBytes, p(&max_bytes)));
        assert_eq!(0, primary.pages.limits.bytes.explicit);
        assert!(primary.no_scrollback);

        assert_eq!(Result::Success, sopt(t, Option::ScrollbackMaxBytes, std::ptr::null()));
        assert_eq!(usize::MAX, primary.pages.limits.bytes.explicit);
        assert!(!primary.no_scrollback);

        let max_lines: usize = 12;
        assert_eq!(Result::Success, sopt(t, Option::ScrollbackMaxLines, p(&max_lines)));
        assert_eq!(max_lines, primary.pages.limits.lines.explicit);

        assert_eq!(Result::Success, sopt(t, Option::ScrollbackMaxLines, std::ptr::null()));
        assert_eq!(usize::MAX, primary.pages.limits.lines.explicit);
        unsafe { free(t) };
    }

    #[test]
    fn free_null() {
        unsafe { free(std::ptr::null_mut()) };
    }

    #[test]
    fn scroll_viewport_test() {
        let t = new_t(5, 2);

        assert!(viewport_active(t));

        // Write "hello" on the first line
        vw(t, b"hello");

        // Push "hello" into scrollback with 3 newlines (index = ESC D)
        vw(t, b"\x1bD\x1bD\x1bD");
        // Viewport should be empty now since hello scrolled off
        assert_eq!("", plain(t));

        // Scroll to top: "hello" should be visible again
        sv(t, ZigScrollViewport::Top);
        assert!(!viewport_active(t));
        assert_eq!("hello", plain(t));

        // Scroll to bottom: viewport should be empty again
        sv(t, ZigScrollViewport::Bottom);
        assert!(viewport_active(t));
        assert_eq!("", plain(t));

        // Scroll up by delta to bring "hello" back into view
        sv(t, ZigScrollViewport::Delta(-3));
        assert!(!viewport_active(t));
        assert_eq!("hello", plain(t));
        unsafe { free(t) };
    }

    #[test]
    fn scroll_viewport_row() {
        let t = new_t(5, 2);

        // Write 4 rows so that rows "1" and "2" are pushed into scrollback:
        // total rows is 4, viewport length is 2.
        vw(t, b"1\r\n2\r\n3\r\n4");
        assert!(viewport_active(t));

        // Row 0 is the top of the scrollback.
        sv(t, ZigScrollViewport::Row(0));
        assert!(!viewport_active(t));
        assert_eq!("1\n2", plain(t));

        // An absolute row within the scrollback becomes the first visible
        // row and round-trips through the scrollbar offset.
        sv(t, ZigScrollViewport::Row(1));
        assert_eq!("2\n3", plain(t));
        let s = scrollbar(t);
        assert_eq!(4, s.total);
        assert_eq!(1, s.offset);
        assert_eq!(2, s.len);

        // A row past the end clamps to the active area.
        sv(t, ZigScrollViewport::Row(9999));
        assert!(viewport_active(t));
        assert_eq!("3\n4", plain(t));
        assert_eq!(2, scrollbar(t).offset);
        unsafe { free(t) };
    }

    #[test]
    fn scroll_viewport_row_alt_screen() {
        let t = new_t(5, 2);

        // Enter the alternate screen, which has no scrollback.
        vw(t, b"\x1b[?1049h");
        let mut screen = TerminalScreen::Primary;
        assert_eq!(Result::Success, gdat(t, TerminalData::ActiveScreen, o(&mut screen)));
        assert_eq!(TerminalScreen::Alternate, screen);

        // Scrolling to any row keeps the viewport on the active area.
        sv(t, ZigScrollViewport::Row(0));
        assert!(viewport_active(t));
        sv(t, ZigScrollViewport::Row(9999));
        assert!(viewport_active(t));

        // With no scrollback the scrollbar covers exactly the active area.
        let s = scrollbar(t);
        assert_eq!(2, s.total);
        assert_eq!(0, s.offset);
        assert_eq!(2, s.len);
        unsafe { free(t) };
    }

    #[test]
    fn scroll_viewport_delta_prompt() {
        let t = new_t(80, 2);

        let transcript = concat!(
            "\x1b]133;P;k=i\x07p1\r\n",
            "\x1b]133;B\x07cmd1\r\n",
            "\x1b]133;C\x07out1\r\n",
            "\x1b]133;P;k=i\x07p2\r\n",
            "\x1b]133;B\x07cmd2\r\n",
            "\x1b]133;C\x07out2\r\n",
            "\x1b]133;P;k=i\x07p3\r\n",
            "\x1b]133;B\x07cmd3\r\n",
            "\x1b]133;C\x07out3\r\n",
            "\x1b]133;P;k=i\x07p4",
        );
        vw(t, transcript.as_bytes());

        // A negative count goes to the previous prompt, and a positive count
        // returns to the next prompt relative to the current viewport top.
        sv(t, ZigScrollViewport::DeltaPrompt(-1));
        assert_eq!(6, scrollbar(t).offset);
        sv(t, ZigScrollViewport::DeltaPrompt(-1));
        assert_eq!(3, scrollbar(t).offset);
        sv(t, ZigScrollViewport::DeltaPrompt(1));
        assert_eq!(6, scrollbar(t).offset);

        // Counts beyond the retained prompt history clamp to the oldest or
        // newest matching prompt, including the signed minimum boundary.
        sv(t, ZigScrollViewport::DeltaPrompt(isize::MIN));
        assert_eq!(0, scrollbar(t).offset);
        sv(t, ZigScrollViewport::DeltaPrompt(isize::MAX));
        assert_eq!(8, scrollbar(t).offset);

        // A zero count leaves the selected prompt unchanged.
        sv(t, ZigScrollViewport::DeltaPrompt(0));
        assert_eq!(8, scrollbar(t).offset);
        unsafe { free(t) };
    }

    #[test]
    fn scroll_viewport_delta_prompt_continuation() {
        let t = new_t(80, 2);

        let transcript = concat!(
            "\x1b]133;P;k=i\x07p1\r\n",
            "\x1b]133;P;k=c\x07p1-continued\r\n",
            "\x1b]133;B\x07cmd1\r\n",
            "\x1b]133;C\x07out1\r\n",
            "\x1b]133;P;k=i\x07p2\r\n",
            "\x1b]133;B\x07cmd2\r\n",
            "\x1b]133;C\x07out2",
        );
        vw(t, transcript.as_bytes());

        // The continuation row belongs to the first prompt group. Moving
        // forward from that group lands on the second prompt.
        sv(t, ZigScrollViewport::Row(0));
        sv(t, ZigScrollViewport::DeltaPrompt(1));
        assert_eq!(4, scrollbar(t).offset);
        unsafe { free(t) };
    }

    #[test]
    fn scroll_viewport_delta_prompt_final_continuation_snaps_to_active() {
        let t = new_t(80, 3);

        let transcript = concat!(
            "\x1b]133;P;k=i\x07prompt\r\n",
            "\x1b]133;P;k=c\x07continued\r\n",
            "continued\r\ncontinued\r\ncontinued\r\ncontinued\r\ncontinued",
        );
        vw(t, transcript.as_bytes());
        sv(t, ZigScrollViewport::Row(1));
        assert_eq!(1, scrollbar(t).offset);

        sv(t, ZigScrollViewport::DeltaPrompt(1));
        assert_eq!(4, scrollbar(t).offset);
        unsafe { free(t) };
    }

    #[test]
    fn scroll_viewport_delta_prompt_no_markers_and_alternate_screen() {
        let t = new_t(80, 2);

        vw(t, b"one\r\ntwo\r\nthree\r\nfour");
        sv(t, ZigScrollViewport::Row(0));

        // Without OSC 133 prompt markers, a request leaves the viewport alone.
        sv(t, ZigScrollViewport::DeltaPrompt(1));
        assert_eq!(0, scrollbar(t).offset);

        vw(t, b"\x1b[?1049h");
        sv(t, ZigScrollViewport::DeltaPrompt(-1));
        let s = scrollbar(t);
        assert_eq!(0, s.offset);
        assert_eq!(2, s.total);
        unsafe { free(t) };
    }

    fn memory_usage(t: Terminal) -> TerminalMemoryUsage {
        let mut m = TerminalMemoryUsage::default();
        assert_eq!(Result::Success, gdat(t, TerminalData::MemoryUsage, o(&mut m)));
        m
    }

    fn compressible_history() -> Vec<u8> {
        let line = b"repeated and compressible terminal history\r\n";
        line.repeat(4_000)
    }

    #[test]
    fn get_memory_usage() {
        let t = new_t(80, 24);

        let fresh = memory_usage(t);
        assert_eq!(std::mem::size_of::<TerminalMemoryUsage>(), fresh.size);
        assert_eq!(
            terminal_mem::can_reclaim(terminal_mem::DecommitMode::Strict),
            fresh.compression_supported
        );
        assert!(fresh.primary_pages > 0);
        assert!(fresh.primary_virtual_bytes > 0);
        assert!(fresh.primary_resident_bytes <= fresh.primary_virtual_bytes);
        assert_eq!(0, fresh.primary_compressed_pages);
        assert_eq!(0, fresh.primary_compressed_bytes);
        assert_eq!(0, fresh.primary_image_bytes);

        // The alternate screen doesn't exist yet.
        assert_eq!(0, fresh.alternate_pages);
        assert_eq!(0, fresh.alternate_virtual_bytes);
        assert_eq!(0, fresh.alternate_resident_bytes);
        assert_eq!(0, fresh.alternate_compressed_pages);
        assert_eq!(0, fresh.alternate_compressed_bytes);
        assert_eq!(0, fresh.alternate_image_bytes);

        // Write compressible history.
        vw(t, &compressible_history());

        let history = memory_usage(t);
        assert!(history.primary_pages > fresh.primary_pages);
        assert!(history.primary_resident_bytes > fresh.primary_resident_bytes);

        let mut compression_result = CompressionResult::Unsupported;
        assert_eq!(Result::Success, unsafe {
            compress(t, CompressionMode::Full as c_int, &mut compression_result)
        });
        assert_eq!(
            history.compression_supported,
            compression_result == CompressionResult::Complete
        );

        let compressed = memory_usage(t);
        assert_eq!(history.primary_pages, compressed.primary_pages);
        assert_eq!(history.primary_virtual_bytes, compressed.primary_virtual_bytes);
        if compressed.compression_supported {
            assert!(compressed.primary_compressed_pages > 0);
            assert!(compressed.primary_compressed_bytes > 0);
            assert!(compressed.primary_resident_bytes < history.primary_resident_bytes);
        } else {
            assert_eq!(0, compressed.primary_compressed_pages);
        }

        // The query never restores a compressed page.
        let again = memory_usage(t);
        assert_eq!(compressed, again);

        // Kitty images are counted separately from pages.
        if build_options::KITTY_GRAPHICS {
            // 1x2 RGB image, 6 bytes of pixel data.
            vw(t, b"\x1b_Ga=t,t=d,f=24,i=1,s=1,v=2;////////\x1b\\");
            let images = memory_usage(t);
            assert!(images.primary_image_bytes > 0);

            vw(t, b"\x1b_Ga=d,d=I,i=1\x1b\\");
            let images = memory_usage(t);
            assert_eq!(0, images.primary_image_bytes);
        }

        // Entering the alternate screen creates it.
        vw(t, b"\x1b[?1049h");
        let alternate = memory_usage(t);
        assert!(alternate.alternate_pages > 0);
        assert!(alternate.alternate_virtual_bytes > 0);
        assert!(alternate.alternate_resident_bytes > 0);
        assert!(alternate.alternate_resident_bytes <= alternate.alternate_virtual_bytes);
        assert_eq!(compressed.primary_pages, alternate.primary_pages);
        unsafe { free(t) };
    }

    #[test]
    fn get_memory_usage_size_rule() {
        let t = new_t(80, 24);

        // A size smaller than the first layout is rejected and nothing is
        // written.
        let mut small = TerminalMemoryUsage {
            size: std::mem::size_of::<TerminalMemoryUsage>() - 1,
            primary_pages: 1234,
            ..Default::default()
        };
        assert_eq!(Result::InvalidValue, gdat(t, TerminalData::MemoryUsage, o(&mut small)));
        assert_eq!(std::mem::size_of::<TerminalMemoryUsage>() - 1, small.size);
        assert_eq!(1234, small.primary_pages);

        // A size larger than ours, from a caller built against a newer
        // layout, gets the known prefix and the rest is left untouched.
        #[repr(C)]
        struct Larger {
            usage: TerminalMemoryUsage,
            extra: u64,
        }
        let mut large = Larger {
            usage: TerminalMemoryUsage {
                size: std::mem::size_of::<Larger>(),
                ..Default::default()
            },
            extra: 0xDEADBEEF,
        };
        assert_eq!(Result::Success, gdat(t, TerminalData::MemoryUsage, o(&mut large)));
        assert_eq!(std::mem::size_of::<Larger>(), large.usage.size);
        assert!(large.usage.primary_pages > 0);
        assert_eq!(0xDEADBEEF, large.extra);
        unsafe { free(t) };
    }

    #[test]
    fn scroll_viewport_null() {
        sv(std::ptr::null_mut(), ZigScrollViewport::Top);
        sv(std::ptr::null_mut(), ZigScrollViewport::Row(1));
    }

    #[test]
    fn compression_invalid_arguments() {
        unsafe {
            let mut activity: u64 = 0;
            let mut compression_result = CompressionResult::Unsupported;

            assert_eq!(
                Result::InvalidValue,
                compression_activity(std::ptr::null_mut(), &mut activity)
            );
            assert_eq!(
                Result::InvalidValue,
                compress(
                    std::ptr::null_mut(),
                    CompressionMode::Incremental as c_int,
                    &mut compression_result
                )
            );

            let t = new_t(80, 24);

            assert_eq!(Result::InvalidValue, compression_activity(t, std::ptr::null_mut()));
            assert_eq!(
                Result::InvalidValue,
                compress(t, CompressionMode::Incremental as c_int, std::ptr::null_mut())
            );
            assert_eq!(Result::InvalidValue, compress(t, -1, &mut compression_result));
            free(t);
        }
    }

    #[test]
    fn compression_activity_and_incremental_scheduling() {
        unsafe {
            let t = new_t(80, 24);

            let mut initial_activity: u64 = 0;
            assert_eq!(Result::Success, compression_activity(t, &mut initial_activity));

            vw(t, &compressible_history());

            let mut activity: u64 = 0;
            assert_eq!(Result::Success, compression_activity(t, &mut activity));
            assert_ne!(activity, initial_activity);

            let mut compression_result = CompressionResult::Unsupported;
            let mut done = false;
            for _ in 0..1_000 {
                assert_eq!(
                    Result::Success,
                    compress(t, CompressionMode::Incremental as c_int, &mut compression_result)
                );

                match compression_result {
                    CompressionResult::Pending => continue,
                    CompressionResult::Complete => {
                        done = true;
                        break;
                    }
                    CompressionResult::Unsupported => unreachable!(),
                }
            }
            assert!(done, "TestUnexpectedResult");

            // Compression changes storage representation, not the activity token.
            let mut final_activity: u64 = 0;
            assert_eq!(Result::Success, compression_activity(t, &mut final_activity));
            assert_eq!(activity, final_activity);

            assert_eq!(
                Result::Success,
                compress(t, CompressionMode::Full as c_int, &mut compression_result)
            );
            assert_eq!(CompressionResult::Complete, compression_result);
            free(t);
        }
    }

    #[test]
    fn reset_test() {
        let t = new_t(80, 24);
        vw(t, b"Hello");
        unsafe { reset(t) };
        assert_eq!("", plain(t));
        unsafe { free(t) };
    }

    #[test]
    fn reset_null() {
        unsafe { reset(std::ptr::null_mut()) };
    }

    #[test]
    fn resize_test() {
        let t = new_t(80, 24);
        assert_eq!(Result::Success, unsafe { resize(t, 40, 12, 9, 18) });
        assert_eq!(40, zt(t).cols);
        assert_eq!(12, zt(t).rows);
        unsafe { free(t) };
    }

    #[test]
    fn resize_null() {
        assert_eq!(Result::InvalidValue, unsafe { resize(std::ptr::null_mut(), 80, 24, 9, 18) });
    }

    #[test]
    fn resize_invalid_value() {
        let t = new_t(80, 24);
        assert_eq!(Result::InvalidValue, unsafe { resize(t, 0, 24, 9, 18) });
        assert_eq!(Result::InvalidValue, unsafe { resize(t, 80, 0, 9, 18) });
        unsafe { free(t) };
    }

    #[test]
    fn resize_shrinks_both_axes_with_cursor_at_bottom() {
        let t = new_t(80, 24);

        // CSI 24;1H -> park the cursor on the bottom row (1-based).
        vw(t, b"\x1b[24;1H");

        // Shrink both axes; pre-resize cursor.y sits past the new bottom row.
        // Previously this underflowed in PageList.resizeCols.
        assert_eq!(Result::Success, unsafe { resize(t, 79, 23, 8, 16) });
        assert_eq!(79, zt(t).cols);
        assert_eq!(23, zt(t).rows);
        unsafe { free(t) };
    }

    #[test]
    fn set_and_get_mode() {
        let t = new_t(80, 24);

        // DEC mode 25 (cursor_visible) defaults to true
        let mut config = ModeConfig {
            mode: mode_tag(25, false),
            value: false,
        };
        assert_eq!(Result::Success, gdat(t, TerminalData::Mode, o(&mut config)));
        assert!(config.value);

        // Set it to false
        config.value = false;
        assert_eq!(Result::Success, sopt(t, Option::Mode, p(&config)));
        assert_eq!(Result::Success, gdat(t, TerminalData::Mode, o(&mut config)));
        assert!(!config.value);

        // ANSI mode 4 (insert) defaults to false
        config.mode = mode_tag(4, true);
        assert_eq!(Result::Success, gdat(t, TerminalData::Mode, o(&mut config)));
        assert!(!config.value);

        config.value = true;
        assert_eq!(Result::Success, sopt(t, Option::Mode, p(&config)));
        assert_eq!(Result::Success, gdat(t, TerminalData::Mode, o(&mut config)));
        assert!(config.value);
        unsafe { free(t) };
    }

    #[test]
    fn set_mode_default_updates_current_and_reset_value() {
        let t = new_t(80, 24);

        let mut config = ModeConfig {
            mode: mode_tag(2027, false),
            value: true,
        };
        assert_eq!(Result::Success, sopt(t, Option::ModeDefault, p(&config)));
        assert!(zt(t).modes.get(Mode::GraphemeCluster));
        assert!(zt(t).modes.default.get(Mode::GraphemeCluster));

        config.value = false;
        assert_eq!(Result::Success, sopt(t, Option::Mode, p(&config)));
        assert!(!zt(t).modes.get(Mode::GraphemeCluster));
        assert!(zt(t).modes.default.get(Mode::GraphemeCluster));

        // Setting the default also unconditionally replaces the current value.
        config.value = true;
        assert_eq!(Result::Success, sopt(t, Option::ModeDefault, p(&config)));
        assert!(zt(t).modes.get(Mode::GraphemeCluster));

        config.value = false;
        assert_eq!(Result::Success, sopt(t, Option::Mode, p(&config)));
        vw(t, b"\x1bc");
        assert!(zt(t).modes.get(Mode::GraphemeCluster));

        config.value = false;
        assert_eq!(Result::Success, sopt(t, Option::ModeDefault, p(&config)));
        assert!(!zt(t).modes.get(Mode::GraphemeCluster));
        assert!(!zt(t).modes.default.get(Mode::GraphemeCluster));
        unsafe { free(t) };
    }

    #[test]
    fn set_mode_default_validates_configuration() {
        let t = new_t(80, 24);

        assert_eq!(Result::InvalidValue, sopt(t, Option::ModeDefault, std::ptr::null()));

        let mut config = ModeConfig {
            mode: mode_tag(9999, false),
            value: true,
        };
        assert_eq!(Result::InvalidValue, sopt(t, Option::ModeDefault, p(&config)));

        config.mode = mode_tag(1047, false);
        assert_eq!(Result::InvalidValue, sopt(t, Option::ModeDefault, p(&config)));
        unsafe { free(t) };
    }

    #[test]
    fn set_and_get_mode_null() {
        let mut config = ModeConfig {
            mode: mode_tag(25, false),
            value: true,
        };
        assert_eq!(Result::InvalidValue, sopt(std::ptr::null_mut(), Option::Mode, p(&config)));
        assert_eq!(
            Result::InvalidValue,
            gdat(std::ptr::null_mut(), TerminalData::Mode, o(&mut config))
        );
    }

    #[test]
    fn set_and_get_mode_validate_configuration() {
        let t = new_t(80, 24);

        assert_eq!(Result::InvalidValue, sopt(t, Option::Mode, std::ptr::null()));
        assert_eq!(Result::InvalidValue, gdat(t, TerminalData::Mode, std::ptr::null_mut()));

        let mut config = ModeConfig {
            mode: mode_tag(9999, false),
            value: true,
        };
        assert_eq!(Result::InvalidValue, sopt(t, Option::Mode, p(&config)));
        assert_eq!(Result::InvalidValue, gdat(t, TerminalData::Mode, o(&mut config)));
        unsafe { free(t) };
    }

    #[test]
    fn vt_write_test() {
        let t = new_t(80, 24);
        vw(t, b"Hello");
        assert_eq!("Hello", plain(t));
        unsafe { free(t) };
    }

    const GROUND: u32 = Boundary::Ground as u32;
    const APC_START: u32 = Boundary::ApcStart as u32;
    const APC_END: u32 = Boundary::ApcEnd as u32;
    const DCS_START: u32 = Boundary::DcsStart as u32;
    const DCS_END: u32 = Boundary::DcsEnd as u32;
    const OSC_START: u32 = Boundary::OscStart as u32;
    const OSC_END: u32 = Boundary::OscEnd as u32;

    fn wu(t: Terminal, input: &[u8], boundaries: u32, stop: &mut WriteStop) -> Result {
        unsafe { vt_write_until(t, input.as_ptr(), input.len(), boundaries, stop) }
    }

    fn ws(f: impl FnOnce(&mut WriteStop)) -> WriteStop {
        let mut s = WriteStop::default();
        f(&mut s);
        s
    }

    fn ground(t: Terminal) -> bool {
        wr(t).stream.ground()
    }

    #[test]
    fn vt_write_until_ground_stops_at_the_first_ground_byte() {
        let t = new_t(80, 24);

        // Input remains untouched when the stream is already at ground.
        let mut stop = WriteStop::default();
        assert_eq!(Result::Success, wu(t, b"unprocessed", GROUND, &mut stop));
        assert_eq!(0, stop.consumed);

        // Complete a split CSI and stop before inspecting the printable suffix.
        vw(t, b"\x1b[31");
        let input = b"mABC\x1b[";
        assert_eq!(Result::Success, wu(t, input, GROUND, &mut stop));
        assert_eq!(1, stop.consumed);
        assert!(ground(t));

        assert_eq!("", plain(t));

        // The untouched suffix can be processed after work at the boundary.
        vw(t, &input[stop.consumed..]);
        assert_eq!("ABC", plain(t));
        assert!(!ground(t));

        // Exhausting input while unfinished is distinct from reaching ground on
        // the final byte, even though both consume the entire input.
        assert_eq!(Result::NoValue, wu(t, b"123", GROUND, &mut stop));
        assert_eq!(3, stop.consumed);
        assert!(!ground(t));

        assert_eq!(Result::Success, wu(t, b"m", GROUND, &mut stop));
        assert_eq!(1, stop.consumed);
        assert!(ground(t));
        unsafe { free(t) };
    }

    #[test]
    fn vt_write_until_ground_handles_utf8_and_abort_boundaries() {
        let t = new_t(80, 24);

        let mut at_ground = false;
        assert_eq!(Result::Success, gdat(t, TerminalData::VtGround, o(&mut at_ground)));
        assert!(at_ground);

        vw(t, &[0xF0]);
        assert_eq!(Result::Success, gdat(t, TerminalData::VtGround, o(&mut at_ground)));
        assert!(!at_ground);

        let utf8_suffix = [0x9Fu8, 0x98, 0x84];
        let mut stop = WriteStop::default();
        assert_eq!(Result::Success, wu(t, &utf8_suffix, GROUND, &mut stop));
        assert_eq!(utf8_suffix.len(), stop.consumed);
        assert!(ground(t));

        // A malformed continuation resets the decoder and reaches ground after
        // processing the retry byte.
        vw(t, &[0xE0, 0xA0]);
        assert_eq!(Result::Success, wu(t, b"A", GROUND, &mut stop));
        assert_eq!(1, stop.consumed);
        assert!(ground(t));

        vw(t, b"\x1b[123");
        assert_eq!(Result::Success, wu(t, &[0x18], GROUND, &mut stop));
        assert_eq!(1, stop.consumed);
        assert!(ground(t));
        unsafe { free(t) };
    }

    #[test]
    fn vt_write_until_invokes_effects_only_for_consumed_input() {
        thread_local!(static BELL_COUNT: Cell<usize> = const { Cell::new(0) });
        unsafe extern "C" fn bell(_: Terminal, _: *mut c_void) {
            BELL_COUNT.set(BELL_COUNT.get() + 1);
        }

        let t = new_t(80, 24);
        assert_eq!(Result::Success, sopt(t, Option::Bell, bell as BellFn as *const c_void));

        vw(t, b"\x1b[31");
        let input = b"m\x07";
        let mut stop = WriteStop::default();
        assert_eq!(Result::Success, wu(t, input, GROUND, &mut stop));
        assert_eq!(1, stop.consumed);
        assert_eq!(0, BELL_COUNT.get());

        vw(t, &input[stop.consumed..]);
        assert_eq!(1, BELL_COUNT.get());
        unsafe { free(t) };
    }

    #[test]
    fn vt_write_until_result_contract() {
        let t = new_t(80, 24);

        // Fill every field so each invalid call below must reset them.
        let stale = ws(|s| {
            s.consumed = 99;
            s.reached = 99;
            s.kind = StringKind::Osc;
            s.introducer = StringIntroducer::C1;
            s.end = StringEnd::Aborted;
            s.byte = 99;
        });

        let mut stop = stale;
        assert_eq!(Result::InvalidValue, wu(std::ptr::null_mut(), b"x", GROUND, &mut stop));
        assert_eq!(WriteStop::default(), stop);

        stop = stale;
        assert_eq!(Result::InvalidValue, unsafe {
            vt_write_until(t, std::ptr::null(), 1, GROUND, &mut stop)
        });
        assert_eq!(WriteStop::default(), stop);

        for bits in [0u32, 1 << 7, 1 << 31, GROUND | 1 << 7] {
            stop = stale;
            assert_eq!(Result::InvalidValue, wu(t, b"x", bits, &mut stop));
            assert_eq!(WriteStop::default(), stop);
        }

        assert_eq!(Result::InvalidValue, unsafe {
            vt_write_until(t, b"x".as_ptr(), 1, GROUND, std::ptr::null_mut())
        });

        // A struct one byte short is rejected and left untouched.
        stop = stale;
        stop.size = std::mem::size_of::<WriteStop>() - 1;
        assert_eq!(Result::InvalidValue, wu(t, b"x", GROUND, &mut stop));
        assert_eq!(99, stop.consumed);

        // Already at ground with ground requested consumes nothing.
        stop = stale;
        assert_eq!(Result::Success, wu(t, b"x", GROUND | APC_END, &mut stop));
        assert_eq!(ws(|s| s.reached = GROUND), stop);

        // No boundary in the whole slice fills consumed and zeroes reached.
        stop = stale;
        let text = b"hello";
        assert_eq!(Result::NoValue, wu(t, text, APC_START | APC_END, &mut stop));
        assert_eq!(ws(|s| s.consumed = text.len()), stop);

        // NULL with len 0 is an empty slice.
        assert_eq!(Result::NoValue, unsafe {
            vt_write_until(t, std::ptr::null(), 0, APC_START, &mut stop)
        });
        assert_eq!(WriteStop::default(), stop);
        unsafe { free(t) };
    }

    #[test]
    fn vt_write_until_kitty_apc_start_and_end() {
        let input = b"\x1b_Gf=100,s=1,v=1;AAAA\x1b\\";
        let content = b"Gf=100,s=1,v=1;AAAA";

        for with_ground in [false, true] {
            let t = new_t(80, 24);

            // The start stop covers ESC and '_'.
            let mut stop = WriteStop::default();
            assert_eq!(Result::Success, wu(t, input, APC_START | APC_END, &mut stop));
            assert_eq!(
                ws(|s| {
                    s.consumed = 2;
                    s.reached = APC_START;
                    s.kind = StringKind::Apc;
                    s.introducer = StringIntroducer::Esc;
                    s.byte = b'_';
                }),
                stop
            );
            let mut off = stop.consumed;
            assert_eq!(b'G', input[off]);

            // The end stop consumes the ESC of ESC \ but not the backslash.
            assert_eq!(Result::Success, wu(t, &input[off..], APC_START | APC_END, &mut stop));
            assert_eq!(
                ws(|s| {
                    s.consumed = content.len() + 1;
                    s.reached = APC_END;
                    s.kind = StringKind::Apc;
                    s.end = StringEnd::StEsc;
                    s.byte = 0x1B;
                }),
                stop
            );
            assert_eq!(&content[..], &input[off..off + content.len()]);
            off += stop.consumed;
            assert_eq!(input.len() - 1, off);

            // The backslash completes the escape, reaching ground only.
            if with_ground {
                assert_eq!(Result::Success, wu(t, b"\\", GROUND | APC_START | APC_END, &mut stop));
                assert_eq!(
                    ws(|s| {
                        s.consumed = 1;
                        s.reached = GROUND;
                    }),
                    stop
                );
            } else {
                assert_eq!(Result::NoValue, wu(t, b"\\", APC_START | APC_END, &mut stop));
                assert_eq!(ws(|s| s.consumed = 1), stop);
            }
            assert!(ground(t));
            unsafe { free(t) };
        }
    }

    #[test]
    fn vt_write_until_split_introducer() {
        let t = new_t(80, 24);

        let mut stop = WriteStop::default();
        assert_eq!(Result::NoValue, wu(t, b"\x1b", APC_START, &mut stop));
        assert_eq!(ws(|s| s.consumed = 1), stop);

        assert_eq!(Result::Success, wu(t, b"_G", APC_START, &mut stop));
        assert_eq!(
            ws(|s| {
                s.consumed = 1;
                s.reached = APC_START;
                s.kind = StringKind::Apc;
                s.introducer = StringIntroducer::Esc;
                s.byte = b'_';
            }),
            stop
        );
        unsafe { free(t) };
    }

    #[test]
    fn vt_write_until_c1_introducer() {
        let t = new_t(80, 24);

        // At ground a raw C1 byte goes through the UTF-8 decoder, so the
        // introducer is only recognized from inside another sequence.
        vw(t, b"\x1b[1");

        let mut stop = WriteStop::default();
        assert_eq!(Result::Success, wu(t, &[0x9F, b'G'], APC_START, &mut stop));
        assert_eq!(
            ws(|s| {
                s.consumed = 1;
                s.reached = APC_START;
                s.kind = StringKind::Apc;
                s.introducer = StringIntroducer::C1;
                s.byte = 0x9F;
            }),
            stop
        );
        unsafe { free(t) };
    }

    #[test]
    fn vt_write_until_string_end_that_reaches_ground() {
        for (byte, end) in [(0x18u8, StringEnd::Cancelled), (0x9C, StringEnd::StC1)] {
            let t = new_t(80, 24);

            vw(t, b"\x1b_Gab");
            let input = [byte, b'x'];
            let mut stop = WriteStop::default();
            assert_eq!(Result::Success, wu(t, &input, GROUND | APC_END, &mut stop));
            assert_eq!(
                ws(|s| {
                    s.consumed = 1;
                    s.reached = GROUND | APC_END;
                    s.kind = StringKind::Apc;
                    s.end = end;
                    s.byte = byte;
                }),
                stop
            );
            unsafe { free(t) };
        }
    }

    #[test]
    fn vt_write_until_sos_and_pm_report_as_apc() {
        for final_ in *b"X^" {
            let t = new_t(80, 24);

            let input = [0x1B, final_, b'a'];
            let mut stop = WriteStop::default();
            assert_eq!(Result::Success, wu(t, &input, APC_START, &mut stop));
            assert_eq!(
                ws(|s| {
                    s.consumed = 2;
                    s.reached = APC_START;
                    s.kind = StringKind::Apc;
                    s.introducer = StringIntroducer::Esc;
                    s.byte = final_;
                }),
                stop
            );
            unsafe { free(t) };
        }
    }

    #[test]
    fn vt_write_until_osc_start_and_bel_end() {
        let t = new_t(80, 24);

        // The start stop covers ESC and ']'.
        let input = b"\x1b]2;title\x07x";
        let mut stop = WriteStop::default();
        assert_eq!(Result::Success, wu(t, input, OSC_START | OSC_END, &mut stop));
        assert_eq!(
            ws(|s| {
                s.consumed = 2;
                s.reached = OSC_START;
                s.kind = StringKind::Osc;
                s.introducer = StringIntroducer::Esc;
                s.byte = b']';
            }),
            stop
        );
        let mut off = stop.consumed;

        // BEL ends the string and reaches ground on the same byte.
        assert_eq!(Result::Success, wu(t, &input[off..], GROUND | OSC_END, &mut stop));
        assert_eq!(
            ws(|s| {
                s.consumed = b"2;title".len() + 1;
                s.reached = GROUND | OSC_END;
                s.kind = StringKind::Osc;
                s.end = StringEnd::Bel;
                s.byte = 0x07;
            }),
            stop
        );
        off += stop.consumed;
        assert_eq!(input.len() - 1, off);
        assert!(ground(t));
        unsafe { free(t) };
    }

    #[test]
    fn vt_write_until_dcs_start_and_end() {
        let t = new_t(80, 24);

        // The start stop covers ESC and 'P', before the header.
        let input = b"\x1bP1;2|data\x1b\\";
        let mut stop = WriteStop::default();
        assert_eq!(Result::Success, wu(t, input, DCS_START | DCS_END, &mut stop));
        assert_eq!(
            ws(|s| {
                s.consumed = 2;
                s.reached = DCS_START;
                s.kind = StringKind::Dcs;
                s.introducer = StringIntroducer::Esc;
                s.byte = b'P';
            }),
            stop
        );
        let mut off = stop.consumed;
        assert_eq!(b'1', input[off]);

        // The end stop consumes the ESC of ESC \ but not the backslash.
        assert_eq!(Result::Success, wu(t, &input[off..], DCS_START | DCS_END, &mut stop));
        assert_eq!(
            ws(|s| {
                s.consumed = b"1;2|data".len() + 1;
                s.reached = DCS_END;
                s.kind = StringKind::Dcs;
                s.end = StringEnd::StEsc;
                s.byte = 0x1B;
            }),
            stop
        );
        off += stop.consumed;
        assert_eq!(input.len() - 1, off);

        assert_eq!(Result::NoValue, wu(t, b"\\", DCS_START | DCS_END, &mut stop));
        assert!(ground(t));
        unsafe { free(t) };
    }

    #[test]
    fn vt_write_until_string_end_that_is_also_a_string_start() {
        let t = new_t(80, 24);

        // 0x9D inside an APC ends it and starts an OSC on the same byte.
        // The end's kind is reported; the OSC_START flag names the start.
        vw(t, b"\x1b_a");
        let input = b"\x9d0;x\x07";
        let mut stop = WriteStop::default();
        assert_eq!(Result::Success, wu(t, input, APC_END | OSC_START | OSC_END, &mut stop));
        assert_eq!(
            ws(|s| {
                s.consumed = 1;
                s.reached = APC_END | OSC_START;
                s.kind = StringKind::Apc;
                s.introducer = StringIntroducer::C1;
                s.end = StringEnd::Aborted;
                s.byte = 0x9D;
            }),
            stop
        );

        assert_eq!(Result::Success, wu(t, &input[1..], APC_END | OSC_START | OSC_END, &mut stop));
        assert_eq!(
            ws(|s| {
                s.consumed = 4;
                s.reached = OSC_END;
                s.kind = StringKind::Osc;
                s.end = StringEnd::Bel;
                s.byte = 0x07;
            }),
            stop
        );
        unsafe { free(t) };
    }

    #[test]
    fn vt_write_split_escape_sequence() {
        let t = new_t(80, 24);

        // Write "Hello" in bold by splitting the CSI bold sequence across two writes.
        // ESC [ 1 m  = bold on, ESC [ 0 m = reset
        // Split ESC from the rest of the CSI sequence.
        vw(t, b"Hello \x1b");
        vw(t, b"[1mBold\x1b[0m");

        // If the escape sequence leaked, we'd see "[1mBold" as literal text.
        assert_eq!("Hello Bold", plain(t));
        unsafe { free(t) };
    }

    #[test]
    fn vt_write_split_combining_mark_after_base_at_right_edge() {
        let t = new_t(2, 2);

        // Put "å" in the final column, then send its combining low line in a
        // separate write so the mark arrives while the cursor has a pending wrap.
        vw(t, "xå".as_bytes());
        vw(t, b"\xcc\xb2");

        assert_eq!("xå̲", plain(t));
        unsafe { free(t) };
    }

    #[test]
    fn get_cols_and_rows() {
        let t = new_t(80, 24);
        let mut cols: CellCountInt = 0;
        let mut rows: CellCountInt = 0;
        assert_eq!(Result::Success, gdat(t, TerminalData::Cols, o(&mut cols)));
        assert_eq!(Result::Success, gdat(t, TerminalData::Rows, o(&mut rows)));
        assert_eq!(80, cols);
        assert_eq!(24, rows);
        unsafe { free(t) };
    }

    #[test]
    fn get_cursor_position() {
        let t = new_t(80, 24);
        vw(t, b"Hello");
        let mut x: CellCountInt = 0;
        let mut y: CellCountInt = 0;
        assert_eq!(Result::Success, gdat(t, TerminalData::CursorX, o(&mut x)));
        assert_eq!(Result::Success, gdat(t, TerminalData::CursorY, o(&mut y)));
        assert_eq!(5, x);
        assert_eq!(0, y);
        unsafe { free(t) };
    }

    #[test]
    fn get_vt_processing_error() {
        use crate::terminal::stream_terminal::{FailPoint, OutOfMemory as VtOom, VT_TW};

        let t = new_t(80, 24);

        let mut processing_error = true;
        assert_eq!(
            Result::Success,
            gdat(t, TerminalData::VtProcessingError, o(&mut processing_error))
        );
        assert!(!processing_error);

        // Force a non-graceful terminal-owned update failure through the public
        // VT write path. (Rust port: the title allocation tripwire stands in
        // for Zig's failing screen allocator.)
        VT_TW.error_always(FailPoint::WindowTitle, VtOom);
        vw(t, b"\x1B]2;unavailable\x1B\\");
        VT_TW.end(crate::tripwire::ResetMode::Reset).unwrap();

        assert_eq!(
            Result::Success,
            gdat(t, TerminalData::VtProcessingError, o(&mut processing_error))
        );
        assert!(processing_error);

        unsafe { reset(t) };
        assert_eq!(
            Result::Success,
            gdat(t, TerminalData::VtProcessingError, o(&mut processing_error))
        );
        assert!(processing_error);
        unsafe { free(t) };
    }

    #[test]
    fn get_null() {
        let mut cols: CellCountInt = 0;
        assert_eq!(
            Result::InvalidValue,
            gdat(std::ptr::null_mut(), TerminalData::Cols, o(&mut cols))
        );
    }

    #[test]
    fn get_cursor_visible() {
        let t = new_t(80, 24);

        let mut visible = false;
        assert_eq!(Result::Success, gdat(t, TerminalData::CursorVisible, o(&mut visible)));
        assert!(visible);

        // DEC mode 25 controls cursor visibility
        let config = ModeConfig {
            mode: mode_tag(25, false),
            value: false,
        };
        assert_eq!(Result::Success, sopt(t, Option::Mode, p(&config)));
        assert_eq!(Result::Success, gdat(t, TerminalData::CursorVisible, o(&mut visible)));
        assert!(!visible);
        unsafe { free(t) };
    }

    #[test]
    fn get_cursor_at_prompt() {
        let t = new_t(80, 24);

        let mut at_prompt = true;
        assert_eq!(Result::Success, gdat(t, TerminalData::CursorAtPrompt, o(&mut at_prompt)));
        assert!(!at_prompt);

        vw(t, b"\x1b]133;A\x07");
        assert_eq!(Result::Success, gdat(t, TerminalData::CursorAtPrompt, o(&mut at_prompt)));
        assert!(at_prompt);

        vw(t, b"\x1b[?1049h");
        assert_eq!(Result::Success, gdat(t, TerminalData::CursorAtPrompt, o(&mut at_prompt)));
        assert!(!at_prompt);
        unsafe { free(t) };
    }

    #[test]
    fn get_active_screen() {
        let t = new_t(80, 24);
        let mut screen = TerminalScreen::Alternate;
        assert_eq!(Result::Success, gdat(t, TerminalData::ActiveScreen, o(&mut screen)));
        assert_eq!(TerminalScreen::Primary, screen);
        unsafe { free(t) };
    }

    #[test]
    fn get_kitty_keyboard_flags() {
        let t = new_t(80, 24);

        let mut flags: u8 = 0xFF;
        assert_eq!(Result::Success, gdat(t, TerminalData::KittyKeyboardFlags, o(&mut flags)));
        assert_eq!(0, flags);

        // Push kitty flags via VT sequence: CSI > 3 u (push disambiguate | report_events)
        vw(t, b"\x1b[>3u");

        assert_eq!(Result::Success, gdat(t, TerminalData::KittyKeyboardFlags, o(&mut flags)));
        assert_eq!(3, flags);
        unsafe { free(t) };
    }

    #[test]
    fn get_mouse_tracking() {
        let t = new_t(80, 24);

        let mut tracking = true;
        assert_eq!(Result::Success, gdat(t, TerminalData::MouseTracking, o(&mut tracking)));
        assert!(!tracking);

        // Each of X10 (9), normal (1000), button (1002) and any (1003)
        // mouse modes enables tracking on its own.
        let mut config = ModeConfig {
            mode: mode_tag(9, false),
            value: true,
        };
        for mode in [9u16, 1000, 1002, 1003] {
            // Disable the previous mode, enable the next one.
            config.value = false;
            assert_eq!(Result::Success, sopt(t, Option::Mode, p(&config)));
            config = ModeConfig {
                mode: mode_tag(mode, false),
                value: true,
            };
            assert_eq!(Result::Success, sopt(t, Option::Mode, p(&config)));
            assert_eq!(Result::Success, gdat(t, TerminalData::MouseTracking, o(&mut tracking)));
            assert!(tracking);
        }

        // Disable all - should be false again
        config.value = false;
        assert_eq!(Result::Success, sopt(t, Option::Mode, p(&config)));
        assert_eq!(Result::Success, gdat(t, TerminalData::MouseTracking, o(&mut tracking)));
        assert!(!tracking);
        unsafe { free(t) };
    }

    #[test]
    fn get_total_rows() {
        let t = new_t(80, 24);
        let mut total: usize = 0;
        assert_eq!(Result::Success, gdat(t, TerminalData::TotalRows, o(&mut total)));
        assert!(total >= 24);
        unsafe { free(t) };
    }

    #[test]
    fn get_scrollback_rows() {
        let t = new_t(80, 3);

        let mut scrollback: usize = 99;
        assert_eq!(Result::Success, gdat(t, TerminalData::ScrollbackRows, o(&mut scrollback)));
        assert_eq!(0, scrollback);

        // Write enough lines to push content into scrollback
        // (Zig passes len 34, dropping the final newline.)
        vw(t, &b"line1\r\nline2\r\nline3\r\nline4\r\nline5\r\n"[..34]);

        assert_eq!(Result::Success, gdat(t, TerminalData::ScrollbackRows, o(&mut scrollback)));
        assert_eq!(2, scrollback);
        unsafe { free(t) };
    }

    #[test]
    fn get_configured_scrollback_limits() {
        let t = new_t(80, 3);

        let mut value: usize = 0;
        assert_eq!(Result::Success, gdat(t, TerminalData::ScrollbackMaxBytes, o(&mut value)));
        assert_eq!(10_000, value);
        assert_eq!(Result::NoValue, gdat(t, TerminalData::ScrollbackMaxLines, o(&mut value)));

        let max_bytes: usize = 0;
        let max_lines: usize = 12;
        assert_eq!(Result::Success, sopt(t, Option::ScrollbackMaxBytes, p(&max_bytes)));
        assert_eq!(Result::Success, sopt(t, Option::ScrollbackMaxLines, p(&max_lines)));
        assert_eq!(Result::Success, gdat(t, TerminalData::ScrollbackMaxBytes, o(&mut value)));
        assert_eq!(max_bytes, value);
        assert_eq!(Result::Success, gdat(t, TerminalData::ScrollbackMaxLines, o(&mut value)));
        assert_eq!(max_lines, value);

        // The configured limits belong to the primary screen and remain
        // readable while an alternate screen is active.
        vw(t, b"\x1b[?1049h");
        assert_eq!(Result::Success, gdat(t, TerminalData::ScrollbackMaxBytes, o(&mut value)));
        assert_eq!(max_bytes, value);
        assert_eq!(Result::Success, gdat(t, TerminalData::ScrollbackMaxLines, o(&mut value)));
        assert_eq!(max_lines, value);

        assert_eq!(Result::Success, sopt(t, Option::ScrollbackMaxBytes, std::ptr::null()));
        assert_eq!(Result::Success, sopt(t, Option::ScrollbackMaxLines, std::ptr::null()));
        assert_eq!(Result::NoValue, gdat(t, TerminalData::ScrollbackMaxBytes, o(&mut value)));
        assert_eq!(Result::NoValue, gdat(t, TerminalData::ScrollbackMaxLines, o(&mut value)));
        unsafe { free(t) };
    }

    #[test]
    fn get_invalid() {
        let t = new_t(80, 24);
        assert_eq!(Result::InvalidValue, gdat(t, TerminalData::Invalid, std::ptr::null_mut()));
        unsafe { free(t) };
    }

    #[test]
    fn set_default_cursor_style_and_blink() {
        let t = new_t(80, 24);
        let cursor_style = |t: Terminal| zt(t).screens.active.cursor.cursor_style;

        let mut default_style = TerminalCursorStyle::Bar;
        let default_blink = true;
        assert_eq!(Result::Success, sopt(t, Option::DefaultCursorStyle, p(&default_style)));
        assert_eq!(Result::Success, sopt(t, Option::DefaultCursorBlink, p(&default_blink)));

        // Setting defaults applies them immediately while the cursor is still default.
        assert_eq!(CursorStyle::Bar, cursor_style(t));
        assert!(zt(t).modes.get(Mode::CursorBlinking));

        // An explicit DECSCUSR style overrides the configured defaults.
        vw(t, b"\x1b[2 q");
        assert_eq!(CursorStyle::Block, cursor_style(t));
        assert!(!zt(t).modes.get(Mode::CursorBlinking));

        // Changing defaults does not override an explicit cursor style.
        default_style = TerminalCursorStyle::Underline;
        assert_eq!(Result::Success, sopt(t, Option::DefaultCursorStyle, p(&default_style)));
        assert_eq!(CursorStyle::Block, cursor_style(t));
        assert!(!zt(t).modes.get(Mode::CursorBlinking));

        // DECSCUSR reset restores the configured default style and blink.
        vw(t, b"\x1b[0 q");
        assert_eq!(CursorStyle::Underline, cursor_style(t));
        assert!(zt(t).modes.get(Mode::CursorBlinking));

        // RIS also restores cursor defaults from Terminal-owned state.
        vw(t, b"\x1b[2 q");
        unsafe { reset(t) };
        assert_eq!(CursorStyle::Underline, cursor_style(t));
        assert!(zt(t).modes.get(Mode::CursorBlinking));
        unsafe { free(t) };
    }

    #[test]
    fn set_resize_pull_scrollback() {
        let t = new_t(80, 24);
        assert!(zt(t).flags.resize_pull_scrollback);

        let disabled = false;
        assert_eq!(Result::Success, sopt(t, Option::ResizePullScrollback, p(&disabled)));
        assert!(!zt(t).flags.resize_pull_scrollback);

        // NULL restores the default.
        assert_eq!(Result::Success, sopt(t, Option::ResizePullScrollback, std::ptr::null()));
        assert!(zt(t).flags.resize_pull_scrollback);
        unsafe { free(t) };
    }

    fn gref(t: Terminal, pt: point::Point) -> grid_ref_c::CGridRef {
        let mut r = grid_ref_c::CGridRef::default();
        assert_eq!(Result::Success, unsafe { grid_ref(t, pt.cval(), &mut r) });
        r
    }

    fn active(x: u16, y: u32) -> point::Point {
        point::Point::Active(point::Coordinate { x, y })
    }

    fn pin_of(r: &grid_ref_c::CGridRef) -> crate::terminal::page_list::Pin {
        r.to_pin().unwrap()
    }

    fn empty_sel() -> selection_c::CSelection {
        selection_c::CSelection::new(grid_ref_c::CGridRef::default(), grid_ref_c::CGridRef::default())
    }

    #[test]
    fn set_and_get_selection() {
        let t = new_t(80, 24);
        vw(t, b"Hello");

        let start_ref = gref(t, active(0, 0));
        let end_ref = gref(t, active(4, 0));

        let mut out = empty_sel();
        assert_eq!(Result::NoValue, gdat(t, TerminalData::Selection, o(&mut out)));

        let sel = selection_c::CSelection {
            rectangle: true,
            ..selection_c::CSelection::new(start_ref, end_ref)
        };
        assert_eq!(Result::Success, sopt(t, Option::Selection, p(&sel)));
        assert!(zt(t).screens.active.selection.unwrap().tracked());

        assert_eq!(Result::Success, gdat(t, TerminalData::Selection, o(&mut out)));
        assert!(pin_of(&out.start).eql(pin_of(&start_ref)));
        assert!(pin_of(&out.end).eql(pin_of(&end_ref)));
        assert!(out.rectangle);

        assert_eq!(Result::Success, sopt(t, Option::Selection, std::ptr::null()));
        assert!(zt(t).screens.active.selection.is_none());
        assert_eq!(Result::NoValue, gdat(t, TerminalData::Selection, o(&mut out)));
        unsafe { free(t) };
    }

    #[test]
    fn selection_derivation_helpers() {
        let t = new_t(80, 24);
        vw(t, b"  Hello  \r\nWorld");

        let mut out = empty_sel();

        let word_ref = gref(t, active(3, 0));
        let empty_ref = gref(t, active(20, 0));
        let line_ref = gref(t, active(0, 0));

        unsafe {
            let mut word_opts = selection_c::SelectWordOptions::new(word_ref);
            assert_eq!(Result::Success, selection_c::word(t, &word_opts, &mut out));
            assert_eq!(2, pin_of(&out.start).x);
            assert_eq!(6, pin_of(&out.end).x);

            word_opts.r#ref = empty_ref;
            assert_eq!(Result::NoValue, selection_c::word(t, &word_opts, &mut out));

            let between_start_ref = gref(t, active(20, 1));
            let between_end_ref = gref(t, active(0, 1));

            let word_between_opts = selection_c::SelectWordBetweenOptions::new(between_start_ref, between_end_ref);
            assert_eq!(Result::Success, selection_c::word_between(t, &word_between_opts, &mut out));
            assert_eq!(0, pin_of(&out.start).x);
            assert_eq!(1, pin_of(&out.start).y);
            assert_eq!(4, pin_of(&out.end).x);
            assert_eq!(1, pin_of(&out.end).y);

            let mut line_opts = selection_c::SelectLineOptions::new(line_ref);
            assert_eq!(Result::Success, selection_c::line(t, &line_opts, &mut out));
            assert_eq!(2, pin_of(&out.start).x);
            assert_eq!(6, pin_of(&out.end).x);

            assert_eq!(Result::Success, selection_c::all(t, &mut out));
            assert_eq!(2, pin_of(&out.start).x);
            assert_eq!(0, pin_of(&out.start).y);
            assert_eq!(4, pin_of(&out.end).x);
            assert_eq!(1, pin_of(&out.end).y);

            assert_eq!(Result::NoValue, selection_c::output(t, line_ref, &mut out));

            line_opts.size = std::mem::size_of::<usize>() - 1;
            assert_eq!(Result::InvalidValue, selection_c::line(t, &line_opts, &mut out));
            assert_eq!(Result::InvalidValue, selection_c::word(t, std::ptr::null(), &mut out));
            assert_eq!(Result::InvalidValue, selection_c::word(t, &word_opts, std::ptr::null_mut()));
            assert_eq!(Result::InvalidValue, selection_c::word_between(t, std::ptr::null(), &mut out));
            assert_eq!(
                Result::InvalidValue,
                selection_c::word_between(t, &word_between_opts, std::ptr::null_mut())
            );
            free(t);
        }
    }

    #[test]
    fn selection_adjust_mutates_snapshot_end() {
        use crate::terminal::selection::Adjustment;
        let t = new_t(80, 24);
        vw(t, b"Hello");

        let start_ref = gref(t, active(0, 0));
        let end_ref = gref(t, active(1, 0));

        unsafe {
            let mut sel = selection_c::CSelection::new(start_ref, end_ref);
            assert_eq!(Result::Success, selection_c::adjust(t, &mut sel, Adjustment::Right as c_int));
            assert_eq!(0, pin_of(&sel.start).x);
            assert_eq!(2, pin_of(&sel.end).x);

            assert_eq!(Result::Success, selection_c::adjust(t, &mut sel, Adjustment::Left as c_int));
            assert_eq!(0, pin_of(&sel.start).x);
            assert_eq!(1, pin_of(&sel.end).x);

            sel = selection_c::CSelection::new(end_ref, start_ref);
            assert_eq!(Result::Success, selection_c::adjust(t, &mut sel, Adjustment::Right as c_int));
            assert_eq!(1, pin_of(&sel.start).x);
            assert_eq!(1, pin_of(&sel.end).x);
            free(t);
        }
    }

    #[test]
    fn selection_order_and_selection_ordered() {
        use crate::terminal::selection::Order;
        let t = new_t(80, 24);
        vw(t, b"Hello\r\nWorld");

        let start_ref = gref(t, active(3, 0));
        let end_ref = gref(t, active(1, 1));

        let sel = selection_c::CSelection {
            rectangle: true,
            ..selection_c::CSelection::new(start_ref, end_ref)
        };

        unsafe {
            let mut order: c_int = -1;
            assert_eq!(Result::Success, selection_c::order(t, &sel, &mut order));
            assert_eq!(Order::MirroredForward as c_int, order);

            let mut out = empty_sel();
            assert_eq!(Result::Success, selection_c::ordered(t, &sel, Order::Forward as c_int, &mut out));
            assert_eq!(1, pin_of(&out.start).x);
            assert_eq!(0, pin_of(&out.start).y);
            assert_eq!(3, pin_of(&out.end).x);
            assert_eq!(1, pin_of(&out.end).y);
            assert!(out.rectangle);

            assert_eq!(Result::Success, selection_c::ordered(t, &sel, Order::Reverse as c_int, &mut out));
            assert_eq!(3, pin_of(&out.start).x);
            assert_eq!(1, pin_of(&out.start).y);
            assert_eq!(1, pin_of(&out.end).x);
            assert_eq!(0, pin_of(&out.end).y);
            assert!(out.rectangle);
            free(t);
        }
    }

    #[test]
    fn selection_contains() {
        let t = new_t(80, 24);
        vw(t, b"Hello\r\nWorld");

        let start_ref = gref(t, active(3, 0));
        let end_ref = gref(t, active(1, 1));

        let linear = selection_c::CSelection::new(start_ref, end_ref);

        unsafe {
            let mut contains = false;
            assert_eq!(
                Result::Success,
                selection_c::contains(t, &linear, active(4, 0).cval(), &mut contains)
            );
            assert!(contains);

            assert_eq!(
                Result::Success,
                selection_c::contains(t, &linear, active(2, 0).cval(), &mut contains)
            );
            assert!(!contains);

            let rectangle = selection_c::CSelection {
                rectangle: true,
                ..linear
            };

            assert_eq!(
                Result::Success,
                selection_c::contains(t, &rectangle, active(2, 0).cval(), &mut contains)
            );
            assert!(contains);
            free(t);
        }
    }

    #[test]
    fn selection_equal() {
        let t = new_t(80, 24);
        let other_t = new_t(80, 24);

        vw(t, b"Hello");
        vw(other_t, b"Hello");

        let start_ref = gref(t, active(0, 0));
        let end_ref = gref(t, active(1, 0));
        let other_end_ref = gref(t, active(2, 0));
        let cross_terminal_ref = gref(other_t, active(1, 0));

        let sel = selection_c::CSelection::new(start_ref, end_ref);
        let equal_sel = selection_c::CSelection::new(start_ref, end_ref);
        let different_endpoint = selection_c::CSelection::new(start_ref, other_end_ref);
        let different_rectangle = selection_c::CSelection {
            rectangle: true,
            ..selection_c::CSelection::new(start_ref, end_ref)
        };
        let cross_terminal = selection_c::CSelection::new(start_ref, cross_terminal_ref);

        unsafe {
            let mut equal = false;
            assert_eq!(Result::Success, selection_c::equal(t, &sel, &equal_sel, &mut equal));
            assert!(equal);

            assert_eq!(Result::Success, selection_c::equal(t, &sel, &different_endpoint, &mut equal));
            assert!(!equal);

            assert_eq!(Result::Success, selection_c::equal(t, &sel, &different_rectangle, &mut equal));
            assert!(!equal);

            assert_eq!(Result::Success, selection_c::equal(t, &sel, &cross_terminal, &mut equal));
            assert!(!equal);
            assert_eq!(
                Result::InvalidValue,
                selection_c::equal(t, &sel, &equal_sel, std::ptr::null_mut())
            );
            free(other_t);
            free(t);
        }
    }

    #[test]
    fn selection_order_invalid_values() {
        let t = new_t(80, 24);
        unsafe {
            let mut order: c_int = 0;
            assert_eq!(
                Result::InvalidValue,
                selection_c::order(std::ptr::null_mut(), std::ptr::null(), &mut order)
            );
            assert_eq!(Result::InvalidValue, selection_c::order(t, std::ptr::null(), &mut order));
            free(t);
        }
    }

    #[test]
    fn grid_ref_test() {
        let t = new_t(80, 24);
        vw(t, b"Hello");

        let out_ref = gref(t, active(0, 0));

        unsafe {
            // Extract cell from grid ref and verify it contains 'H'
            let mut out_cell: super::super::cell::CCell = 0;
            assert_eq!(Result::Success, grid_ref_c::grid_ref_cell(&out_ref, &mut out_cell));

            let mut cp: u32 = 0;
            assert_eq!(
                Result::Success,
                super::super::cell::get(out_cell, super::super::cell::CellData::Codepoint as c_int, o(&mut cp))
            );
            assert_eq!('H' as u32, cp);
            free(t);
        }
    }

    #[test]
    fn grid_ref_null_terminal() {
        let mut out_ref = grid_ref_c::CGridRef::default();
        assert_eq!(Result::InvalidValue, unsafe {
            grid_ref(std::ptr::null_mut(), active(0, 0).cval(), &mut out_ref)
        });
    }

    #[test]
    fn point_from_grid_ref_roundtrip_active() {
        let t = new_t(80, 24);
        vw(t, b"Hello");

        // Get a grid ref at (2, 0) in active coords
        let r = gref(t, active(2, 0));

        // Convert back to active coords
        let mut coord = point::Coordinate::default();
        assert_eq!(Result::Success, unsafe {
            point_from_grid_ref(t, &r, point::Tag::Active as c_int, &mut coord)
        });
        assert_eq!(2, coord.x);
        assert_eq!(0, coord.y);
        unsafe { free(t) };
    }

    #[test]
    fn point_from_grid_ref_roundtrip_viewport() {
        let t = new_t(80, 24);
        vw(t, b"Hello");

        let r = gref(t, point::Point::Viewport(point::Coordinate { x: 0, y: 0 }));

        let mut coord = point::Coordinate { x: 9, y: 9 };
        assert_eq!(Result::Success, unsafe {
            point_from_grid_ref(t, &r, point::Tag::Viewport as c_int, &mut coord)
        });
        assert_eq!(0, coord.x);
        assert_eq!(0, coord.y);
        unsafe { free(t) };
    }

    #[test]
    fn point_from_grid_ref_history_ref_to_active_returns_no_value() {
        let t = new_t(80, 4);

        // Write enough lines to push content into scrollback
        for _ in 0..10 {
            vw(t, b"line\n");
        }

        // Get a ref to the first line (now in scrollback)
        let r = gref(t, point::Point::Screen(point::Coordinate { x: 0, y: 0 }));

        unsafe {
            // Should succeed for screen coords
            let mut coord = point::Coordinate { x: 9, y: 9 };
            assert_eq!(
                Result::Success,
                point_from_grid_ref(t, &r, point::Tag::Screen as c_int, &mut coord)
            );
            assert_eq!(0, coord.y);

            // Should fail for active coords (it's in scrollback)
            assert_eq!(
                Result::NoValue,
                point_from_grid_ref(t, &r, point::Tag::Active as c_int, &mut coord)
            );
            free(t);
        }
    }

    #[test]
    fn point_from_grid_ref_null_terminal() {
        let r = grid_ref_c::CGridRef::default();
        assert_eq!(Result::InvalidValue, unsafe {
            point_from_grid_ref(std::ptr::null_mut(), &r, point::Tag::Active as c_int, std::ptr::null_mut())
        });
    }

    #[test]
    fn point_from_grid_ref_null_node() {
        let t = new_t(80, 24);
        let r = grid_ref_c::CGridRef::default();
        assert_eq!(Result::InvalidValue, unsafe {
            point_from_grid_ref(t, &r, point::Tag::Active as c_int, std::ptr::null_mut())
        });
        unsafe { free(t) };
    }

    #[test]
    fn grid_ref_out_of_bounds() {
        let t = new_t(80, 24);
        let mut out_ref = grid_ref_c::CGridRef::default();
        assert_eq!(Result::InvalidValue, unsafe {
            grid_ref(t, active(100, 0).cval(), &mut out_ref)
        });
        unsafe { free(t) };
    }

    #[test]
    fn set_write_pty_callback_decrqm() {
        let t = new_t(80, 24);

        // Set userdata and write_pty callback
        let mut sentinel: u8 = 42;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));
        set_write_pty(t);

        // DECRQM for wraparound mode (mode 7, set by default) should trigger write_pty
        vw(t, b"\x1B[?7$p");
        assert_eq!(Some(b"\x1B[?7;1$y".to_vec()), last());
        assert_eq!(o(&mut sentinel), LAST_UD.get());
        assert_eq!(1, CALLS.get());

        vw(t, b"\x1B[4h\x1B[4$p");
        assert_eq!(2, CALLS.get());
        assert_eq!(Some(b"\x1B[4;1$y".to_vec()), last());
        assert_eq!(o(&mut sentinel), LAST_UD.get());
        unsafe { free(t) };
    }

    #[test]
    fn write_pty_receives_decrqss_response() {
        let t = new_t(80, 24);
        set_write_pty(t);
        vw(t, b"\x1B[1m\x1BP$qm\x1B\\");
        assert_eq!(Some(b"\x1BP1$r0;1m\x1B\\".to_vec()), last());
        unsafe { free(t) };
    }

    #[test]
    fn write_pty_receives_osc_color_query_response() {
        let t = new_t(80, 24);
        set_write_pty(t);

        vw(t, b"\x1B]10;rgb:01/02/03\x1B\\");
        assert!(last().is_none());

        vw(t, b"\x1B]10;?\x1B\\");
        assert_eq!(Some(b"\x1B]10;rgb:0101/0202/0303\x1B\\".to_vec()), last());
        unsafe { free(t) };
    }

    #[test]
    fn set_write_pty_without_callback_ignores_queries() {
        let t = new_t(80, 24);
        // Without setting a callback, DECRQM should be silently ignored (no crash)
        vw(t, b"\x1B[?7$p");
        vw(t, b"\x1B[4$p");
        unsafe { free(t) };
    }

    #[test]
    fn set_write_pty_null_clears_callback() {
        let t = new_t(80, 24);

        // Set then clear the callback
        set_write_pty(t);
        assert_eq!(Result::Success, sopt(t, Option::WritePty, std::ptr::null()));

        vw(t, b"\x1B[?7$p");
        assert_eq!(0, CALLS.get());
        unsafe { free(t) };
    }

    thread_local! {
        static COUNT: Cell<usize> = const { Cell::new(0) };
        static UD: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
    }

    /// A counting callback for the `fn(Terminal, userdata)` effects.
    unsafe extern "C" fn count_cb(_: Terminal, ud: *mut c_void) {
        COUNT.set(COUNT.get() + 1);
        UD.set(ud);
    }

    #[test]
    fn set_bell_callback() {
        let t = new_t(80, 24);

        // Set userdata and bell callback
        let mut sentinel: u8 = 99;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));
        assert_eq!(Result::Success, sopt(t, Option::Bell, count_cb as BellFn as *const c_void));

        // Single BEL
        vw(t, b"\x07");
        assert_eq!(1, COUNT.get());
        assert_eq!(o(&mut sentinel), UD.get());

        // Multiple BELs
        vw(t, b"\x07\x07");
        assert_eq!(3, COUNT.get());
        unsafe { free(t) };
    }

    #[test]
    fn bell_without_callback_is_silent() {
        let t = new_t(80, 24);
        // BEL without a callback should not crash
        vw(t, b"\x07");
        unsafe { free(t) };
    }

    #[test]
    fn set_enquiry_callback() {
        unsafe extern "C" fn enquiry(_: Terminal, _: *mut c_void) -> LibString {
            LibString::init(b"OK")
        }

        let t = new_t(80, 24);
        set_write_pty(t);
        assert_eq!(Result::Success, sopt(t, Option::Enquiry, enquiry as EnquiryFn as *const c_void));

        // ENQ (0x05) should trigger the enquiry callback and write response via write_pty
        vw(t, b"\x05");
        assert_eq!(Some(b"OK".to_vec()), last());
        unsafe { free(t) };
    }

    #[test]
    fn enquiry_without_callback_is_silent() {
        let t = new_t(80, 24);
        // ENQ without a callback should not crash
        vw(t, b"\x05");
        unsafe { free(t) };
    }

    #[test]
    fn set_xtversion_callback() {
        unsafe extern "C" fn xtversion(_: Terminal, _: *mut c_void) -> LibString {
            LibString::init(b"myterm 1.0")
        }

        let t = new_t(80, 24);
        set_write_pty(t);
        assert_eq!(Result::Success, sopt(t, Option::Xtversion, xtversion as XtversionFn as *const c_void));

        // XTVERSION: CSI > q
        vw(t, b"\x1B[>q");
        // Response should be DCS >| version ST
        assert_eq!(Some(b"\x1BP>|myterm 1.0\x1B\\".to_vec()), last());
        unsafe { free(t) };
    }

    #[test]
    fn xtversion_without_callback_reports_default() {
        let t = new_t(80, 24);

        // Set write_pty but not xtversion — should get default "libghostty"
        set_write_pty(t);

        vw(t, b"\x1B[>q");
        assert_eq!(Some(b"\x1BP>|libghostty\x1B\\".to_vec()), last());
        unsafe { free(t) };
    }

    #[test]
    fn set_terminfo_name_option() {
        let t = new_t(80, 24);
        set_write_pty(t);

        // While no name is set the query goes unanswered; other capabilities
        // are still served from the static map.
        let query = format!("\x1BP+q{}\x1B\\", hex_upper(b"TN"));
        vw(t, query.as_bytes());
        assert!(last().is_none());
        let co_query = format!("\x1BP+q{}\x1B\\", hex_upper(b"Co"));
        vw(t, co_query.as_bytes());
        assert_eq!(
            Some(format!("\x1BP1+r{}={}\x1B\\", hex_upper(b"Co"), hex_upper(b"256")).into_bytes()),
            last()
        );
        clear_last();

        // The name is copied, so the caller's buffer can go away afterwards.
        let mut name: [u8; 14] = *b"xterm-256color";
        let value = LibString {
            ptr: name.as_ptr(),
            len: name.len(),
        };
        assert_eq!(Result::Success, sopt(t, Option::TerminfoName, p(&value)));
        name.fill(b'z');

        vw(t, query.as_bytes());
        assert_eq!(
            Some(format!("\x1BP1+r{}={}\x1B\\", hex_upper(b"TN"), hex_upper(b"xterm-256color")).into_bytes()),
            last()
        );

        // Clearing with NULL leaves the query unanswered again.
        clear_last();
        assert_eq!(Result::Success, sopt(t, Option::TerminfoName, std::ptr::null()));
        vw(t, query.as_bytes());
        assert!(last().is_none());

        // Names beyond the maximum are rejected rather than truncated.
        let long = [b'a'; Handler::MAX_TERMINFO_NAME_BYTES + 1];
        let long_value = LibString {
            ptr: long.as_ptr(),
            len: long.len(),
        };
        assert_eq!(Result::InvalidValue, sopt(t, Option::TerminfoName, p(&long_value)));
        unsafe { free(t) };
    }

    #[test]
    fn set_title_changed_callback() {
        let t = new_t(80, 24);

        let mut sentinel: u8 = 77;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));
        assert_eq!(
            Result::Success,
            sopt(t, Option::TitleChanged, count_cb as TitleChangedFn as *const c_void)
        );

        // OSC 2 ; title ST — set window title
        vw(t, b"\x1B]2;Hello\x1B\\");
        assert_eq!(1, COUNT.get());
        assert_eq!(o(&mut sentinel), UD.get());

        // Another title change
        vw(t, b"\x1B]2;World\x1B\\");
        assert_eq!(2, COUNT.get());
        unsafe { free(t) };
    }

    #[test]
    fn set_render_hold_callback() {
        thread_local! {
            static EVENTS: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
        }
        unsafe extern "C" fn hold(_: Terminal, ud: *mut c_void, held: bool) {
            EVENTS.with(|e| e.borrow_mut().push(held));
            UD.set(ud);
        }
        let events = || EVENTS.with(|e| e.borrow().clone());
        let clear = || EVENTS.with(|e| e.borrow_mut().clear());

        let t = new_t(80, 24);

        let mut sentinel: u8 = 0;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));
        assert_eq!(Result::Success, sopt(t, Option::RenderHold, hold as RenderHoldFn as *const c_void));

        // A set during a hold and a reset without a hold are ignored.
        let frame = b"\x1b[?2026h\x1b[?2026hA\x1b[?2026l\x1b[?2026l";
        vw(t, frame);
        assert_eq!(vec![true, false], events());
        assert_eq!(o(&mut sentinel), UD.get());

        // Resize and reset turn the mode off and end the hold.
        let begin = b"\x1b[?2026h";
        clear();
        vw(t, begin);
        assert_eq!(Result::Success, unsafe { resize(t, 80, 24, 9, 18) });
        vw(t, begin);
        unsafe {
            reset(t);
            reset(t);
        }
        assert_eq!(vec![true, false, true, false], events());

        // Changing the mode ourselves (e.g. for a timeout) isn't reported.
        clear();
        vw(t, begin);
        let off = ModeConfig {
            mode: 2026,
            value: false,
        };
        assert_eq!(Result::Success, sopt(t, Option::Mode, p(&off)));
        assert!(!zt(t).modes.get(Mode::SynchronizedOutput));
        assert_eq!(vec![true], events());

        // Clearing the callback silences it.
        clear();
        assert_eq!(Result::Success, sopt(t, Option::RenderHold, std::ptr::null()));
        vw(t, frame);
        assert!(events().is_empty());
        unsafe { free(t) };
    }

    #[test]
    fn title_changed_without_callback_is_silent() {
        let t = new_t(80, 24);
        // OSC 2 without a callback should not crash
        vw(t, b"\x1B]2;Hello\x1B\\");
        unsafe { free(t) };
    }

    #[test]
    fn kitty_image_medium_temp_file_empty_output() {
        if !build_options::KITTY_GRAPHICS {
            return;
        }
        let t = new_t(80, 24);

        let empty = LibString::init(b"");
        assert_eq!(Result::Success, sopt(t, Option::KittyImageMediumTempFile, p(&empty)));
        let mut result = LibString {
            ptr: std::ptr::null(),
            len: 99,
        };
        assert_eq!(Result::Success, gdat(t, TerminalData::KittyImageMediumTempFile, o(&mut result)));
        assert_eq!(0, result.len);
        assert_eq!(LibString::empty_ptr(), result.ptr);
        unsafe { free(t) };
    }

    #[test]
    fn set_desktop_notification_callback() {
        thread_local! {
            static LAST_N: RefCell<(usize, Vec<u8>, Vec<u8>)> = const { RefCell::new((0, Vec::new(), Vec::new())) };
        }
        unsafe extern "C" fn notify(_: Terminal, ud: *mut c_void, n: *const DesktopNotification) {
            COUNT.set(COUNT.get() + 1);
            UD.set(ud);
            let n = &*n;
            LAST_N.with(|l| *l.borrow_mut() = (n.size, cs(n.title), cs(n.body)));
        }
        let last_n = || LAST_N.with(|l| l.borrow().clone());

        let t = new_t(80, 24);

        let mut sentinel: u8 = 99;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));
        assert_eq!(
            Result::Success,
            sopt(t, Option::DesktopNotification, notify as DesktopNotificationFn as *const c_void)
        );

        // Split OSC 777 across writes to exercise the persistent VT parser.
        vw(t, b"\x1B]777;notify;Codex;");
        assert_eq!(0, COUNT.get());
        vw(t, b"Needs attention\x1B\\");
        assert_eq!(1, COUNT.get());
        assert_eq!(o(&mut sentinel), UD.get());
        let (size, title, body) = last_n();
        assert_eq!(std::mem::size_of::<DesktopNotification>(), size);
        assert_eq!(b"Codex", title.as_slice());
        assert_eq!(b"Needs attention", body.as_slice());

        // OSC 9 has no title and preserves its body.
        let seq_c = b"\x1B]9;Build complete\x07";
        vw(t, seq_c);
        assert_eq!(2, COUNT.get());
        let (_, title, body) = last_n();
        assert_eq!(b"", title.as_slice());
        assert_eq!(b"Build complete", body.as_slice());

        // Removing the callback takes effect immediately.
        assert_eq!(Result::Success, sopt(t, Option::DesktopNotification, std::ptr::null()));
        vw(t, seq_c);
        assert_eq!(2, COUNT.get());
        unsafe { free(t) };
    }

    #[test]
    fn set_resize_window_callback() {
        thread_local! {
            static LAST_RW: Cell<(c_int, ResizeWindowGrid)> = const { Cell::new((-1, ResizeWindowGrid { rows: 0, columns: 0 })) };
        }
        unsafe extern "C" fn resize_window(_: Terminal, ud: *mut c_void, request: *const ResizeWindow) {
            COUNT.set(COUNT.get() + 1);
            UD.set(ud);
            let r = &*request;
            LAST_RW.set((r.tag as c_int, r.value.grid));
        }

        let t = new_t(80, 24);

        let mut sentinel: u8 = 44;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));
        assert_eq!(
            Result::Success,
            sopt(t, Option::ResizeWindow, resize_window as ResizeWindowFn as *const c_void)
        );

        let seq = b"\x1B[8;30;100t";
        vw(t, seq);
        assert_eq!(1, COUNT.get());
        assert_eq!(o(&mut sentinel), UD.get());
        let (tag, grid) = LAST_RW.get();
        assert_eq!(ResizeWindowTag::Grid as c_int, tag);
        assert_eq!(30, grid.rows);
        assert_eq!(100, grid.columns);

        // Removing the callback takes effect immediately.
        assert_eq!(Result::Success, sopt(t, Option::ResizeWindow, std::ptr::null()));
        vw(t, seq);
        assert_eq!(1, COUNT.get());
        unsafe { free(t) };
    }

    #[test]
    fn set_progress_report_callback() {
        thread_local! {
            static LAST_PR: Cell<(usize, ProgressState, i8)> = const { Cell::new((0, ProgressState::Remove, -1)) };
        }
        unsafe extern "C" fn progress_report(_: Terminal, ud: *mut c_void, report: *const ProgressReport) {
            COUNT.set(COUNT.get() + 1);
            UD.set(ud);
            let r = &*report;
            LAST_PR.set((r.size, r.state, r.progress));
        }

        let t = new_t(80, 24);

        let mut sentinel: u8 = 100;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));
        assert_eq!(
            Result::Success,
            sopt(t, Option::ProgressReport, progress_report as ProgressReportFn as *const c_void)
        );

        let cases: [(&[u8], ProgressState, i8); 5] = [
            (b"\x1B]9;4;0;\x1B\\", ProgressState::Remove, -1),
            (b"\x1B]9;4;1;42\x07", ProgressState::Set, 42),
            (b"\x1B]9;4;2;7\x1B\\", ProgressState::Error, 7),
            (b"\x1B]9;4;3\x1B\\", ProgressState::Indeterminate, -1),
            (b"\x1B]9;4;4;75\x1B\\", ProgressState::Pause, 75),
        ];

        for (i, (sequence, state, progress)) in cases.iter().enumerate() {
            let expected_count = i + 1;
            let midpoint = sequence.len() / 2;
            vw(t, &sequence[..midpoint]);
            assert_eq!(expected_count - 1, COUNT.get());
            vw(t, &sequence[midpoint..]);
            assert_eq!(expected_count, COUNT.get());
            assert_eq!(o(&mut sentinel), UD.get());
            let (size, last_state, last_progress) = LAST_PR.get();
            assert_eq!(std::mem::size_of::<ProgressReport>(), size);
            assert_eq!(*state, last_state);
            assert_eq!(*progress, last_progress);
        }

        assert_eq!(Result::Success, sopt(t, Option::ProgressReport, std::ptr::null()));
        vw(t, b"\x1B]9;4;1;90\x1B\\");
        assert_eq!(cases.len(), COUNT.get());
        unsafe { free(t) };
    }

    #[test]
    fn set_program_status_callback() {
        #[derive(Clone)]
        struct Last {
            size: usize,
            state: ProgramStatusState,
            kind: ProgramStatusKind,
            progress: i8,
            id: Vec<u8>,
            app: Vec<u8>,
            title: Vec<u8>,
            message: Vec<u8>,
        }
        thread_local! {
            static LAST_PS: RefCell<std::option::Option<Last>> = const { RefCell::new(None) };
        }
        unsafe extern "C" fn program_status(_: Terminal, ud: *mut c_void, report: *const ProgramStatus) {
            COUNT.set(COUNT.get() + 1);
            UD.set(ud);
            let r = &*report;
            LAST_PS.with(|l| {
                *l.borrow_mut() = Some(Last {
                    size: r.size,
                    state: r.state,
                    kind: r.kind,
                    progress: r.progress,
                    id: cs(r.id),
                    app: cs(r.app),
                    title: cs(r.title),
                    message: cs(r.message),
                })
            });
        }
        let last_ps = || LAST_PS.with(|l| l.borrow().clone().unwrap());

        let t = new_t(80, 24);

        let mut sentinel: u8 = 100;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));
        set_write_pty(t);

        // Without the callback, the support query is not answered.
        let query = b"\x1B]7501;?\x1B\\";
        vw(t, query);
        assert!(all_written().is_empty());

        assert_eq!(
            Result::Success,
            sopt(t, Option::ProgramStatus, program_status as ProgramStatusFn as *const c_void)
        );

        // With the callback, it is answered with the states and kinds we
        // accept and the same terminator.
        vw(t, query);
        assert_eq!(
            b"\x1B]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1B\\".to_vec(),
            all_written()
        );
        assert_eq!(0, COUNT.get());

        // "Plan" and "Apply?"
        let report =
            b"\x1B]7501;state=blocked:kind=permission:progress=40:id=a/b:app=terraform:title=UGxhbg==:msg=QXBwbHk/\x07";
        vw(t, report);
        assert_eq!(1, COUNT.get());
        assert_eq!(o(&mut sentinel), UD.get());
        let l = last_ps();
        assert_eq!(std::mem::size_of::<ProgramStatus>(), l.size);
        assert_eq!(ProgramStatusState::Blocked, l.state);
        assert_eq!(ProgramStatusKind::Permission, l.kind);
        assert_eq!(40, l.progress);
        assert_eq!(b"a/b", l.id.as_slice());
        assert_eq!(b"terraform", l.app.as_slice());
        assert_eq!(b"Plan", l.title.as_slice());
        assert_eq!(b"Apply?", l.message.as_slice());

        // Absent values are empty, none, or -1.
        let minimal = b"\x1B]7501;state=done\x1B\\";
        vw(t, minimal);
        assert_eq!(2, COUNT.get());
        let l = last_ps();
        assert_eq!(ProgramStatusState::Done, l.state);
        assert_eq!(ProgramStatusKind::None, l.kind);
        assert_eq!(-1, l.progress);
        assert!(l.id.is_empty());
        assert!(l.app.is_empty());
        assert!(l.title.is_empty());
        assert!(l.message.is_empty());

        // Unsetting the callback ignores reports and the query again.
        assert_eq!(Result::Success, sopt(t, Option::ProgramStatus, std::ptr::null()));
        clear_all();
        vw(t, query);
        vw(t, minimal);
        assert!(all_written().is_empty());
        assert_eq!(2, COUNT.get());
        unsafe { free(t) };
    }

    #[test]
    fn set_semantic_prompt_callback() {
        #[derive(Clone)]
        struct Last {
            size: usize,
            kind: SemanticPromptKind,
            prompt_kind: SemanticPromptPromptKind,
            has_exit_code: bool,
            exit_code: i32,
            command: Vec<u8>,
            error: Vec<u8>,
        }
        thread_local! {
            static LAST_SP: RefCell<std::option::Option<Last>> = const { RefCell::new(None) };
        }
        unsafe extern "C" fn semantic_prompt(_: Terminal, ud: *mut c_void, event: *const SemanticPrompt) {
            COUNT.set(COUNT.get() + 1);
            UD.set(ud);
            let e = &*event;
            LAST_SP.with(|l| {
                *l.borrow_mut() = Some(Last {
                    size: e.size,
                    kind: e.kind,
                    prompt_kind: e.prompt_kind,
                    has_exit_code: e.has_exit_code,
                    exit_code: e.exit_code,
                    command: cs(e.command),
                    error: cs(e.error),
                })
            });
        }
        let last_sp = || LAST_SP.with(|l| l.borrow().clone().unwrap());

        let t = new_t(80, 24);

        let mut sentinel: u8 = 100;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));
        assert_eq!(
            Result::Success,
            sopt(t, Option::SemanticPrompt, semantic_prompt as SemanticPromptFn as *const c_void)
        );

        let prompt = b"\x1B]133;P;k=r\x07";
        vw(t, prompt);
        assert_eq!(1, COUNT.get());
        assert_eq!(o(&mut sentinel), UD.get());
        let l = last_sp();
        assert_eq!(std::mem::size_of::<SemanticPrompt>(), l.size);
        assert_eq!(SemanticPromptKind::PromptStart, l.kind);
        assert_eq!(SemanticPromptPromptKind::Right, l.prompt_kind);
        assert!(!l.has_exit_code);

        vw(t, b"\x1B]133;B\x07");
        assert_eq!(2, COUNT.get());
        let l = last_sp();
        assert_eq!(SemanticPromptKind::InputStart, l.kind);
        assert_eq!(SemanticPromptPromptKind::Primary, l.prompt_kind);

        vw(t, b"\x1B]133;C;cmdline_url=ls%20-la\x07");
        assert_eq!(3, COUNT.get());
        let l = last_sp();
        assert_eq!(SemanticPromptKind::OutputStart, l.kind);
        assert_eq!(b"ls -la", l.command.as_slice());

        vw(t, b"\x1B]133;D;-1;err=boom\x07");
        assert_eq!(4, COUNT.get());
        let l = last_sp();
        assert_eq!(SemanticPromptKind::CommandEnd, l.kind);
        assert!(l.has_exit_code);
        assert_eq!(-1, l.exit_code);
        assert_eq!(b"boom", l.error.as_slice());
        assert!(l.command.is_empty());

        vw(t, b"\x1B]133;D\x07");
        assert_eq!(5, COUNT.get());
        let l = last_sp();
        assert!(!l.has_exit_code);
        assert_eq!(0, l.exit_code);
        assert!(l.error.is_empty());

        assert_eq!(Result::Success, sopt(t, Option::SemanticPrompt, std::ptr::null()));
        vw(t, prompt);
        assert_eq!(5, COUNT.get());
        unsafe { free(t) };
    }

    #[test]
    fn set_reset_callback() {
        let t = new_t(80, 24);

        let mut sentinel: u8 = 100;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));
        assert_eq!(Result::Success, sopt(t, Option::Reset, count_cb as ResetFn as *const c_void));

        // A soft reset (DECSTR) doesn't report a reset.
        vw(t, b"\x1B[!p");
        assert_eq!(0, COUNT.get());

        let full = b"\x1Bc";
        vw(t, full);
        assert_eq!(1, COUNT.get());
        assert_eq!(o(&mut sentinel), UD.get());

        assert_eq!(Result::Success, sopt(t, Option::Reset, std::ptr::null()));
        vw(t, full);
        assert_eq!(1, COUNT.get());
        unsafe { free(t) };
    }

    #[test]
    fn set_unknown_sequence_callback() {
        #[derive(Clone)]
        struct Last {
            terminal: Terminal,
            tag: UnknownSequenceTag,
            truncated: bool,
            terminator: osc::TerminatorC,
            content: Vec<u8>,
        }
        thread_local! {
            static LAST_US: RefCell<std::option::Option<Last>> = const { RefCell::new(None) };
        }
        unsafe extern "C" fn unknown_sequence(terminal_: Terminal, ud: *mut c_void, sequence: *const UnknownSequence) {
            COUNT.set(COUNT.get() + 1);
            UD.set(ud);
            let s = &*sequence;
            let prev_terminator = LAST_US.with(|l| l.borrow().as_ref().map_or(osc::TerminatorC::St, |l| l.terminator));
            let (truncated, terminator, str_) = match s.tag {
                UnknownSequenceTag::Apc => {
                    let apc_value = s.value.apc;
                    (apc_value.truncated, prev_terminator, apc_value.content)
                }
                UnknownSequenceTag::Osc => {
                    let osc_value = s.value.osc;
                    (osc_value.truncated, osc_value.terminator, osc_value.content)
                }
            };
            let mut content = cs(str_);
            content.truncate(64);
            LAST_US.with(|l| {
                *l.borrow_mut() = Some(Last {
                    terminal: terminal_,
                    tag: s.tag,
                    truncated,
                    terminator,
                    content,
                })
            });
        }
        let last_us = || LAST_US.with(|l| l.borrow().clone().unwrap());

        let t = new_t(80, 24);

        let mut sentinel: u8 = 101;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));

        let max_bytes: usize = 8;
        assert_eq!(Result::Success, sopt(t, Option::UnknownMaxBytes, p(&max_bytes)));
        assert_eq!(max_bytes, wr(t).stream.handler.apc_handler.unknown_max_bytes);
        assert_eq!(max_bytes, wr(t).stream.parser.osc_parser.unknown_max_bytes);

        // A byte limit without a callback performs no external effect.
        let before_callback = b"\x1B_abc;xy\x1B\\";
        vw(t, before_callback);
        assert_eq!(0, COUNT.get());
        assert!(wr(t).stream.handler.unknown_sequence.is_none());

        let cb = unknown_sequence as UnknownSequenceFn as *const c_void;
        assert_eq!(Result::Success, sopt(t, Option::UnknownSequence, cb));
        assert!(wr(t).stream.handler.unknown_sequence.is_some());

        // Split a complete APC across writes to exercise persistent parser state.
        vw(t, b"\x1B_abc;");
        assert_eq!(0, COUNT.get());
        vw(t, b"xy\x1B\\");
        assert_eq!(1, COUNT.get());
        let l = last_us();
        assert_eq!(t, l.terminal);
        assert_eq!(o(&mut sentinel), UD.get());
        assert_eq!(UnknownSequenceTag::Apc, l.tag);
        assert!(!l.truncated);
        assert_eq!(b"abc;xy", l.content.as_slice());

        // Content beyond the generic limit is omitted and marked truncated.
        vw(t, b"\x1B_abcdefghijkl\x1B\\");
        assert_eq!(2, COUNT.get());
        let l = last_us();
        assert!(l.truncated);
        assert_eq!(b"abcdefgh", l.content.as_slice());

        // CAN aborts the APC and must not invoke the callback.
        vw(t, b"\x1B_abcdef\x18");
        assert_eq!(2, COUNT.get());

        // Unknown OSCs share the callback and the byte limit.
        let osc_st = b"\x1B]7400;x\x1B\\";
        vw(t, osc_st);
        assert_eq!(3, COUNT.get());
        let l = last_us();
        assert_eq!(UnknownSequenceTag::Osc, l.tag);
        assert!(!l.truncated);
        assert_eq!(osc::TerminatorC::St, l.terminator);
        assert_eq!(b"7400;x", l.content.as_slice());

        vw(t, b"\x1B]7400;abcdef\x07");
        assert_eq!(4, COUNT.get());
        let l = last_us();
        assert_eq!(UnknownSequenceTag::Osc, l.tag);
        assert!(l.truncated);
        assert_eq!(osc::TerminatorC::Bel, l.terminator);
        assert_eq!(b"7400;abc", l.content.as_slice());

        // Aborted unknown OSCs and supported OSCs are not reported.
        vw(t, b"\x1B]7400;x\x18\x1B]2;title\x07");
        assert_eq!(4, COUNT.get());

        // Clearing the callback restores the null fast path immediately.
        assert_eq!(Result::Success, sopt(t, Option::UnknownSequence, std::ptr::null()));
        assert!(wr(t).stream.handler.unknown_sequence.is_none());
        vw(t, before_callback);
        vw(t, osc_st);
        assert_eq!(4, COUNT.get());

        // A NULL limit disables capture even after reinstalling the callback.
        assert_eq!(Result::Success, sopt(t, Option::UnknownSequence, cb));
        assert_eq!(Result::Success, sopt(t, Option::UnknownMaxBytes, std::ptr::null()));
        assert_eq!(0, wr(t).stream.handler.apc_handler.unknown_max_bytes);
        assert_eq!(0, wr(t).stream.parser.osc_parser.unknown_max_bytes);
        vw(t, before_callback);
        vw(t, osc_st);
        assert_eq!(4, COUNT.get());
        unsafe { free(t) };
    }

    #[test]
    fn set_pwd_changed_callback() {
        let t = new_t(80, 24);

        let mut sentinel: u8 = 88;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));
        assert_eq!(Result::Success, sopt(t, Option::PwdChanged, count_cb as PwdChangedFn as *const c_void));

        // OSC 7 ; file:///tmp ST — report pwd
        vw(t, b"\x1B]7;file:///tmp\x1B\\");
        assert_eq!(1, COUNT.get());
        assert_eq!(o(&mut sentinel), UD.get());
        assert_eq!(Some(&b"file:///tmp"[..]), zt(t).get_pwd());

        // Another pwd change
        vw(t, b"\x1B]7;file:///home/user\x1B\\");
        assert_eq!(2, COUNT.get());
        assert_eq!(Some(&b"file:///home/user"[..]), zt(t).get_pwd());
        unsafe { free(t) };
    }

    /// Capture state for the clipboard write callbacks.
    #[derive(Clone, Default)]
    struct ClipWriteState {
        count: usize,
        last_terminal: usize,
        last_userdata: usize,
        last_size: usize,
        last_location: c_int,
        last_contents_null: bool,
        last_contents_len: usize,
        last_mimes: Vec<Vec<u8>>,
        last_data: Vec<Vec<u8>>,
        last_name_len: usize,
        last_granted: bool,
        last_can_remember: bool,
        next_result: c_int,
        next_remember: bool,
    }

    thread_local! {
        static CW: RefCell<ClipWriteState> = RefCell::new(ClipWriteState::default());
    }

    fn cw() -> ClipWriteState {
        CW.with(|c| c.borrow().clone())
    }

    fn cw_mut(f: impl FnOnce(&mut ClipWriteState)) {
        CW.with(|c| f(&mut c.borrow_mut()))
    }

    unsafe extern "C" fn clipboard_write_cb(terminal_: Terminal, ud: *mut c_void, request: *const ClipboardWrite) {
        let r = &*request;
        let contents: &[ClipboardContent] = super::super::types::slice_or_empty(r.contents, r.contents_len);
        let (next_result, next_remember) = CW.with(|c| {
            let mut s = c.borrow_mut();
            s.count += 1;
            s.last_terminal = terminal_ as usize;
            s.last_userdata = ud as usize;
            s.last_size = r.size;
            s.last_location = r.location.0;
            s.last_contents_null = r.contents.is_null();
            s.last_contents_len = r.contents_len;
            s.last_name_len = r.name.len;
            s.last_granted = r.granted;
            s.last_can_remember = r.can_remember;
            if !r.contents.is_null() {
                s.last_mimes = contents.iter().map(|c| cs(c.mime)).collect();
                s.last_data = contents.iter().map(|c| cs(c.data)).collect();
            }
            (s.next_result, s.next_remember)
        });
        (r.reply)(
            request,
            &ClipboardWriteReply {
                size: std::mem::size_of::<ClipboardWriteReply>(),
                result: clipboard::WriteStatus(next_result),
                remember: next_remember,
            },
        );
    }

    #[test]
    fn set_clipboard_write_callback() {
        let t = new_t(80, 24);
        cw_mut(|s| s.next_result = clipboard::WriteStatus::DENIED.0);

        let mut sentinel: u8 = 88;
        assert_eq!(Result::Success, sopt(t, Option::Userdata, o(&mut sentinel)));
        assert_eq!(
            Result::Success,
            sopt(t, Option::ClipboardWrite, clipboard_write_cb as ClipboardWriteFn as *const c_void)
        );

        // Split OSC 52 write whose decoded payload contains an embedded NUL.
        vw(t, b"\x1B]52;c;aGVs");
        vw(t, b"bG8Ad29ybGQ=\x1B\\");
        let s = cw();
        assert_eq!(1, s.count);
        assert_eq!(t as usize, s.last_terminal);
        assert_eq!(o(&mut sentinel) as usize, s.last_userdata);
        assert_eq!(std::mem::size_of::<ClipboardWrite>(), s.last_size);
        assert_eq!(clipboard::Location::STANDARD.0, s.last_location);
        assert!(!s.last_contents_null);
        assert_eq!(1, s.last_contents_len);
        assert_eq!(b"text/plain", s.last_mimes[0].as_slice());
        assert_eq!(b"hello\x00world", s.last_data[0].as_slice());

        // OSC 52 carries no program identity or password grant state.
        assert_eq!(0, s.last_name_len);
        assert!(!s.last_granted);
        assert!(!s.last_can_remember);

        // OSC 52 destinations are normalized rather than exposed as wire bytes.
        for (selector, expected) in [
            (b's', clipboard::Location::SELECTION),
            (b'p', clipboard::Location::PRIMARY),
            (b'q', clipboard::Location::STANDARD),
        ] {
            let seq = [0x1B, b']', b'5', b'2', b';', selector, b';', b'e', b'A', b'=', b'=', 0x1B, b'\\'];
            vw(t, &seq);
            assert_eq!(expected.0, cw().last_location);
        }
        assert_eq!(4, cw().count);

        // An empty content list is a clear and retains a null descriptor pointer.
        vw(t, b"\x1B]52;s;\x1B\\");
        let s = cw();
        assert_eq!(5, s.count);
        assert_eq!(clipboard::Location::SELECTION.0, s.last_location);
        assert!(s.last_contents_null);
        assert_eq!(0, s.last_contents_len);

        // Read requests and malformed base64 must never reach the callback.
        vw(t, b"\x1B]52;c;?\x1B\\");
        vw(t, b"\x1B]52;c;%%%\x1B\\");
        assert_eq!(5, cw().count);

        // iTerm2 Copy reaches the same normalized callback.
        vw(t, b"\x1B]1337;Copy=:aVRlcm0=\x1B\\");
        let s = cw();
        assert_eq!(6, s.count);
        assert_eq!(clipboard::Location::STANDARD.0, s.last_location);
        assert_eq!(b"text/plain", s.last_mimes[0].as_slice());
        assert_eq!(b"iTerm", s.last_data[0].as_slice());

        // Every representation is converted, and callback replies propagate
        // back through the C trampoline for protocols that can acknowledge
        // writes.
        let internal_contents = [
            clipboard::Content {
                mime: b"text/plain",
                data: b"plain",
            },
            clipboard::Content {
                mime: b"application/octet-stream",
                data: b"a\x00b",
            },
            clipboard::Content {
                mime: b"text/html",
                data: b"<b>plain</b>",
            },
            clipboard::Content {
                mime: b"text/rtf",
                data: b"{\\rtf1 plain}",
            },
            clipboard::Content {
                mime: b"image/png",
                data: b"\x89PNG",
            },
        ];
        thread_local! {
            static REPLY_LAST: Cell<std::option::Option<clipboard::WriteResult>> = const { Cell::new(None) };
        }
        fn reply(_: *mut c_void, result: clipboard::WriteResult) {
            REPLY_LAST.set(Some(result));
        }
        cw_mut(|s| s.next_result = clipboard::WriteStatus::BUSY.0);
        let handler: *mut Handler<'static> = &mut wr(t).stream.handler;
        unsafe {
            let f = (*handler).effects.clipboard_write.unwrap();
            f(
                &mut *handler,
                clipboard::Write {
                    location: clipboard::Location::PRIMARY,
                    contents: &internal_contents,
                    name: b"app",
                    granted: true,
                    can_remember: true,
                    reply_ctx: handler as *mut c_void,
                    reply_fn: reply,
                },
            );
        }
        assert_eq!(Some(clipboard::WriteResult::Busy), REPLY_LAST.get());
        let s = cw();
        assert_eq!(3, s.last_name_len);
        assert!(s.last_granted);
        assert!(s.last_can_remember);
        assert_eq!(7, s.count);
        assert_eq!(5, s.last_contents_len);
        assert_eq!(b"application/octet-stream", s.last_mimes[1].as_slice());
        assert_eq!(b"a\x00b", s.last_data[1].as_slice());
        assert_eq!(b"image/png", s.last_mimes[4].as_slice());
        assert_eq!(b"\x89PNG", s.last_data[4].as_slice());

        // Removing the callback takes effect immediately and uninstalls
        // the trampoline.
        assert_eq!(Result::Success, sopt(t, Option::ClipboardWrite, std::ptr::null()));
        assert!(wr(t).stream.handler.effects.clipboard_write.is_none());
        vw(t, b"\x1B]52;c;eA==\x1B\\");
        assert_eq!(7, cw().count);
        unsafe { free(t) };
    }

    #[test]
    fn clipboard_write_without_callback_is_unsupported_and_silent() {
        let t = new_t(80, 24);

        // OSC 52 without a callback should not crash
        vw(t, b"\x1B]52;c;aGVsbG8=\x1B\\");

        // No trampoline is installed until a callback is set, so the
        // stream skips clipboard work (and never spools a Kitty clipboard
        // transaction it can't deliver).
        assert!(wr(t).stream.handler.effects.clipboard_write.is_none());
        unsafe { free(t) };
    }

    /// Capture state for the clipboard read callbacks.
    #[derive(Clone, Default)]
    struct ClipReadState {
        count: usize,
        last_size: usize,
        last_location: c_int,
        last_mimes_len: usize,
        last_mime_is_text: bool,
        last_list: bool,
        last_name_len: usize,
        last_granted: bool,
        last_can_remember: bool,
        result: c_int,
    }

    thread_local! {
        static CR: RefCell<ClipReadState> = RefCell::new(ClipReadState::default());
    }

    fn cr() -> ClipReadState {
        CR.with(|c| c.borrow().clone())
    }

    unsafe extern "C" fn clipboard_read_cb(_: Terminal, _: *mut c_void, request: *const ClipboardRead) {
        let r = &*request;
        let mimes: &[LibString] = super::super::types::slice_or_empty(r.mimes, r.mimes_len);
        let result = CR.with(|c| {
            let mut s = c.borrow_mut();
            s.count += 1;
            s.last_size = r.size;
            s.last_location = r.location.0;
            s.last_mimes_len = r.mimes_len;
            s.last_mime_is_text = r.mimes_len > 0 && cs(mimes[0]) == b"text/plain";
            s.last_list = r.list;
            s.last_name_len = r.name.len;
            s.last_granted = r.granted;
            s.last_can_remember = r.can_remember;
            s.result
        });

        let contents = [ClipboardContent {
            mime: LibString::init(b"text/plain"),
            data: LibString::init(b"hello"),
        }];
        (r.reply)(
            request,
            &ClipboardReadReply {
                size: std::mem::size_of::<ClipboardReadReply>(),
                result: clipboard::ReadStatus(result),
                contents: contents.as_ptr(),
                contents_len: contents.len(),
                available: std::ptr::null(),
                available_len: 0,
                remember: false,
            },
        );
    }

    #[test]
    fn kitty_clipboard_write_via_c_effects() {
        let t = new_t(80, 24);
        set_write_pty(t);
        assert_eq!(
            Result::Success,
            sopt(t, Option::ClipboardWrite, clipboard_write_cb as ClipboardWriteFn as *const c_void)
        );

        // A full OSC 5522 write transaction: begin, chunked data for two
        // representations, commit. Only the commit invokes the callback,
        // and its result maps to the DONE response.
        let seqs: [&[u8]; 5] = [
            b"\x1B]5522;type=write:id=c1\x1B\\",
            b"\x1B]5522;type=wdata:mime=dGV4dC9wbGFpbg==;R2hvc3Q=\x1B\\", // "Ghost"
            b"\x1B]5522;type=wdata:mime=dGV4dC9wbGFpbg==;dHk=\x1B\\",     // "ty"
            b"\x1B]5522;type=wdata:mime=dGV4dC9odG1s;PGI+aGk8L2I+\x1B\\", // "<b>hi</b>"
            b"\x1B]5522;type=wdata\x1B\\",
        ];
        for seq in seqs {
            vw(t, seq);
        }

        let s = cw();
        assert_eq!(1, s.count);
        assert_eq!(clipboard::Location::STANDARD.0, s.last_location);
        assert_eq!(2, s.last_contents_len);
        assert_eq!(b"text/plain", s.last_mimes[0].as_slice());
        assert_eq!(b"Ghostty", s.last_data[0].as_slice());
        assert_eq!(b"text/html", s.last_mimes[1].as_slice());
        assert_eq!(b"<b>hi</b>", s.last_data[1].as_slice());
        assert_eq!(b"\x1B]5522;type=write:status=DONE:id=c1\x1B\\".to_vec(), all_written());
        assert!(!s.last_granted);
        assert!(!s.last_can_remember);

        // Password grants round-trip through the C reply: the first pw'd
        // commit isn't granted and asks to remember, so the next one
        // arrives granted.
        clear_all();
        cw_mut(|s| s.next_remember = true);
        let grant_seqs: [&[u8]; 4] = [
            b"\x1B]5522;type=write:id=g1:pw=c2VjcmV0:name=YXBw\x1B\\",
            b"\x1B]5522;type=wdata\x1B\\",
            b"\x1B]5522;type=write:id=g2:pw=c2VjcmV0:name=YXBw\x1B\\",
            b"\x1B]5522;type=wdata\x1B\\",
        ];
        for seq in grant_seqs {
            vw(t, seq);
        }
        let s = cw();
        assert_eq!(3, s.count);
        assert!(s.last_granted);
        assert!(s.last_can_remember);
        assert_eq!(
            b"\x1B]5522;type=write:status=DONE:id=g1\x1B\\\x1B]5522;type=write:status=DONE:id=g2\x1B\\".to_vec(),
            all_written()
        );
        cw_mut(|s| s.next_remember = false);

        // Without a read callback reads are denied.
        clear_all();
        vw(t, b"\x1B]5522;type=read:id=r1;dGV4dC9wbGFpbg==\x1B\\");
        assert_eq!(3, cw().count);
        assert_eq!(b"\x1B]5522;type=read:status=EPERM:id=r1\x1B\\".to_vec(), all_written());

        // With a read callback the request is served through it.
        assert_eq!(
            Result::Success,
            sopt(t, Option::ClipboardRead, clipboard_read_cb as ClipboardReadFn as *const c_void)
        );
        clear_all();
        // name="app" without a password: forwarded for prompts, not
        // rememberable.
        vw(t, b"\x1B]5522;type=read:id=r2:name=YXBw;dGV4dC9wbGFpbg==\x1B\\");
        let r = cr();
        assert_eq!(1, r.count);
        assert_eq!(1, r.last_mimes_len);
        assert!(r.last_mime_is_text);
        assert!(!r.last_list);
        assert_eq!(3, r.last_name_len);
        assert!(!r.last_granted);
        assert!(!r.last_can_remember);
        assert_eq!(
            concat!(
                "\x1B]5522;type=read:status=OK:id=r2\x1B\\",
                "\x1B]5522;type=read:status=DATA:id=r2:mime=dGV4dC9wbGFpbg==;aGVsbG8=\x1B\\",
                "\x1B]5522;type=read:status=DONE:id=r2\x1B\\",
            )
            .as_bytes()
            .to_vec(),
            all_written()
        );

        // Without a clipboard callback the transaction fails up front.
        assert_eq!(Result::Success, sopt(t, Option::ClipboardWrite, std::ptr::null()));
        clear_all();
        vw(t, b"\x1B]5522;type=write:id=c2\x1B\\");
        assert_eq!(b"\x1B]5522;type=write:status=ENOSYS:id=c2\x1B\\".to_vec(), all_written());
        unsafe { free(t) };
    }

    #[test]
    fn set_clipboard_write_max_bytes() {
        let t = new_t(80, 24);
        set_write_pty(t);
        assert_eq!(
            Result::Success,
            sopt(t, Option::ClipboardWrite, clipboard_write_cb as ClipboardWriteFn as *const c_void)
        );

        // The built-in default reads back.
        let mut max: usize = 0;
        assert_eq!(Result::Success, gdat(t, TerminalData::ClipboardWriteMaxBytes, o(&mut max)));
        assert_eq!(kitty_clipboard::MAX_WRITE_SIZE, max);

        // Set a tiny limit; an oversized text write fails with EFBIG and
        // never reaches the callback.
        let limit: usize = 4;
        assert_eq!(Result::Success, sopt(t, Option::ClipboardWriteMaxBytes, p(&limit)));
        assert_eq!(Result::Success, gdat(t, TerminalData::ClipboardWriteMaxBytes, o(&mut max)));
        assert_eq!(limit, max);

        let seqs: [&[u8]; 4] = [
            b"\x1B]5522;type=write:id=c1\x1B\\",
            b"\x1B]5522;type=wdata:mime=dGV4dC9wbGFpbg==;SGVsbA==\x1B\\", // "Hell"
            b"\x1B]5522;type=wdata:mime=dGV4dC9wbGFpbg==;bw==\x1B\\",     // "o"
            b"\x1B]5522;type=wdata\x1B\\",
        ];
        for seq in seqs {
            vw(t, seq);
        }
        assert_eq!(0, cw().count);
        assert_eq!(b"\x1B]5522;type=write:status=EFBIG:id=c1\x1B\\".to_vec(), all_written());

        // A NULL value reverts to the built-in default.
        assert_eq!(Result::Success, sopt(t, Option::ClipboardWriteMaxBytes, std::ptr::null()));
        assert_eq!(Result::Success, gdat(t, TerminalData::ClipboardWriteMaxBytes, o(&mut max)));
        assert_eq!(kitty_clipboard::MAX_WRITE_SIZE, max);
        unsafe { free(t) };
    }

    #[test]
    fn set_clipboard_read_callback() {
        let t = new_t(80, 24);
        set_write_pty(t);

        // Without a callback the handler effect is unset and reads are silent.
        assert!(wr(t).stream.handler.effects.clipboard_read.is_none());
        let read_st = b"\x1B]52;c;?\x1B\\";
        vw(t, read_st);
        assert!(last().is_none());

        assert_eq!(
            Result::Success,
            sopt(t, Option::ClipboardRead, clipboard_read_cb as ClipboardReadFn as *const c_void)
        );
        assert!(wr(t).stream.handler.effects.clipboard_read.is_some());

        vw(t, b"\x1B]52;p;?\x07");
        let r = cr();
        assert_eq!(1, r.count);
        assert_eq!(std::mem::size_of::<ClipboardRead>(), r.last_size);
        assert_eq!(clipboard::Location::PRIMARY.0, r.last_location);
        assert_eq!(1, r.last_mimes_len);
        assert!(r.last_mime_is_text);
        assert!(!r.last_list);
        assert_eq!(0, r.last_name_len);
        assert!(!r.last_granted);
        assert!(!r.last_can_remember);
        assert_eq!(Some(b"\x1B]52;p;aGVsbG8=\x07".to_vec()), last());

        // Denied replies with an empty clipboard.
        CR.with(|c| c.borrow_mut().result = clipboard::ReadStatus::DENIED.0);
        vw(t, read_st);
        assert_eq!(2, cr().count);
        assert_eq!(Some(b"\x1B]52;c;\x1B\\".to_vec()), last());

        // Clearing the callback uninstalls the handler effect.
        assert_eq!(Result::Success, sopt(t, Option::ClipboardRead, std::ptr::null()));
        assert!(wr(t).stream.handler.effects.clipboard_read.is_none());
        unsafe { free(t) };
    }

    #[test]
    fn pwd_changed_without_callback_is_silent() {
        let t = new_t(80, 24);
        // OSC 7 without a callback should not crash, but should still set the pwd
        vw(t, b"\x1B]7;file:///tmp\x1B\\");
        assert_eq!(Some(&b"file:///tmp"[..]), zt(t).get_pwd());
        unsafe { free(t) };
    }

    unsafe extern "C" fn size_cb(_: Terminal, _: *mut c_void, out_size: *mut size_report::Size) -> bool {
        COUNT.set(COUNT.get() + 1);
        *out_size = size_report::Size {
            rows: 24,
            columns: 80,
            cell_width: 8,
            cell_height: 16,
        };
        true
    }

    #[test]
    fn set_size_callback() {
        let t = new_t(80, 24);
        set_write_pty(t);
        assert_eq!(Result::Success, sopt(t, Option::Size, size_cb as SizeFn as *const c_void));

        // CSI 18 t — report text area size in characters
        vw(t, b"\x1B[18t");
        assert_eq!(Some(b"\x1b[8;24;80t".to_vec()), last());
        unsafe { free(t) };
    }

    #[test]
    fn size_without_callback_is_silent() {
        let t = new_t(80, 24);
        // CSI 18 t without a size callback should not crash
        vw(t, b"\x1B[18t");
        unsafe { free(t) };
    }

    #[test]
    fn mode_2048_enable_and_disable_use_c_callbacks() {
        let t = new_t(80, 24);
        set_write_pty(t);
        assert_eq!(Result::Success, sopt(t, Option::Size, size_cb as SizeFn as *const c_void));

        let enable = b"\x1B[?2048h";
        let disable = b"\x1B[?2048l";
        vw(t, enable);
        vw(t, enable);

        assert_eq!(2, CALLS.get());
        assert_eq!(Some(b"\x1B[48;24;80;384;640t".to_vec()), last());
        vw(t, disable);

        assert_eq!(2, CALLS.get());
        assert!(!zt(t).modes.get(Mode::InBandSizeReports));
        unsafe { free(t) };
    }

    #[test]
    fn mode_2048_enable_tolerates_missing_c_callbacks() {
        let t = new_t(80, 24);
        set_write_pty(t);

        let sequence = b"\x1B[?2048h";
        vw(t, sequence);

        assert_eq!(0, CALLS.get());
        assert!(zt(t).modes.get(Mode::InBandSizeReports));
        let no_write = new_t(80, 24);
        assert_eq!(Result::Success, sopt(no_write, Option::Size, size_cb as SizeFn as *const c_void));
        vw(no_write, sequence);
        assert_eq!(1, COUNT.get());
        assert!(zt(no_write).modes.get(Mode::InBandSizeReports));
        unsafe {
            free(no_write);
            free(t);
        }
    }

    thread_local! {
        static DA: Cell<std::option::Option<DeviceAttributes>> = const { Cell::new(None) };
    }

    unsafe extern "C" fn da_cb(_: Terminal, _: *mut c_void, out: *mut DeviceAttributes) -> bool {
        match DA.get() {
            Some(da) => {
                *out = da;
                true
            }
            None => false,
        }
    }

    fn da(conformance_level: u16, features: &[u16], device_type: u16, firmware_version: u16, unit_id: u32) -> DeviceAttributes {
        let mut f = [0u16; 64];
        f[..features.len()].copy_from_slice(features);
        DeviceAttributes {
            primary: DeviceAttributesPrimary {
                conformance_level,
                features: f,
                num_features: features.len(),
            },
            secondary: DeviceAttributesSecondary {
                device_type,
                firmware_version,
                rom_cartridge: 0,
            },
            tertiary: DeviceAttributesTertiary { unit_id },
        }
    }

    fn da_terminal(attrs: std::option::Option<DeviceAttributes>) -> Terminal {
        DA.set(attrs);
        let t = new_t(80, 24);
        set_write_pty(t);
        assert_eq!(
            Result::Success,
            sopt(t, Option::DeviceAttributes, da_cb as DeviceAttributesFn as *const c_void)
        );
        t
    }

    #[test]
    fn set_device_attributes_callback_primary() {
        let t = da_terminal(Some(da(64, &[22, 52], 1, 10, 0)));
        // CSI c — primary DA
        vw(t, b"\x1B[c");
        assert_eq!(Some(b"\x1b[?64;22;52c".to_vec()), last());
        unsafe { free(t) };
    }

    #[test]
    fn set_device_attributes_callback_secondary() {
        let t = da_terminal(Some(da(62, &[22], 1, 10, 0)));
        // CSI > c — secondary DA
        vw(t, b"\x1B[>c");
        assert_eq!(Some(b"\x1b[>1;10;0c".to_vec()), last());
        unsafe { free(t) };
    }

    #[test]
    fn set_device_attributes_callback_tertiary() {
        let t = da_terminal(Some(da(62, &[], 1, 0, 0xAABBCCDD)));
        // CSI = c — tertiary DA
        vw(t, b"\x1B[=c");
        assert_eq!(Some(b"\x1bP!|AABBCCDD\x1b\\".to_vec()), last());
        unsafe { free(t) };
    }

    #[test]
    fn device_attributes_without_callback_uses_default() {
        let t = new_t(80, 24);
        set_write_pty(t);

        // Without setting a device_attributes callback, DA1 should return the default
        vw(t, b"\x1B[c");
        assert_eq!(Some(b"\x1b[?62;22c".to_vec()), last());
        unsafe { free(t) };
    }

    #[test]
    fn device_attributes_callback_returns_false_uses_default() {
        // Callback returns false, should use default response
        let t = da_terminal(None);
        vw(t, b"\x1B[c");
        assert_eq!(Some(b"\x1b[?62;22c".to_vec()), last());
        unsafe { free(t) };
    }

    fn get_string(t: Terminal, data: TerminalData) -> Vec<u8> {
        let mut s = LibString {
            ptr: std::ptr::null(),
            len: 99,
        };
        assert_eq!(Result::Success, gdat(t, data, o(&mut s)));
        cs(s)
    }

    #[test]
    fn set_and_get_title() {
        let t = new_t(80, 24);

        // No title set yet — should return empty string
        assert!(get_string(t, TerminalData::Title).is_empty());

        // Set title via option
        let hello = LibString::init(b"Hello");
        assert_eq!(Result::Success, sopt(t, Option::Title, p(&hello)));
        assert_eq!(b"Hello", get_string(t, TerminalData::Title).as_slice());

        // Overwrite title
        let world = LibString::init(b"World");
        assert_eq!(Result::Success, sopt(t, Option::Title, p(&world)));
        assert_eq!(b"World", get_string(t, TerminalData::Title).as_slice());

        // Clear title with NULL
        assert_eq!(Result::Success, sopt(t, Option::Title, std::ptr::null()));
        assert!(get_string(t, TerminalData::Title).is_empty());
        unsafe { free(t) };
    }

    #[test]
    fn set_and_get_pwd() {
        let t = new_t(80, 24);

        // No pwd set yet — should return empty string
        assert!(get_string(t, TerminalData::Pwd).is_empty());

        // Set pwd via option
        let home = LibString::init(b"/home/user");
        assert_eq!(Result::Success, sopt(t, Option::Pwd, p(&home)));
        assert_eq!(b"/home/user", get_string(t, TerminalData::Pwd).as_slice());

        // Clear pwd with NULL
        assert_eq!(Result::Success, sopt(t, Option::Pwd, std::ptr::null()));
        assert!(get_string(t, TerminalData::Pwd).is_empty());
        unsafe { free(t) };
    }

    #[test]
    fn get_title_set_via_vt_write() {
        let t = new_t(80, 24);
        // Set title via OSC 2
        vw(t, b"\x1B]2;VT Title\x1B\\");
        assert_eq!(b"VT Title", get_string(t, TerminalData::Title).as_slice());
        unsafe { free(t) };
    }

    #[test]
    fn title_report_requires_explicit_opt_in() {
        let t = new_t(80, 24);
        set_write_pty(t);

        let query_title = b"\x1B[21t";
        vw(t, b"\x1B]2;echo vulnerable\x1B\\");

        // WRITE_PTY alone must not enable the security-sensitive response.
        vw(t, query_title);
        assert!(last().is_none());

        let enabled = true;
        assert_eq!(Result::Success, sopt(t, Option::TitleReport, p(&enabled)));
        vw(t, query_title);
        assert_eq!(Some(b"\x1b]lecho vulnerable\x1b\\".to_vec()), last());

        // NULL restores the secure default.
        clear_last();
        assert_eq!(Result::Success, sopt(t, Option::TitleReport, std::ptr::null()));
        vw(t, query_title);
        assert!(last().is_none());
        unsafe { free(t) };
    }

    #[test]
    fn checksum_report_requires_explicit_opt_in() {
        let t = new_t(80, 24);
        set_write_pty(t);

        let query = b"\x1B[1;1;1;1;1;5*y";
        vw(t, b"hello");

        // WRITE_PTY alone must not enable the security-sensitive response.
        vw(t, query);
        assert!(last().is_none());

        let enabled = true;
        assert_eq!(Result::Success, sopt(t, Option::XtChecksumReport, p(&enabled)));
        vw(t, query);
        assert_eq!(Some(b"\x1bP1!~FDEC\x1b\\".to_vec()), last());

        // NULL restores the secure default.
        clear_last();
        assert_eq!(Result::Success, sopt(t, Option::XtChecksumReport, std::ptr::null()));
        vw(t, query);
        assert!(last().is_none());
        unsafe { free(t) };
    }

    #[test]
    fn checksum_extension_survives_resets() {
        let t = new_t(80, 24);

        let enabled = true;
        set_write_pty(t);
        assert_eq!(Result::Success, sopt(t, Option::XtChecksumReport, p(&enabled)));

        // Don't negate the result.
        let positive: u8 = 1;
        assert_eq!(Result::Success, sopt(t, Option::XtChecksumExtension, p(&positive)));

        let text = b"hello";
        let query = b"\x1B[1;1;1;1;1;5*y";
        vw(t, text);
        vw(t, query);
        assert_eq!(Some(b"\x1bP1!~0214\x1b\\".to_vec()), last());

        // A reset restores the configured calculation, not the DEC one.
        vw(t, b"\x1B[0#y");
        vw(t, b"\x1Bc");
        vw(t, text);
        vw(t, query);
        assert_eq!(Some(b"\x1bP1!~0214\x1b\\".to_vec()), last());

        // NULL restores the DEC calculation.
        assert_eq!(Result::Success, sopt(t, Option::XtChecksumExtension, std::ptr::null()));
        vw(t, query);
        assert_eq!(Some(b"\x1bP1!~FDEC\x1b\\".to_vec()), last());

        let invalid: u8 = 32;
        assert_eq!(Result::InvalidValue, sopt(t, Option::XtChecksumExtension, p(&invalid)));
        unsafe { free(t) };
    }

    #[test]
    fn resize_updates_pixel_dimensions() {
        let t = new_t(80, 24);

        // Pixel geometry must still be applied when the cell dimensions match.
        assert_eq!(Result::Success, unsafe { resize(t, 80, 24, 9, 18) });
        assert_eq!(80 * 9, zt(t).width_px);
        assert_eq!(24 * 18, zt(t).height_px);
        unsafe { free(t) };
    }

    #[test]
    fn resize_pixel_overflow_saturates() {
        let t = new_t(80, 24);
        assert_eq!(Result::Success, unsafe { resize(t, 100, 40, u32::MAX, u32::MAX) });
        assert_eq!(u32::MAX, zt(t).width_px);
        assert_eq!(u32::MAX, zt(t).height_px);
        unsafe { free(t) };
    }

    #[test]
    fn resize_disables_synchronized_output() {
        let t = new_t(80, 24);
        zt(t).modes.set(Mode::SynchronizedOutput, true);

        // The terminal-level reset must run even if grid work is unnecessary.
        assert_eq!(Result::Success, unsafe { resize(t, 80, 24, 9, 18) });
        assert!(!zt(t).modes.get(Mode::SynchronizedOutput));
        unsafe { free(t) };
    }

    #[test]
    fn resize_sends_in_band_size_report() {
        let t = new_t(80, 24);
        set_write_pty(t);

        // Enable in-band size reports (mode 2048)
        zt(t).modes.set(Mode::InBandSizeReports, true);

        assert_eq!(Result::Success, unsafe { resize(t, 100, 40, 9, 18) });

        // Expected: \x1B[48;rows;cols;height_px;width_pxt
        // height_px = 40*18 = 720, width_px = 100*9 = 900
        assert_eq!(Some(b"\x1B[48;40;100;720;900t".to_vec()), last());
        unsafe { free(t) };
    }

    #[test]
    fn resize_no_size_report_without_mode_2048() {
        let t = new_t(80, 24);
        set_write_pty(t);

        // in_band_size_reports is off by default
        assert_eq!(Result::Success, unsafe { resize(t, 100, 40, 9, 18) });
        assert_eq!(0, CALLS.get());
        unsafe { free(t) };
    }

    #[test]
    fn resize_in_band_report_without_write_pty_callback() {
        let t = new_t(80, 24);

        // Enable mode 2048 but don't set a write_pty callback — should not crash
        zt(t).modes.set(Mode::InBandSizeReports, true);
        assert_eq!(Result::Success, unsafe { resize(t, 100, 40, 9, 18) });
        unsafe { free(t) };
    }

    #[test]
    fn resize_null_terminal() {
        assert_eq!(Result::InvalidValue, unsafe { resize(std::ptr::null_mut(), 100, 40, 9, 18) });
    }

    #[test]
    fn resize_zero_cols() {
        let t = new_t(80, 24);
        assert_eq!(Result::InvalidValue, unsafe { resize(t, 0, 40, 9, 18) });
        unsafe { free(t) };
    }

    #[test]
    fn resize_zero_rows() {
        let t = new_t(80, 24);
        assert_eq!(Result::InvalidValue, unsafe { resize(t, 100, 0, 9, 18) });
        unsafe { free(t) };
    }

    fn rgbc(r: u8, g: u8, b: u8) -> color::RgbC {
        color::RgbC { r, g, b }
    }

    fn color_roundtrip(option: Option, data: TerminalData, value: color::RgbC) {
        let t = new_t(80, 24);

        // Initially unset
        let mut rgb = rgbc(0, 0, 0);
        assert_eq!(Result::NoValue, gdat(t, data, o(&mut rgb)));

        // Set a value
        assert_eq!(Result::Success, sopt(t, option, p(&value)));
        assert_eq!(Result::Success, gdat(t, data, o(&mut rgb)));
        assert_eq!(value, rgb);

        // Clear with null
        assert_eq!(Result::Success, sopt(t, option, std::ptr::null()));
        assert_eq!(Result::NoValue, gdat(t, data, o(&mut rgb)));
        unsafe { free(t) };
    }

    #[test]
    fn set_and_get_color_foreground() {
        color_roundtrip(Option::ColorForeground, TerminalData::ColorForeground, rgbc(0xAA, 0xBB, 0xCC));
    }

    #[test]
    fn set_and_get_color_background() {
        color_roundtrip(Option::ColorBackground, TerminalData::ColorBackground, rgbc(0x11, 0x22, 0x33));
    }

    #[test]
    fn set_and_get_color_cursor() {
        color_roundtrip(Option::ColorCursor, TerminalData::ColorCursor, rgbc(0xFF, 0x00, 0x88));
    }

    #[test]
    fn set_and_get_color_palette() {
        let t = new_t(80, 24);

        // Get default palette
        let mut palette: color::PaletteC = [rgbc(0, 0, 0); 256];
        assert_eq!(Result::Success, gdat(t, TerminalData::ColorPalette, o(&mut palette)));
        assert_eq!(color::DEFAULT[0].cval(), palette[0]);

        // Set custom palette
        let mut custom = color::palette_cval(&color::DEFAULT);
        custom[0] = rgbc(0x12, 0x34, 0x56);
        assert_eq!(Result::Success, sopt(t, Option::ColorPalette, p(&custom)));
        assert_eq!(Result::Success, gdat(t, TerminalData::ColorPalette, o(&mut palette)));
        assert_eq!(custom[0], palette[0]);

        // Reset with null restores default
        assert_eq!(Result::Success, sopt(t, Option::ColorPalette, std::ptr::null()));
        assert_eq!(Result::Success, gdat(t, TerminalData::ColorPalette, o(&mut palette)));
        assert_eq!(color::DEFAULT[0].cval(), palette[0]);
        unsafe { free(t) };
    }

    #[test]
    fn get_color_default_vs_effective_with_override() {
        let t = new_t(80, 24);
        let mut rgb = rgbc(1, 1, 1);

        // Set defaults
        let fg = rgbc(0xAA, 0xBB, 0xCC);
        let bg = rgbc(0x11, 0x22, 0x33);
        let cur = rgbc(0xFF, 0x00, 0x88);
        assert_eq!(Result::Success, sopt(t, Option::ColorForeground, p(&fg)));
        assert_eq!(Result::Success, sopt(t, Option::ColorBackground, p(&bg)));
        assert_eq!(Result::Success, sopt(t, Option::ColorCursor, p(&cur)));

        // Simulate OSC overrides
        let ov = color::Rgb { r: 0, g: 0, b: 0 };
        zt(t).colors.foreground.r#override = Some(ov);
        zt(t).colors.background.r#override = Some(ov);
        zt(t).colors.cursor.r#override = Some(ov);

        // Effective returns override
        for data in [TerminalData::ColorForeground, TerminalData::ColorBackground, TerminalData::ColorCursor] {
            assert_eq!(Result::Success, gdat(t, data, o(&mut rgb)));
            assert_eq!(ov.cval(), rgb);
        }

        // Default returns original
        assert_eq!(Result::Success, gdat(t, TerminalData::ColorForegroundDefault, o(&mut rgb)));
        assert_eq!(fg, rgb);
        assert_eq!(Result::Success, gdat(t, TerminalData::ColorBackgroundDefault, o(&mut rgb)));
        assert_eq!(bg, rgb);
        assert_eq!(Result::Success, gdat(t, TerminalData::ColorCursorDefault, o(&mut rgb)));
        assert_eq!(cur, rgb);
        unsafe { free(t) };
    }

    #[test]
    fn get_color_default_returns_no_value_when_unset() {
        let t = new_t(80, 24);
        let mut rgb = rgbc(0, 0, 0);
        assert_eq!(Result::NoValue, gdat(t, TerminalData::ColorForegroundDefault, o(&mut rgb)));
        assert_eq!(Result::NoValue, gdat(t, TerminalData::ColorBackgroundDefault, o(&mut rgb)));
        assert_eq!(Result::NoValue, gdat(t, TerminalData::ColorCursorDefault, o(&mut rgb)));
        unsafe { free(t) };
    }

    #[test]
    fn get_color_palette_default_vs_current() {
        let t = new_t(80, 24);

        // Set a custom default palette
        let mut custom = color::palette_cval(&color::DEFAULT);
        custom[0] = rgbc(0x12, 0x34, 0x56);
        assert_eq!(Result::Success, sopt(t, Option::ColorPalette, p(&custom)));

        // Simulate OSC override on index 0
        zt(t).colors.palette.set(0, color::Rgb { r: 0xFF, g: 0xFF, b: 0xFF });

        // Current palette returns the override
        let mut palette: color::PaletteC = [rgbc(0, 0, 0); 256];
        assert_eq!(Result::Success, gdat(t, TerminalData::ColorPalette, o(&mut palette)));
        assert_eq!(rgbc(0xFF, 0xFF, 0xFF), palette[0]);

        // Default palette returns the original
        assert_eq!(Result::Success, gdat(t, TerminalData::ColorPaletteDefault, o(&mut palette)));
        assert_eq!(custom[0], palette[0]);
        unsafe { free(t) };
    }

    #[test]
    fn set_color_sets_dirty_flag() {
        let t = new_t(80, 24);
        zt(t).flags.dirty.palette = false;

        let fg = rgbc(0xFF, 0xFF, 0xFF);
        assert_eq!(Result::Success, sopt(t, Option::ColorForeground, p(&fg)));
        assert!(zt(t).flags.dirty.palette);
        unsafe { free(t) };
    }

    #[test]
    fn set_glyph_protocol_disables_apc_handling_and_clears_glossary() {
        if !build_options::GLYPH_PROTOCOL {
            return;
        }
        use crate::terminal::apc::Protocol;

        let t = new_t(80, 24);

        let register = b"\x1B_25a1;r;cp=e0a0;AAAAAAAAAAAAAA==\x1B\\";
        vw(t, register);
        assert!(zt(t).glyph_glossary.contains(0xE0A0));

        let disabled = false;
        assert_eq!(Result::Success, sopt(t, Option::GlyphProtocol, p(&disabled)));
        assert!(!wr(t).stream.handler.apc_handler.enabled.contains(Protocol::Glyph));
        assert!(!zt(t).glyph_glossary.contains(0xE0A0));

        vw(t, register);
        assert!(!zt(t).glyph_glossary.contains(0xE0A0));

        let enabled = true;
        assert_eq!(Result::Success, sopt(t, Option::GlyphProtocol, p(&enabled)));
        vw(t, register);
        assert!(zt(t).glyph_glossary.contains(0xE0A0));
        unsafe { free(t) };
    }

    #[test]
    fn get_multi_success() {
        let t = new_t(80, 24);

        let mut cols: u16 = 0;
        let mut rows: u16 = 0;
        let mut written: usize = 0;

        let keys = [TerminalData::Cols as c_int, TerminalData::Rows as c_int];
        let values = [o(&mut cols), o(&mut rows)];
        assert_eq!(Result::Success, unsafe {
            get_multi(t, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
        });
        assert_eq!(keys.len(), written);
        assert_eq!(80, cols);
        assert_eq!(24, rows);
        unsafe { free(t) };
    }

    #[test]
    fn get_multi_error_sets_out_written() {
        let t = new_t(80, 24);

        let mut cols: u16 = 0;
        let mut written: usize = 99;

        let keys = [TerminalData::Cols as c_int, TerminalData::Invalid as c_int];
        let values = [o(&mut cols), o(&mut cols)];
        assert_eq!(Result::InvalidValue, unsafe {
            get_multi(t, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
        });
        assert_eq!(1, written);
        assert_eq!(80, cols);
        unsafe { free(t) };
    }

    #[test]
    fn get_multi_null_keys_returns_invalid_value() {
        let mut cols: u16 = 0;
        let values = [o(&mut cols)];
        assert_eq!(Result::InvalidValue, unsafe {
            get_multi(std::ptr::null_mut(), 1, std::ptr::null(), values.as_ptr(), std::ptr::null_mut())
        });
    }

    #[test]
    fn get_mouse_shape() {
        use crate::terminal::mouse::Shape;

        let t = new_t(80, 24);

        let mut shape: c_int = -1;
        assert_eq!(Result::Success, gdat(t, TerminalData::MouseShape, o(&mut shape)));
        assert_eq!(Shape::Text as c_int, shape);

        let cases: [(&[u8], Shape); 7] = [
            (b"\x1b]22;pointer\x07", Shape::Pointer),
            (b"\x1b]22;crosshair\x1b\\", Shape::Crosshair),
            // Invalid names leave the last accepted shape unchanged.
            (b"\x1b]22;not-a-pointer-shape\x07", Shape::Crosshair),
            // Hyperlinks don't override the application's requested shape.
            (b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\", Shape::Crosshair),
            (b"\x1b]22;\x1b\\", Shape::Text),
            (b"\x1b]22;default\x07", Shape::Default),
            (b"\x1b]22;text\x07", Shape::Text),
        ];
        for (seq, expected) in cases {
            vw(t, seq);
            assert_eq!(Result::Success, gdat(t, TerminalData::MouseShape, o(&mut shape)));
            assert_eq!(expected as c_int, shape);
        }

        // An incomplete OSC must not update the shape before its terminator.
        vw(t, b"\x1b]22;wait");
        assert_eq!(Result::Success, gdat(t, TerminalData::MouseShape, o(&mut shape)));
        assert_eq!(Shape::Text as c_int, shape);
        vw(t, b"\x07");
        assert_eq!(Result::Success, gdat(t, TerminalData::MouseShape, o(&mut shape)));
        assert_eq!(Shape::Wait as c_int, shape);
        unsafe { free(t) };
    }
}
