//! Port of `pkg/opengl/Program.zig`.
//!
//! STATUS: ported

use std::ffi::CStr;

use super::c;
use super::errors::{self, Error};
use super::glad;
use super::shader::{slice_to_nul, Shader};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Program {
    pub id: c::GLuint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Binding;

impl Binding {
    pub fn unbind(self) {
        glad::call!(UseProgram(0));
    }
}

/// A value that can be passed to [`Program::set_uniform`] (Zig: the
/// `switch (@TypeOf(value))` in `setUniform`).
pub trait UniformValue {
    /// Perform the correct call depending on the type of the value.
    fn set(&self, loc: c::GLint);
}

impl UniformValue for bool {
    fn set(&self, loc: c::GLint) {
        glad::call!(Uniform1i(loc, if *self { 1 } else { 0 }));
    }
}

/// Zig: `comptime_int`.
impl UniformValue for i32 {
    fn set(&self, loc: c::GLint) {
        glad::call!(Uniform1i(loc, *self));
    }
}

impl UniformValue for f32 {
    fn set(&self, loc: c::GLint) {
        glad::call!(Uniform1f(loc, *self));
    }
}

/// Zig: `@Vector(2, f32)`.
impl UniformValue for [f32; 2] {
    fn set(&self, loc: c::GLint) {
        glad::call!(Uniform2f(loc, self[0], self[1]));
    }
}

/// Zig: `@Vector(3, f32)`.
impl UniformValue for [f32; 3] {
    fn set(&self, loc: c::GLint) {
        glad::call!(Uniform3f(loc, self[0], self[1], self[2]));
    }
}

/// Zig: `@Vector(4, f32)`.
impl UniformValue for [f32; 4] {
    fn set(&self, loc: c::GLint) {
        glad::call!(Uniform4f(loc, self[0], self[1], self[2], self[3]));
    }
}

/// Zig: `[4]@Vector(4, f32)`.
impl UniformValue for [[f32; 4]; 4] {
    fn set(&self, loc: c::GLint) {
        glad::call!(UniformMatrix4fv(
            loc,
            1,
            c::GL_FALSE as c::GLboolean,
            self.as_ptr() as *const c::GLfloat,
        ));
    }
}

impl Program {
    pub fn create() -> Result<Program, Error> {
        let id = glad::call!(CreateProgram());
        if id == 0 {
            errors::must_error()?;
        }
        log::debug!(target: "opengl", "program created id={id}");
        Ok(Program { id })
    }

    /// Create a program from a vertex and fragment shader source. This will
    /// compile and link the vertex and fragment shader.
    pub fn create_vf(vsrc: &CStr, fsrc: &CStr) -> Result<Program, Error> {
        let vs = Shader::create(c::GL_VERTEX_SHADER)?;
        let vs_res = vs.set_source_and_compile(vsrc);
        if let Err(err) = vs_res {
            vs.destroy();
            return Err(err);
        }

        let fs = match Shader::create(c::GL_FRAGMENT_SHADER) {
            Ok(fs) => fs,
            Err(err) => {
                vs.destroy();
                return Err(err);
            }
        };

        let res = (|| {
            fs.set_source_and_compile(fsrc)?;
            let p = Program::create()?;
            p.attach_shader(vs)?;
            p.attach_shader(fs)?;
            p.link()?;
            Ok(p)
        })();

        // defer vs.destroy(); defer fs.destroy();
        fs.destroy();
        vs.destroy();

        res
    }

    pub fn destroy(self) {
        debug_assert!(self.id != 0);
        glad::call!(DeleteProgram(self.id));
        log::debug!(target: "opengl", "program destroyed id={}", self.id);
    }

    pub fn attach_shader(self, s: Shader) -> Result<(), Error> {
        glad::call!(AttachShader(self.id, s.id));
        errors::get_error()
    }

    pub fn link(self) -> Result<(), Error> {
        glad::call!(LinkProgram(self.id));

        // Check if linking succeeded
        let mut success: c::GLint = 0;
        glad::call!(GetProgramiv(self.id, c::GL_LINK_STATUS, &mut success));
        if success == c::GL_TRUE as c::GLint {
            log::debug!(target: "opengl", "program linked id={}", self.id);
            return Ok(());
        }

        log::error!(
            target: "opengl",
            "program link failure id={} message={}",
            self.id,
            String::from_utf8_lossy(slice_to_nul(&self.get_info_log())),
        );
        Err(Error::CompileFailed)
    }

    pub fn r#use(self) -> Result<Binding, Error> {
        glad::call!(UseProgram(self.id));
        errors::get_error()?;
        Ok(Binding)
    }

    pub fn uniform_block_binding(self, index: c::GLuint, binding: c::GLuint) -> Result<(), Error> {
        glad::call!(UniformBlockBinding(self.id, index, binding));
        errors::get_error()
    }

    /// Requires the program is currently in use.
    pub fn set_uniform<V: UniformValue>(self, n: &CStr, value: V) -> Result<(), Error> {
        let loc = glad::call!(GetUniformLocation(self.id, n.as_ptr()));
        if loc < 0 {
            return Err(Error::UniformNameInvalid);
        }
        errors::get_error()?;

        // Perform the correct call depending on the type of the value.
        value.set(loc);
        errors::get_error()
    }

    /// getInfoLog returns the info log for this program. This attempts to
    /// keep the log fully stack allocated and is therefore limited to a max
    /// amount of elements.
    //
    // NOTE(mitchellh): we can add a dynamic version that uses an allocator
    // if we ever need it.
    pub fn get_info_log(self) -> [u8; 512] {
        let mut msg = [0u8; 512];
        glad::call!(GetProgramInfoLog(
            self.id,
            msg.len() as c::GLsizei,
            std::ptr::null_mut(),
            msg.as_mut_ptr() as *mut c::GLchar,
        ));
        msg
    }
}
