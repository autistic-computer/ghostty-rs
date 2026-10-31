//! Port of `src/terminal/c/sys.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - The global C callback state lives in a `Mutex` (Zig: a plain global).
//! - `terminal.sys.DecodePngFn` is a plain Rust `fn` that returns an owned
//!   `Vec<u8>`. The decode wrapper hands the C callback an allocator over
//!   the Rust global allocator, then copies the pixels into a buffer from
//!   [`terminal_sys::decode_alloc`] (which honors the decoder's size limit)
//!   and frees the callback's buffer; Zig takes ownership of the callback's
//!   allocation directly.
//! - Zig's `std.log` sink `logFn` is [`log_fn`] (taking `fmt::Arguments`);
//!   [`CLogger`] adapts it to the `log` crate for C ABI builds that want to
//!   install it with `log::set_logger`. The scope is the `log` target (the
//!   empty string is Zig's `.default` scope).

use std::ffi::{c_int, c_void};
use std::fmt;
use std::sync::Mutex;

use super::result::Result;
use crate::lib::allocator::{Allocator as CAllocator, GlobalAllocator, ZigAllocator};
use crate::lib::String as LibString;
use crate::os::stderr;
use crate::terminal::sys as terminal_sys;

/// C: GhosttySysImage
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: *mut u8,
    pub data_len: usize,
}

impl Image {
    /// `std.mem.zeroes(Image)`.
    pub const ZEROES: Image = Image {
        width: 0,
        height: 0,
        data: std::ptr::null_mut(),
        data_len: 0,
    };
}

/// C: GhosttySysDecodePngFn
pub type DecodePngFn =
    unsafe extern "C" fn(*mut c_void, *const CAllocator, *const u8, usize, *mut Image) -> bool;

/// C: GhosttySysRandomSecureFn
pub type RandomSecureFn = unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> bool;

crate::c_api_enum! {
    /// C: GhosttySysLogLevel
    pub enum LogLevel {
        Error = 0,
        Warning = 1,
        Info = 2,
        Debug = 3,
    }
}

impl LogLevel {
    pub fn from_std(level: log::Level) -> LogLevel {
        match level {
            log::Level::Error => LogLevel::Error,
            log::Level::Warn => LogLevel::Warning,
            log::Level::Info => LogLevel::Info,
            log::Level::Debug | log::Level::Trace => LogLevel::Debug,
        }
    }
}

/// C: GhosttySysLogFn
pub type LogFn = unsafe extern "C" fn(*mut c_void, LogLevel, *const u8, usize, *const u8, usize);

crate::c_api_enum! {
    /// C: GhosttySysOption
    ///
    /// Input type expected for setting each option (Zig: `Option.InType`):
    /// `userdata` takes the pointer itself, `decode_png`, `log` and
    /// `random_secure` take the callback (or NULL to clear it).
    pub enum Option {
        Userdata = 0,
        DecodePng = 1,
        Log = 2,
        RandomSecure = 3,
    }
}

/// Global state for the sys interface so we can call through to the C
/// callbacks from Rust.
#[derive(Clone, Copy)]
struct Global {
    userdata: *mut c_void,
    decode_png: std::option::Option<DecodePngFn>,
    log: std::option::Option<LogFn>,
    random_secure: std::option::Option<RandomSecureFn>,
}

// SAFETY: the userdata pointer is only handed back to the embedder's own
// callbacks, which the C API requires to be callable from any thread.
unsafe impl Send for Global {}

/// Global state for the C sys interface.
static GLOBAL: Mutex<Global> = Mutex::new(Global {
    userdata: std::ptr::null_mut(),
    decode_png: None,
    log: None,
    random_secure: None,
});

