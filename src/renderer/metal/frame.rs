//! Port of `src/renderer/metal/Frame.zig`.
//!
//! STATUS: ported (macOS only; type-checked, not run)
//!
//! Wrapper for handling render passes.

use std::ffi::{c_ulong, c_void};

use crate::renderer::Health;

use super::api as mtl;
use super::objc;
use super::render_pass::{self, RenderPass};
use super::{Renderer, Target};

const LOG: &str = "metal";

/// Options for beginning a frame.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// MTLCommandQueue
    pub queue: objc::Object,
}

/// The captures of the completion block (Zig: the `CompletionBlock`
/// context struct).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CompletionCaptures {
    pub renderer: *mut Renderer,
    pub target: *mut Target,
    pub sync: bool,
}

/// This is the block type used for the addCompletedHandler callback.
pub type CompletionBlock = objc::Block<CompletionCaptures>;

static COMPLETION_DESCRIPTOR: objc::BlockDescriptor = objc::BlockDescriptor {
    reserved: 0,
    size: std::mem::size_of::<CompletionBlock>() as c_ulong,
    copy_helper: None,
    dispose_helper: None,
};

#[derive(Debug)]
pub struct Frame {
    /// MTLCommandBuffer
    pub buffer: objc::Object,

    pub block: CompletionBlock,
}

impl Frame {
    /// Begin encoding a frame.
    ///
    /// Once the frame has been completed, the `frameCompleted` method
    /// on the renderer is called with the health status of the frame.
    /// The target is presented via the provided renderer's API when completed.
    pub fn begin(opts: Options, renderer: *mut Renderer, target: *mut Target) -> Frame {
        // SAFETY: valid MTLCommandQueue.
        let buffer: objc::Object = unsafe { opts.queue.msg_send(objc::sel(c"commandBuffer"), ()) };

        // Create our block to register for completion updates.
        // The block is deallocated by the objC runtime on success.
        let block = CompletionBlock::init(
            CompletionCaptures {
                renderer,
                target,
                sync: false,
            },
            buffer_completed as *const c_void,
            &COMPLETION_DESCRIPTOR,
        );

        Frame { buffer, block }
    }

    /// Add a render pass to this frame with the provided attachments.
    /// Returns a RenderPass which allows render steps to be added.
    #[inline]
    pub fn render_pass(&self, attachments: &[render_pass::Attachment]) -> RenderPass {
        RenderPass::begin(render_pass::Options {
            attachments,
            command_buffer: self.buffer,
        })
    }

    /// Complete this frame and present the target.
    ///
    /// If `sync` is true, this will block until the frame is presented.
    #[inline]
    pub fn complete(&mut self, sync: bool) {
        // SAFETY: valid MTLCommandBuffer; the block is a valid stack block.
        unsafe {
            // If we don't need to complete synchronously,
            // we add our block as a completion handler.
            //
            // It will be copied when we add the handler, and then the
            // copy will be deallocated by the objc runtime on success.
            if !sync {
                self.buffer.msg_send::<(), _>(
                    objc::sel(c"addCompletedHandler:"),
                    (&mut self.block as *mut CompletionBlock as *mut c_void,),
                );
            }

            self.buffer.msg_send::<(), _>(objc::sel(c"commit"), ());

            // If we need to complete synchronously, we wait until
            // the buffer is completed and invoke the block directly.
            if sync {
                self.buffer
                    .msg_send::<(), _>(objc::sel(c"waitUntilCompleted"), ());
                self.block.captures.sync = true;
                buffer_completed(&self.block, self.buffer.value);
            }
        }
    }
}

unsafe extern "C" fn buffer_completed(block: *const CompletionBlock, buffer_id: objc::Id) {
    let buffer = objc::Object::from_id(buffer_id);

    // SAFETY: called with a valid block whose renderer and target are
    // alive until the frame completes (like Zig).
    unsafe {
        let captures = (*block).captures;

        // Get our command buffer status to pass back to the generic renderer.
        let status = mtl::MTLCommandBufferStatus(buffer.get_property(c"status"));
        let health = if status == mtl::MTLCommandBufferStatus::ERROR {
            Health::Unhealthy
        } else {
            Health::Healthy
        };

        let renderer = &mut *captures.renderer;

        // If the frame is healthy, present it.
        if health == Health::Healthy {
            if let Err(err) = renderer.api.present(*captures.target, captures.sync) {
                log::error!(target: LOG, "Failed to present render target: err={err}");
            }
        }

        renderer.frame_completed(health);
    }
}
