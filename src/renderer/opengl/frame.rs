//! Port of `src/renderer/opengl/Frame.zig`.
//!
//! STATUS: ported
//!
//! Wrapper for handling render passes.
//!
//! The renderer and target are raw pointers like in Zig: the frame is a
//! short-lived handle used on the render thread while both are alive.

use crate::apprt;
use crate::datastruct::blocking_queue::Timeout as QueueTimeout;
use crate::pkg::opengl as gl;
use crate::renderer::opengl::{render_pass, ExportedFrame, RenderPass, Renderer, Target};
use crate::renderer::Health;

const LOG: &str = "opengl";

/// Options for beginning a frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {}

#[derive(Debug)]
pub struct Frame {
    pub renderer: *mut Renderer,
    pub target: *mut Target,
}

impl Frame {
    /// Begin encoding a frame.
    ///
    /// Once the frame has been completed, the `frameCompleted` method
    /// on the renderer is called with the health status of the frame.
    /// The target is presented via the provided renderer's API when completed.
    pub fn begin(opts: Options, renderer: *mut Renderer, target: *mut Target) -> Frame {
        let _ = opts;

        Frame { renderer, target }
    }

    /// Add a render pass to this frame with the provided attachments.
    /// Returns a RenderPass which allows render steps to be added.
    #[inline]
    pub fn render_pass<'a>(&self, attachments: &'a [render_pass::Attachment]) -> RenderPass<'a> {
        RenderPass::begin(render_pass::Options { attachments })
    }

    /// Complete this frame and present the target.
    ///
    /// If `sync` is true, this will block until the frame is presented.
    ///
    /// NOTE: For OpenGL, `sync` is ignored and we never block, instead
    /// pushing the newly presented and exported frame to the frame queue.
    pub fn complete(&self, sync: bool) {
        let _ = sync;
        // SAFETY: the renderer and target outlive the frame (see module docs).
        let renderer = unsafe { &mut *self.renderer };
        let target = unsafe { *self.target };

        // If there are any GL errors, consider the frame unhealthy.
        let health = if gl::errors::get_error().is_ok() {
            Health::Healthy
        } else {
            Health::Unhealthy
        };

        // If the frame is healthy, draw it to an ExportedFrame
        // Then sync before exporting and pushing to the present queue.
        // The apprt pulls from this queue in its snapshot handler.
        let presented_frame: Option<ExportedFrame> = if health == Health::Healthy {
            let presentation_health = renderer.presentation_health();
            match renderer.api.present(target, presentation_health) {
                Ok(frame) => Some(frame),
                Err(err) => {
                    log::warn!(target: LOG, "failed to present render target: err={err}");
                    None
                }
            }
        } else {
            None
        };

        // Sync after frame draw AND frame present GL calls.
        gl::finish();

        // At this point the ExportedFrame is finished and can be shared
        if let Some(frame) = presented_frame {
            renderer.push_frame(frame);

            // Notify the surface that it should redraw
            let _ = renderer
                .surface_mailbox
                .push(apprt::surface::Message::Redraw, QueueTimeout::Forever);
        }

        // Report the health to the renderer.
        renderer.frame_completed(health);
    }
}

#[cfg(test)]
mod tests {
    // Not a Zig test: Frame.zig has none.
    use super::*;
    use crate::app::{MailboxQueue as AppQueue, Message as AppMessage};
    use crate::renderer::opengl::device::{self, test_context::egl_lock, Device};
    use crate::renderer::opengl::test_util::test_options_with_mailbox;
    use crate::renderer::opengl::OpenGL;

    #[test]
    fn complete_presents_and_reports() {
        let _guard = egl_lock();
        let mut device = Device::init(device::Options::default()).unwrap();
        let app = apprt::App::default();
        let queue = AppQueue::create();
        let opts = test_options_with_mailbox(&mut *device, &app, &*queue);
        let mut renderer = Renderer::init(opts).unwrap();
        renderer.thread_enter(std::ptr::null_mut()).unwrap();

        let mut target = renderer.api.init_target(8, 4).unwrap();
        let rp: *mut Renderer = &mut renderer;
        // SAFETY: `rp` points at the live renderer.
        let frame = unsafe { (*rp).api.begin_frame(rp, &mut target) }.unwrap();
        frame.complete(false);

        // A frame was exported and the surface was told to redraw.
        let exported = renderer.take_frame().expect("exported frame");
        drop(exported);
        let mut redraws = 0;
        while let Some(msg) = queue.pop() {
            match msg {
                AppMessage::SurfaceMessage {
                    message: apprt::surface::Message::Redraw,
                    ..
                } => redraws += 1,
                other => panic!("unexpected message {other:?}"),
            }
        }
        assert_eq!(1, redraws);
        assert_eq!(Health::Healthy, renderer.health.load());

        target.deinit();
        renderer.thread_exit();
        renderer.deinit();
        device.deinit();
    }
}
