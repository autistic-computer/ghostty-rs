//! Port of `pkg/opengl/Buffer.zig`.
//!
//! STATUS: ported

use std::os::raw::c_void;

use super::c;
use super::errors::{self, gl_enum, Error};
use super::glad;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buffer {
    pub id: c::GLuint,
}

impl Buffer {
    /// Create a single buffer.
    pub fn create() -> Result<Buffer, Error> {
        let mut vbo: c::GLuint = 0;
        glad::call!(GenBuffers(1, &mut vbo));
        Ok(Buffer { id: vbo })
    }

    /// glBindBuffer
    pub fn bind(self, target: Target) -> Result<Binding, Error> {
        glad::call!(BindBuffer(target.0, self.id));
        Ok(Binding {
            id: self.id,
            target,
        })
    }

    pub fn destroy(self) {
        glad::call!(DeleteBuffers(1, &self.id));
    }

    pub fn bind_base(self, target: Target, idx: c::GLuint) -> Result<(), Error> {
        glad::call!(BindBufferBase(target.0, idx, self.id));
        errors::get_error()
    }
}

/// Binding is a bound buffer. By using this for functions that operate
/// on bound buffers, you can easily defer unbinding and in safety-enabled
/// modes verify that unbound buffers are never accessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub id: c::GLuint,
    pub target: Target,
}

/// An array type usable with [`Binding::attribute`] (Zig: the comptime
/// `@typeInfo(T).Array` switch, which only supports arrays of `f32`).
pub trait AttributeArray {
    /// Type of the each component in the array.
    const TYP: c::GLenum;
    /// The size of each component used in calculating the offset.
    const COMPONENT_SIZE: usize;
}

impl<const N: usize> AttributeArray for [f32; N] {
    const TYP: c::GLenum = c::GL_FLOAT;
    const COMPONENT_SIZE: usize = std::mem::size_of::<f32>();
}

impl Binding {
    pub fn unbind(self) {
        glad::call!(BindBuffer(self.target.0, 0));
    }

    /// Sets the data of this bound buffer. The data can be any array-like
    /// type (a single value or a slice). The size of the data is
    /// automatically determined based on the type.
    pub fn set_data<T: ?Sized>(self, data: &T, usage: Usage) -> Result<(), Error> {
        let (size, ptr) = data_info(data);
        glad::call!(BufferData(self.target.0, size, ptr, usage.0));
        errors::get_error()
    }

    /// Sets the data of this bound buffer. The data can be any array-like
    /// type. The size of the data is automatically determined based on the type.
    pub fn set_sub_data<T: ?Sized>(self, offset: usize, data: &T) -> Result<(), Error> {
        let (size, ptr) = data_info(data);
        glad::call!(BufferSubData(
            self.target.0,
            offset as c::GLintptr,
            size,
            ptr
        ));
        errors::get_error()
    }

    /// Sets the buffer data with a null buffer that is expected to be
    /// filled in the future using subData. This requires the type just so
    /// we can setup the data size.
    pub fn set_data_null<T>(self, usage: Usage) -> Result<(), Error> {
        glad::call!(BufferData(
            self.target.0,
            std::mem::size_of::<T>() as c::GLsizeiptr,
            std::ptr::null(),
            usage.0,
        ));
        errors::get_error()
    }

    /// Same as setDataNull but lets you manually specify the buffer size.
    pub fn set_data_null_manual(self, size: usize, usage: Usage) -> Result<(), Error> {
        glad::call!(BufferData(
            self.target.0,
            size as c::GLsizeiptr,
            std::ptr::null(),
            usage.0,
        ));
        errors::get_error()
    }

    /// Shorthand for vertexAttribPointer that is specialized towards the
    /// common use case of specifying an array of homogeneous types that
    /// don't need normalization. This also enables the attribute at idx.
    pub fn attribute<T: AttributeArray>(
        self,
        idx: c::GLuint,
        size: c::GLint,
        offset: usize,
    ) -> Result<(), Error> {
        // The byte offset between each full set of attributes.
        let stride = std::mem::size_of::<T>() as c::GLsizei;

        self.attribute_advanced(idx, size, T::TYP, false, stride, offset * T::COMPONENT_SIZE)?;

        // Zig calls `b.enableAttribArray(idx)`, which only exists on
        // `VertexArray.Binding` (the Zig function is never instantiated);
        // this is what it means.
        glad::call!(EnableVertexAttribArray(idx));
        errors::get_error()
    }

    /// VertexAttribDivisor
    pub fn attribute_divisor(self, idx: c::GLuint, divisor: c::GLuint) -> Result<(), Error> {
        glad::call!(VertexAttribDivisor(idx, divisor));
        errors::get_error()
    }

    pub fn attribute_advanced(
        self,
        idx: c::GLuint,
        size: c::GLint,
        typ: c::GLenum,
        normalized: bool,
        stride: c::GLsizei,
        offset: usize,
    ) -> Result<(), Error> {
        let normalized_c: c::GLboolean = if normalized { c::GL_TRUE } else { c::GL_FALSE } as _;
        let offset_ptr: *const c_void = if offset > 0 {
            offset as *const c_void
        } else {
            std::ptr::null()
        };

        glad::call!(VertexAttribPointer(
            idx,
            size,
            typ,
            normalized_c,
            stride,
            offset_ptr
        ));
        errors::get_error()
    }

    pub fn attribute_i_advanced(
        self,
        idx: c::GLuint,
        size: c::GLint,
        typ: c::GLenum,
        stride: c::GLsizei,
        offset: usize,
    ) -> Result<(), Error> {
        let offset_ptr: *const c_void = if offset > 0 {
            offset as *const c_void
        } else {
            std::ptr::null()
        };

        glad::call!(VertexAttribIPointer(idx, size, typ, stride, offset_ptr));
        errors::get_error()
    }
}

/// Zig: `dataInfo` — the byte size of and pointer to a single value or a
/// slice.
fn data_info<T: ?Sized>(data: &T) -> (c::GLsizeiptr, *const c_void) {
    (
        std::mem::size_of_val(data) as c::GLsizeiptr,
        data as *const T as *const c_void,
    )
}

gl_enum! {
    /// Enum for possible binding targets.
    pub struct Target(u32) {
        Array = c::GL_ARRAY_BUFFER,
        ElementArray = c::GL_ELEMENT_ARRAY_BUFFER,
        Uniform = c::GL_UNIFORM_BUFFER,
        Storage = c::GL_SHADER_STORAGE_BUFFER,
    }
}

gl_enum! {
    /// Enum for possible buffer usages.
    pub struct Usage(u32) {
        StreamDraw = c::GL_STREAM_DRAW,
        StreamRead = c::GL_STREAM_READ,
        StreamCopy = c::GL_STREAM_COPY,
        StaticDraw = c::GL_STATIC_DRAW,
        StaticRead = c::GL_STATIC_READ,
        StaticCopy = c::GL_STATIC_COPY,
        DynamicDraw = c::GL_DYNAMIC_DRAW,
        DynamicRead = c::GL_DYNAMIC_READ,
        DynamicCopy = c::GL_DYNAMIC_COPY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_info_sizes() {
        let one: [f32; 4] = [0.0; 4];
        assert_eq!(16, data_info(&one).0);
        let many: Vec<u32> = vec![0; 10];
        assert_eq!(40, data_info(many.as_slice()).0);
        assert_eq!(c::GL_ARRAY_BUFFER, Target::Array.0);
    }
}
