//! Port of `pkg/harfbuzz/freetype.zig`.
//!
//! STATUS: ported

use crate::pkg::freetype;

use super::c;
use super::errors::Error;
use super::face::Face;
use super::font::Font;

// The Zig version declares its own extern functions so that the proper
// freetype structs are used without a ptrcast. The generated bindings in
// `c` already use `freetype::c::FT_Face`, so they are used directly.

/// Creates an hb_face_t face object from the specified FT_Face.
///
/// This is the preferred variant of the hb_ft_face_create* function
/// family, because it calls FT_Reference_Face() on ft_face , ensuring
/// that ft_face remains alive as long as the resulting hb_face_t face
/// object remains alive. Also calls FT_Done_Face() when the hb_face_t
/// face object is destroyed.
///
/// Use this version unless you know you have good reasons not to.
///
/// # Safety
/// `face` must be a valid `FT_Face`.
pub unsafe fn create_face(face: freetype::c::FT_Face) -> Result<Face, Error> {
    let handle = c::hb_ft_face_create_referenced(face);
    if handle.is_null() {
        return Err(Error::HarfbuzzFailed);
    }
    Ok(Face { handle })
}

/// Creates an hb_font_t font object from the specified FT_Face.
///
/// # Safety
/// `face` must be a valid `FT_Face`.
pub unsafe fn create_font(face: freetype::c::FT_Face) -> Result<Font, Error> {
    let handle = c::hb_ft_font_create_referenced(face);
    if handle.is_null() {
        return Err(Error::HarfbuzzFailed);
    }
    Ok(Font { handle })
}

/// Configures the font-functions structure of the specified hb_font_t font
/// object to use FreeType font functions.
///
/// In particular, you can use this function to configure an existing
/// hb_face_t face object for use with FreeType font functions even if that
/// hb_face_t face object was initially created with hb_face_create(), and
/// therefore was not initially configured to use FreeType font functions.
///
/// An hb_face_t face object created with hb_ft_face_create() is preconfigured
/// for FreeType font functions and does not require this function to be used.
pub fn set_font_funcs(font: Font) {
    // SAFETY: valid font handle.
    unsafe { c::hb_ft_font_set_funcs(font.handle) }
}

/// Returns the FT_Face of a font created with [`create_font`]
/// (Zig: the `hb_ft_font_get_face` extern declaration).
///
/// # Safety
/// `font` must have been created from an FT_Face.
pub unsafe fn font_get_face(font: Font) -> freetype::c::FT_Face {
    c::hb_ft_font_get_face(font.handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freetype_font() {
        let test_font = freetype::testing::FONT_REGULAR;
        let ftc = freetype::c::FT_Err_Ok as freetype::c::FT_Error;

        // SAFETY: plain FreeType C API usage with valid out pointers; the
        // font data is 'static.
        unsafe {
            let mut ft_lib: freetype::c::FT_Library = std::ptr::null_mut();
            assert_eq!(freetype::c::FT_Init_FreeType(&mut ft_lib), ftc);

            let mut ft_face: freetype::c::FT_Face = std::ptr::null_mut();
            assert!(
                freetype::c::FT_New_Memory_Face(
                    ft_lib,
                    test_font.as_ptr(),
                    test_font.len() as freetype::c::FT_Long,
                    0,
                    &mut ft_face,
                ) == ftc
            );

            let mut face = create_face(ft_face).unwrap();
            let mut font = create_font(ft_face).unwrap();
            set_font_funcs(font);

            font.destroy();
            face.destroy();
            freetype::c::FT_Done_Face(ft_face);
            freetype::c::FT_Done_FreeType(ft_lib);
        }
    }
}
