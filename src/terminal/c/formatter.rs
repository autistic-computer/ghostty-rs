//! Port of `src/terminal/c/formatter.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - Zig's wrapper stores a `formatterpkg.TerminalFormatter`, which holds a
//!   `*Terminal`. The Rust `TerminalFormatter<'a>` borrows the terminal, and
//!   keeping that borrow alive in a heap handle while the C API mutates the
//!   terminal (`vt_write`, ...) would alias a `&mut`. The wrapper therefore
//!   stores the formatter's state (terminal pointer, options, content,
//!   extra) and builds the `TerminalFormatter` for each format call, which
//!   formats exactly what the Zig formatter would.
//! - `emit` crosses the ABI as `c_int` and is validated in `terminal_new`.

use std::ffi::c_int;

use super::io as io_c;
use super::result::Result;
use super::selection as selection_c;
use super::terminal as terminal_c;
use super::types::{encode_to_buffer, HandleAllocator};
use crate::lib::allocator::{self as lib_alloc, Allocator as CAllocator, ZigAllocator};
use crate::terminal::formatter as formatterpkg;
use crate::terminal::terminal::Terminal as ZigTerminal;

/// Wrapper around formatter that tracks the allocator for C API usage.
pub struct FormatterWrapper {
    kind: Kind,
    alloc: HandleAllocator,
}

/// (Zig: `FormatterWrapper.Kind`.)
enum Kind {
    Terminal(TerminalFormatterState),
}

/// The state of a `formatterpkg.TerminalFormatter` (see the module notes).
struct TerminalFormatterState {
    /// The native terminal. The C API contract requires the terminal to
    /// outlive the formatter.
    terminal: *const ZigTerminal,
    emit: formatterpkg::Format,
    unwrap: bool,
    trim: bool,
    content: formatterpkg::ScreenFormatterContent,
    extra: formatterpkg::TerminalFormatterExtra,
}

impl TerminalFormatterState {
    fn format(&self, writer: &mut dyn std::io::Write) -> std::io::Result<()> {
        // SAFETY: the terminal outlives the formatter (C API contract) and
        // is not mutated while a format call runs.
        let t = unsafe { &*self.terminal };
        let mut opts = formatterpkg::Options::new(self.emit);
        opts.unwrap = self.unwrap;
        opts.trim = self.trim;
        let mut formatter = formatterpkg::TerminalFormatter::init(t, opts);
        formatter.extra = self.extra;
        formatter.content = self.content;
        formatter.format(writer)
    }
}

/// C: GhosttyFormatter
pub type Formatter = *mut FormatterWrapper;

/// C: GhosttyFormatterFormat
pub type Format = formatterpkg::Format;

use selection_c::CSelection;

fn format_from_int(v: c_int) -> Option<Format> {
    [Format::Plain, Format::Vt, Format::Html]
        .into_iter()
        .find(|f| *f as c_int == v)
}

/// C: GhosttyFormatterScreenExtra
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenOptionsExtra {
    pub size: usize,
    pub cursor: bool,
    pub style: bool,
    pub hyperlink: bool,
    pub protection: bool,
    pub kitty_keyboard: bool,
    pub charsets: bool,
}

impl Default for ScreenOptionsExtra {
    fn default() -> Self {
        ScreenOptionsExtra {
            size: std::mem::size_of::<ScreenOptionsExtra>(),
            cursor: false,
            style: false,
            hyperlink: false,
            protection: false,
            kitty_keyboard: false,
            charsets: false,
        }
    }
}

impl ScreenOptionsExtra {
    fn to_zig(self) -> formatterpkg::ScreenFormatterExtra {
        formatterpkg::ScreenFormatterExtra {
            cursor: self.cursor,
            style: self.style,
            hyperlink: self.hyperlink,
            protection: self.protection,
            kitty_keyboard: self.kitty_keyboard,
            charsets: self.charsets,
        }
    }
}

/// C: GhosttyFormatterTerminalOptions
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TerminalOptions {
    pub size: usize,
    /// `GhosttyFormatterFormat`.
    pub emit: c_int,
    pub unwrap: bool,
    pub trim: bool,
    pub extra: TerminalOptionsExtra,

    /// Optional selection to restrict output to a range.
    /// If null, the entire screen is formatted.
    pub selection: *const CSelection,
}

