//! Port of `src/config/CApi.zig`.
//!
//! STATUS: ported
//!
//! The C API exports (`ghostty_config_*`, see include/ghostty.h).

use std::ffi::{c_char, c_void, CStr};

use super::c_get;
use super::config::Config;
use super::edit;
use super::key::Key;
use crate::cli::args::CliEnum;
use crate::input::binding::{Action, TriggerC};

const LOG_TARGET: &str = "config";

// Sync with ghostty_string_s (Zig `main_c.String`).
use crate::main_c::String;

/// Zig `String.fromSlice` for a `[:0]const u8` (ownership passes to
/// the caller, who frees it with `ghostty_string_free`).
fn string_from_owned_z(s: crate::cli::args::StringZ) -> String {
    let bytes = s.as_bytes_with_nul();
    String::from_vec_z(bytes[..bytes.len() - 1].to_vec())
}

/// Create a new configuration filled with the initial default values.
#[no_mangle]
pub extern "C" fn ghostty_config_new() -> *mut Config {
    Box::into_raw(Box::new(Config::default()))
}

/// # Safety
///
/// `ptr` must be null or a config returned by this API that wasn't freed.
#[no_mangle]
pub unsafe extern "C" fn ghostty_config_free(ptr: *mut Config) {
    if !ptr.is_null() {
        // SAFETY: the caller guarantees this came from Box::into_raw.
        let mut v = unsafe { Box::from_raw(ptr) };
        v.deinit();
    }
}

/// Deep clone the configuration.
///
/// # Safety
///
/// `self_` must be a valid config.
#[no_mangle]
pub unsafe extern "C" fn ghostty_config_clone(self_: *mut Config) -> *mut Config {
    // SAFETY: the caller guarantees self_ is valid.
    let this = unsafe { &*self_ };
    Box::into_raw(Box::new(this.clone()))
}

/// Load the configuration from the CLI args.
///
/// # Safety
///
/// `self_` must be a valid config.
#[no_mangle]
pub unsafe extern "C" fn ghostty_config_load_cli_args(self_: *mut Config) {
    // SAFETY: the caller guarantees self_ is valid.
    let this = unsafe { &mut *self_ };
    if let Err(err) = this.load_cli_args() {
        log::error!(target: LOG_TARGET, "error loading config err={}", err);
    }
}

/// Load the configuration from the default file locations. This
/// is usually done first. The default file locations are locations
/// such as the home directory.
///
/// # Safety
///
/// `self_` must be a valid config.
#[no_mangle]
pub unsafe extern "C" fn ghostty_config_load_default_files(self_: *mut Config) {
    // SAFETY: the caller guarantees self_ is valid.
    let this = unsafe { &mut *self_ };
    if let Err(err) = this.load_default_files() {
        log::error!(target: LOG_TARGET, "error loading config err={}", err);
    }
}

/// Load the configuration from a specific file path.
/// The path must be null-terminated.
///
/// # Safety
///
/// `self_` must be a valid config and `path` a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn ghostty_config_load_file(self_: *mut Config, path: *const c_char) {
    // SAFETY: the caller guarantees both pointers are valid.
    let (this, path_slice) = unsafe { (&mut *self_, CStr::from_ptr(path)) };
    let path_slice = path_slice.to_string_lossy();
    if let Err(err) = this.load_file(&path_slice) {
        log::error!(
            target: LOG_TARGET,
            "error loading config from file path={} err={}",
            path_slice,
            err
        );
    }
}

/// Load the configuration from the user-specified configuration
/// file locations in the previously loaded configuration. This will
/// recursively continue to load up to a built-in limit.
///
/// # Safety
///
/// `self_` must be a valid config.
#[no_mangle]
pub unsafe extern "C" fn ghostty_config_load_recursive_files(self_: *mut Config) {
    // SAFETY: the caller guarantees self_ is valid.
    let this = unsafe { &mut *self_ };
    if let Err(err) = this.load_recursive_files() {
        log::error!(target: LOG_TARGET, "error loading config err={}", err);
    }
}

