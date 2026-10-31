//! Port of `src/apprt/gtk/class/render_surface.zig`.
//!
//! STATUS: ported
//!
//! Rust notes: the DMABUF planes and memory pixels handed to GDK are owned
//! by the release closures (Zig: heap holders freed by `dmabufDestroy` and
//! `memoryDestroy`); `glib::Bytes::from_owned` takes the pixel `Vec`.
#![cfg(feature = "gtk")]

use std::cell::{Cell, RefCell};
use std::ffi::c_int;
use std::sync::LazyLock;

use glib::prelude::*;
use glib::subclass::Signal;
use gtk4::gdk;
use gtk4::gdk::prelude::*;
use gtk4::graphene;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;

use crate::apprt::gtk::scale as scale_util;
use crate::renderer;
use crate::renderer::dmabuf::{Dmabuf, Planes};
use crate::renderer::opengl::{ExportedFrame, Memory};
use crate::surface::Surface as CoreSurface;

const LOG: &str = "gtk_render_surface";

/// Signal names (Zig: `signals`).
pub mod signals {
    pub const RESIZE: &str = "resize";
}

/// Errors from rebuilding the texture.
#[derive(Debug)]
enum RebuildError {
    /// GTK can't import our DMABUFs.
    DmabufBuildFailed,
}

impl std::fmt::Display for RebuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct RenderSurface {
        /// The core surface, used to pull presents from the renderer.
        /// Set by the apprt Surface when it realizes the render surface.
        pub core_surface: Cell<Option<*mut CoreSurface>>,

        /// The GDK Texture currently being displayed.
        pub texture: RefCell<Option<gdk::Texture>>,

        /// `notify::scale` handler id on `scale_surface`.
        pub scale_notify_id: RefCell<Option<glib::SignalHandlerId>>,

        /// GdkSurface we connected; ref'd so a replacement cannot dangle the handler.
        pub scale_surface: RefCell<Option<gdk::Surface>>,

        /// One-shot so we log the first real allocation, not every size-allocate.
        pub scale_logged_size: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RenderSurface {
        const NAME: &'static str = "GhosttyRenderSurface";
        type Type = super::RenderSurface;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for RenderSurface {
        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![Signal::builder(signals::RESIZE)
                    .param_types([i32::static_type(), i32::static_type()])
                    .build()]
            });
            SIGNALS.as_ref()
        }
    }

    impl WidgetImpl for RenderSurface {
        fn realize(&self) {
            // Call the parent realize.
            self.parent_realize();

            // Request a draw in case frames were produced before we
            // realized. The snapshot will pull from the renderer's queue.
            let obj = self.obj();
            obj.connect_scale_notify();
            obj.queue_draw();
        }

        fn unrealize(&self) {
            self.obj().disconnect_scale_notify();
            self.texture.replace(None);
            self.parent_unrealize();
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let obj = self.obj();
            obj.connect_scale_notify();
            obj.emit_device_resize(width, height);

            self.parent_size_allocate(width, height, baseline);

            if !self.scale_logged_size.get() && width > 0 && height > 0 {
                obj.log_scale("allocated", width, height);
                self.scale_logged_size.set(true);
            }
        }

        fn snapshot(&self, snap: &gtk4::Snapshot) {
            self.obj().snapshot_impl(snap);
        }
    }
}

