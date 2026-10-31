//! Port of `pkg/fontconfig/matrix.zig`.
//!
//! STATUS: ported

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Matrix {
    pub xx: f64,
    pub xy: f64,
    pub yx: f64,
    pub yy: f64,
}

const _: () = assert!(std::mem::size_of::<Matrix>() == std::mem::size_of::<super::c::FcMatrix>());
