//! Port of `src/renderer/metal/RenderPass.zig`.
//!
//! STATUS: ported (macOS only; type-checked, not run)
//!
//! Wrapper for handling render passes.

use std::ffi::c_ulong;

use super::api as mtl;
use super::objc;
use super::{Pipeline, Sampler, Target, Texture};

/// Options for beginning a render pass.
#[derive(Debug, Clone, Copy)]
pub struct Options<'a> {
    /// MTLCommandBuffer
    pub command_buffer: objc::Object,
    /// Color attachments for this render pass.
    pub attachments: &'a [Attachment],
}

/// Describes a color attachment (Zig: `Options.Attachment`).
#[derive(Debug, Clone, Copy)]
pub struct Attachment {
    pub target: AttachmentTarget,
    pub clear_color: Option<[f64; 4]>,
}

/// Zig: the anonymous `union(enum) { texture, target }`.
#[derive(Debug, Clone, Copy)]
pub enum AttachmentTarget {
    Texture(Texture),
    Target(Target),
}

/// Describes a step in a render pass.
#[derive(Debug, Clone, Copy)]
pub struct Step<'a> {
    pub pipeline: Pipeline,
    /// MTLBuffer
    pub uniforms: Option<objc::Object>,
    /// MTLBuffer
    pub buffers: &'a [Option<objc::Object>],
    pub textures: &'a [Option<Texture>],
    /// Set of samplers to use for this step. The index maps to an index
    /// of a fragment texture, set via setFragmentSamplerState(_:index:).
    pub samplers: &'a [Option<Sampler>],
    pub draw: Draw,
}

/// Describes the draw call for this step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Draw {
    pub r#type: mtl::MTLPrimitiveType,
    pub vertex_count: usize,
    pub instance_count: usize,
}

impl Draw {
    /// A draw with the Zig field default `instance_count = 1`.
    pub const fn new(r#type: mtl::MTLPrimitiveType, vertex_count: usize) -> Draw {
        Draw {
            r#type,
            vertex_count,
            instance_count: 1,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RenderPass {
    /// MTLRenderCommandEncoder
    pub encoder: objc::Object,
}

impl RenderPass {
    /// Begin a render pass.
    pub fn begin(opts: Options<'_>) -> RenderPass {
        // SAFETY: standard Metal calls on valid objects.
        unsafe {
            // Create a pass descriptor
            let desc = {
                let class = objc::get_class(c"MTLRenderPassDescriptor").expect("MTLRenderPassDescriptor");
                let desc: objc::Object = class.msg_send(objc::sel(c"renderPassDescriptor"), ());

                // Set our color attachment to be our drawable surface.
                let attachments = objc::Object::from_id(desc.get_property(c"colorAttachments"));
                for (i, at) in opts.attachments.iter().enumerate() {
                    let attachment: objc::Object =
                        attachments.msg_send(objc::sel(c"objectAtIndexedSubscript:"), (i as c_ulong,));

                    attachment.set_property(
                        c"loadAction",
                        if at.clear_color.is_some() {
                            mtl::MTLLoadAction::Clear
                        } else {
                            mtl::MTLLoadAction::Load
                        } as c_ulong,
                    );
                    attachment.set_property(c"storeAction", mtl::MTLStoreAction::Store as c_ulong);
                    attachment.set_property(
                        c"texture",
                        match at.target {
                            AttachmentTarget::Texture(t) => t.texture.value,
                            AttachmentTarget::Target(t) => t.texture.value,
                        },
                    );
                    if let Some(c) = at.clear_color {
                        attachment.set_property(
                            c"clearColor",
                            mtl::MTLClearColor {
                                red: c[0],
                                green: c[1],
                                blue: c[2],
                                alpha: c[3],
                            },
                        );
                    }
                }

                desc
            };

            // MTLRenderCommandEncoder
            let encoder: objc::Object = opts
                .command_buffer
                .msg_send(objc::sel(c"renderCommandEncoderWithDescriptor:"), (desc.value,));

            RenderPass { encoder }
        }
    }

    /// Add a step to this render pass.
    pub fn step(&self, s: Step<'_>) {
        if s.draw.instance_count == 0 {
            return;
        }

        let set_buffer = |buf: objc::Object, index: usize| {
            // SAFETY: valid encoder and buffer.
            unsafe {
                self.encoder.msg_send::<(), _>(
                    objc::sel(c"setVertexBuffer:offset:atIndex:"),
                    (buf.value, 0 as c_ulong, index as c_ulong),
                );
                self.encoder.msg_send::<(), _>(
                    objc::sel(c"setFragmentBuffer:offset:atIndex:"),
                    (buf.value, 0 as c_ulong, index as c_ulong),
                );
            }
        };

        // Set pipeline state
        // SAFETY: valid encoder and pipeline.
        unsafe {
            self.encoder
                .msg_send::<(), _>(objc::sel(c"setRenderPipelineState:"), (s.pipeline.state.value,));
        }

        if !s.buffers.is_empty() {
            // We reserve index 0 for the vertex buffer, this isn't very
            // flexible but it lines up with the API we have for OpenGL.
            if let Some(buf) = s.buffers[0] {
                set_buffer(buf, 0);
            }

            // Set the rest of the buffers starting at index 2, this is
            // so that we can use index 1 for the uniforms if present.
            //
            // Also, we set buffers (and textures) for both stages.
            //
            // Again, not very flexible, but it's consistent and predictable,
            // and we need to treat the uniforms as special because of OpenGL.
            //
            // TODO: Maybe in the future add info to the pipeline struct which
            //       allows it to define a mapping between provided buffers and
            //       what index they get set at for the vertex / fragment stage.
            for (b, i) in s.buffers[1..].iter().zip(2..) {
                if let Some(buf) = *b {
                    set_buffer(buf, i);
                }
            }
        }

        // Set the uniforms as buffer index 1 if present.
        if let Some(buf) = s.uniforms {
            set_buffer(buf, 1);
        }

        // SAFETY: valid encoder and resources.
        unsafe {
            // Set textures.
            for (i, t) in s.textures.iter().enumerate() {
                if let Some(tex) = t {
                    self.encoder.msg_send::<(), _>(
                        objc::sel(c"setVertexTexture:atIndex:"),
                        (tex.texture.value, i as c_ulong),
                    );
                    self.encoder.msg_send::<(), _>(
                        objc::sel(c"setFragmentTexture:atIndex:"),
                        (tex.texture.value, i as c_ulong),
                    );
                }
            }

            // Set samplers.
            for (i, samp) in s.samplers.iter().enumerate() {
                if let Some(sampler) = samp {
                    self.encoder.msg_send::<(), _>(
                        objc::sel(c"setFragmentSamplerState:atIndex:"),
                        (sampler.sampler.value, i as c_ulong),
                    );
                }
            }

            // Draw!
            self.encoder.msg_send::<(), _>(
                objc::sel(c"drawPrimitives:vertexStart:vertexCount:instanceCount:"),
                (
                    s.draw.r#type as c_ulong,
                    0 as c_ulong,
                    s.draw.vertex_count as c_ulong,
                    s.draw.instance_count as c_ulong,
                ),
            );
        }
    }

    /// Complete this render pass.
    /// This struct can no longer be used after calling this.
    pub fn complete(&self) {
        // SAFETY: valid encoder.
        unsafe { self.encoder.msg_send::<(), _>(objc::sel(c"endEncoding"), ()) };
    }
}
