//! Port of `pkg/harfbuzz/coretext.zig`.
//!
//! STATUS: ported (macOS only; not compiled or tested on Linux)

#![cfg(target_os = "macos")]

use super::c;
use super::errors::Error;
use super::font::Font;
use crate::pkg::macos;

// Use custom extern functions so that the proper CoreText structs are used
// without a ptrcast.
#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn hb_coretext_font_create(ct_face: *const macos::text::Font) -> *mut c::hb_font_t;
}

/// Creates an hb_font_t font object from the specified CTFontRef.
pub fn create_font(face: &macos::text::Font) -> Result<Font, Error> {
    // SAFETY: face is a live CTFontRef.
    let handle = unsafe { hb_coretext_font_create(face) };
    if handle.is_null() {
        return Err(Error::HarfbuzzFailed);
    }
    Ok(Font { handle })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coretext_font() {
        let name =
            macos::foundation::String::create_with_bytes(b"Monaco", macos::foundation::StringEncoding::Utf8, false)
                .unwrap();
        let desc = macos::text::FontDescriptor::create_with_name_and_size(name, 12.0).unwrap();
        let font = macos::text::Font::create_with_font_descriptor(desc, 12.0).unwrap();

        let mut hb_font = create_font(font).unwrap();
        // SAFETY: the font is not used afterwards.
        unsafe { hb_font.destroy() };

        font.release();
        desc.release();
        name.release();
    }
}
