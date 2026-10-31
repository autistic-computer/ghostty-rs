//! Port of `src/main_wasm.zig`.
//!
//! STATUS: ported
//!
//! This is the main file for the WASM module. The WASM module has to
//! export a C ABI compatible API.
//!
//! Notes on the port: Zig's `comptime` block references the Wasm
//! modules (`os/wasm`, `font`, `terminal`, `config.Wasm`, `App.Wasm`) so
//! their exports are emitted; Rust compiles and exports every module of
//! the crate (the wasm-only ones are gated on `wasm32` where they are
//! declared). The root `std_options` (log level and log function) are
//! exposed as [`LOG_LEVEL`] and, on `wasm32`, [`install_logger`] (a Rust
//! library cannot set the global logger implicitly).

/// Set our log level. We try to get as much logging as possible but in
/// ReleaseSmall mode where we're optimizing for space, we elevate the
/// log level. (Rust cannot tell a size-optimized build apart from other
/// release builds, so release builds use `info`.)
pub const LOG_LEVEL: log::LevelFilter = if cfg!(debug_assertions) {
    log::LevelFilter::Debug
} else {
    log::LevelFilter::Info
};

/// Set our log function (Zig: `std_options.logFn = os/wasm/log.zig`'s
/// `log`). Returns false if a logger is already installed.
#[cfg(target_arch = "wasm32")]
pub fn install_logger() -> bool {
    static LOGGER: crate::os::wasm::log::WasmLogger = crate::os::wasm::log::WasmLogger;
    if log::set_logger(&LOGGER).is_err() {
        return false;
    }
    log::set_max_level(LOG_LEVEL);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_level_matches_build_mode() {
        if cfg!(debug_assertions) {
            assert_eq!(log::LevelFilter::Debug, LOG_LEVEL);
        } else {
            assert_eq!(log::LevelFilter::Info, LOG_LEVEL);
        }
    }
}
