//! Port of `src/macos/graphics/path.zig`.
//!
//! STATUS: ported

use std::ptr;

use super::affine_transform::AffineTransform;
use super::c;
use super::geometry::Rect;
use crate::pkg::macos::{created, opaque_type, Error};

opaque_type!(
    /// `CGPathRef`
    Path
);

impl Path {
    pub fn create_with_rect(
        rect: Rect,
        transform: Option<&AffineTransform>,
    ) -> Result<&'static Path, Error> {
        // SAFETY: AffineTransform is layout-compatible with CGAffineTransform.
        unsafe {
            created(c::CGPathCreateWithRect(
                rect.cval(),
                transform.map_or(ptr::null(), |t| {
                    t as *const AffineTransform as *const c::CGAffineTransform
                }),
            ))
        }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }
}

opaque_type!(
    /// `CGMutablePathRef`
    MutablePath
);

impl MutablePath {
    pub fn create() -> Result<&'static MutablePath, Error> {
        // SAFETY: plain constructor.
        unsafe { created(c::CGPathCreateMutable()) }
    }

    pub fn release(&self) {
        // SAFETY: self is a live object owned by the caller.
        unsafe { c::CFRelease(self.as_ptr()) }
    }

    /// View as an immutable path (`@ptrCast`).
    pub fn as_path(&self) -> &Path {
        // SAFETY: a mutable path is a path.
        unsafe { &*(self as *const Self as *const Path) }
    }

    pub fn add_rect(&self, transform: Option<&AffineTransform>, rect: Rect) {
        // SAFETY: self is live; AffineTransform is layout-compatible.
        unsafe {
            c::CGPathAddRect(
                self.as_ptr(),
                transform.map_or(ptr::null(), |t| {
                    t as *const AffineTransform as *const c::CGAffineTransform
                }),
                rect.cval(),
            )
        }
    }

    pub fn get_bounding_box(&self) -> Rect {
        // SAFETY: self is live.
        Rect::from_c(unsafe { c::CGPathGetBoundingBox(self.as_ptr()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutable_path() {
        let path = MutablePath::create().unwrap();
        path.add_rect(None, Rect::init(0.0, 0.0, 100.0, 200.0));
        assert_eq!(path.get_bounding_box(), Rect::init(0.0, 0.0, 100.0, 200.0));
        path.release();
    }

    #[test]
    fn path_from_rect() {
        let path = Path::create_with_rect(Rect::init(0.0, 0.0, 100.0, 200.0), None).unwrap();
        path.release();
    }
}
