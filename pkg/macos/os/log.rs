//! Port of `src/macos/os/log.zig`.
//!
//! STATUS: ported
//!
//! `os_log_with_type` is a macro, so (like the Zig package) it is called
//! through a tiny C shim, `zig_os_log_with_type` in `zig_macos.c`, which
//! `build.rs` compiles on macOS.

use std::ffi::{c_char, CStr, CString};
use std::fmt;

use super::c;
use crate::pkg::macos::opaque_type;

opaque_type!(
    /// `os_log_t`
    Log
);

extern "C" {
    fn zig_os_log_with_type(
        log: *mut std::ffi::c_void,
        typ: c::os_log_type_t,
        message: *const c_char,
    );
}

impl Log {
    pub fn create(subsystem: &CStr, category: &CStr) -> &'static Log {
        // SAFETY: both strings are NUL-terminated.
        let ptr = unsafe { c::os_log_create(subsystem.as_ptr(), category.as_ptr()) };
        assert!(!ptr.is_null());
        // SAFETY: non-null os_log_t.
        unsafe { &*(ptr as *const Log) }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::os_release(self.as_ptr()) }
    }

    pub fn type_enabled(&self, typ: LogType) -> bool {
        // SAFETY: self is live.
        unsafe { c::os_log_type_enabled(self.as_ptr(), typ as c::os_log_type_t) }
    }

    /// Log a formatted message (`log.log(alloc, .default, "hello {d}", .{12})`
    /// in Zig is `log.log(LogType::Default, format_args!("hello {}", 12))`).
    pub fn log(&self, typ: LogType, args: fmt::Arguments<'_>) {
        let Ok(str) = CString::new(fmt::format(args)) else {
            return;
        };
        // SAFETY: self is live; str is NUL-terminated.
        unsafe { zig_os_log_with_type(self.as_ptr(), typ as c::os_log_type_t, str.as_ptr()) }
    }
}

/// https://developer.apple.com/documentation/os/os_log_type_t?language=objc
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogType {
    Default = c::OS_LOG_TYPE_DEFAULT,
    Debug = c::OS_LOG_TYPE_DEBUG,
    Info = c::OS_LOG_TYPE_INFO,
    Err = c::OS_LOG_TYPE_ERROR,
    Fault = c::OS_LOG_TYPE_FAULT,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log() {
        let log = Log::create(c"com.mitchellh.ghostty", c"test");

        assert!(log.type_enabled(LogType::Fault));
        log.log(LogType::Default, format_args!("hello {}", 12));
        log.release();
    }
}