fn global() -> Global {
    *GLOBAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn with_global(f: impl FnOnce(&mut Global)) {
    f(&mut GLOBAL.lock().unwrap_or_else(|e| e.into_inner()));
}

/// Rust-compatible wrapper that calls through to the stored C callback.
/// The C callback allocates the pixel data through the provided allocator.
///
/// Returns `InvalidData` if no callback is installed, if the callback
/// returns false, or if it returns true without setting `data`.
fn decode_png_wrapper(data: &[u8]) -> std::result::Result<terminal_sys::Image, terminal_sys::DecodeError> {
    let g = global();
    let func = g.decode_png.ok_or(terminal_sys::DecodeError::InvalidData)?;

    let zig: &dyn ZigAllocator = &GlobalAllocator;
    let c_alloc = CAllocator::from_zig(&zig);

    // Hand the callback a zeroed struct instead of an undefined one. The
    // callback stores a pointer into it, and runtimes that track pointers
    // (Go, for example) require the memory to be initialized first. It
    // also means a callback that returns true without setting `data` is
    // caught by the null check below. The header documents this guarantee.
    let mut out = Image::ZEROES;
    // SAFETY: calling the embedder's callback with valid arguments.
    let ok = unsafe { func(g.userdata, &c_alloc, LibString::init(data).ptr, data.len(), &mut out) };
    if !ok {
        return Err(terminal_sys::DecodeError::InvalidData);
    }

    if out.data.is_null() {
        return Err(terminal_sys::DecodeError::InvalidData);
    }

    // SAFETY: the callback allocated `data_len` bytes at `data`.
    let pixels = unsafe { std::slice::from_raw_parts(out.data, out.data_len) };
    let result = terminal_sys::decode_alloc(out.data_len).map(|mut buf| {
        buf.copy_from_slice(pixels);
        buf
    });
    GlobalAllocator.free(out.data, out.data_len, 0, 0);

    Ok(terminal_sys::Image {
        width: out.width,
        height: out.height,
        data: result?,
    })
}

/// Rust-compatible wrapper that calls through to the stored C callback.
fn random_secure_wrapper(buffer: &mut [u8]) -> std::result::Result<(), terminal_sys::RandomSecureError> {
    let g = global();
    let func = g
        .random_secure
        .ok_or(terminal_sys::RandomSecureError::EntropyUnavailable)?;
    // SAFETY: calling the embedder's callback with a valid buffer.
    if !unsafe { func(g.userdata, buffer.as_mut_ptr(), buffer.len()) } {
        return Err(terminal_sys::RandomSecureError::EntropyUnavailable);
    }
    Ok(())
}

/// C: ghostty_sys_set
///
/// # Safety
/// `value` must be NULL or of the input type for `option`.
#[export_name = "ghostty_sys_set"]
pub unsafe extern "C" fn set(option: c_int, value: *const c_void) -> Result {
    let Some(option) = Option::from_int(option) else {
        return Result::InvalidValue;
    };

    match option {
        Option::Userdata => with_global(|g| g.userdata = value as *mut c_void),
        Option::DecodePng => {
            let f: std::option::Option<DecodePngFn> = std::mem::transmute(value);
            with_global(|g| g.decode_png = f);
            terminal_sys::set_decode_png(if f.is_some() {
                Some(decode_png_wrapper as terminal_sys::DecodePngFn)
            } else {
                None
            });
        }
        Option::Log => {
            let f: std::option::Option<LogFn> = std::mem::transmute(value);
            with_global(|g| g.log = f);
        }
        Option::RandomSecure => {
            let f: std::option::Option<RandomSecureFn> = std::mem::transmute(value);
            with_global(|g| g.random_secure = f);
            terminal_sys::set_random_secure(if f.is_some() {
                Some(random_secure_wrapper as terminal_sys::RandomSecureFn)
            } else {
                None
            });
        }
    }
    Result::Success
}

/// Dispatch a log message to the installed C callback, if any.
fn emit_log(level: LogLevel, scope: &[u8], message: &[u8]) {
    let g = global();
    let Some(func) = g.log else {
        return;
    };
    // SAFETY: calling the embedder's callback with valid slices.
    unsafe {
        func(
            g.userdata,
            level,
            scope.as_ptr(),
            scope.len(),
            message.as_ptr(),
            message.len(),
        )
    };
}

/// Emits logs in chunks. Almost all logs will be less than the chunk size
/// but this allows emitting larger logs without heap allocation.
struct LogEmitter<'a> {
    c_level: LogLevel,
    scope_text: &'a [u8],
    buffer: [u8; LogEmitter::BUFFER_SIZE],
    end: usize,
}

