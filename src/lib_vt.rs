//! Port of `src/lib_vt.zig`.
//!
//! STATUS: ported
//!
//! This is the public API of the ghostty-vt Zig module.
//!
//! WARNING: The API is not guaranteed to be stable.
//!
//! The functionality is extremely stable, since it is extracted
//! directly from Ghostty which has been used in real world scenarios
//! by thousands of users for years. However, the API itself (functions,
//! types, etc.) may change without warning. We're working on stabilizing
//! this in the future.
//!
//! Notes on the port:
//! - Zig only emits the C API symbols when this file is the root of the
//!   build (`@import("root") == lib`) and gates groups of them on the
//!   `terminal_options` features. The Rust C API functions carry their
//!   `ghostty_*` names with `#[export_name]` on their definitions
//!   (`terminal::c`) and every feature is built, so the export block
//!   reduces to the Wasm allocator conveniences, which are exported here
//!   under their C names on `wasm32`. The MSVC `_fltused` marker is
//!   Windows/MSVC-only and has no Rust counterpart (Rust's runtime
//!   provides it). The `memset` override is `crate::quirks_memset`.
//! - Zig's `std_options` (log level and log function of the root module)
//!   cannot be applied implicitly to a Rust library: [`std_options`]
//!   describes them and [`install_logger`] installs the corresponding
//!   `log` crate logger for embedders that want it.
//! - The panic handler: Rust's own panic machinery replaces
//!   `std.debug.FullPanic`; [`tiny_panic_impl`] is the release-mode
//!   handler body (write the message to stderr, then trap) for builds
//!   that install it with `std::panic::set_hook`. `std_options_debug_io`
//!   (a comptime guard against linking `std.Io.Threaded`) has no Rust
//!   counterpart.

use crate::os::stderr;

// The public API below reproduces a lot of terminal/main.zig but
// is separate because (1) we need our root file to be in `src/`
// so we can access other directories and (2) we may want to withhold
// parts of `terminal` that are not ready for public consumption
// or are too Ghostty-internal.
use crate::terminal;

/// System interface for the terminal package.
///
/// This module provides runtime-swappable function pointers for operations
/// that depend on external implementations. Embedders can use this to
/// provide or override default behaviors. These must be set at startup
/// before any terminal functionality is used.
///
/// This lets libghostty-vt have no runtime dependencies on external
/// libraries, while still allowing rich functionality that may require
/// external libraries (e.g. image decoding or regular expresssions).
///
/// Setting these will enable various features of the terminal package.
/// For example, setting a PNG decoder will enable support for PNG images in
/// the Kitty Graphics Protocol.
///
/// Additional functionality will be added here over time as needed.
pub use crate::terminal::sys;

/// A tiny, blocking `std.Io` implementation optimized for binary size.
///
/// Constructing a `Terminal` requires a `std.Io` for features that touch
/// the filesystem (e.g. Kitty graphics file transmission). Embedders that
/// don't have their own `Io` can use `TinyIo` instead of a full
/// implementation. See the TinyIo docs for the exact tradeoffs.
pub use crate::lib::TinyIo;

pub use crate::terminal::apc;
pub use crate::terminal::clipboard;
pub use crate::terminal::color;
pub use crate::terminal::dcs;
pub use crate::terminal::device_attributes;
pub use crate::terminal::device_status;
pub use crate::terminal::formatter;
pub use crate::terminal::highlight;
pub use crate::terminal::kitty;
pub use crate::terminal::modes;
pub use crate::terminal::osc;
pub use crate::terminal::page;
pub use crate::terminal::parse_table;
pub use crate::terminal::point;
pub use crate::terminal::search;
pub use crate::terminal::sgr;
pub use crate::terminal::size;
pub use crate::terminal::snapshot;
pub use crate::terminal::x11_color;
pub use crate::terminal::xt_checksum;

pub use crate::terminal::Charset;
pub use crate::terminal::CharsetActiveSlot;
pub use crate::terminal::CharsetSlot;
pub use crate::terminal::MouseShape;
pub use crate::terminal::PageList;
pub use crate::terminal::Parser;
pub use crate::terminal::RenderState;
pub use crate::terminal::Screen;
pub use crate::terminal::ScreenSet;
pub use crate::terminal::Selection;
pub use crate::terminal::SelectionGesture;
pub use crate::terminal::SemanticPrompt;
pub use crate::terminal::SizeReportStyle;
pub use crate::terminal::Stream;
pub use crate::terminal::StreamAction;
pub use crate::terminal::StreamBoundary;
pub use crate::terminal::StringMap;
pub use crate::terminal::Style;
pub use crate::terminal::Terminal;
pub use crate::terminal::TerminalStream;
pub use crate::terminal::UnknownSequence;
pub use crate::terminal::size_report;
pub use page::Cell;
pub use page::Page;
pub use point::Coordinate;
pub use point::Point;
pub use crate::terminal::page_list::Pin;
pub use crate::terminal::CSI;
pub use crate::terminal::DCS;

