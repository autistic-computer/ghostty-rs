//! Port of `src/apprt/gtk/ext/slice.zig`.
//!
//! STATUS: ported
//!
//! Zig's arena and allocator plumbing is dropped: the strings are owned
//! `CString`s, and the boxed type's copy/free functions are the `Clone`
//! and `Drop` implementations registered by `#[derive(glib::Boxed)]`.

use std::ffi::{CStr, CString};

/// A boxed type that holds a list of string slices.
#[derive(Debug, Clone, PartialEq, Eq, glib::Boxed)]
#[boxed_type(name = "GhosttyStringList", nullable)]
pub struct StringList {
    pub strings: Vec<CString>,
}

impl StringList {
    pub fn create(strings: &[&CStr]) -> Box<StringList> {
        let stored = strings.iter().map(|s| (*s).to_owned()).collect();
        Box::new(StringList { strings: stored })
    }

    pub fn destroy(self: Box<Self>) {
        drop(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glib::prelude::*;
    use glib::translate::IntoGlib;

    #[test]
    fn string_list_create_and_destroy() {
        let input: &[&CStr] = &[c"hello", c"world"];
        let list = StringList::create(input);

        assert_eq!(2, list.strings.len());
        assert_eq!(c"hello", list.strings[0].as_c_str());
        assert_eq!(c"world", list.strings[1].as_c_str());
        list.destroy();
    }

    #[test]
    fn string_list_create_empty_list() {
        let input: &[&CStr] = &[];
        let list = StringList::create(input);

        assert_eq!(0, list.strings.len());
        list.destroy();
    }

    #[test]
    fn string_list_boxed_copy_and_boxed_free() {
        let input: &[&CStr] = &[c"foo", c"bar", c"baz"];
        let original = StringList::create(input);

        // SAFETY: `original` is a valid StringList, the registered boxed
        // type's copy function clones it into a new box that we free below.
        unsafe {
            let copied = glib::gobject_ffi::g_boxed_copy(
                StringList::static_type().into_glib(),
                &*original as *const StringList as *const _,
            ) as *mut StringList;

            let copied_ref: &StringList = &*copied;
            assert_eq!(3, copied_ref.strings.len());
            assert_eq!(c"foo", copied_ref.strings[0].as_c_str());
            assert_eq!(c"bar", copied_ref.strings[1].as_c_str());
            assert_eq!(c"baz", copied_ref.strings[2].as_c_str());

            assert!(original.strings.as_ptr() != copied_ref.strings.as_ptr());

            glib::gobject_ffi::g_boxed_free(StringList::static_type().into_glib(), copied as *mut _);
        }
        original.destroy();
    }
}
