//! Port of `src/macos/graphics/geometry.zig`.
//!
//! STATUS: ported

use super::c;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: c::CGFloat,
    pub y: c::CGFloat,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub fn init(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            origin: Point { x, y },
            size: Size { width, height },
        }
    }

    pub fn cval(self) -> c::CGRect {
        c::CGRect {
            origin: c::CGPoint {
                x: self.origin.x,
                y: self.origin.y,
            },
            size: c::CGSize {
                width: self.size.width,
                height: self.size.height,
            },
        }
    }

    pub fn from_c(value: c::CGRect) -> Rect {
        Rect {
            origin: Point {
                x: value.origin.x,
                y: value.origin.y,
            },
            size: Size {
                width: value.size.width,
                height: value.size.height,
            },
        }
    }

    pub fn is_null(self) -> bool {
        // SAFETY: pure function.
        unsafe { c::CGRectIsNull(self.cval()) }
    }

    pub fn get_height(self) -> c::CGFloat {
        // SAFETY: pure function.
        unsafe { c::CGRectGetHeight(self.cval()) }
    }

    pub fn get_width(self) -> c::CGFloat {
        // SAFETY: pure function.
        unsafe { c::CGRectGetWidth(self.cval()) }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub width: c::CGFloat,
    pub height: c::CGFloat,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect() {
        let r = Rect::init(1.0, 2.0, 3.0, 4.0);
        assert_eq!(Rect::from_c(r.cval()), r);
        assert_eq!(r.get_width(), 3.0);
        assert_eq!(r.get_height(), 4.0);
        assert!(!r.is_null());
    }
}
