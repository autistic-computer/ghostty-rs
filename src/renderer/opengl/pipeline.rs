//! Port of `src/renderer/opengl/Pipeline.zig`.
//!
//! STATUS: ported
//!
//! Wrapper for handling render pipelines.

use std::ffi::CStr;

use crate::pkg::opengl as gl;

/// Options for initializing a render pipeline.
#[derive(Debug, Clone, Copy)]
pub struct Options<'a> {
    /// GLSL source of the vertex function
    pub vertex_fn: &'a CStr,
    /// GLSL source of the fragment function
    pub fragment_fn: &'a CStr,

    /// Vertex step function
    pub step_fn: StepFunction,

    /// Whether to enable blending.
    pub blending_enabled: bool,
}

impl<'a> Options<'a> {
    /// Options with the Zig field defaults (`step_fn = .per_vertex`,
    /// `blending_enabled = true`).
    pub fn new(vertex_fn: &'a CStr, fragment_fn: &'a CStr) -> Self {
        Options {
            vertex_fn,
            fragment_fn,
            step_fn: StepFunction::PerVertex,
            blending_enabled: true,
        }
    }
}

/// Zig: `Options.StepFunction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StepFunction {
    Constant,
    #[default]
    PerVertex,
    PerInstance,
}

/// The scalar component type of a vertex attribute (Zig: the element type
/// that `autoAttribute` switches on, after unwrapping arrays, packed
/// structs (their backing integer) and enums (their tag type)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeType {
    U8,
    U16,
    U32,
    I8,
    I16,
    I32,
    F16,
    F32,
    F64,
}

/// One field of a vertex attribute struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexAttribute {
    /// `@offsetOf(T, field_name)`.
    pub offset: usize,
    /// The number of components (the array length, or 1).
    pub size: usize,
    /// The component type.
    pub typ: AttributeType,
}

/// A type usable as the vertex attributes of a pipeline (Zig: the
/// `comptime VertexAttributes: ?type` parameter, whose fields are
/// reflected with `@typeInfo`). Implementors list one entry per field, in
/// declaration order.
pub trait VertexAttributes: Sized {
    const ATTRIBUTES: &'static [VertexAttribute];
}

/// The reflected layout of a [`VertexAttributes`] type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexLayout {
    /// `@sizeOf(T)`.
    pub size: usize,
    pub attributes: &'static [VertexAttribute],
}

impl VertexLayout {
    pub const fn of<T: VertexAttributes>() -> VertexLayout {
        VertexLayout {
            size: std::mem::size_of::<T>(),
            attributes: T::ATTRIBUTES,
        }
    }
}

/// Errors from [`Pipeline::init`].
pub type Error = gl::errors::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pipeline {
    pub program: gl::Program,

    pub fbo: gl::Framebuffer,

    pub vao: gl::VertexArray,

    pub stride: usize,

    pub blending_enabled: bool,
}

impl Pipeline {
    pub fn init(vertex_attributes: Option<VertexLayout>, opts: Options) -> Result<Pipeline, Error> {
        // Load and compile our shaders.
        let program = gl::Program::create_vf(opts.vertex_fn, opts.fragment_fn)?;
        let pbind = match program.r#use() {
            Ok(b) => b,
            Err(e) => {
                program.destroy();
                return Err(e);
            }
        };

        let fbo = match gl::Framebuffer::create() {
            Ok(f) => f,
            Err(e) => {
                pbind.unbind();
                program.destroy();
                return Err(e);
            }
        };
        let fbobind = match fbo.bind(gl::framebuffer::Target::Framebuffer) {
            Ok(b) => b,
            Err(e) => {
                fbo.destroy();
                pbind.unbind();
                program.destroy();
                return Err(e);
            }
        };

        let r = (|| {
            let vao = gl::VertexArray::create()?;
            let vaobind = match vao.bind() {
                Ok(b) => b,
                Err(e) => {
                    vao.destroy();
                    return Err(e);
                }
            };

            let r = match vertex_attributes {
                Some(va) => auto_attribute(va, vaobind, opts.step_fn),
                None => Ok(()),
            };
            vaobind.unbind();
            if let Err(e) = r {
                vao.destroy();
                return Err(e);
            }
            Ok(vao)
        })();

