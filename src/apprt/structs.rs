//! Port of `src/apprt/structs.zig`.
//!
//! STATUS: ported
//!
//! Deviations:
//! * `getGObjectType` (GObject boxed/enum registration for `Clipboard` and
//!   `ClipboardRequest`) is not ported here; the GTK apprt registers its
//!   own GObject types.
//! * Zig `[:0]const u8` strings in [`ClipboardContent`] are borrowed byte
//!   slices without the sentinel (crate convention, see `terminal::osc`).
//!   The arena-owned Kitty request states own their memory directly
//!   (`CString`/`Vec`), and owned clipboard contents are
//!   [`ClipboardContentBuf`].

use std::ffi::{c_int, CStr, CString};

use crate::terminal::osc::Terminator;

/// ContentScale is the ratio between the current DPI and the platform's
/// default DPI. This is used to determine how much certain rendered elements
/// need to be scaled up or down.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ContentScale {
    pub x: f32,
    pub y: f32,
}

/// The size of the surface in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SurfaceSize {
    pub width: u32,
    pub height: u32,
}

impl SurfaceSize {
    pub fn eql(&self, other: &SurfaceSize) -> bool {
        self.width == other.width && self.height == other.height
    }
}

/// The position of the cursor in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CursorPos {
    pub x: f32,
    pub y: f32,
}

/// Input Method Editor (IME) position.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct IMEPos {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The clipboard type.
///
/// If this is changed, you must also update ghostty.h
///
/// Our backing isn't is as small as we can in Zig, but a full
/// C int if we're binding to C APIs (the GTK runtime, which is the
/// Linux build, uses `c_int`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub enum Clipboard {
    #[default]
    Standard = 0, // ctrl+c/v
    Selection = 1,
    Primary = 2,
}

impl Clipboard {
    /// The backing integer value.
    pub const fn backing(self) -> c_int {
        self as c_int
    }

    /// Convert from the backing integer value.
    pub const fn from_backing(v: c_int) -> Option<Clipboard> {
        match v {
            0 => Some(Clipboard::Standard),
            1 => Some(Clipboard::Selection),
            2 => Some(Clipboard::Primary),
            _ => None,
        }
    }
}

/// Clipboard contents: a MIME type and its data (borrowed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardContent<'a> {
    pub mime: &'a [u8],
    pub data: &'a [u8],
}

/// Owned clipboard contents (the arena-owned form used by
/// [`KittyWrite`]). The MIME type is NUL-terminated so it can cross a C
/// apprt boundary without copies; the data is binary-safe via its length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardContentBuf {
    pub mime: CString,
    pub data: Vec<u8>,
}

impl ClipboardContentBuf {
    /// Borrow as a [`ClipboardContent`].
    pub fn as_content(&self) -> ClipboardContent<'_> {
        ClipboardContent {
            mime: self.mime.to_bytes(),
            data: &self.data,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ClipboardRequestType {
    Paste,
    Osc52Read,
    Osc52Write,
    KittyRead,
    KittyWrite,
    List,
}

/// The result of starting a clipboard read request. This only reports
/// facts about the clipboard; how each state is answered on the wire
/// (if at all) is up to the protocol handling of the requester.
///
/// If this is changed, you must also update ghostty.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum ClipboardReadResult {
    /// The request was started and will be completed asynchronously
    /// via the core surface completeClipboardRequest API.
    Started = 0,

    /// The clipboard exists but has no contents the apprt can serve
    /// (e.g. no text-like data). The request was not started.
    Unavailable = 1,

    /// The clipboard itself can't be read (e.g. a primary selection on
    /// a platform without one). The request was not started.
    Unsupported = 2,
}

/// Clipboard request. This is used to request clipboard contents and must
/// be sent as a response to a ClipboardRequest event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardRequest {
    /// A direct paste of clipboard contents.
    Paste(Clipboard),

    /// A request to read clipboard contents via OSC 52.
    Osc52Read(Clipboard),

    /// A request to write clipboard contents via OSC 52.
    Osc52Write(Clipboard),

    /// A request to read clipboard contents via the Kitty clipboard
    /// protocol (OSC 5522).
    KittyRead(Box<KittyRead>),

    /// A request to write clipboard contents via the Kitty clipboard
    /// protocol (OSC 5522), carrying a fully committed transaction.
    KittyWrite(Box<KittyWrite>),

    /// A request to list the available clipboard MIME types without
    /// reading any of their data.
    List(Clipboard),
}

impl ClipboardRequest {
    /// The active tag.
    pub fn tag(&self) -> ClipboardRequestType {
        match self {
            ClipboardRequest::Paste(_) => ClipboardRequestType::Paste,
            ClipboardRequest::Osc52Read(_) => ClipboardRequestType::Osc52Read,
            ClipboardRequest::Osc52Write(_) => ClipboardRequestType::Osc52Write,
            ClipboardRequest::KittyRead(_) => ClipboardRequestType::KittyRead,
            ClipboardRequest::KittyWrite(_) => ClipboardRequestType::KittyWrite,
            ClipboardRequest::List(_) => ClipboardRequestType::List,
        }
    }
}

