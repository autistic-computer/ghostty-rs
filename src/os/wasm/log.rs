//! Port of `src/os/wasm/log.zig`.
//!
//! STATUS: ported (wasm32-only; not compiled on Linux)

#![cfg(target_arch = "wasm32")]

use std::fmt::{self, Write as _};

/// A log function that discards everything. In Zig the format string and
/// scope are comptime parameters, so using this compiles out all logging
/// machinery.
pub fn noop(_level: ::log::Level, _scope: &str, _args: fmt::Arguments<'_>) {}

/// Use the correct implementation.
pub fn log(level: ::log::Level, scope: &str, args: fmt::Arguments<'_>) {
    Freestanding::log(level, scope, args)
}

/// Freestanding implementation calls an extern "log" function.
pub struct Freestanding;

impl Freestanding {
    /// The function the logger will call.
    pub fn log(level: ::log::Level, scope: &str, args: fmt::Arguments<'_>) {
        // Build the string: "<level>: msg" for the default scope, or
        // "<level>(<scope>): msg" otherwise.
        let level_txt = match level {
            ::log::Level::Error => "error",
            ::log::Level::Warn => "warning",
            ::log::Level::Info => "info",
            ::log::Level::Debug | ::log::Level::Trace => "debug",
        };

        let mut str = String::with_capacity(2048);
        str.push_str(level_txt);
        if scope.is_empty() || scope == "default" {
            str.push_str(": ");
        } else {
            str.push('(');
            str.push_str(scope);
            str.push_str("): ");
        }
        if str.write_fmt(args).is_err() {
            return;
        }

        // Send it over to the JS side
        // SAFETY: pointer/length describe a live buffer.
        unsafe { js::log(str.as_ptr(), str.len()) };
    }
}

/// We wrap our externs in this namespace so we can reuse symbols,
/// otherwise "log" would collide.
mod js {
    #[link(wasm_import_module = "env")]
    extern "C" {
        pub fn log(ptr: *const u8, len: usize);
    }
}

/// A `log::Log` implementation forwarding to [`log`].
pub struct WasmLogger;

impl ::log::Log for WasmLogger {
    fn enabled(&self, _metadata: &::log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &::log::Record<'_>) {
        log(record.level(), record.target(), *record.args());
    }

    fn flush(&self) {}
}