glib::wrapper! {
    /// A widget that displays the rendered output of the Ghostty renderer.
    ///
    /// In the past, Ghostty relied on GTK's builtin `GLArea` to display the
    /// rendered output within the GTK-based UI, but this had numerous
    /// debilitating limitations.
    ///
    /// Its most significant limitation is that GTK's render lifecycle lives
    /// **exclusively on the main UI thread**, from initialization to drawing
    /// the rendered framebuffers. This is not only inefficient but also
    /// error-prone, as we offload our render work to a dedicated render
    /// thread which has to be careful not to access the renderer at the same
    /// time as the main thread. Initialization is also far more complex as we
    /// had to wait for GTK's OpenGL context to initialize, then we can
    /// proceed with initializing the renderer and then the core surface.
    /// (The GTK surface widget could not assume it always has a valid core
    /// surface, which is obviously absurd!) It was a messy, chicken-and-egg
    /// relationship that held us back a lot.
    ///
    /// With our custom `RenderSurface`, the renderer instead renders frames,
    /// exports them into DMABUFs, then pushes them into a queue that this
    /// widget can consume at its own pace on the main thread, whenever GTK
    /// calls for it to be snapshotted. This also allowed us to do
    /// triple-buffering and should greatly reduce tearing. Initialization is
    /// done on the render thread and always completes before the core surface
    /// is created. Furthermore, this surface is actually OpenGL-agnostic,
    /// since it operates exclusively on DMABUFs which can also be produced by
    /// other graphics APIs like Vulkan.
    pub struct RenderSurface(ObjectSubclass<imp::RenderSurface>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for RenderSurface {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// The size of a render surface in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeviceSize {
    pub width: u32,
    pub height: u32,
}

impl RenderSurface {
    fn snapshot_impl(&self, snap: &gtk4::Snapshot) {
        let priv_ = self.imp();

        // Take the latest present from the renderer's queue, if any, and
        // build a texture from it. `take` closes any present that was
        // still unconsumed in the queue, but not the texture we're
        // currently displaying.
        if let Some(core) = priv_.core_surface.get() {
            // SAFETY: the core surface outlives the render surface's use of
            // it (it is unset before the core surface is freed).
            let core = unsafe { &mut *core };
            if let Some(frame) = core.renderer.take_frame() {
                if let Err(err) = self.rebuild_texture(frame) {
                    match err {
                        // GTK can't import our DMABUFs, e.g. due to an
                        // incompatible format modifier on multi-GPU setups.
                        // Report unhealthy presentation health so the
                        // renderer presents via CPU readback instead.
                        RebuildError::DmabufBuildFailed => {
                            log::warn!(
                                target: LOG,
                                "failed to import dmabuf, asking renderer for CPU presentation"
                            );
                            core.report_presentation_health(renderer::Health::Unhealthy);
                        }
                    }
                }
            }
        }

        // If we have a texture, draw it.
        let Some(texture) = priv_.texture.borrow().clone() else { return };

        if self.width() == 0 || self.height() == 0 {
            return;
        }

        // Map one texture texel to one device pixel. Widget CSS size would
        // stretch a fractionally scaled buffer. Snap the surface-relative
        // origin so parent chrome cannot place the texture between pixels.
        let layout = scale_util::widget_layout(self);
        let origin = layout.snap_origin();
        let scale = layout.scale as f32;
        let css_w = texture.width() as f32 / scale;
        let css_h = texture.height() as f32 / scale;

        // GTK, Metal, Vulkan, etc. all have their origin points in the
        // top left, but not OpenGL!
        let flip = !renderer::Renderer::CUSTOM_SHADER_Y_IS_DOWN;
        if flip {
            snap.save();
            snap.translate(&graphene::Point::new(0.0, css_h));
            snap.scale(1.0, -1.0);
        }

        snap.append_texture(
            &texture,
            &graphene::Rect::new(origin.x as f32, origin.y as f32, css_w, css_h),
        );

        if flip {
            snap.restore();
        }
    }

    //---------------------------------------------------------------
    // Presenting

    /// Return the size of this surface in device pixels. Used by the
    /// apprt surface to report the size to the renderer.
    pub fn device_size(&self) -> DeviceSize {
        let size = scale_util::widget_layout(self).size;
        DeviceSize {
            width: size.width,
            height: size.height,
        }
    }

    fn emit_device_resize(&self, css_w: c_int, css_h: c_int) {
        let size = scale_util::widget_layout_for_size(self, css_w, css_h).size;
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.emit_by_name::<()>(signals::RESIZE, &[&(size.width as i32), &(size.height as i32)]);
    }

    fn connect_scale_notify(&self) {
        let Some(native) = self.native() else { return };
        let Some(surface) = native.surface() else { return };

        let priv_ = self.imp();
        if priv_.scale_notify_id.borrow().is_some() {
            if priv_.scale_surface.borrow().as_ref() == Some(&surface) {
                return;
            }
            self.disconnect_scale_notify();
        }

        priv_.scale_surface.replace(Some(surface.clone()));
        priv_.scale_logged_size.set(false);
        let weak = self.downgrade();
        let id = surface.connect_notify_local(Some("scale"), move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.on_scale_notify();
            }
        });
        priv_.scale_notify_id.replace(Some(id));
        self.log_scale("connected", self.width(), self.height());
    }

    fn disconnect_scale_notify(&self) {
        let priv_ = self.imp();
        let Some(id) = priv_.scale_notify_id.take() else { return };
        if let Some(surface) = priv_.scale_surface.take() {
            surface.disconnect(id);
        }
    }

    fn on_scale_notify(&self) {
        self.log_scale("changed", self.width(), self.height());
        self.emit_device_resize(self.width(), self.height());
        self.queue_draw();
    }

    fn log_scale(&self, event: &str, css_width: c_int, css_height: c_int) {
        let Some(native) = self.native() else { return };
        let Some(surface) = native.surface() else { return };
        let layout = scale_util::widget_layout_for_size(self, css_width, css_height);

        log::debug!(
            target: LOG,
            "surface scale {}: css={}x{} device={}x{} surface={} effective={} surface_factor={} widget_factor={}",
            event,
            css_width,
            css_height,
            layout.size.width,
            layout.size.height,
            surface_scale(&surface),
            layout.scale,
            surface.scale_factor(),
            self.scale_factor(),
        );
    }

    /// Set the core surface to pull presents from. Nothing will
    /// render when this is unset.
    pub fn set_core_surface(&self, core: Option<*mut CoreSurface>) {
        self.imp().core_surface.set(core);
    }

    /// Build a `GdkTexture` from the present and set it as our current
    /// texture, unrefing any previous texture.
    fn rebuild_texture(&self, mut frame: ExportedFrame) -> Result<(), RebuildError> {
        match &mut frame {
            ExportedFrame::Dmabuf(dmabuf) => {
                // The texture takes ownership of the planes: clear them from
                // the frame so dropping it doesn't close the FDs.
                let owned = *dmabuf;
                dmabuf.planes.count = 0;
                self.rebuild_dmabuf_texture(owned)
            }
            ExportedFrame::Memory(memory) => {
                let owned = Memory {
                    width: memory.width,
                    height: memory.height,
                    pixels: std::mem::take(&mut memory.pixels),
                };
                self.rebuild_memory_texture(owned);
                Ok(())
            }
        }
    }

    /// Build a `GdkDmabufTexture` from the present and set it as our
    /// current texture, unrefing any previous texture.
    fn rebuild_dmabuf_texture(&self, frame: Dmabuf) -> Result<(), RebuildError> {
        let display = self.display();

        let mut builder = gdk::DmabufTextureBuilder::new()
            .set_display(&display)
            .set_width(frame.width)
            .set_height(frame.height)
            .set_fourcc(frame.fourcc)
            .set_modifier(frame.modifier)
            .set_premultiplied(frame.premultiplied)
            .set_n_planes(frame.planes.count as u32);

        for i in 0..frame.planes.count as usize {
            // SAFETY: the FDs stay open until the release function runs.
            builder = unsafe { builder.set_fd(i as u32, frame.planes.fds[i]) }
                .set_offset(i as u32, frame.planes.offsets[i] as u32)
                .set_stride(i as u32, frame.planes.strides[i] as u32);
        }

        // Build the texture. We retain ownership over the DMABUF FDs
        // until the asynchronous texture building process is complete,
        // so we hand a copy of them to GTK's release function, which
        // destroys them once GDK no longer needs them.
        let planes: Planes = frame.planes;
        // SAFETY: the FDs set above are valid until the release function
        // closes them.
        let texture = unsafe { builder.build_with_release_func(move || dmabuf_destroy(planes)) };
        let texture = match texture {
            Ok(t) => t,
            Err(err) => {
                log::warn!(target: LOG, "failed to build dmabuf err={}", err.message());
                // GDK doesn't call the release function on failure.
                planes.deinit();
                return Err(RebuildError::DmabufBuildFailed);
            }
        };

        // Swap out the old texture.
        self.imp().texture.replace(Some(texture));
        Ok(())
    }

    /// Build a `GdkMemoryTexture` from a CPU memory frame and set it as
    /// our current texture, unrefing any previous texture. This is the
    /// fallback path for drivers that can't export DMA-BUFs.
    fn rebuild_memory_texture(&self, frame: Memory) {
        // `GdkMemoryTexture` keeps a reference to the `GBytes` for as
        // long as it needs the pixel data; the bytes own the pixels and
        // free them once the last reference is gone (Zig: `memoryDestroy`).
        let width = frame.width;
        let height = frame.height;
        let bytes = glib::Bytes::from_owned(frame.pixels);

        let texture = gdk::MemoryTexture::new(
            width as i32,
            height as i32,
            gdk::MemoryFormat::R8g8b8a8Premultiplied,
            &bytes,
            width as usize * 4, // stride
        );

        // Swap out the old texture.
        self.imp().texture.replace(Some(texture.upcast()));
    }
}