        fbobind.unbind();
        pbind.unbind();

        let vao = match r {
            Ok(vao) => vao,
            Err(e) => {
                fbo.destroy();
                program.destroy();
                return Err(e);
            }
        };

        Ok(Pipeline {
            program,
            fbo,
            vao,
            stride: vertex_attributes.map_or(0, |va| va.size),
            blending_enabled: opts.blending_enabled,
        })
    }

    pub fn deinit(&self) {
        self.program.destroy();
    }
}

fn auto_attribute(
    layout: VertexLayout,
    vaobind: gl::vertex_array::Binding,
    step_fn: StepFunction,
) -> Result<(), Error> {
    let divisor: gl::c::GLuint = match step_fn {
        StepFunction::PerVertex => 0,
        StepFunction::PerInstance => 1,
        StepFunction::Constant => gl::c::GLuint::MAX,
    };

    for (i, attr) in layout.attributes.iter().enumerate() {
        let i = i as gl::c::GLuint;
        vaobind.enable_attrib_array(i)?;
        vaobind.attribute_binding(i, 0)?;
        vaobind.binding_divisor(i, divisor)?;

        let offset = attr.offset as gl::c::GLuint;
        let size = attr.size as gl::c::GLint;

        match attr.typ {
            AttributeType::U8 => {
                vaobind.attribute_i_format(i, size, gl::c::GL_UNSIGNED_BYTE, offset)
            }
            AttributeType::U16 => {
                vaobind.attribute_i_format(i, size, gl::c::GL_UNSIGNED_SHORT, offset)
            }
            AttributeType::U32 => {
                vaobind.attribute_i_format(i, size, gl::c::GL_UNSIGNED_INT, offset)
            }
            AttributeType::I8 => vaobind.attribute_i_format(i, size, gl::c::GL_BYTE, offset),
            AttributeType::I16 => vaobind.attribute_i_format(i, size, gl::c::GL_SHORT, offset),
            AttributeType::I32 => vaobind.attribute_i_format(i, size, gl::c::GL_INT, offset),
            AttributeType::F16 => {
                vaobind.attribute_format(i, size, gl::c::GL_HALF_FLOAT, false, offset)
            }
            AttributeType::F32 => vaobind.attribute_format(i, size, gl::c::GL_FLOAT, false, offset),
            AttributeType::F64 => vaobind.attribute_l_format(i, size, offset),
        }?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::opengl::device::test_context::TestContext;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Vertex {
        pos: [f32; 2],
        color: [u8; 4],
        flags: u16,
    }

    impl VertexAttributes for Vertex {
        const ATTRIBUTES: &'static [VertexAttribute] = &[
            VertexAttribute {
                offset: std::mem::offset_of!(Vertex, pos),
                size: 2,
                typ: AttributeType::F32,
            },
            VertexAttribute {
                offset: std::mem::offset_of!(Vertex, color),
                size: 4,
                typ: AttributeType::U8,
            },
            VertexAttribute {
                offset: std::mem::offset_of!(Vertex, flags),
                size: 1,
                typ: AttributeType::U16,
            },
        ];
    }

    // Not a Zig test: Pipeline.zig has none.
    #[test]
    fn init_with_attributes() {
        let _ctx = TestContext::new();

        let vs = c"#version 430 core
            layout(location = 0) in vec2 pos;
            layout(location = 1) in uvec4 color;
            layout(location = 2) in uint flags;
            flat out uvec4 c;
            void main() { c = color + flags; gl_Position = vec4(pos, 0.0, 1.0); }";
        let fs = c"#version 430 core
            flat in uvec4 c;
            out vec4 frag;
            void main() { frag = vec4(c) / 255.0; }";

        let mut opts = Options::new(vs, fs);
        opts.step_fn = StepFunction::PerInstance;
        let p = Pipeline::init(Some(VertexLayout::of::<Vertex>()), opts).unwrap();
        assert_eq!(std::mem::size_of::<Vertex>(), p.stride);
        assert!(p.blending_enabled);
        p.deinit();

        let p = Pipeline::init(None, Options::new(vs, fs)).unwrap();
        assert_eq!(0, p.stride);
        p.deinit();

        // Bad shader source fails.
        assert!(Pipeline::init(None, Options::new(c"nope", fs)).is_err());
    }
}
