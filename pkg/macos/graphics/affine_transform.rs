//! Port of `src/macos/graphics/affine_transform.zig`.
//!
//! STATUS: ported

use super::c;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AffineTransform {
    pub a: c::CGFloat,
    pub b: c::CGFloat,
    pub c: c::CGFloat,
    pub d: c::CGFloat,
    pub tx: c::CGFloat,
    pub ty: c::CGFloat,
}

impl AffineTransform {
    pub fn identity() -> AffineTransform {
        // SAFETY: reading a framework constant.
        Self::from_c(unsafe { c::CGAffineTransformIdentity })
    }

    pub fn cval(self) -> c::CGAffineTransform {
        c::CGAffineTransform {
            a: self.a,
            b: self.b,
            c: self.c,
            d: self.d,
            tx: self.tx,
            ty: self.ty,
        }
    }

    fn from_c(value: c::CGAffineTransform) -> AffineTransform {
        AffineTransform {
            a: value.a,
            b: value.b,
            c: value.c,
            d: value.d,
            tx: value.tx,
            ty: value.ty,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity() {
        let t = AffineTransform::identity();
        assert_eq!(
            (t.a, t.b, t.c, t.d, t.tx, t.ty),
            (1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
        );
    }
}