/// Destroy callback for `GdkDmabufTexture`. GDK calls this when the
/// texture is released; we close the FDs.
fn dmabuf_destroy(planes: Planes) {
    planes.deinit();
}

/// `gdk.Surface.getScale` (GTK 4.12+; the scale factor before).
fn surface_scale(surface: &gdk::Surface) -> f64 {
    surface.scale()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprt::gtk::ext::test_gtk;

    #[test]
    fn render_surface_smoke() {
        let Some(()) = test_gtk::run(|| {
            let rs = RenderSurface::default();
            // Unrealized: zero device size and no texture.
            assert_eq!(rs.device_size(), DeviceSize::default());
            assert!(rs.imp().texture.borrow().is_none());
            rs.set_core_surface(None);
            assert!(rs.imp().core_surface.get().is_none());

            // A memory frame becomes the current texture.
            rs.rebuild_memory_texture(Memory {
                width: 2,
                height: 1,
                pixels: vec![255; 8],
            });
            let tex = rs.imp().texture.borrow().clone().unwrap();
            assert_eq!((tex.width(), tex.height()), (2, 1));

            // The resize signal carries device pixels.
            let got = std::rc::Rc::new(Cell::new((0, 0)));
            let got2 = got.clone();
            rs.connect_local(signals::RESIZE, false, move |args| {
                got2.set((args[1].get::<i32>().unwrap(), args[2].get::<i32>().unwrap()));
                None
            });
            rs.emit_device_resize(10, 20);
            assert_eq!(got.get(), (10, 20));
        }) else {
            return;
        };
    }
}
