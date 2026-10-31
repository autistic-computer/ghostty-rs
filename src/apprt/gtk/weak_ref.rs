//! Port of `src/apprt/gtk/weak_ref.zig`.
//!
//! STATUS: ported
#![cfg(feature = "gtk")]

use glib::prelude::*;

/// A lightweight wrapper around gobject.WeakRef to make it type-safe
/// to hold a single type of value.
///
/// The port wraps `glib::WeakRef<T>`, which keeps the `GWeakRef` in its own
/// heap allocation and clears it on drop, so the use-after-free hazard
/// described on [`WeakRef::deinit`] cannot happen in Rust; `deinit` is kept
/// so call sites still name the requirement.
#[derive(Debug)]
pub struct WeakRef<T: IsA<glib::Object>> {
    r#ref: glib::WeakRef<T>,
}

impl<T: IsA<glib::Object>> Default for WeakRef<T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T: IsA<glib::Object>> WeakRef<T> {
    /// `pub const empty: Self = .{};`
    pub fn empty() -> Self {
        WeakRef {
            r#ref: glib::WeakRef::new(),
        }
    }

    /// Set the weak reference to the given object. This will not
    /// increase the reference count of the object.
    pub fn set(&mut self, v: Option<&T>) {
        self.r#ref.set(v);
    }

    /// Release this weak reference.
    ///
    /// You MUST call this before the memory holding this struct is freed,
    /// which in practice means from the owner's `dispose`. The target keeps
    /// a pointer to this `GWeakRef` so that it can clear it when the target
    /// is finalized; if this memory is gone by then, the target walks into
    /// whatever now occupies it. That is an invalid read at best, and can
    /// hang: the target takes a lock inside each registered weak ref, and
    /// reused memory with the low bit set is a lock nothing will release.
    ///
    /// `set(null)` also unregisters and remains valid. This exists so the
    /// requirement has a name at the use site rather than looking like an
    /// ordinary assignment.
    pub fn deinit(&mut self) {
        self.r#ref.set(None);
    }

    /// Get a strong reference to the object, or null if the object
    /// has been finalized. This increases the reference count by one.
    pub fn get(&self) -> Option<T> {
        // We can't use `as` because `as` guarantees conversion and
        // that can't be statically guaranteed.
        self.r#ref.upgrade()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weak_ref() {
        // `gtk.TextBuffer.new` needs no display in C, but gtk-rs insists on
        // an initialized GTK, so this runs on the shared GTK test thread.
        crate::apprt::gtk::ext::test_gtk::run(|| {
            let mut r#ref: WeakRef<gtk4::TextBuffer> = WeakRef::empty();
            let obj = gtk4::TextBuffer::new(None);
            r#ref.set(Some(&obj));
            drop(r#ref.get().unwrap()); // The "?" asserts non-null
            drop(obj);
            assert!(r#ref.get().is_none());
        });
    }

    #[test]
    fn weak_ref_plain_object() {
        // The same contract for a plain GObject, which needs no GTK.
        let mut r#ref: WeakRef<glib::Object> = WeakRef::empty();
        let obj = glib::Object::new::<glib::Object>();
        r#ref.set(Some(&obj));
        drop(r#ref.get().unwrap());
        drop(obj);
        assert!(r#ref.get().is_none());
        r#ref.deinit();
    }
}