pub use crate::terminal::Paste;
pub use crate::terminal::PasteError;
pub use crate::terminal::PasteSource;
/// (Zig: `Screen.Cursor`.)
pub use crate::terminal::screen::Cursor;
/// (Zig: `Screen.CursorStyle`.)
pub use crate::terminal::cursor::Style as CursorStyle;
pub use crate::terminal::Attribute;
pub use crate::terminal::CursorStyleReq;
pub use crate::terminal::DeviceAttributeReq;
pub use crate::terminal::EraseDisplay;
pub use crate::terminal::EraseLine;
pub use crate::terminal::ModifyKeyFormat;
pub use crate::terminal::ProtectedMode;
pub use crate::terminal::StatusDisplay;
pub use crate::terminal::StatusLineType;
pub use crate::terminal::TabClear;
pub use modes::Mode;
pub use modes::ModePacked;

/// Terminal-specific input encoding is also part of libghostty-vt.
pub mod input {
    // We have to be careful to only import targeted files within
    // the input package because the full package brings in too many
    // other dependencies.
    use crate::input::{key, key_encode, mouse, mouse_encode, paste};
    use crate::terminal::focus;

    // Focus-related APIs
    pub const MAX_FOCUS_ENCODE_SIZE: usize = focus::MAX_ENCODE_SIZE;
    pub use focus::encode as encode_focus;
    pub use focus::Event as FocusEvent;

    // Paste-related APIs
    pub const MAX_PASTE_FRAME_SIZE: usize = paste::MAX_FRAME_SIZE;
    pub use paste::encode as encode_paste;
    pub use paste::encode_writer as encode_paste_writer;
    pub use paste::is_safe as is_safe_paste;
    pub use paste::is_safe_with as is_safe_paste_with;
    pub use paste::Error as PasteError;
    pub use paste::Options as PasteOptions;

    // Key encoding
    pub use key::Action as KeyAction;
    pub use key::Key;
    pub use key::KeyEvent;
    pub use key::Mods as KeyMods;
    pub use key_encode::encode as encode_key;
    pub use key_encode::Options as KeyEncodeOptions;

    // Mouse encoding
    pub use mouse::Action as MouseAction;
    pub use mouse::Button as MouseButton;
    pub use mouse_encode::encode as encode_mouse;
    pub use mouse_encode::Event as MouseEncodeEvent;
    pub use mouse_encode::Options as MouseEncodeOptions;
}

/// Unicode utilities that match the terminal's text layout semantics.
pub mod unicode {
    pub use crate::unicode::codepoint_width;
    pub use crate::unicode::grapheme_width;
}

/// The Wasm allocator convenience functions, exported under their C
/// names (Zig: the `@export`s in the root `comptime` block).
#[cfg(target_arch = "wasm32")]
mod wasm_exports {
    use crate::lib::allocator::wasm as alloc;

    #[export_name = "ghostty_wasm_alloc"]
    pub extern "C" fn alloc_bytes(len: usize) -> *mut u8 {
        alloc::alloc_bytes(len)
    }

    #[export_name = "ghostty_wasm_free"]
    pub extern "C" fn free_bytes(ptr: *mut u8, len: usize) {
        alloc::free_bytes(ptr, len)
    }

    #[export_name = "ghostty_wasm_alloc_opaque"]
    pub extern "C" fn alloc_opaque() -> *mut alloc::Opaque {
        alloc::alloc_opaque()
    }

    #[export_name = "ghostty_wasm_free_opaque"]
    pub extern "C" fn free_opaque(ptr: *mut alloc::Opaque) {
        alloc::free_opaque(ptr)
    }

    #[export_name = "ghostty_wasm_take_opaque"]
    pub extern "C" fn take_opaque(ptr: *mut alloc::Opaque) -> alloc::Opaque {
        alloc::take_opaque(ptr)
    }
}

/// Native freestanding targets (not Wasm). Rust has no freestanding std
/// target for this crate, so this is always false.
const NATIVE_FREESTANDING: bool = false;

/// True for builds where we keep the full std debug machinery (stack
/// traces on panic, std.debug.print, etc.). These builds are for
/// development, where the roughly 160KB of binary size it costs is
/// worth it. (Rust: debug assertions stand in for the debug/safe modes.)
pub const DEBUG_MACHINERY: bool = !NATIVE_FREESTANDING && (cfg!(test) || cfg!(debug_assertions));

