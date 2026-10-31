//! Port of `pkg/opengl/primitives.zig`.
//!
//! STATUS: ported

use super::c;

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primitive {
    Point = c::GL_POINTS as i32,
    Line = c::GL_LINES as i32,
    LineStrip = c::GL_LINE_STRIP as i32,
    Triangle = c::GL_TRIANGLES as i32,
    TriangleStrip = c::GL_TRIANGLE_STRIP as i32,
    // Commented out primitive types are excluded for parity with Metal.
    //
    // line_loop = c.GL_LINE_LOOP,
    // line_adjacency = c.GL_LINES_ADJACENCY,
    // line_strip_adjacency = c.GL_LINE_STRIP_ADJACENCY,
    // triangle_fan = c.GL_TRIANGLE_FAN,
    // triangle_adjacency = c.GL_TRIANGLES_ADJACENCY,
    // triangle_strip_adjacency = c.GL_TRIANGLE_STRIP_ADJACENCY,
}
