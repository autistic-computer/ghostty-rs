//! Port of `src/gtk4-layer-shell/src/main.zig`.
//!
//! STATUS: ported
//!
//! Only compiled with the `gtk` Cargo feature.
//!
//! Deviation: the Zig build compiles gtk4-layer-shell from source and links
//! it statically. It is not a dependency the Rust build can assume is
//! installed (most distributions don't ship it by default), so the library is
//! loaded at runtime with `dlopen` (`libgtk4-layer-shell.so.0`). When it
//! isn't available, [`is_supported`] returns false (which is how Ghostty
//! already decides whether it can use layer-shell features, like the quick
//! terminal) and the other functions do nothing. Note that gtk4-layer-shell
//! 1.x must be loaded before `libwayland-client`; preloading it
//! (`LD_PRELOAD`) guarantees that when it's loaded dynamically.
//!
//! Windows and monitors are passed as raw `GtkWindow*`/`GdkMonitor*`
//! pointers (e.g. from `gtk4::Window::as_ptr()`), so this module doesn't
//! depend on a particular GTK binding crate.

#![cfg(feature = "gtk")]

use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::sync::OnceLock;

/// `GtkWindow` (opaque).
#[repr(C)]
pub struct GtkWindow {
    _private: [u8; 0],
}

/// `GdkMonitor` (opaque).
#[repr(C)]
pub struct GdkMonitor {
    _private: [u8; 0],
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellLayer {
    /// GTK_LAYER_SHELL_LAYER_BACKGROUND
    Background = 0,
    /// GTK_LAYER_SHELL_LAYER_BOTTOM
    Bottom = 1,
    /// GTK_LAYER_SHELL_LAYER_TOP
    Top = 2,
    /// GTK_LAYER_SHELL_LAYER_OVERLAY
    Overlay = 3,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellEdge {
    /// GTK_LAYER_SHELL_EDGE_LEFT
    Left = 0,
    /// GTK_LAYER_SHELL_EDGE_RIGHT
    Right = 1,
    /// GTK_LAYER_SHELL_EDGE_TOP
    Top = 2,
    /// GTK_LAYER_SHELL_EDGE_BOTTOM
    Bottom = 3,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardMode {
    /// GTK_LAYER_SHELL_KEYBOARD_MODE_NONE
    None = 0,
    /// GTK_LAYER_SHELL_KEYBOARD_MODE_EXCLUSIVE
    Exclusive = 1,
    /// GTK_LAYER_SHELL_KEYBOARD_MODE_ON_DEMAND
    OnDemand = 2,
}

/// `std.SemanticVersion` (without pre-release/build metadata).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemanticVersion {
    pub major: usize,
    pub minor: usize,
    pub patch: usize,
}

/// The dynamically loaded gtk4-layer-shell entry points.
struct Lib {
    is_supported: unsafe extern "C" fn() -> c_int,
    get_protocol_version: unsafe extern "C" fn() -> c_uint,
    get_major_version: unsafe extern "C" fn() -> c_uint,
    get_minor_version: unsafe extern "C" fn() -> c_uint,
    get_micro_version: unsafe extern "C" fn() -> c_uint,
    init_for_window: unsafe extern "C" fn(*mut GtkWindow),
    set_layer: unsafe extern "C" fn(*mut GtkWindow, c_uint),
    set_anchor: unsafe extern "C" fn(*mut GtkWindow, c_uint, c_int),
    set_margin: unsafe extern "C" fn(*mut GtkWindow, c_uint, c_int),
    set_keyboard_mode: unsafe extern "C" fn(*mut GtkWindow, c_uint),
    set_monitor: unsafe extern "C" fn(*mut GtkWindow, *mut GdkMonitor),
    set_namespace: unsafe extern "C" fn(*mut GtkWindow, *const c_char),
}

fn lib() -> Option<&'static Lib> {
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();
    LIB.get_or_init(|| {
        // SAFETY: loading a well-known shared library and looking up
        // functions with the signatures from gtk4-layer-shell.h.
        let lib = unsafe { load() };
        if lib.is_none() {
            log::info!(target: "gtk4_layer_shell", "gtk4-layer-shell library not available");
        }
        lib
    })
    .as_ref()
}

unsafe fn load() -> Option<Lib> {
    let names: [&CStr; 2] = [c"libgtk4-layer-shell.so.0", c"libgtk4-layer-shell.so"];
    let handle = names.iter().find_map(|name| {
        // SAFETY: name is NUL-terminated.
        let h = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        (!h.is_null()).then_some(h)
    })?;

    unsafe fn sym<T>(handle: *mut c_void, name: &CStr) -> Option<T> {
        // SAFETY: handle is a live dlopen handle; name is NUL-terminated.
        let p = unsafe { libc::dlsym(handle, name.as_ptr()) };
        if p.is_null() {
            return None;
        }
        debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
        // SAFETY: T is a function pointer type matching the symbol.
        Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
    }

    // SAFETY: the symbol types match gtk4-layer-shell.h.
    unsafe {
        Some(Lib {
            is_supported: sym(handle, c"gtk_layer_is_supported")?,
            get_protocol_version: sym(handle, c"gtk_layer_get_protocol_version")?,
            get_major_version: sym(handle, c"gtk_layer_get_major_version")?,
            get_minor_version: sym(handle, c"gtk_layer_get_minor_version")?,
            get_micro_version: sym(handle, c"gtk_layer_get_micro_version")?,
            init_for_window: sym(handle, c"gtk_layer_init_for_window")?,
            set_layer: sym(handle, c"gtk_layer_set_layer")?,
            set_anchor: sym(handle, c"gtk_layer_set_anchor")?,
            set_margin: sym(handle, c"gtk_layer_set_margin")?,
            set_keyboard_mode: sym(handle, c"gtk_layer_set_keyboard_mode")?,
            set_monitor: sym(handle, c"gtk_layer_set_monitor")?,
            set_namespace: sym(handle, c"gtk_layer_set_namespace")?,
        })
    }
}

pub fn is_supported() -> bool {
    // SAFETY: plain FFI query.
    lib().is_some_and(|l| unsafe { (l.is_supported)() } != 0)
}

pub fn get_protocol_version() -> c_uint {
    // SAFETY: plain FFI query.
    lib().map_or(0, |l| unsafe { (l.get_protocol_version)() })
}

/// The library version, or 0.0.0 if the library isn't available.
pub fn get_library_version() -> SemanticVersion {
    match lib() {
        // SAFETY: plain FFI queries.
        Some(l) => unsafe {
            SemanticVersion {
                major: (l.get_major_version)() as usize,
                minor: (l.get_minor_version)() as usize,
                patch: (l.get_micro_version)() as usize,
            }
        },
        None => SemanticVersion {
            major: 0,
            minor: 0,
            patch: 0,
        },
    }
}

/// # Safety
/// `window` must be a valid `GtkWindow*`.
pub unsafe fn init_for_window(window: *mut GtkWindow) {
    if let Some(l) = lib() {
        // SAFETY: guaranteed by the caller.
        unsafe { (l.init_for_window)(window) }
    }
}

/// # Safety
/// `window` must be a valid `GtkWindow*`.
pub unsafe fn set_layer(window: *mut GtkWindow, layer: ShellLayer) {
    if let Some(l) = lib() {
        // SAFETY: guaranteed by the caller.
        unsafe { (l.set_layer)(window, layer as c_uint) }
    }
}

/// # Safety
/// `window` must be a valid `GtkWindow*`.
pub unsafe fn set_anchor(window: *mut GtkWindow, edge: ShellEdge, anchor_to_edge: bool) {
    if let Some(l) = lib() {
        // SAFETY: guaranteed by the caller.
        unsafe { (l.set_anchor)(window, edge as c_uint, anchor_to_edge as c_int) }
    }
}

/// # Safety
/// `window` must be a valid `GtkWindow*`.
pub unsafe fn set_margin(window: *mut GtkWindow, edge: ShellEdge, margin_size: c_int) {
    if let Some(l) = lib() {
        // SAFETY: guaranteed by the caller.
        unsafe { (l.set_margin)(window, edge as c_uint, margin_size) }
    }
}

/// # Safety
/// `window` must be a valid `GtkWindow*`.
pub unsafe fn set_keyboard_mode(window: *mut GtkWindow, mode: KeyboardMode) {
    if let Some(l) = lib() {
        // SAFETY: guaranteed by the caller.
        unsafe { (l.set_keyboard_mode)(window, mode as c_uint) }
    }
}

/// # Safety
/// `window` must be a valid `GtkWindow*`; `monitor` must be null or a valid
/// `GdkMonitor*`.
pub unsafe fn set_monitor(window: *mut GtkWindow, monitor: *mut GdkMonitor) {
    if let Some(l) = lib() {
        // SAFETY: guaranteed by the caller.
        unsafe { (l.set_monitor)(window, monitor) }
    }
}

/// # Safety
/// `window` must be a valid `GtkWindow*`.
pub unsafe fn set_namespace(window: *mut GtkWindow, name: &CStr) {
    if let Some(l) = lib() {
        // SAFETY: guaranteed by the caller; name is NUL-terminated.
        unsafe { (l.set_namespace)(window, name.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_values() {
        assert_eq!(ShellLayer::Overlay as u32, 3);
        assert_eq!(ShellEdge::Bottom as u32, 3);
        assert_eq!(KeyboardMode::OnDemand as u32, 2);
    }

    #[test]
    fn version_consistent_with_support() {
        // Without a display this can't be supported; it must not crash.
        let v = get_library_version();
        if lib().is_none() {
            assert_eq!(v.major, 0);
            assert!(!is_supported());
            assert_eq!(get_protocol_version(), 0);
        }
    }
}