/// The root module options (Zig: `std_options`).
#[derive(Debug, Clone, Copy)]
pub struct StdOptions {
    /// The log level.
    pub log_level: log::LevelFilter,

    /// The log function, or `None` for the default.
    pub log_fn: Option<fn(log::Level, &str, std::fmt::Arguments<'_>)>,

    /// Whether stack traces are collected on panic.
    pub allow_stack_tracing: bool,
}

/// The options this library would set as the root module.
pub fn std_options() -> StdOptions {
    let mut options = StdOptions {
        log_level: if cfg!(debug_assertions) {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        },
        log_fn: None,
        allow_stack_tracing: true,
    };

    if cfg!(target_family = "wasm") {
        // In non-debug modes, we want to ship effectively no logging
        // warn and lower add ~200KB at the time of this comment.
        #[cfg(target_arch = "wasm32")]
        {
            if cfg!(debug_assertions) {
                options.log_level = log::LevelFilter::Debug;
                options.log_fn = Some(crate::os::wasm::log::log);
            } else {
                options.log_level = log::LevelFilter::Error;
                options.log_fn = Some(crate::os::wasm::log::noop);
            }
        }
    } else if terminal::build_options::C_ABI {
        // For C ABI builds, use a custom log function that dispatches to an
        // embedder-provided callback (or silently discards when none is set).
        options.log_fn = Some(terminal::c::sys::log_fn);
    }

    if cfg!(any(target_os = "ios", target_os = "tvos", target_os = "watchos", target_os = "visionos")) {
        // If are building for a non-MacOS Darwin target (e.g., iOS), we need
        // to disable stack tracing for the time being (see the Zig source for
        // the upstream Zig issue).
        options.allow_stack_tracing = false;
    }

    options
}

/// A `log` crate logger that dispatches to the [`std_options`] log function.
struct RootLogger(fn(log::Level, &str, std::fmt::Arguments<'_>));

impl log::Log for RootLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        (self.0)(record.level(), record.target(), *record.args());
    }

    fn flush(&self) {}
}

/// Install the root module's logging configuration (see the module notes).
/// Returns false if another logger is already installed.
pub fn install_logger() -> bool {
    let options = std_options();
    let Some(log_fn) = options.log_fn else {
        return false;
    };
    let logger: &'static RootLogger = Box::leak(Box::new(RootLogger(log_fn)));
    if log::set_logger(logger).is_err() {
        return false;
    }
    log::set_max_level(options.log_level);
    true
}

/// Prints the panic message to stderr (best-effort) and traps.
///
/// This is safe to call from any thread (and even from signal handlers):
/// it takes no locks, performs no allocation, and touches no shared
/// mutable state. The message is emitted with a single raw write so that
/// concurrent stderr output doesn't interleave with it.
#[cold]
pub fn tiny_panic_impl(msg: &str, _ra: Option<usize>) -> ! {
    // 256 bytes is enough for most messages, so try that first
    // so that we can try to write in a single syscall.
    let mut buf = [0u8; 256];
    let prefix = b"panic: ";
    let total = prefix.len() + msg.len() + 1;
    if total <= buf.len() {
        buf[..prefix.len()].copy_from_slice(prefix);
        buf[prefix.len()..prefix.len() + msg.len()].copy_from_slice(msg.as_bytes());
        buf[total - 1] = b'\n';
        stderr::write(&buf[..total]);
    } else {
        stderr::write(prefix);
        stderr::write(msg.as_bytes());
        stderr::write(b"\n");
    }

    // Trap forces a standard crash that embedder-provided debuggers
    // or environments can catch.
    std::process::abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zig: `refAllDecls(input)` / `refAllDecls(unicode)`; the Rust
    /// counterpart makes sure the re-exports resolve and work.
    #[test]
    fn public_api_reexports() {
        assert_eq!(1, unicode::codepoint_width('A' as u32));
        assert_eq!(2, unicode::codepoint_width(0x1F600));
        assert!(input::is_safe_paste(b"hello"));
        assert!(!input::is_safe_paste(b"hello\n"));

        let mut out: Vec<u8> = Vec::new();
        input::encode_focus(&mut out, input::FocusEvent::Gained).unwrap();
        assert!(out.len() <= input::MAX_FOCUS_ENCODE_SIZE);

        // The C API is part of the library.
        assert!(!terminal::c::types::get_json().is_null());
    }

    #[test]
    fn std_options_match_build() {
        let options = std_options();
        if terminal::build_options::C_ABI && !cfg!(target_family = "wasm") {
            assert!(options.log_fn.is_some());
        }
        assert!(DEBUG_MACHINERY);
    }
}
