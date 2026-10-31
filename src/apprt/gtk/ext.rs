//! Port of `src/apprt/gtk/ext.zig`.
//!
//! STATUS: ported
//!
//! Extensions/helpers for GTK objects, following a similar naming
//! style to zig-gobject. These should, wherever possible, be Zig-friendly
//! wrappers around existing GTK functionality, rather than complex new
//! helpers.
#![cfg(feature = "gtk")]

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "ext/actions.rs"]
pub mod actions;
#[path = "ext/slice.rs"]
pub mod slice;
// @generated-mods-end

use glib::prelude::*;
use glib::translate::IntoGlib;
use gtk4::prelude::*;

pub use self::slice::StringList;

/// Wrapper around `gobject.boxedCopy` to copy a boxed type `T`.
///
/// # Safety
/// `ptr` must point to a valid boxed value of type `T::static_type()`.
pub unsafe fn boxed_copy<T: StaticType>(ptr: *const T) -> *mut T {
    let copy = glib::gobject_ffi::g_boxed_copy(T::static_type().into_glib(), ptr as *const _);
    copy as *mut T
}

/// Wrapper around `gobject.boxedFree` to free a boxed type `T`.
///
/// # Safety
/// `ptr`, if non-null, must point to a valid boxed value of type
/// `T::static_type()` that the caller owns.
pub unsafe fn boxed_free<T: StaticType>(ptr: *mut T) {
    if !ptr.is_null() {
        glib::gobject_ffi::g_boxed_free(T::static_type().into_glib(), ptr as *mut _);
    }
}

/// A wrapper around `glib.List.findCustom` to find an element in the list.
/// The type `T` must be the guaranteed type of every list element.
///
/// (gtk-rs converts `GList`s into `Vec`s, so this works on the raw list.)
///
/// # Safety
/// `list` must be a valid `GList` (or null) whose elements all point to `T`.
pub unsafe fn list_find<T>(list: *mut glib::ffi::GList, func: fn(&T) -> bool) -> Option<*mut T> {
    let mut elem = list;
    while !elem.is_null() {
        let data = (*elem).data as *mut T;
        if !data.is_null() && func(&*data) {
            return Some(data);
        }
        elem = (*elem).next;
    }
    None
}

/// Wrapper around `gtk.Widget.getAncestor` to get the widget ancestor
/// of the given type `T`, or null if it doesn't exist.
pub fn get_ancestor<T: IsA<gtk4::Widget>>(widget: &impl IsA<gtk4::Widget>) -> Option<T> {
    let ancestor = widget.ancestor(T::static_type())?;
    // We can assert the unwrap because getAncestor above
    Some(ancestor.downcast::<T>().unwrap())
}

/// Check a gobject.Value to see what type it is wrapping. This is equivalent to GTK's
/// `G_VALUE_HOLDS()` macro but Zig's C translator does not like it.
pub fn g_value_holds(value: Option<&glib::Value>, g_type: glib::Type) -> bool {
    let Some(value) = value else { return false };
    if value.type_() == g_type {
        return true;
    }
    // SAFETY: `value` is a valid, initialized GValue.
    unsafe {
        glib::gobject_ffi::g_type_check_value_holds(
            glib::translate::ToGlibPtr::to_glib_none(value).0,
            g_type.into_glib(),
        ) != glib::ffi::GFALSE
    }
}

/// `glib.Source.remove(id) != 0`: removes the source, returning false
/// (instead of panicking like `glib::SourceId::remove`) if it no longer
/// exists, so callers can log it as the Zig code does.
pub(crate) fn source_remove(id: glib::SourceId) -> bool {
    // SAFETY: g_source_remove accepts any ID and reports unknown ones.
    unsafe { glib::ffi::g_source_remove(id.as_raw()) != glib::ffi::GFALSE }
}

/// Test-only helper: GTK may only be used from the one thread that
/// initialized it, but the Rust test harness runs tests on many threads.
/// [`test_gtk::run`] runs a closure on a single, dedicated GTK thread
/// (initializing GTK there once) and returns its result, or `None` if GTK
/// could not be initialized (no display), which the Zig tests treat as
/// `error.SkipZigTest`.
#[cfg(test)]
pub(crate) mod test_gtk {
    use std::sync::mpsc;
    use std::sync::{Mutex, OnceLock};

    type Job = Box<dyn FnOnce() + Send>;

    struct GtkThread {
        jobs: Mutex<mpsc::Sender<Job>>,
        initialized: bool,
    }

    fn thread() -> &'static GtkThread {
        static THREAD: OnceLock<GtkThread> = OnceLock::new();
        THREAD.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            let (init_tx, init_rx) = mpsc::channel::<bool>();
            std::thread::Builder::new()
                .name("gtk-test".into())
                .spawn(move || {
                    let ok = gtk4::init().is_ok();
                    init_tx.send(ok).unwrap();
                    for job in rx {
                        job();
                    }
                })
                .unwrap();
            let initialized = init_rx.recv().unwrap();
            GtkThread {
                jobs: Mutex::new(tx),
                initialized,
            }
        })
    }

    /// Runs `f` on the GTK thread. Returns `None` when there is no display.
    /// Panics in `f` are propagated to the caller.
    pub fn run<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
        let t = thread();
        if !t.initialized {
            return None;
        }
        let (tx, rx) = mpsc::channel();
        let job: Job = Box::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            let _ = tx.send(result);
        });
        t.jobs.lock().unwrap().send(job).unwrap();
        match rx.recv().unwrap() {
            Ok(v) => Some(v),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_value_holds_types() {
        let v = 5i32.to_value();
        assert!(g_value_holds(Some(&v), glib::Type::I32));
        assert!(!g_value_holds(Some(&v), glib::Type::STRING));
        assert!(!g_value_holds(None, glib::Type::I32));

        let obj = glib::Object::new::<glib::Object>().to_value();
        assert!(g_value_holds(Some(&obj), glib::Type::OBJECT));
    }

    #[test]
    fn list_find_custom() {
        let mut values = [1i32, 2, 3];
        unsafe {
            let mut list: *mut glib::ffi::GList = std::ptr::null_mut();
            for v in values.iter_mut() {
                list = glib::ffi::g_list_append(list, v as *mut i32 as *mut _);
            }
            let found = list_find::<i32>(list, |v| *v == 2).unwrap();
            assert_eq!(*found, 2);
            assert!(list_find::<i32>(list, |v| *v == 4).is_none());
            glib::ffi::g_list_free(list);
        }
    }

    #[test]
    fn get_ancestor_of_type() {
        let Some(()) = test_gtk::run(|| {
            let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            let inner = gtk4::Label::new(None);
            outer.append(&inner);
            let found: Option<gtk4::Box> = get_ancestor(&inner);
            assert_eq!(found.as_ref(), Some(&outer));
            let none: Option<gtk4::Window> = get_ancestor(&inner);
            assert!(none.is_none());
        }) else {
            return;
        };
    }
}
