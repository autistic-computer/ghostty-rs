//! Port of `pkg/opengl/VertexArray.zig`.
//!
//! STATUS: ported

use super::c;
use super::errors::{self, Error};
use super::glad;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexArray {
    pub id: c::GLuint,
}

impl VertexArray {
    /// Create a single vertex array object.
    pub fn create() -> Result<VertexArray, Error> {
        let mut vao: c::GLuint = 0;
        glad::call!(GenVertexArrays(1, &mut vao));
        Ok(VertexArray { id: vao })
    }

    /// glBindVertexArray
    pub fn bind(self) -> Result<Binding, Error> {
        glad::call!(BindVertexArray(self.id));
        errors::get_error()?;
        Ok(Binding)
    }

    pub fn destroy(self) {
        glad::call!(DeleteVertexArrays(1, &self.id));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Binding;

impl Binding {
    pub fn unbind(self) {
        glad::call!(BindVertexArray(0));
    }

    pub fn enable_attrib_array(self, idx: c::GLuint) -> Result<(), Error> {
        glad::call!(EnableVertexAttribArray(idx));
        errors::get_error()
    }

    pub fn binding_divisor(self, idx: c::GLuint, divisor: c::GLuint) -> Result<(), Error> {
        glad::call!(VertexBindingDivisor(idx, divisor));
        errors::get_error()
    }

    pub fn attribute_binding(
        self,
        attrib_idx: c::GLuint,
        binding_idx: c::GLuint,
    ) -> Result<(), Error> {
        glad::call!(VertexAttribBinding(attrib_idx, binding_idx));
        errors::get_error()
    }

    pub fn attribute_format(
        self,
        idx: c::GLuint,
        size: c::GLint,
        typ: c::GLenum,
        normalized: bool,
        offset: c::GLuint,
    ) -> Result<(), Error> {
        glad::call!(VertexAttribFormat(
            idx,
            size,
            typ,
            normalized as c::GLboolean,
            offset,
        ));
        errors::get_error()
    }

    pub fn attribute_i_format(
        self,
        idx: c::GLuint,
        size: c::GLint,
        typ: c::GLenum,
        offset: c::GLuint,
    ) -> Result<(), Error> {
        glad::call!(VertexAttribIFormat(idx, size, typ, offset));
        errors::get_error()
    }

    pub fn attribute_l_format(
        self,
        idx: c::GLuint,
        size: c::GLint,
        offset: c::GLuint,
    ) -> Result<(), Error> {
        glad::call!(VertexAttribLFormat(idx, size, c::GL_DOUBLE, offset));
        errors::get_error()
    }

    pub fn bind_vertex_buffer(
        self,
        idx: c::GLuint,
        buffer: c::GLuint,
        offset: c::GLintptr,
        stride: c::GLsizei,
    ) -> Result<(), Error> {
        glad::call!(BindVertexBuffer(idx, buffer, offset, stride));
        errors::get_error()
    }
}
