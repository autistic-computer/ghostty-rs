//! Port of `src/apprt/gtk/gsettings.zig`.
//!
//! STATUS: ported
//!
//! Zig computes each key's value type at comptime (`Key.Type()`) and makes
//! `get` a compile error for allocating types. The port expresses the same
//! with one marker type per key implementing [`Setting`] (whose associated
//! `Type` is the value type) and the [`NonAllocating`] marker trait that
//! [`get`] requires.
#![cfg(feature = "gtk")]

use std::os::raw::c_int;

use glib::prelude::*;

/// GTK Settings keys with well-defined types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    GtkEnablePrimaryPaste,
    GtkXftDpi,
    GtkFontName,
}

impl Key {
    /// `@tagName(key)`: the GTK property name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Key::GtkEnablePrimaryPaste => "gtk-enable-primary-paste",
            Key::GtkXftDpi => "gtk-xft-dpi",
            Key::GtkFontName => "gtk-font-name",
        }
    }

    /// The GValue type used to read the property (`GValueType` in Zig,
    /// which names the C type: `c_int` for both `bool`/`c_int`,
    /// `?[*:0]const u8` for strings).
    fn g_value_type(self) -> glib::Type {
        match self {
            Key::GtkEnablePrimaryPaste => glib::Type::I32,
            Key::GtkXftDpi => glib::Type::I32,
            Key::GtkFontName => glib::Type::STRING,
        }
    }

    /// Returns true if this setting type requires memory allocation.
    /// Types that do not need allocation must be explicitly marked.
    const fn requires_allocation(self) -> bool {
        match self {
            Key::GtkEnablePrimaryPaste | Key::GtkXftDpi => false,
            Key::GtkFontName => true,
        }
    }
}

/// A key with its value type (`Key.Type()`).
pub trait Setting {
    const KEY: Key;
    type Type;

    /// Converts the GValue read with [`Key::g_value_type`].
    fn from_value(value: &glib::Value) -> Option<Self::Type>;
}

/// Keys whose type does not require allocation (the Zig
/// `requiresAllocation() == false` check).
pub trait NonAllocating: Setting {}

pub struct GtkEnablePrimaryPaste;
pub struct GtkXftDpi;
pub struct GtkFontName;

impl Setting for GtkEnablePrimaryPaste {
    const KEY: Key = Key::GtkEnablePrimaryPaste;
    type Type = bool;
    fn from_value(value: &glib::Value) -> Option<bool> {
        Some(value.get::<i32>().ok()? != 0)
    }
}
impl NonAllocating for GtkEnablePrimaryPaste {}

impl Setting for GtkXftDpi {
    const KEY: Key = Key::GtkXftDpi;
    type Type = c_int;
    fn from_value(value: &glib::Value) -> Option<c_int> {
        value.get::<i32>().ok()
    }
}
impl NonAllocating for GtkXftDpi {}

impl Setting for GtkFontName {
    const KEY: Key = Key::GtkFontName;
    type Type = String;
    fn from_value(value: &glib::Value) -> Option<String> {
        value.get::<Option<String>>().ok().flatten()
    }
}

/// Reads a GTK setting for non-allocating types.
/// Automatically uses XDG Desktop Portal in Flatpak environments.
/// Returns null if the setting is unavailable.
pub fn get<K: NonAllocating>() -> Option<K::Type> {
    debug_assert!(!K::KEY.requires_allocation());
    let settings = gtk4::Settings::default()?;
    get_impl::<K>(&settings)
}

/// Reads a GTK setting, allocating memory if necessary.
/// Automatically uses XDG Desktop Portal in Flatpak environments.
/// Caller must free returned memory with the provided allocator.
/// Returns null if the setting is unavailable.
pub fn get_alloc<K: Setting>() -> Option<K::Type> {
    let settings = gtk4::Settings::default()?;
    get_impl::<K>(&settings)
}

fn get_impl<K: Setting>(settings: &gtk4::Settings) -> Option<K::Type> {
    let mut value = glib::Value::from_type(K::KEY.g_value_type());
    // SAFETY: `value` is an initialized GValue of a type the property
    // transforms into; g_object_get_property fills it in.
    unsafe {
        use glib::translate::*;
        glib::gobject_ffi::g_object_get_property(
            settings.as_ptr() as *mut glib::gobject_ffi::GObject,
            K::KEY.as_str().to_glib_none().0,
            value.to_glib_none_mut().0,
        );
    }

    K::from_value(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::TypeId;

    #[test]
    fn key_type_returns_correct_types() {
        assert_eq!(TypeId::of::<bool>(), TypeId::of::<<GtkEnablePrimaryPaste as Setting>::Type>());
        assert_eq!(TypeId::of::<c_int>(), TypeId::of::<<GtkXftDpi as Setting>::Type>());
        assert_eq!(TypeId::of::<String>(), TypeId::of::<<GtkFontName as Setting>::Type>());
    }

    #[test]
    fn key_requires_allocation_identifies_allocating_types() {
        assert!(!Key::GtkEnablePrimaryPaste.requires_allocation());
        assert!(!Key::GtkXftDpi.requires_allocation());
        assert!(Key::GtkFontName.requires_allocation());
    }

    #[test]
    fn key_g_value_type_returns_correct_gobject_types() {
        assert_eq!(glib::Type::I32, Key::GtkEnablePrimaryPaste.g_value_type());
        assert_eq!(glib::Type::I32, Key::GtkXftDpi.g_value_type());
        assert_eq!(glib::Type::STRING, Key::GtkFontName.g_value_type());
    }

    #[test]
    fn tag_name_returns_correct_gtk_property_names() {
        assert_eq!("gtk-enable-primary-paste", Key::GtkEnablePrimaryPaste.as_str());
        assert_eq!("gtk-xft-dpi", Key::GtkXftDpi.as_str());
        assert_eq!("gtk-font-name", Key::GtkFontName.as_str());
    }

    #[test]
    fn reads_settings_with_a_display() {
        crate::apprt::gtk::ext::test_gtk::run(|| {
            let _: Option<bool> = get::<GtkEnablePrimaryPaste>();
            let _: Option<c_int> = get::<GtkXftDpi>();
            let font = get_alloc::<GtkFontName>();
            assert!(font.is_some());
        });
    }
}
