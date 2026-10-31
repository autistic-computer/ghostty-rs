//! Port of `src/main_c.zig`.
//!
//! STATUS: ported
//!
//! This is the main file for the C API. The C API is used to embed Ghostty
//! within other applications. Depending on the build settings some APIs
//! may not be available (i.e. embedding into macOS exposes various Metal
//! support).
//!
//! This currently isn't supported as a general purpose embedding API.
//! This is currently used only to embed ghostty within a macOS app. However,
//! it could be expanded to be general purpose in the future.
//!
//! Deviations:
//! * Zig force-references the `export` declarations of the config C API,
//!   the apprt C API (`apprt.runtime.CAPI`), the benchmark C API and
//!   `quirks_memset` in a `comptime` block so they are emitted. Rust emits
//!   every `#[no_mangle]` function of the crate, so nothing is needed here.
//! * The comptime assertion that `apprt.runtime == apprt.embedded` has no
//!   counterpart: the Rust library is a single crate for every target.
//! * `std_options` (logging) is configured by `main_ghostty` / the `log`
//!   crate rather than a root-file declaration.
//! * `DllMain` (MSVC CRT bootstrap on Windows) is not ported: Rust's
//!   runtime initializes the CRT for `cdylib`s itself.
//! * [`String::from_slice`] is split by sentinel-ness (Zig inspects the
//!   slice type at comptime): [`String::from_vec`] for `[]u8` and
//!   [`String::from_vec_z`] for `[:0]u8`. Both take ownership of a heap
//!   allocation, which [`String::deinit`] (`ghostty_string_free`) frees.
//! * The build mode is `debug` with `debug_assertions` and `release_fast`
//!   otherwise (like `build_config::MODE_STRING`): Cargo has no
//!   safe/small distinction.

use std::ffi::{c_char, c_int, CStr};

use crate::build_config;
use crate::global;
use crate::os as internal_os;

/// ghostty_info_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Info {
    pub mode: BuildMode,
    pub version: *const u8,
    pub version_len: usize,
}

/// `Info.BuildMode` (ghostty_build_mode_e)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    Debug,
    ReleaseSafe,
    ReleaseFast,
    ReleaseSmall,
}

/// ghostty_string_s
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct String {
    pub ptr: *const u8,
    pub len: usize,
    pub sentinel: bool,
}

impl String {
    pub const EMPTY: String = String {
        ptr: std::ptr::null(),
        len: 0,
        sentinel: false,
    };

    /// Zig `String.fromSlice` for a `[]u8`: takes ownership of the bytes
    /// (freed by [`String::deinit`]).
    pub fn from_vec(bytes: Vec<u8>) -> String {
        let bytes: Box<[u8]> = bytes.into_boxed_slice();
        let len = bytes.len();
        let ptr = Box::into_raw(bytes) as *const u8;
        String {
            ptr,
            len,
            sentinel: false,
        }
    }

    /// Zig `String.fromSlice` for a `[:0]u8`: takes ownership of the bytes
    /// (without the terminator, which is appended here) and frees them in
    /// [`String::deinit`]. The length excludes the terminator.
    pub fn from_vec_z(mut bytes: Vec<u8>) -> String {
        let len = bytes.len();
        bytes.push(0);
        let bytes: Box<[u8]> = bytes.into_boxed_slice();
        let ptr = Box::into_raw(bytes) as *const u8;
        String {
            ptr,
            len,
            sentinel: true,
        }
    }

    /// Free the string.
    ///
    /// # Safety
    ///
    /// The string must be [`String::EMPTY`] or have been created by
    /// [`String::from_vec`]/[`String::from_vec_z`] and not freed yet.
    pub unsafe fn deinit(&self) {
        if self.ptr.is_null() {
            return;
        }
        let len = if self.sentinel { self.len + 1 } else { self.len };
        // SAFETY: the caller guarantees ptr/len describe a leaked Box<[u8]>
        // (including the terminator for sentinel strings).
        unsafe {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                self.ptr as *mut u8,
                len,
            )));
        }
    }
}