impl LogEmitter<'_> {
    const BUFFER_SIZE: usize = 2048;

    fn flush(&mut self) {
        if self.end > 0 {
            emit_log(self.c_level, self.scope_text, &self.buffer[..self.end]);
            self.end = 0;
        }
    }
}

impl fmt::Write for LogEmitter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut data = s.as_bytes();
        while !data.is_empty() {
            if self.end == self.buffer.len() {
                self.flush();
            }
            let len = data.len().min(self.buffer.len() - self.end);
            self.buffer[self.end..self.end + len].copy_from_slice(&data[..len]);
            self.end += len;
            data = &data[len..];
        }
        Ok(())
    }
}

/// Custom log sink for C ABI builds (Zig: `logFn`).
///
/// When a log callback is installed via ghostty_sys_set(), messages are
/// dispatched through it. When no callback is installed, messages are
/// silently discarded. Large messages that exceed the stack buffer are
/// delivered across multiple callback invocations.
pub fn log_fn(level: log::Level, scope: &str, args: fmt::Arguments<'_>) {
    if global().log.is_none() {
        return;
    }

    let mut log_emitter = LogEmitter {
        c_level: LogLevel::from_std(level),
        scope_text: scope.as_bytes(),
        buffer: [0; LogEmitter::BUFFER_SIZE],
        end: 0,
    };

    let _ = fmt::Write::write_fmt(&mut log_emitter, args);
    log_emitter.flush();
}

/// A `log` crate logger that forwards to [`log_fn`].
pub struct CLogger;

impl log::Log for CLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        global().log.is_some()
    }

    fn log(&self, record: &log::Record<'_>) {
        log_fn(record.level(), record.target(), *record.args());
    }

    fn flush(&self) {}
}