/// # Safety
///
/// `self_` must be a valid config.
#[no_mangle]
pub unsafe extern "C" fn ghostty_config_finalize(self_: *mut Config) {
    // SAFETY: the caller guarantees self_ is valid.
    let this = unsafe { &mut *self_ };
    if let Err(err) = this.finalize() {
        log::error!(target: LOG_TARGET, "error finalizing config err={}", err);
    }
}

/// # Safety
///
/// `self_` must be a valid config, `key_str` valid for `len` bytes and
/// `ptr` valid for writes of the key's C type.
#[no_mangle]
pub unsafe extern "C" fn ghostty_config_get(
    self_: *mut Config,
    ptr: *mut c_void,
    key_str: *const u8,
    len: usize,
) -> bool {
    // SAFETY: the caller guarantees the pointers are valid.
    let (this, key_bytes) = unsafe { (&*self_, std::slice::from_raw_parts(key_str, len)) };
    let Ok(key_str) = std::str::from_utf8(key_bytes) else {
        return false;
    };
    let Some(key) = Key::from_tag(key_str) else {
        return false;
    };
    // SAFETY: forwarded from the caller.
    unsafe { c_get::get(this, key, ptr) }
}

/// # Safety
///
/// `self_` must be a valid config and `str` valid for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn ghostty_config_trigger(
    self_: *mut Config,
    str: *const u8,
    len: usize,
) -> TriggerC {
    // SAFETY: the caller guarantees the pointers are valid.
    let (this, s) = unsafe { (&*self_, std::slice::from_raw_parts(str, len)) };
    match config_trigger_(this, s) {
        Ok(v) => v,
        Err(err) => {
            log::error!(target: LOG_TARGET, "error finding trigger err={}", err);
            TriggerC::default()
        }
    }
}

fn config_trigger_(self_: &Config, str: &[u8]) -> Result<TriggerC, crate::input::binding::Error> {
    let action = Action::parse(str)?;
    let trigger = self_.keybind.set.get_trigger(&action).unwrap_or_default();
    Ok(trigger.cval())
}

/// # Safety
///
/// `self_` must be a valid config.
#[no_mangle]
pub unsafe extern "C" fn ghostty_config_diagnostics_count(self_: *mut Config) -> u32 {
    // SAFETY: the caller guarantees self_ is valid.
    let this = unsafe { &*self_ };
    this._diagnostics.items().len() as u32
}

/// # Safety
///
/// `self_` must be a valid config.
#[no_mangle]
pub unsafe extern "C" fn ghostty_config_get_diagnostic(self_: *mut Config, idx: u32) -> Diagnostic {
    // SAFETY: the caller guarantees self_ is valid.
    let this = unsafe { &*self_ };
    let items = this._diagnostics.items();
    if idx as usize >= items.len() {
        return Diagnostic::default();
    }
    // Messages are only precomputed for libghostty builds (and tests).
    match this._diagnostics.precompute.messages.get(idx as usize) {
        Some(message) => Diagnostic {
            message: message.as_ptr(),
        },
        None => Diagnostic::default(),
    }
}

#[no_mangle]
pub extern "C" fn ghostty_config_open_path() -> String {
    match edit::open_path() {
        Ok(path) => string_from_owned_z(path),
        Err(err) => {
            log::error!(target: LOG_TARGET, "error opening config in editor err={}", err);
            String::EMPTY
        }
    }
}

/// Sync with ghostty_diagnostic_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Diagnostic {
    pub message: *const c_char,
}