/// Initialize ghostty global state.
///
/// # Safety
///
/// `argv` must point to `argc` valid NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn ghostty_init(argc: usize, argv: *mut *mut c_char) -> c_int {
    if let Err(err) = global::init(global::InitOpts::C {
        argc,
        argv: argv as *const *const c_char,
        // Asserting libc means that we can fast-path all POSIX blocks
        // (Rust: a snapshot of the process environment).
        environ: internal_os::environ_map(),
    }) {
        log::error!("failed to initialize ghostty error={err}");
        return 1;
    }

    0
}

/// Runs an action if it is specified. If there is no action this returns
/// false. If there is an action then this doesn't return.
#[no_mangle]
pub extern "C" fn ghostty_cli_try_action() {
    let Some(action) = global::action() else {
        return;
    };
    log::info!("executing CLI action={action:?}");
    match action.run() {
        Ok(code) => std::process::exit(code as i32),
        Err(err) => {
            log::error!("CLI action failed error={err:?}");
            std::process::exit(1);
        }
    }
}

/// Return metadata about Ghostty, such as version, build mode, etc.
#[no_mangle]
pub extern "C" fn ghostty_info() -> Info {
    Info {
        mode: if cfg!(debug_assertions) {
            BuildMode::Debug
        } else {
            BuildMode::ReleaseFast
        },
        version: build_config::VERSION_STRING.as_ptr(),
        version_len: build_config::VERSION_STRING.len(),
    }
}

/// Translate a string maintained by libghostty into the current
/// application language. This will return the same string (same pointer)
/// if no translation is found, so the pointer must be stable through
/// the function call.
///
/// This should only be used for singular strings maintained by Ghostty.
///
/// # Safety
///
/// `msgid` must be a valid NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn ghostty_translate(msgid: *const c_char) -> *const c_char {
    // SAFETY: the caller guarantees a valid C string.
    let msgid = unsafe { CStr::from_ptr(msgid) };
    internal_os::i18n::gettext(msgid).as_ptr()
}

/// Free a string allocated by Ghostty.
///
/// # Safety
///
/// `str` must be a string returned by the Ghostty C API that wasn't freed.
#[no_mangle]
pub unsafe extern "C" fn ghostty_string_free(str: String) {
    // SAFETY: forwarded from the caller.
    unsafe { str.deinit() };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghostty_string_s_empty_string() {
        let empty_string = String::EMPTY;
        assert!(empty_string.len == 0);
        assert!(!empty_string.sentinel);
        // SAFETY: the empty string owns nothing.
        unsafe { empty_string.deinit() };
    }

    #[test]
    fn ghostty_string_s_c_string() {
        let slice: &[u8] = b"hello";
        let c_null_string = String::from_vec_z(slice.to_vec());

        // SAFETY: from_vec_z allocated len + 1 bytes.
        assert_eq!(0, unsafe { *c_null_string.ptr.add(5) });
        assert!(c_null_string.len == 5);
        assert!(c_null_string.sentinel);
        // SAFETY: created by from_vec_z.
        unsafe { c_null_string.deinit() };
    }

    #[test]
    fn ghostty_string_s_zig_string() {
        let slice: &[u8] = b"hello";
        let zig_string = String::from_vec(slice.to_vec());

        assert!(zig_string.len == 5);
        assert!(!zig_string.sentinel);
        // SAFETY: created by from_vec.
        unsafe { zig_string.deinit() };
    }

    #[test]
    fn ghostty_info_version() {
        let info = ghostty_info();
        // SAFETY: the version points at a static string of version_len bytes.
        let version = unsafe { std::slice::from_raw_parts(info.version, info.version_len) };
        assert_eq!(build_config::VERSION_STRING.as_bytes(), version);
    }
}
