//! Port of `pkg/opengl/Shader.zig`.
//!
//! STATUS: ported

use std::ffi::CStr;

use super::c;
use super::errors::{self, Error};
use super::glad;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shader {
    pub id: c::GLuint,
}

impl Shader {
    pub fn create(typ: c::GLenum) -> Result<Shader, Error> {
        let id = glad::call!(CreateShader(typ));
        if id == 0 {
            errors::must_error()?;
            unreachable!();
        }

        log::debug!(target: "opengl", "shader created id={id}");
        Ok(Shader { id })
    }

    /// Set the source and compile a shader.
    pub fn set_source_and_compile(self, source: &CStr) -> Result<(), Error> {
        let src_ptr: *const c::GLchar = source.as_ptr();
        glad::call!(ShaderSource(self.id, 1, &src_ptr, std::ptr::null()));
        glad::call!(CompileShader(self.id));

        // Check if compilation succeeded
        let mut success: c::GLint = 0;
        glad::call!(GetShaderiv(self.id, c::GL_COMPILE_STATUS, &mut success));
        if success == c::GL_TRUE as c::GLint {
            return Ok(());
        }
        log::error!(
            target: "opengl",
            "shader compilation failure id={} message={}",
            self.id,
            String::from_utf8_lossy(slice_to_nul(&self.get_info_log())),
        );
        Err(Error::CompileFailed)
    }

    /// getInfoLog returns the info log for this shader. This attempts to
    /// keep the log fully stack allocated and is therefore limited to a max
    /// amount of elements.
    //
    // NOTE(mitchellh): we can add a dynamic version that uses an allocator
    // if we ever need it.
    pub fn get_info_log(self) -> [u8; 512] {
        let mut msg = [0u8; 512];
        glad::call!(GetShaderInfoLog(
            self.id,
            msg.len() as c::GLsizei,
            std::ptr::null_mut(),
            msg.as_mut_ptr() as *mut c::GLchar,
        ));
        msg
    }

    pub fn destroy(self) {
        debug_assert!(self.id != 0);
        glad::call!(DeleteShader(self.id));
        log::debug!(target: "opengl", "shader destroyed id={}", self.id);
    }
}

/// `std.mem.sliceTo(buf, 0)`
pub(crate) fn slice_to_nul(buf: &[u8]) -> &[u8] {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    &buf[..end]
}
