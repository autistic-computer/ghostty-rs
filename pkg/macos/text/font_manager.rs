//! Port of `src/macos/text/font_manager.zig`.
//!
//! STATUS: ported

use super::c;
use super::font_descriptor::FontDescriptor;
use crate::pkg::macos::foundation;
use crate::pkg::macos::maybe;

pub fn create_font_descriptors_from_url(
    url: &foundation::URL,
) -> Option<&'static foundation::Array> {
    // SAFETY: url is live; the result is +1 or null.
    unsafe { maybe(c::CTFontManagerCreateFontDescriptorsFromURL(url.as_ptr())) }
}

pub fn create_font_descriptors_from_data(
    data: &foundation::Data,
) -> Option<&'static foundation::Array> {
    // SAFETY: data is live; the result is +1 or null.
    unsafe { maybe(c::CTFontManagerCreateFontDescriptorsFromData(data.as_ptr())) }
}

pub fn create_font_descriptor_from_data(
    data: &foundation::Data,
) -> Option<&'static FontDescriptor> {
    // SAFETY: data is live; the result is +1 or null.
    unsafe { maybe(c::CTFontManagerCreateFontDescriptorFromData(data.as_ptr())) }
}