impl TerminalOptions {
    /// Options with the Zig defaults (`size`, `selection = null`).
    pub fn new(emit: Format, unwrap: bool, trim: bool, extra: TerminalOptionsExtra) -> TerminalOptions {
        TerminalOptions {
            size: std::mem::size_of::<TerminalOptions>(),
            emit: emit as c_int,
            unwrap,
            trim,
            extra,
            selection: std::ptr::null(),
        }
    }
}

impl Default for TerminalOptions {
    /// `std.mem.zeroInit(TerminalOptions, .{})`: defaults where Zig has
    /// them, zero elsewhere.
    fn default() -> Self {
        TerminalOptions::new(Format::Plain, false, false, TerminalOptionsExtra::default())
    }
}

/// C: GhosttyFormatterTerminalExtra
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalOptionsExtra {
    pub size: usize,
    pub palette: bool,
    pub modes: bool,
    pub scrolling_region: bool,
    pub tabstops: bool,
    pub pwd: bool,
    pub keyboard: bool,
    pub screen: ScreenOptionsExtra,
}

impl Default for TerminalOptionsExtra {
    fn default() -> Self {
        TerminalOptionsExtra {
            size: std::mem::size_of::<TerminalOptionsExtra>(),
            palette: false,
            modes: false,
            scrolling_region: false,
            tabstops: false,
            pwd: false,
            keyboard: false,
            screen: ScreenOptionsExtra::default(),
        }
    }
}

impl TerminalOptionsExtra {
    fn to_zig(self) -> formatterpkg::TerminalFormatterExtra {
        formatterpkg::TerminalFormatterExtra {
            palette: self.palette,
            modes: self.modes,
            scrolling_region: self.scrolling_region,
            tabstops: self.tabstops,
            pwd: self.pwd,
            keyboard: self.keyboard,
            screen: self.screen.to_zig(),
        }
    }
}

// comptime: every `formatterpkg` extra field has a C counterpart (Rust:
// the exhaustive struct literals in `to_zig` fail to compile otherwise).

/// C: ghostty_formatter_terminal_new
///
/// # Safety
/// `terminal_` must be NULL or live and outlive the formatter; a non-null
/// `opts.selection` must be valid.
#[export_name = "ghostty_formatter_terminal_new"]
pub unsafe extern "C" fn terminal_new(
    alloc_: Option<&CAllocator>,
    result: &mut Formatter,
    terminal_: terminal_c::Terminal,
    opts: TerminalOptions,
) -> Result {
    match terminal_new_(alloc_, terminal_, opts) {
        Ok(ptr) => {
            *result = ptr;
            Result::Success
        }
        Err(err) => {
            *result = std::ptr::null_mut();
            err
        }
    }
}

unsafe fn terminal_new_(
    alloc_: Option<&CAllocator>,
    terminal_: terminal_c::Terminal,
    opts: TerminalOptions,
) -> std::result::Result<*mut FormatterWrapper, Result> {
    let Some(wrapper) = terminal_.as_ref() else {
        return Err(Result::InvalidValue);
    };
    let t: *const ZigTerminal = wrapper.terminal();
    let Some(emit) = format_from_int(opts.emit) else {
        return Err(Result::InvalidValue);
    };

    // Setup the content that we're formatting
    let content = match opts.selection.as_ref() {
        Some(sel) => formatterpkg::ScreenFormatterContent::Selection(Some(
            sel.to_zig().ok_or(Result::InvalidValue)?,
        )),
        None => formatterpkg::ScreenFormatterContent::Selection(None),
    };

    let alloc = HandleAllocator::new(alloc_);
    let ptr = alloc.create(FormatterWrapper {
        kind: Kind::Terminal(TerminalFormatterState {
            terminal: t,
            emit,
            unwrap: opts.unwrap,
            trim: opts.trim,
            content,
            extra: opts.extra.to_zig(),
        }),
        alloc,
    });
    if ptr.is_null() {
        return Err(Result::OutOfMemory);
    }
    Ok(ptr)
}

