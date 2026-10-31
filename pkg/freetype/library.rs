//! Port of `pkg/freetype/Library.zig`.
//!
//! STATUS: ported

use std::ffi::CStr;
use std::io::Write as _;

use super::c;
use super::errors::{int_to_error, Error};
use super::face::Face;

/// A FreeType library handle. This is a thin, copyable wrapper around
/// `FT_Library`, like the Zig struct; it must be released explicitly with
/// [`Library::deinit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Library {
    pub handle: c::FT_Library,
}

impl Library {
    /// Initialize a new FreeType library object. The set of modules that are
    /// registered by this function is determined at build time.
    pub fn init() -> Result<Library, Error> {
        let mut res = Library {
            handle: std::ptr::null_mut(),
        };
        // SAFETY: valid out pointer.
        int_to_error(unsafe { c::FT_Init_FreeType(&mut res.handle) })?;
        Ok(res)
    }

    /// Destroy a given FreeType library object and all of its children,
    /// including resources, drivers, faces, sizes, etc.
    ///
    /// # Safety
    /// The library (and every copy of this handle and every face created
    /// from it) must not be used afterwards.
    pub unsafe fn deinit(self) {
        c::FT_Done_FreeType(self.handle);
    }

    /// Return the version of the FreeType library being used. This is useful when
    /// dynamically linking to the library, since one cannot use the macros
    /// FREETYPE_MAJOR, FREETYPE_MINOR, and FREETYPE_PATCH.
    pub fn version(self) -> Version {
        let mut v = Version {
            major: 0,
            minor: 0,
            patch: 0,
        };
        // SAFETY: valid handle and out pointers.
        unsafe {
            c::FT_Library_Version(self.handle, &mut v.major, &mut v.minor, &mut v.patch);
        }
        v
    }

    /// Call FT_New_Face to open a font from a file.
    pub fn init_face(self, path: &CStr, index: i32) -> Result<Face, Error> {
        let mut face = Face {
            handle: std::ptr::null_mut(),
        };
        // SAFETY: valid handle, NUL-terminated path and out pointer.
        int_to_error(unsafe {
            c::FT_New_Face(
                self.handle,
                path.as_ptr(),
                index as c::FT_Long,
                &mut face.handle,
            )
        })?;
        Ok(face)
    }

    /// Call FT_Open_Face to open a font that has been loaded into memory.
    ///
    /// # Safety
    /// FreeType does not copy `data`: it must stay alive (and unmodified)
    /// for as long as the returned face is in use.
    pub unsafe fn init_memory_face(self, data: &[u8], index: i32) -> Result<Face, Error> {
        let mut face = Face {
            handle: std::ptr::null_mut(),
        };
        int_to_error(c::FT_New_Memory_Face(
            self.handle,
            data.as_ptr(),
            data.len() as c::FT_Long,
            index as c::FT_Long,
            &mut face.handle,
        ))?;
        Ok(face)
    }

    /// Call when you're done with a loaded MM var.
    ///
    /// # Safety
    /// `mm` must have been returned by [`Face::get_mm_var`] for a face of
    /// this library and must not be used afterwards.
    pub unsafe fn done_mm_var(self, mm: *mut c::FT_MM_Var) {
        c::FT_Done_MM_Var(self.handle, mm);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: i32,
    pub minor: i32,
    pub patch: i32,
}

/// The buffer passed to [`Version::to_string`] was too small.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoSpaceLeft;

impl std::fmt::Display for NoSpaceLeft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NoSpaceLeft")
    }
}

impl std::error::Error for NoSpaceLeft {}

impl Version {
    /// Convert the version to a string. The buffer should be able to
    /// accommodate the size, recommended to be at least 8 chars wide.
    /// The returned slice will be a slice of buf that contains the full
    /// version string.
    pub fn to_string(self, buf: &mut [u8]) -> Result<&[u8], NoSpaceLeft> {
        let total = buf.len();
        let mut cursor: &mut [u8] = &mut *buf;
        write!(cursor, "{}.{}.{}", self.major, self.minor, self.patch).map_err(|_| NoSpaceLeft)?;
        let len = total - cursor.len();
        Ok(&buf[..len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let lib = Library::init().unwrap();
        let vsn = lib.version();
        assert!(vsn.major > 1);

        let mut buf = [0u8; 32];
        vsn.to_string(&mut buf).unwrap();

        // SAFETY: not used afterwards.
        unsafe { lib.deinit() };
    }
}