impl Default for Diagnostic {
    fn default() -> Self {
        Diagnostic {
            message: c"".as_ptr(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::config::{Color, ColorC, WindowTheme};
    use crate::input::binding::TriggerCTag;
    use crate::input::key::Key as InputKey;

    fn get(cfg: &mut Config, ptr: *mut c_void, key: &str) -> bool {
        unsafe { ghostty_config_get(cfg, ptr, key.as_ptr(), key.len()) }
    }

    #[test]
    fn ghostty_config_get_bool() {
        let mut cfg = Config::default();
        cfg.maximize = true;

        let mut out = false;
        assert!(get(&mut cfg, &mut out as *mut bool as *mut c_void, "maximize"));
        assert!(out);
    }

    #[test]
    fn ghostty_config_get_enum() {
        let mut cfg = Config::default();
        cfg.window_theme = WindowTheme::Dark;

        let mut out: *const c_char = std::ptr::null();
        assert!(get(&mut cfg, &mut out as *mut _ as *mut c_void, "window-theme"));
        let str = unsafe { CStr::from_ptr(out) };
        assert_eq!("dark", str.to_str().unwrap());
    }

    #[test]
    fn ghostty_config_get_optional_null_returns_false() {
        let mut cfg = Config::default();
        cfg.unfocused_split_fill = None;

        let mut out = ColorC::default();
        assert!(!get(&mut cfg, &mut out as *mut _ as *mut c_void, "unfocused-split-fill"));
    }

    #[test]
    fn ghostty_config_get_unknown_key_returns_false() {
        let mut cfg = Config::default();

        let mut out = false;
        assert!(!get(&mut cfg, &mut out as *mut bool as *mut c_void, "not-a-real-key"));
    }

    #[test]
    fn ghostty_config_get_optional_string_null_returns_true() {
        let mut cfg = Config::default();
        cfg.title = None;

        let mut out: *const c_char = c"x".as_ptr();
        assert!(get(&mut cfg, &mut out as *mut _ as *mut c_void, "title"));
        assert!(out.is_null());
    }

    #[test]
    fn ghostty_config_get_float() {
        let mut cfg = Config::default();
        cfg.background_opacity = 0.42;

        let mut out: f64 = 0.0;
        assert!(get(&mut cfg, &mut out as *mut f64 as *mut c_void, "background-opacity"));
        assert!((0.42 - out).abs() <= 0.000001);
    }

    #[test]
    fn ghostty_config_get_struct_cval_conversion() {
        let mut cfg = Config::default();
        cfg.background = Color { r: 12, g: 34, b: 56 };

        let mut out = ColorC::default();
        assert!(get(&mut cfg, &mut out as *mut _ as *mut c_void, "background"));
        assert_eq!(12u8, out.r);
        assert_eq!(34u8, out.g);
        assert_eq!(56u8, out.b);
    }

    #[test]
    fn ghostty_config_trigger_default_keybind() {
        let cfg = Config::default();

        // Default commands should be fetchable through config_trigger_
        {
            let trigger = config_trigger_(&cfg, b"open_config").unwrap();
            assert_eq!(TriggerCTag::Unicode, trigger.tag);
            assert_eq!(',' as u32, unsafe { trigger.key.unicode });
        }
        {
            let trigger = config_trigger_(&cfg, b"reload_config").unwrap();
            assert_eq!(TriggerCTag::Unicode, trigger.tag);
            assert_eq!(',' as u32, unsafe { trigger.key.unicode });
        }
        // Performable bindings are not tracked in the reverse map,
        // so config_trigger_ should return a default (empty) trigger.
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            let next = config_trigger_(&cfg, b"navigate_search:next").unwrap();
            assert_eq!(TriggerCTag::Physical, next.tag);
            assert_eq!(InputKey::Unidentified, unsafe { next.key.physical });

            let prev = config_trigger_(&cfg, b"navigate_search:previous").unwrap();
            assert_eq!(TriggerCTag::Physical, prev.tag);
            assert_eq!(InputKey::Unidentified, unsafe { prev.key.physical });
        }
        {
            let trigger = config_trigger_(&cfg, b"adjust_selection:left").unwrap();
            assert_eq!(TriggerCTag::Physical, trigger.tag);
            assert_eq!(InputKey::Unidentified, unsafe { trigger.key.physical });
        }
    }
}
