//! Port of `src/renderer/opengl/RenderPass.zig`.
//!
//! STATUS: ported
//!
//! Wrapper for handling render passes.

use crate::pkg::opengl as gl;

use super::pipeline::Pipeline;
use super::sampler::Sampler;
use super::target::Target;
use super::texture::Texture;

/// Options for beginning a render pass.
#[derive(Debug, Clone, Copy)]
pub struct Options<'a> {
    /// Color attachments for this render pass.
    pub attachments: &'a [Attachment],
}

/// Describes a color attachment (Zig: `Options.Attachment`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Attachment {
    pub target: AttachmentTarget,
    pub clear_color: Option<[f32; 4]>,
}

/// Zig: the anonymous `union(enum) { texture, target }` of
/// `Options.Attachment.target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentTarget {
    Texture(Texture),
    Target(Target),
}

/// Describes a step in a render pass.
#[derive(Debug, Clone, Copy)]
pub struct Step<'a> {
    pub pipeline: Pipeline,
    pub uniforms: Option<gl::Buffer>,
    pub buffers: &'a [Option<gl::Buffer>],
    pub textures: &'a [Option<Texture>],
    pub samplers: &'a [Option<Sampler>],
    pub draw: Draw,
}

/// Describes the draw call for this step (Zig: `Step.Draw`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Draw {
    pub r#type: gl::Primitive,
    pub vertex_count: usize,
    /// Zig default: 1.
    pub instance_count: usize,
}

#[derive(Debug)]
pub struct RenderPass<'a> {
    pub attachments: &'a [Attachment],

    pub step_number: usize,
}

impl<'a> RenderPass<'a> {
    /// Begin a render pass.
    pub fn begin(opts: Options<'a>) -> Self {
        RenderPass {
            attachments: opts.attachments,
            step_number: 0,
        }
    }

    /// Add a step to this render pass.
    ///
    /// TODO: Errors are silently ignored in this function, maybe they shouldn't be?
    pub fn step(&mut self, s: Step) {
        if s.draw.instance_count == 0 {
            return;
        }

        let Ok(pbind) = s.pipeline.program.r#use() else {
            return;
        };

        let Ok(vaobind) = s.pipeline.vao.bind() else {
            pbind.unbind();
            return;
        };

        let fbobind = match self.attachments[0].target {
            AttachmentTarget::Target(t) => t
                .framebuffer
                .bind(gl::framebuffer::Target::Framebuffer)
                .ok(),
            AttachmentTarget::Texture(t) => {
                match s.pipeline.fbo.bind(gl::framebuffer::Target::Framebuffer) {
                    Ok(fbobind) => {
                        match fbobind.texture_2d(
                            gl::framebuffer::Attachment::Color0,
                            t.target,
                            t.texture,
                            0,
                        ) {
                            Ok(()) => Some(fbobind),
                            Err(_) => {
                                fbobind.unbind();
                                None
                            }
                        }
                    }
                    Err(_) => None,
                }
            }
        };
        let Some(fbobind) = fbobind else {
            vaobind.unbind();
            pbind.unbind();
            return;
        };

        // Errors end the step early (Zig: `catch return`); the deferred
        // cleanup below runs either way.
        let _ = self.encode(&s, vaobind);

