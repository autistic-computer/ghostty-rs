//! Port of `src/macos/text/font_collection.zig`.
//!
//! STATUS: ported

use std::ffi::c_void;
use std::ptr;

use super::c;
use super::font_descriptor::{attr, FontDescriptor};
use crate::pkg::macos::foundation;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CTFontCollectionRef`
    FontCollection
);

impl FontCollection {
    pub fn create_from_available_fonts() -> Result<&'static FontCollection, Error> {
        // SAFETY: plain constructor.
        unsafe { created(c::CTFontCollectionCreateFromAvailableFonts(ptr::null())) }
    }

    pub fn create_with_font_descriptors(
        descs: &foundation::Array,
    ) -> Result<&'static FontCollection, Error> {
        // SAFETY: descs is live.
        unsafe {
            created(c::CTFontCollectionCreateWithFontDescriptors(
                descs.as_ptr(),
                ptr::null(),
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    pub fn create_matching_font_descriptors(&self) -> &'static foundation::Array {
        // SAFETY: self is live.
        let result = unsafe { c::CTFontCollectionCreateMatchingFontDescriptors(self.as_ptr()) };
        if !result.is_null() {
            // SAFETY: non-null CFArray (+1).
            return unsafe { &*(result as *const foundation::Array) };
        }

        // If we have no results, we create an empty array. This is not
        // exactly matching the Mac API. We can fix this later if we want
        // but I chose this to make it slightly more Zig-like at the cost
        // of some memory in the rare case.
        foundation::Array::create::<c_void>(&[]).expect("empty array allocation failed")
    }
}

/// Log every descriptor in `list` (debugging helper).
#[allow(dead_code)]
fn debug_dump_list(list: &foundation::Array) -> Result<(), Error> {
    for i in 0..list.get_count() {
        // SAFETY: the list holds font descriptors.
        let desc: &FontDescriptor = unsafe { list.get_value_at_index(i) };

        let mut buf = [0u8; 128];
        let name = desc.copy_attribute(attr::Name);
        let cstr = name
            .and_then(|n| n.cstring(&mut buf, foundation::StringEncoding::Utf8))
            .map(|s| std::string::String::from_utf8_lossy(s).into_owned())
            .unwrap_or_default();

        let mut family_buf = [0u8; 128];
        let family = desc.copy_attribute(attr::FamilyName);
        let family_cstr = family
            .and_then(|n| n.cstring(&mut family_buf, foundation::StringEncoding::Utf8))
            .map(|s| std::string::String::from_utf8_lossy(s).into_owned())
            .unwrap_or_default();

        let mut buf2 = [0u8; 128];
        let url = desc.copy_attribute(attr::Url);
        let path = match url.and_then(|u| u.copy_path()) {
            None => "<no path>".to_owned(),
            Some(path) => {
                let blank = foundation::String::create_with_bytes(
                    b"",
                    foundation::StringEncoding::Utf8,
                    false,
                )?;
                let decoded =
                    foundation::URL::create_string_by_replacing_percent_escapes(path, blank)?;
                let s = decoded
                    .cstring(&mut buf2, foundation::StringEncoding::Utf8)
                    .map(|s| std::string::String::from_utf8_lossy(s).into_owned())
                    .unwrap_or_else(|| "<path cannot be converted to string>".to_owned());
                decoded.release();
                blank.release();
                path.release();
                s
            }
        };

        log::warn!("i={i} name={cstr} family={family_cstr} path={path}");

        if let Some(v) = name {
            v.release();
        }
        if let Some(v) = family {
            v.release();
        }
        if let Some(v) = url {
            v.release();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection() {
        let v = FontCollection::create_from_available_fonts().unwrap();
        let list = v.create_matching_font_descriptors();
        assert!(list.get_count() > 0);
        list.release();
        v.release();
    }

    fn collection_for(name: &[u8]) -> usize {
        let name =
            foundation::String::create_with_bytes(name, foundation::StringEncoding::Utf8, false)
                .unwrap();
        let desc = FontDescriptor::create_with_name_and_size(name, 12.0).unwrap();
        let arr = foundation::Array::create(&[desc]).unwrap();
        let v = FontCollection::create_with_font_descriptors(arr).unwrap();
        let list = v.create_matching_font_descriptors();
        let count = list.get_count();
        // debug_dump_list(list).unwrap();
        list.release();
        v.release();
        arr.release();
        desc.release();
        name.release();
        count
    }

    #[test]
    fn from_descriptors() {
        assert!(collection_for(b"AppleColorEmoji") > 0);
    }

    #[test]
    fn from_descriptors_no_match() {
        assert!(collection_for(b"ThisShouldNeverExist") == 0);
    }
}
