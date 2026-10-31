//! Port of `src/math.zig`.
//!
//! STATUS: ported

/// Matrix type
pub type Mat = [F32x4; 4];

/// A 4-wide f32 vector. Zig uses `@Vector(4, f32)`; portable SIMD is not
/// stable in Rust so this is a plain array with the same size and layout.
pub type F32x4 = [f32; 4];

/// 2D orthographic projection matrix
pub fn ortho2d(left: f32, right: f32, bottom: f32, top: f32) -> Mat {
    let w = right - left;
    let h = top - bottom;
    [
        [2.0 / w, 0.0, 0.0, 0.0],
        [0.0, 2.0 / h, 0.0, 0.0],
        [0.0, 0.0, -1.0, 0.0],
        [-(right + left) / w, -(top + bottom) / h, 0.0, 1.0],
    ]
}