/// State for one in-flight Kitty clipboard protocol read. This is
/// created on the IO thread and completed on the app thread, so it
/// owns all of its memory: everything, including the struct itself,
/// is allocated from the arena (in Rust: owned by the boxed value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyRead {
    /// The clipboard being read. The protocol can only name the
    /// standard clipboard or the primary selection.
    pub location: Clipboard,

    /// The requested MIME types in request order, already capped at
    /// terminal.kitty.clipboard.max_read_mimes by the sender. Only
    /// these representations may be served in the response. The
    /// values are sentinel-terminated so they can cross a C apprt
    /// boundary without copies.
    pub mimes: Vec<CString>,

    /// True when the targets ('.') listing was requested.
    pub list: bool,

    /// The sanitized request id, echoed in every response packet.
    pub id: Vec<u8>,

    /// The effective session password, empty when the request had
    /// none. A non-empty password means the user's decision may be
    /// remembered as a session grant.
    pub pw: Vec<u8>,

    /// The human friendly name of the requesting program, shown in
    /// permission prompts. Empty when absent. Sentinel-terminated
    /// so it can cross a C apprt boundary without copies.
    pub name: CString,

    /// True when a stored session grant already covers this
    /// request, so any permission prompt is skipped.
    pub granted: bool,

    /// The response terminator, matching the request's.
    pub terminator: Terminator,
}

impl KittyRead {
    /// Free the request state. The struct itself is owned by the box, so
    /// this consumes it.
    pub fn destroy(self: Box<Self>) {
        drop(self);
    }

    /// The name as a C string.
    pub fn name_c(&self) -> &CStr {
        &self.name
    }
}

/// State for one committed Kitty clipboard protocol write
/// transaction. Like KittyRead, this is created on the IO thread
/// and completed on the app thread, so everything, including the
/// struct itself, is allocated from the arena (in Rust: owned by the
/// boxed value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyWrite {
    /// The clipboard being written. The protocol can only name the
    /// standard clipboard or the primary selection.
    pub location: Clipboard,

    /// The committed representations. These are the authoritative
    /// contents of the write: completions apply these rather than
    /// any contents echoed back by the apprt. The values are
    /// sentinel-terminated so they can cross a C apprt boundary
    /// without copies; data is binary-safe via its length.
    pub contents: Vec<ClipboardContentBuf>,

    /// The sanitized request id, echoed in every response packet.
    pub id: Vec<u8>,

    /// The effective session password, empty when the request had
    /// none. A non-empty password means the user's decision may be
    /// remembered as a session grant.
    pub pw: Vec<u8>,

    /// The human friendly name of the requesting program, shown in
    /// permission prompts. Empty when absent. Sentinel-terminated
    /// so it can cross a C apprt boundary without copies.
    pub name: CString,

    /// True when a stored session grant already covers this
    /// request, so any permission prompt is skipped.
    pub granted: bool,

    /// The response terminator, matching the request's.
    pub terminator: Terminator,
}

impl KittyWrite {
    /// Free the transaction state. The struct itself is owned by the
    /// box, so this consumes it.
    pub fn destroy(self: Box<Self>) {
        drop(self);
    }
}

/// The color scheme in use (light vs dark).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ColorScheme {
    Light = 0,
    Dark = 1,
}

/// Selection information
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Selection {
    /// Top-left point of the selection in the viewport in scaled
    /// window pixels. (0,0) is the top-left of the window.
    pub tl_x_px: f64,
    pub tl_y_px: f64,

    /// The offset of the selection start in cells from the top-left
    /// of the viewport.
    ///
    /// This is a strange metric but its used by macOS.
    pub offset_start: u32,
    pub offset_len: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::r#enum::parse_c_constants;

    const GHOSTTY_H: &str = include_str!("../../include/ghostty.h");

    #[test]
    fn clipboard_matches_ghostty_h() {
        let c = parse_c_constants(GHOSTTY_H);
        assert_eq!(Some(&0), c.get("GHOSTTY_CLIPBOARD_STANDARD"));
        assert_eq!(Some(&1), c.get("GHOSTTY_CLIPBOARD_SELECTION"));
        assert_eq!(Some(&2), c.get("GHOSTTY_CLIPBOARD_PRIMARY"));
        assert_eq!(Some(&0), c.get("GHOSTTY_CLIPBOARD_READ_STARTED"));
        assert_eq!(Some(&1), c.get("GHOSTTY_CLIPBOARD_READ_UNAVAILABLE"));
        assert_eq!(Some(&2), c.get("GHOSTTY_CLIPBOARD_READ_UNSUPPORTED"));
        assert_eq!(Some(Clipboard::Primary), Clipboard::from_backing(2));
    }

    #[test]
    fn surface_size_eql() {
        let a = SurfaceSize {
            width: 1,
            height: 2,
        };
        assert!(a.eql(&SurfaceSize {
            width: 1,
            height: 2
        }));
        assert!(!a.eql(&SurfaceSize {
            width: 2,
            height: 2
        }));
    }

    #[test]
    fn clipboard_request_tag() {
        let r = ClipboardRequest::KittyRead(Box::new(KittyRead {
            location: Clipboard::Standard,
            mimes: vec![CString::new("text/plain").unwrap()],
            list: false,
            id: b"id".to_vec(),
            pw: Vec::new(),
            name: CString::default(),
            granted: false,
            terminator: Terminator::St,
        }));
        assert_eq!(ClipboardRequestType::KittyRead, r.tag());
        if let ClipboardRequest::KittyRead(v) = r {
            v.destroy();
        }
        assert_eq!(
            ClipboardRequestType::Paste,
            ClipboardRequest::Paste(Clipboard::Standard).tag()
        );
    }
}
