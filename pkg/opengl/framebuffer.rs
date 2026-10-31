//! Port of `pkg/opengl/Framebuffer.zig`.
//!
//! STATUS: ported

use super::c;
use super::errors::{self, gl_enum, Error};
use super::glad;
use super::renderbuffer::Renderbuffer;
use super::texture::{self, Texture};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Framebuffer {
    pub id: c::GLuint,
}

impl Framebuffer {
    /// Create a single buffer.
    pub fn create() -> Result<Framebuffer, Error> {
        let mut fbo: c::GLuint = 0;
        glad::call!(GenFramebuffers(1, &mut fbo));
        Ok(Framebuffer { id: fbo })
    }

    pub fn destroy(self) {
        glad::call!(DeleteFramebuffers(1, &self.id));
    }

    pub fn bind(self, target: Target) -> Result<Binding, Error> {
        // The default framebuffer is documented as being zero but
        // on multiple OpenGL drivers its not zero, so we grab it
        // at runtime.
        let mut current: c::GLint = 0;
        glad::call!(GetIntegerv(c::GL_FRAMEBUFFER_BINDING, &mut current));
        glad::call!(BindFramebuffer(target.0, self.id));
        Ok(Binding {
            target,
            previous: current as c::GLuint,
        })
    }
}

gl_enum! {
    /// Enum for possible binding targets.
    pub struct Target(u32) {
        Framebuffer = c::GL_FRAMEBUFFER,
        Draw = c::GL_DRAW_FRAMEBUFFER,
        Read = c::GL_READ_FRAMEBUFFER,
    }
}

gl_enum! {
    pub struct Attachment(u32) {
        Color0 = c::GL_COLOR_ATTACHMENT0,
        Depth = c::GL_DEPTH_ATTACHMENT,
        Stencil = c::GL_STENCIL_ATTACHMENT,
        DepthStencil = c::GL_DEPTH_STENCIL_ATTACHMENT,
    }
}

gl_enum! {
    pub struct Status(u32) {
        Complete = c::GL_FRAMEBUFFER_COMPLETE,
        Undefined = c::GL_FRAMEBUFFER_UNDEFINED,
        IncompleteAttachment = c::GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT,
        IncompleteMissingAttachment = c::GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT,
        IncompleteDrawBuffer = c::GL_FRAMEBUFFER_INCOMPLETE_DRAW_BUFFER,
        IncompleteReadBuffer = c::GL_FRAMEBUFFER_INCOMPLETE_READ_BUFFER,
        Unsupported = c::GL_FRAMEBUFFER_UNSUPPORTED,
        IncompleteMultisample = c::GL_FRAMEBUFFER_INCOMPLETE_MULTISAMPLE,
        IncompleteLayerTargets = c::GL_FRAMEBUFFER_INCOMPLETE_LAYER_TARGETS,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub target: Target,
    pub previous: c::GLuint,
}

impl Binding {
    pub fn unbind(self) {
        glad::call!(BindFramebuffer(self.target.0, self.previous));
    }

    pub fn texture_2d(
        self,
        attachment: Attachment,
        textarget: texture::Target,
        texture: Texture,
        level: c::GLint,
    ) -> Result<(), Error> {
        glad::call!(FramebufferTexture2D(
            self.target.0,
            attachment.0,
            textarget as c::GLenum,
            texture.id,
            level,
        ));
        errors::get_error()
    }

    pub fn renderbuffer(self, attachment: Attachment, buffer: Renderbuffer) -> Result<(), Error> {
        glad::call!(FramebufferRenderbuffer(
            self.target.0,
            attachment.0,
            c::GL_RENDERBUFFER,
            buffer.id,
        ));
        errors::get_error()
    }

    pub fn draw_buffers(self, bufs: &[Attachment]) -> Result<(), Error> {
        // Attachment is a transparent GLenum.
        glad::call!(DrawBuffers(
            bufs.len() as c::GLsizei,
            bufs.as_ptr() as *const c::GLenum
        ));
        errors::get_error()
    }

    pub fn check_status(self) -> Status {
        Status(glad::call!(CheckFramebufferStatus(self.target.0)))
    }
}