/// Format directly to a synchronous C writer callback.
///
/// # Safety
/// `formatter_` must be NULL or live; `writer` must follow the
/// `GhosttyWriter` contract.
#[export_name = "ghostty_formatter_format"]
pub unsafe extern "C" fn format(formatter_: Formatter, writer: io_c::Writer) -> Result {
    let Some(wrapper) = formatter_.as_ref() else {
        return Result::InvalidValue;
    };
    if !writer.valid() {
        return Result::InvalidValue;
    }

    // Batch the formatter's many small writes into few callback calls. The
    // trailing flush upholds the API contract that success means the
    // callback has accepted every byte.
    let mut buffer = [0u8; io_c::WriterAdapter::RECOMMENDED_BUFFER_LEN];
    let mut adapter = io_c::WriterAdapter::init_buffered(writer, &mut buffer);
    'format: {
        match &wrapper.kind {
            Kind::Terminal(t) => {
                if t.format(&mut adapter).is_err() {
                    break 'format;
                }
            }
        }
        if adapter.flush().is_err() {
            break 'format;
        }
        return Result::Success;
    }

    if adapter.invalid_write {
        return Result::LimitExceeded;
    }
    if adapter.callback_failed {
        return Result::IoError;
    }
    Result::IoError
}

/// C: ghostty_formatter_format_buf
///
/// # Safety
/// `formatter_` must be NULL or live; a non-null `out_` must be valid for
/// `out_len` writes.
#[export_name = "ghostty_formatter_format_buf"]
pub unsafe extern "C" fn format_buf(
    formatter_: Formatter,
    out_: *mut u8,
    out_len: usize,
    out_written: &mut usize,
) -> Result {
    let Some(wrapper) = formatter_.as_ref() else {
        return Result::InvalidValue;
    };

    match &wrapper.kind {
        // On write failed we always report how much space we actually
        // needed (`encode_to_buffer`).
        Kind::Terminal(t) => encode_to_buffer(out_, out_len, out_written, |w| t.format(w)),
    }
}

/// C: ghostty_formatter_format_alloc
///
/// # Safety
/// `formatter_` must be NULL or live.
#[export_name = "ghostty_formatter_format_alloc"]
pub unsafe extern "C" fn format_alloc(
    formatter_: Formatter,
    alloc_: Option<&CAllocator>,
    out_ptr: &mut *mut u8,
    out_len: &mut usize,
) -> Result {
    let Some(wrapper) = formatter_.as_ref() else {
        return Result::InvalidValue;
    };

    let mut buf: Vec<u8> = Vec::new();
    match &wrapper.kind {
        Kind::Terminal(t) => {
            if t.format(&mut buf).is_err() {
                return Result::OutOfMemory;
            }
        }
    }

    // Empty Zig slices may contain sentinel pointers that foreign runtimes
    // reject even when the length is zero (for example Go's stack scanner).
    if buf.is_empty() {
        *out_ptr = std::ptr::null_mut();
        *out_len = 0;
        return Result::Success;
    }
    let alloc = lib_alloc::default(alloc_);
    let ptr = alloc.alloc(buf.len(), 0, 0);
    if ptr.is_null() {
        return Result::OutOfMemory;
    }
    std::ptr::copy_nonoverlapping(buf.as_ptr(), ptr, buf.len());
    *out_ptr = ptr;
    *out_len = buf.len();
    Result::Success
}

