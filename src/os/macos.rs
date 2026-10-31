//! Port of `src/os/macos.zig`.
//!
//! STATUS: ported (macOS-only; best effort, not compiled on Linux)

#![cfg(target_os = "macos")]

use std::ffi::{c_int, c_uint, c_ulong, CStr};
use std::fmt;
use std::path::Path;

use crate::build_config::BUNDLE_ID;

/// Minimal Objective-C runtime bindings used by the `os` module.
///
/// NOTE(port): Zig uses the third-party zig-objc package, which has no
/// Rust port (`pkg/macos` has no objc bindings); only the calls `os` needs.
pub mod objc {
    use std::ffi::{c_char, c_void, CStr};

    pub type Id = *mut c_void;
    pub type Sel = *const c_void;

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
        pub fn objc_autoreleasePoolPush() -> *mut c_void;
        pub fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    #[link(name = "Foundation", kind = "framework")]
    extern "C" {}

    pub fn get_class(name: &CStr) -> Option<Id> {
        // SAFETY: valid NUL-terminated name.
        let cls = unsafe { objc_getClass(name.as_ptr()) };
        (!cls.is_null()).then_some(cls)
    }

    pub fn sel(name: &CStr) -> Sel {
        // SAFETY: valid NUL-terminated name.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    /// `[obj sel]` returning an object.
    ///
    /// # Safety
    /// `obj` must respond to `sel` with that signature.
    pub unsafe fn msg_send_id(obj: Id, sel: Sel) -> Id {
        let f: unsafe extern "C" fn(Id, Sel) -> Id =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        f(obj, sel)
    }

    /// `[obj sel]` returning a double.
    ///
    /// # Safety
    /// `obj` must respond to `sel` with that signature.
    pub unsafe fn msg_send_f64(obj: Id, sel: Sel) -> f64 {
        let f: unsafe extern "C" fn(Id, Sel) -> f64 =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        f(obj, sel)
    }

    /// `[obj sel]` returning an NSUInteger.
    ///
    /// # Safety
    /// `obj` must respond to `sel` with that signature.
    pub unsafe fn msg_send_usize(obj: Id, sel: Sel) -> usize {
        let f: unsafe extern "C" fn(Id, Sel) -> usize =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        f(obj, sel)
    }

    /// `[obj sel:index]` returning an object.
    ///
    /// # Safety
    /// `obj` must respond to `sel` with that signature.
    pub unsafe fn msg_send_id_usize(obj: Id, sel: Sel, arg: usize) -> Id {
        let f: unsafe extern "C" fn(Id, Sel, usize) -> Id =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        f(obj, sel, arg)
    }

    /// `[obj sel]` returning a C string (e.g. `UTF8String`).
    ///
    /// # Safety
    /// `obj` must respond to `sel` with that signature.
    pub unsafe fn msg_send_cstr(obj: Id, sel: Sel) -> *const c_char {
        let f: unsafe extern "C" fn(Id, Sel) -> *const c_char =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        f(obj, sel)
    }

    /// Send a message with an arbitrary signature.
    ///
    /// # Safety
    /// `F` must be the exact `extern "C"` function pointer type of the
    /// method implementation.
    pub unsafe fn msg_send_fn<F: Copy>() -> F {
        let f = objc_msgSend as unsafe extern "C" fn();
        std::mem::transmute_copy(&f)
    }

    /// Read `-[NSString UTF8String]` into an owned string.
    ///
    /// # Safety
    /// `ns_string` must be an NSString (or nil).
    pub unsafe fn ns_string_to_string(ns_string: Id) -> Option<String> {
        if ns_string.is_null() {
            return None;
        }
        let c_str = msg_send_cstr(ns_string, sel(c"UTF8String"));
        if c_str.is_null() {
            return None;
        }
        Some(CStr::from_ptr(c_str).to_string_lossy().into_owned())
    }
}

/// Verifies that the running macOS system version is at least the given
/// version.
pub fn is_at_least_version(major: i64, minor: i64, patch: i64) -> bool {
    let ns_process_info = objc::get_class(c"NSProcessInfo").unwrap();
    // SAFETY: standard Foundation selectors and signatures.
    unsafe {
        let info = objc::msg_send_id(ns_process_info, objc::sel(c"processInfo"));
        let f: unsafe extern "C" fn(objc::Id, objc::Sel, NSOperatingSystemVersion) -> bool =
            objc::msg_send_fn();
        f(
            info,
            objc::sel(c"isOperatingSystemAtLeastVersion:"),
            NSOperatingSystemVersion {
                major,
                minor,
                patch,
            },
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppSupportDirError {
    AppleAPIFailed,
}

impl fmt::Display for AppSupportDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AppleAPIFailed")
    }
}

impl std::error::Error for AppSupportDirError {}

pub type CacheDirError = AppSupportDirError;

/// Return the path to the application support directory for Ghostty
/// with the given sub path joined.
pub fn app_support_dir(sub_path: &str) -> Result<String, AppSupportDirError> {
    common_dir(
        NSSearchPathDirectory::NSApplicationSupportDirectory,
        sub_path,
    )
}

/// Return the path to the system cache directory with the given sub path
/// joined.
pub fn cache_dir(sub_path: &str) -> Result<String, CacheDirError> {
    common_dir(NSSearchPathDirectory::NSCachesDirectory, sub_path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetQosClassError {
    /// The thread can't have its QoS class changed usually because
    /// a different pthread API was called that makes it an invalid
    /// target.
    ThreadIncompatible,
}

/// Set the QoS class of the running thread.
///
/// https://developer.apple.com/documentation/apple-silicon/tuning-your-code-s-performance-for-apple-silicon?preferredLanguage=occ
pub fn set_qos_class(class: QosClass) -> Result<(), SetQosClassError> {
    // SAFETY: plain libc call.
    match unsafe { pthread_set_qos_class_self_np(class, 0) } {
        0 => Ok(()),
        libc::EPERM => Err(SetQosClassError::ThreadIncompatible),

        // EPERM is the only known error that can happen based on
        // the man pages for pthread_set_qos_class_self_np. I haven't
        // checked the XNU source code to see if there are other
        // possible errors.
        _ => panic!("unexpected pthread_set_qos_class_self_np error"),
    }
}

/// https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/PrioritizeWorkAtTheTaskLevel.html#//apple_ref/doc/uid/TP40013929-CH35-SW1
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QosClass {
    UserInteractive = 0x21,
    UserInitiated = 0x19,
    Default = 0x15,
    Utility = 0x11,
    Background = 0x09,
    Unspecified = 0x00,
}

extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: QosClass, relative_priority: c_int) -> c_int;
    pub fn pthread_setname_np(name: *const std::ffi::c_char) -> c_int;
}

const _: () = assert!(std::mem::size_of::<QosClass>() == std::mem::size_of::<c_uint>());

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NSOperatingSystemVersion {
    pub major: i64,
    pub minor: i64,
    pub patch: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum NSSearchPathDirectory {
    NSCachesDirectory = 13,
    NSApplicationSupportDirectory = 14,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum NSSearchPathDomainMask {
    NSUserDomainMask = 1,
}

fn common_dir(
    directory: NSSearchPathDirectory,
    sub_path: &str,
) -> Result<String, AppSupportDirError> {
    let ns_file_manager = objc::get_class(c"NSFileManager").unwrap();
    // SAFETY: standard Foundation selectors and signatures.
    let base_dir = unsafe {
        let manager = objc::msg_send_id(ns_file_manager, objc::sel(c"defaultManager"));
        let f: unsafe extern "C" fn(
            objc::Id,
            objc::Sel,
            c_ulong,
            c_ulong,
            objc::Id,
            bool,
            *mut objc::Id,
        ) -> objc::Id = objc::msg_send_fn();
        let url = f(
            manager,
            objc::sel(c"URLForDirectory:inDomain:appropriateForURL:create:error:"),
            directory as c_ulong,
            NSSearchPathDomainMask::NSUserDomainMask as c_ulong,
            std::ptr::null_mut(),
            true,
            std::ptr::null_mut(),
        );
        if url.is_null() {
            return Err(AppSupportDirError::AppleAPIFailed);
        }

        let path = objc::msg_send_id(url, objc::sel(c"path"));
        objc::ns_string_to_string(path).ok_or(AppSupportDirError::AppleAPIFailed)?
    };

    let mut p = Path::new(&base_dir).join(BUNDLE_ID);
    if !sub_path.is_empty() {
        p = p.join(sub_path);
    }
    Ok(p.to_string_lossy().into_owned())
}

/// `[NSFileManager defaultManager].homeDirectoryForCurrentUser.path`
/// (used by `homedir`).
pub fn home_directory_for_current_user() -> Option<String> {
    let ns_file_manager = objc::get_class(c"NSFileManager")?;
    // SAFETY: standard Foundation selectors and signatures.
    unsafe {
        let manager = objc::msg_send_id(ns_file_manager, objc::sel(c"defaultManager"));
        let home_url = objc::msg_send_id(manager, objc::sel(c"homeDirectoryForCurrentUser"));
        if home_url.is_null() {
            return None;
        }
        let home_path = objc::msg_send_id(home_url, objc::sel(c"path"));
        objc::ns_string_to_string(home_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_paths() {
        // Test base path
        {
            let cache_path = cache_dir("").unwrap();
            assert!(cache_path.contains("Caches"));
            assert!(cache_path.contains(BUNDLE_ID));
        }

        // Test with subdir
        {
            let cache_path = cache_dir("test").unwrap();
            assert!(cache_path.contains("Caches"));
            assert!(cache_path.contains(BUNDLE_ID));

            let bundle_path = format!("{BUNDLE_ID}/test");
            assert!(cache_path.contains(&bundle_path));
        }
    }

    #[test]
    fn version_check() {
        assert!(is_at_least_version(10, 0, 0));
        let _ = CStr::from_bytes_with_nul(b"x\0");
    }
}