/// Built-in log callback that writes to stderr.
///
/// Formats each message as "[level](scope): message\n". Can be passed
/// directly to ghostty_sys_set(GHOSTTY_SYS_OPT_LOG, &ghostty_sys_log_stderr).
///
/// Each log line is emitted with a single raw write to stderr, which keeps
/// concurrent log lines from interleaving.
///
/// # Safety
/// The pointers must be valid for their lengths.
#[export_name = "ghostty_sys_log_stderr"]
pub unsafe extern "C" fn log_stderr(
    _: *mut c_void,
    level: LogLevel,
    scope_ptr: *const u8,
    scope_len: usize,
    message_ptr: *const u8,
    message_len: usize,
) {
    if cfg!(target_family = "wasm") {
        return;
    }

    let scope = super::types::slice_or_empty(scope_ptr, scope_len);
    let message = super::types::slice_or_empty(message_ptr, message_len);

    let level_text: &[u8] = match level {
        LogLevel::Error => b"error",
        LogLevel::Warning => b"warning",
        LogLevel::Info => b"info",
        LogLevel::Debug => b"debug",
    };

    // Large enough for a full log_fn chunk plus the level/scope prefix.
    let mut buffer = [0u8; LogEmitter::BUFFER_SIZE + 128];
    let parts: &[&[u8]] = if !scope.is_empty() {
        &[b"[", level_text, b"](", scope, b"): ", message, b"\n"]
    } else {
        &[b"[", level_text, b"]: ", message, b"\n"]
    };
    let total: usize = parts.iter().map(|p| p.len()).sum();
    if total <= buffer.len() {
        let mut end = 0;
        for p in parts {
            buffer[end..end + p.len()].copy_from_slice(p);
            end += p.len();
        }
        stderr::write(&buffer[..end]);
        return;
    }

    // The line didn't fit in our buffer (an embedder called us directly
    // with a very large message). Fall back to writing the pieces
    // separately; interleaving with other threads is possible here but
    // this is a best-effort diagnostic path.
    for p in parts {
        stderr::write(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Serializes the tests below: they share the process-global callback
    /// state (Zig runs tests sequentially).
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Hold the terminal PNG decoder lock and restore the previous decoder
    /// when done, so other tests decoding PNGs are unaffected.
    struct DecodePngGuard {
        _lock: std::sync::RwLockWriteGuard<'static, ()>,
        prev: std::option::Option<terminal_sys::DecodePngFn>,
    }

    impl DecodePngGuard {
        fn new() -> Self {
            DecodePngGuard {
                _lock: terminal_sys::DECODE_PNG_TEST_LOCK
                    .write()
                    .unwrap_or_else(|e| e.into_inner()),
                prev: terminal_sys::decode_png(),
            }
        }
    }

    impl Drop for DecodePngGuard {
        fn drop(&mut self) {
            with_global(|g| g.decode_png = None);
            terminal_sys::set_decode_png(self.prev);
        }
    }

    #[test]
    fn set_decode_png_with_null_clears() {
        let _l = lock();
        let _g = DecodePngGuard::new();
        // Start from a known state.
        with_global(|g| g.decode_png = None);
        terminal_sys::set_decode_png(None);

        assert_eq!(Result::Success, unsafe {
            set(Option::DecodePng as c_int, std::ptr::null())
        });
        assert!(terminal_sys::decode_png().is_none());
    }

    #[test]
    fn set_decode_png_installs_wrapper() {
        unsafe extern "C" fn decode(
            _: *mut c_void,
            _: *const CAllocator,
            _: *const u8,
            _: usize,
            out: *mut Image,
        ) -> bool {
            *out = Image {
                width: 1,
                height: 1,
                data: std::ptr::null_mut(),
                data_len: 0,
            };
            true
        }

        let _l = lock();
        let _g = DecodePngGuard::new();
        assert_eq!(Result::Success, unsafe {
            set(Option::DecodePng as c_int, decode as DecodePngFn as *const c_void)
        });
        assert!(terminal_sys::decode_png().is_some());

        // Clear it again.
        assert_eq!(Result::Success, unsafe {
            set(Option::DecodePng as c_int, std::ptr::null())
        });
        assert!(terminal_sys::decode_png().is_none());
    }

    #[test]
    fn decode_png_output_is_zeroed_before_the_callback() {
        static SEEN: Mutex<std::option::Option<(u32, u32, usize, usize)>> = Mutex::new(None);
        unsafe extern "C" fn decode(
            _: *mut c_void,
            _: *const CAllocator,
            _: *const u8,
            _: usize,
            out: *mut Image,
        ) -> bool {
            // Record what we were given, then report success without
            // filling anything in.
            let o = *out;
            *SEEN.lock().unwrap() = Some((o.width, o.height, o.data as usize, o.data_len));
            true
        }

        let _l = lock();
        let _g = DecodePngGuard::new();
        assert_eq!(Result::Success, unsafe {
            set(Option::DecodePng as c_int, decode as DecodePngFn as *const c_void)
        });

        // Success with no data is rejected rather than read as garbage.
        assert_eq!(
            Err(terminal_sys::DecodeError::InvalidData),
            decode_png_wrapper(b"png")
        );

        // The callback saw an all-zero struct.
        assert_eq!(Some((0, 0, 0, 0)), *SEEN.lock().unwrap());
    }

    #[test]
    fn decode_png_takes_callback_pixels() {
        unsafe extern "C" fn decode(
            _: *mut c_void,
            alloc: *const CAllocator,
            _: *const u8,
            _: usize,
            out: *mut Image,
        ) -> bool {
            let data = (*alloc).zig().alloc(4, 0, 0);
            std::ptr::copy_nonoverlapping([1u8, 2, 3, 4].as_ptr(), data, 4);
            *out = Image {
                width: 1,
                height: 1,
                data,
                data_len: 4,
            };
            true
        }

        let _l = lock();
        let _g = DecodePngGuard::new();
        assert_eq!(Result::Success, unsafe {
            set(Option::DecodePng as c_int, decode as DecodePngFn as *const c_void)
        });
        let image = decode_png_wrapper(b"png").unwrap();
        assert_eq!(1, image.width);
        assert_eq!(vec![1, 2, 3, 4], image.data);
    }

    #[test]
    fn set_log_with_null_clears() {
        let _l = lock();
        with_global(|g| g.log = None);

        assert_eq!(Result::Success, unsafe { set(Option::Log as c_int, std::ptr::null()) });
        assert!(global().log.is_none());
    }

    #[test]
    fn set_log_installs_callback() {
        static CALLED: AtomicBool = AtomicBool::new(false);
        unsafe extern "C" fn log_cb(_: *mut c_void, _: LogLevel, _: *const u8, _: usize, _: *const u8, _: usize) {
            CALLED.store(true, Ordering::SeqCst);
        }

        let _l = lock();
        assert_eq!(Result::Success, unsafe {
            set(Option::Log as c_int, log_cb as LogFn as *const c_void)
        });
        assert!(global().log.is_some());

        emit_log(LogLevel::Info, b"test", b"hello");
        assert!(CALLED.load(Ordering::SeqCst));

        // Clear it again.
        CALLED.store(false, Ordering::SeqCst);
        assert_eq!(Result::Success, unsafe { set(Option::Log as c_int, std::ptr::null()) });
        assert!(global().log.is_none());

        emit_log(LogLevel::Info, b"test", b"should not arrive");
        assert!(!CALLED.load(Ordering::SeqCst));
    }

    static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
    static TOTAL_LEN: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn counting_log_cb(
        _: *mut c_void,
        _: LogLevel,
        _: *const u8,
        _: usize,
        _: *const u8,
        msg_len: usize,
    ) {
        CALL_COUNT.fetch_add(1, Ordering::SeqCst);
        TOTAL_LEN.fetch_add(msg_len, Ordering::SeqCst);
        // Each chunk must not exceed the buffer size.
        assert!(msg_len <= 2048);
    }

    fn with_counting_log(f: impl FnOnce()) -> (usize, usize) {
        CALL_COUNT.store(0, Ordering::SeqCst);
        TOTAL_LEN.store(0, Ordering::SeqCst);
        with_global(|g| g.log = Some(counting_log_cb));
        f();
        with_global(|g| g.log = None);
        (CALL_COUNT.load(Ordering::SeqCst), TOTAL_LEN.load(Ordering::SeqCst))
    }

    #[test]
    fn log_fn_small_message_single_chunk() {
        let _l = lock();
        let (count, total) = with_counting_log(|| log_fn(log::Level::Info, "", format_args!("hello")));
        assert_eq!(1, count);
        assert_eq!(5, total);
    }

    #[test]
    fn log_fn_message_exceeding_chunk_size_is_split() {
        let _l = lock();
        // Format a message larger than the 2048-byte buffer.
        let fill = "A".repeat(3000);
        let (count, total) = with_counting_log(|| log_fn(log::Level::Info, "", format_args!("{fill}")));
        assert!(count >= 2);
        assert_eq!(3000, total);
    }

    #[test]
    fn log_fn_message_exactly_at_chunk_boundary() {
        let _l = lock();
        // Exactly 2048 bytes — should emit one full chunk, no remainder.
        let fill = "B".repeat(2048);
        let (count, total) = with_counting_log(|| log_fn(log::Level::Info, "", format_args!("{fill}")));
        assert_eq!(1, count);
        assert_eq!(2048, total);
    }

    #[test]
    fn log_fn_message_exactly_double_chunk_size() {
        let _l = lock();
        // Exactly 4096 bytes — should emit exactly two full chunks.
        let fill = "C".repeat(4096);
        let (count, total) = with_counting_log(|| log_fn(log::Level::Info, "", format_args!("{fill}")));
        assert_eq!(2, count);
        assert_eq!(4096, total);
    }

    #[test]
    fn set_userdata() {
        let _l = lock();
        let mut data: u32 = 42;
        let ptr = &mut data as *mut u32 as *mut c_void;
        assert_eq!(Result::Success, unsafe { set(Option::Userdata as c_int, ptr) });
        assert_eq!(ptr, global().userdata);

        // Clear it.
        assert_eq!(Result::Success, unsafe {
            set(Option::Userdata as c_int, std::ptr::null())
        });
        assert!(global().userdata.is_null());
    }

    #[test]
    fn log_stderr_formats() {
        // Smoke test: writes "[info](scope): message" to stderr.
        let scope = b"scope";
        let msg = b"message";
        unsafe {
            log_stderr(
                std::ptr::null_mut(),
                LogLevel::Info,
                scope.as_ptr(),
                scope.len(),
                msg.as_ptr(),
                msg.len(),
            )
        };
    }
}
