//! Port of `pkg/opengl/Renderbuffer.zig`.
//!
//! STATUS: ported

use super::c;
use super::errors::{self, Error};
use super::glad;
use super::texture::InternalFormat;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Renderbuffer {
    pub id: c::GLuint,
}

impl Renderbuffer {
    /// Create a single buffer.
    pub fn create() -> Result<Renderbuffer, Error> {
        let mut rbo: c::GLuint = 0;
        glad::call!(GenRenderbuffers(1, &mut rbo));
        Ok(Renderbuffer { id: rbo })
    }

    pub fn destroy(self) {
        glad::call!(DeleteRenderbuffers(1, &self.id));
    }

    pub fn bind(self) -> Result<Binding, Error> {
        // Keep track of the previous binding so we can restore it in unbind.
        let mut current: c::GLint = 0;
        glad::call!(GetIntegerv(c::GL_RENDERBUFFER_BINDING, &mut current));
        glad::call!(BindRenderbuffer(c::GL_RENDERBUFFER, self.id));
        Ok(Binding {
            previous: current as c::GLuint,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub previous: c::GLuint,
}

impl Binding {
    pub fn unbind(self) {
        glad::call!(BindRenderbuffer(c::GL_RENDERBUFFER, self.previous));
    }

    pub fn storage(
        self,
        format: InternalFormat,
        width: c::GLsizei,
        height: c::GLsizei,
    ) -> Result<(), Error> {
        glad::call!(RenderbufferStorage(
            c::GL_RENDERBUFFER,
            format.0 as c::GLenum,
            width,
            height,
        ));
        errors::get_error()
    }
}