        // defer self.step_number += 1;
        self.step_number += 1;
        fbobind.unbind();
        vaobind.unbind();
        pbind.unbind();
    }

    /// The body of [`RenderPass::step`] after the program, vertex array
    /// and framebuffer are bound.
    fn encode(
        &self,
        s: &Step,
        vaobind: gl::vertex_array::Binding,
    ) -> Result<(), gl::errors::Error> {
        // If we have a clear color and this is the
        // first step in the pass, go ahead and clear.
        if self.step_number == 0 {
            if let Some(c) = self.attachments[0].clear_color {
                gl::clear_color(c[0], c[1], c[2], c[3]);
                gl::clear(gl::c::GL_COLOR_BUFFER_BIT);
            }
        }

        // Bind the uniform buffer we bind at index 1 to align with Metal.
        if let Some(ubo) = s.uniforms {
            ubo.bind_base(gl::buffer::Target::Uniform, 1)?;
        }

        // Bind relevant texture units.
        for (i, t) in s.textures.iter().enumerate() {
            if let Some(tex) = t {
                gl::Texture::active(i as _)?;
                let _ = tex.texture.bind(tex.target)?;
            }
        }

        // Bind relevant samplers.
        for (i, s_) in s.samplers.iter().enumerate() {
            if let Some(sampler) = s_ {
                sampler.sampler.bind(i as _)?;
            }
        }

        // Bind 0th buffer as the vertex buffer,
        // and bind the rest as storage buffers.
        if !s.buffers.is_empty() {
            if let Some(vbo) = s.buffers[0] {
                vaobind.bind_vertex_buffer(0, vbo.id, 0, s.pipeline.stride as _)?;
            }

            for (i, b) in s.buffers.iter().enumerate().skip(1) {
                if let Some(buf) = b {
                    buf.bind_base(gl::buffer::Target::Storage, i as _)?;
                }
            }
        }

        if s.pipeline.blending_enabled {
            gl::enable(gl::c::GL_BLEND)?;
            gl::blend_func(gl::c::GL_ONE, gl::c::GL_ONE_MINUS_SRC_ALPHA)?;
        } else {
            gl::disable(gl::c::GL_BLEND)?;
        }

        gl::draw_arrays_instanced(
            s.draw.r#type,
            0,
            s.draw.vertex_count as _,
            s.draw.instance_count as _,
        )
    }

    /// Complete this render pass.
    /// This struct can no longer be used after calling this.
    pub fn complete(&self) {
        gl::flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::opengl::device::test_context::TestContext;
    use crate::renderer::opengl::pipeline;
    use crate::renderer::opengl::target;

    // Not a Zig test: RenderPass.zig has none.
    #[test]
    fn clear_and_draw() {
        let _ctx = TestContext::new();
        let mut t = Target::init(target::Options {
            width: 2,
            height: 2,
        })
        .unwrap();
        gl::viewport(0, 0, 2, 2).unwrap();

        // Draws a full screen triangle, with the given color only in the
        // left half.
        let p = Pipeline::init(
            None,
            pipeline::Options {
                blending_enabled: false,
                ..pipeline::Options::new(
                    c"#version 430 core
                    void main() {
                        vec2 pos = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
                        gl_Position = vec4(pos * 2.0 - 1.0, 0.0, 1.0);
                    }",
                    c"#version 430 core
                    out vec4 frag;
                    void main() {
                        if (gl_FragCoord.x > 1.0) discard;
                        frag = vec4(0.0, 0.0, 1.0, 1.0);
                    }",
                )
            },
        )
        .unwrap();

        let attachments = [Attachment {
            target: AttachmentTarget::Target(t),
            clear_color: Some([1.0, 0.0, 0.0, 1.0]),
        }];
        let mut pass = RenderPass::begin(Options {
            attachments: &attachments,
        });
        let step = Step {
            pipeline: p,
            uniforms: None,
            buffers: &[],
            textures: &[],
            samplers: &[],
            draw: Draw {
                r#type: gl::Primitive::Triangle,
                vertex_count: 3,
                instance_count: 1,
            },
        };
        // Zero instances is a no-op.
        pass.step(Step {
            draw: Draw {
                instance_count: 0,
                ..step.draw
            },
            ..step
        });
        assert_eq!(0, pass.step_number);
        pass.step(step);
        assert_eq!(1, pass.step_number);
        pass.complete();
        gl::finish();

        let pixels = t.read_pixels_alloc().unwrap();
        // Row-major from the bottom left: the left column was drawn
        // blue, the right column keeps the red clear color.
        assert_eq!(&[0, 0, 255, 255], &pixels[0..4]);
        assert_eq!(&[255, 0, 0, 255], &pixels[4..8]);
        assert_eq!(&[0, 0, 255, 255], &pixels[8..12]);
        assert_eq!(&[255, 0, 0, 255], &pixels[12..16]);

        p.deinit();
        t.deinit();
    }
}