/// C: ghostty_formatter_free
///
/// # Safety
/// `formatter_` must be NULL or a formatter not used afterwards.
#[export_name = "ghostty_formatter_free"]
pub unsafe extern "C" fn free(formatter_: Formatter) {
    let Some(wrapper) = formatter_.as_ref() else {
        return;
    };
    let alloc = wrapper.alloc;
    alloc.destroy(formatter_);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;
    use crate::terminal::c::allocator as allocator_c;
    use crate::terminal::c::grid_ref::CGridRef;
    use crate::terminal::c::terminal::tests::FAILING_C_ALLOCATOR;
    use crate::terminal::point::{Coordinate, Point};
    use std::ffi::c_void;

    fn new_terminal(cols: u16, rows: u16) -> terminal_c::Terminal {
        let mut t: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut t, cols, rows));
        t
    }

    fn write(t: terminal_c::Terminal, s: &[u8]) {
        unsafe { terminal_c::vt_write(t, s.as_ptr(), s.len()) };
    }

    fn extra_none() -> TerminalOptionsExtra {
        TerminalOptionsExtra::default()
    }

    fn plain_opts() -> TerminalOptions {
        TerminalOptions::new(Format::Plain, false, true, extra_none())
    }

    fn new_formatter(t: terminal_c::Terminal, opts: TerminalOptions) -> Formatter {
        let mut f: Formatter = std::ptr::null_mut();
        assert_eq!(Result::Success, unsafe { terminal_new(Some(&C_ALLOCATOR), &mut f, t, opts) });
        assert!(!f.is_null());
        f
    }

    #[test]
    fn terminal_new_free() {
        let t = new_terminal(80, 24);
        let f = new_formatter(t, plain_opts());
        unsafe {
            free(f);
            terminal_c::free(t);
        }
    }

    #[test]
    fn terminal_new_invalid_value_on_null_terminal() {
        let mut f: Formatter = std::ptr::null_mut();
        assert_eq!(Result::InvalidValue, unsafe {
            terminal_new(Some(&C_ALLOCATOR), &mut f, std::ptr::null_mut(), plain_opts())
        });
        assert!(f.is_null());
    }

    #[test]
    fn free_null() {
        unsafe { free(std::ptr::null_mut()) };
    }

    #[test]
    fn format_alloc_empty_output() {
        let failing: &CAllocator = &FAILING_C_ALLOCATOR;
        let t = new_terminal(100, 40);

        // Match a zero-initialized C options struct, including trim = false.
        let opts = TerminalOptions::default();
        let f = new_formatter(t, opts);

        unsafe {
            for allocator in [None, Some(failing)] {
                let mut sentinel: u8 = 0;
                let mut ptr: *mut u8 = &mut sentinel;
                let mut len: usize = 123;
                assert_eq!(Result::Success, format_alloc(f, allocator, &mut ptr, &mut len));
                assert_eq!(0, len);
                assert!(ptr.is_null());
                allocator_c::free(allocator, ptr, len);
            }

            // Reusing the same formatter must still allocate and return real output.
            write(t, b"hello");
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut len: usize = 0;
            assert_eq!(Result::Success, format_alloc(f, None, &mut ptr, &mut len));
            assert_eq!(b"hello", std::slice::from_raw_parts(ptr, len));
            allocator_c::free(None, ptr, len);

            free(f);
            terminal_c::free(t);
        }
    }

    #[test]
    fn format_plain() {
        let t = new_terminal(80, 24);
        write(t, b"Hello");
        let f = new_formatter(t, plain_opts());
        unsafe {
            let mut buf = [0u8; 1024];
            let mut written: usize = 0;
            assert_eq!(Result::Success, format_buf(f, buf.as_mut_ptr(), buf.len(), &mut written));
            assert_eq!(b"Hello", &buf[..written]);
            free(f);
            terminal_c::free(t);
        }
    }

    struct WriteContext {
        destination: Vec<u8>,
        offset: usize,
        calls: usize,
    }

    unsafe extern "C" fn context_write(userdata: *mut c_void, data: *const u8, len: usize) -> bool {
        let this = &mut *(userdata as *mut WriteContext);
        if len > this.destination.len() - this.offset {
            return false;
        }
        let off = this.offset;
        this.destination[off..off + len].copy_from_slice(std::slice::from_raw_parts(data, len));
        this.offset += len;
        this.calls += 1;
        true
    }

    #[test]
    fn format_streams_to_writer() {
        let t = new_terminal(80, 24);
        write(t, b"Hello\r\nWorld");
        let f = new_formatter(t, plain_opts());
        unsafe {
            let mut context = WriteContext {
                destination: vec![0; 1024],
                offset: 0,
                calls: 0,
            };
            assert_eq!(
                Result::Success,
                format(
                    f,
                    io_c::Writer {
                        write: Some(context_write),
                        userdata: &mut context as *mut WriteContext as *mut c_void,
                    }
                )
            );
            assert!(context.calls > 0);
            assert_eq!(b"Hello\nWorld", &context.destination[..context.offset]);
            free(f);
            terminal_c::free(t);
        }
    }

    unsafe extern "C" fn fail_write(_: *mut c_void, _: *const u8, _: usize) -> bool {
        false
    }

    #[test]
    fn format_writer_errors() {
        let fail_writer = io_c::Writer {
            write: Some(fail_write),
            userdata: std::ptr::null_mut(),
        };
        unsafe {
            assert_eq!(Result::InvalidValue, format(std::ptr::null_mut(), fail_writer));

            let t = new_terminal(80, 24);
            write(t, b"Hello");
            let f = new_formatter(t, plain_opts());

            assert_eq!(Result::InvalidValue, format(f, io_c::Writer::default()));
            assert_eq!(Result::IoError, format(f, fail_writer));
            free(f);
            terminal_c::free(t);
        }
    }

    #[test]
    fn format_reflects_terminal_changes() {
        let t = new_terminal(80, 24);
        write(t, b"Hello");
        let f = new_formatter(t, plain_opts());
        unsafe {
            let mut buf = [0u8; 1024];
            let mut written: usize = 0;
            assert_eq!(Result::Success, format_buf(f, buf.as_mut_ptr(), buf.len(), &mut written));
            assert_eq!(b"Hello", &buf[..written]);

            // Write more data and re-format
            write(t, b"\r\nWorld");

            assert_eq!(Result::Success, format_buf(f, buf.as_mut_ptr(), buf.len(), &mut written));
            assert_eq!(b"Hello\nWorld", &buf[..written]);
            free(f);
            terminal_c::free(t);
        }
    }

    #[test]
    fn format_null_returns_required_size() {
        let t = new_terminal(80, 24);
        write(t, b"Hello");
        let f = new_formatter(t, plain_opts());
        unsafe {
            // Pass null buffer to query required size
            let mut required: usize = 0;
            assert_eq!(Result::OutOfSpace, format_buf(f, std::ptr::null_mut(), 0, &mut required));
            assert!(required > 0);

            // Now allocate and format
            let mut buf = [0u8; 1024];
            let mut written: usize = 0;
            assert_eq!(Result::Success, format_buf(f, buf.as_mut_ptr(), buf.len(), &mut written));
            assert_eq!(required, written);
            free(f);
            terminal_c::free(t);
        }
    }

    #[test]
    fn format_buffer_too_small() {
        let t = new_terminal(80, 24);
        write(t, b"Hello");
        let f = new_formatter(t, plain_opts());
        unsafe {
            // Buffer too small
            let mut buf = [0u8; 2];
            let mut written: usize = 0;
            assert_eq!(Result::OutOfSpace, format_buf(f, buf.as_mut_ptr(), buf.len(), &mut written));
            // written contains the required size
            assert_eq!(5, written);
            free(f);
            terminal_c::free(t);
        }
    }

    #[test]
    fn format_null_formatter() {
        let mut written: usize = 0;
        assert_eq!(Result::InvalidValue, unsafe {
            format_buf(std::ptr::null_mut(), std::ptr::null_mut(), 0, &mut written)
        });
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn format_vt() {
        let t = new_terminal(80, 24);
        write(t, b"Test");
        let mut extra = extra_none();
        extra.palette = true;
        extra.screen.style = true;
        extra.screen.hyperlink = true;
        let f = new_formatter(t, TerminalOptions::new(Format::Vt, false, true, extra));
        unsafe {
            let mut buf = vec![0u8; 65536];
            let mut written: usize = 0;
            assert_eq!(Result::Success, format_buf(f, buf.as_mut_ptr(), buf.len(), &mut written));
            assert!(written > 0);
            assert!(contains(&buf[..written], b"Test"));
            free(f);
            terminal_c::free(t);
        }
    }

    #[test]
    fn format_plain_with_selection() {
        let t = new_terminal(80, 24);
        write(t, b"Hello World");
        unsafe {
            // Get grid refs for "World" (columns 6..10 on row 0)
            let mut start_ref = CGridRef::default();
            assert_eq!(
                Result::Success,
                terminal_c::grid_ref(t, Point::Active(Coordinate { x: 6, y: 0 }).cval(), &mut start_ref)
            );
            let mut end_ref = CGridRef::default();
            assert_eq!(
                Result::Success,
                terminal_c::grid_ref(t, Point::Active(Coordinate { x: 10, y: 0 }).cval(), &mut end_ref)
            );

            let sel = CSelection::new(start_ref, end_ref);
            let mut opts = plain_opts();
            opts.selection = &sel;
            let f = new_formatter(t, opts);

            let mut buf = [0u8; 1024];
            let mut written: usize = 0;
            assert_eq!(Result::Success, format_buf(f, buf.as_mut_ptr(), buf.len(), &mut written));
            assert_eq!(b"World", &buf[..written]);
            free(f);
            terminal_c::free(t);
        }
    }

    #[test]
    fn format_html() {
        let t = new_terminal(80, 24);
        write(t, b"Html");
        let f = new_formatter(t, TerminalOptions::new(Format::Html, false, true, extra_none()));
        unsafe {
            let mut buf = vec![0u8; 65536];
            let mut written: usize = 0;
            assert_eq!(Result::Success, format_buf(f, buf.as_mut_ptr(), buf.len(), &mut written));
            assert!(written > 0);
            assert!(contains(&buf[..written], b"Html"));
            free(f);
            terminal_c::free(t);
        }
    }
}
