//! Port of `src/apprt/gtk/Surface.zig`.
//!
//! STATUS: ported
//!
//! The apprt surface handed to the core surface. It points back at the
//! GObject surface (`class::surface::Surface`) that owns it, like Zig's
//! `surface: *Surface`: the GObject private data embeds this struct, so the
//! pointer is never dangling while the struct exists.
#![cfg(feature = "gtk")]

use std::ffi::CStr;
use std::fmt;

use glib::subclass::prelude::InstanceStructExt;
use glib::translate::{from_glib_borrow, Borrowed, ToGlibPtr};

use crate::apprt;
use crate::apprt::gtk::class::application::Application;
use crate::apprt::gtk::class::surface::Surface as GSurface;
use crate::apprt::structs::{
    Clipboard, ClipboardContent, ClipboardReadResult, ClipboardRequest, ContentScale, CursorPos,
    SurfaceSize,
};
use crate::command::EnvMap;
use crate::renderer;
use crate::surface::Surface as CoreSurface;

/// See the module documentation.
pub struct Surface {
    /// The GObject Surface
    surface: *mut <GSurface as glib::object::ObjectType>::GlibType,
}

impl fmt::Debug for Surface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("gtk::Surface").field("surface", &self.surface).finish()
    }
}

impl Surface {
    /// Create the apprt surface for the given GObject surface. The GObject
    /// surface must outlive the returned value (it owns it).
    pub fn new(surface: &GSurface) -> Surface {
        Surface {
            surface: surface.to_glib_none().0,
        }
    }

    pub fn deinit(&mut self) {}

    /// Returns the GObject surface for this apprt surface without taking a
    /// reference. Only valid while the surface is alive (not finalizing).
    fn borrow(&self) -> Borrowed<GSurface> {
        // SAFETY: the GObject surface owns (and outlives) this struct.
        unsafe { from_glib_borrow(self.surface) }
    }

    /// Returns the GObject surface for this apprt surface. This is a
    /// function so we can add some extra logic if we ever have to here.
    pub fn gobj(&self) -> GSurface {
        (*self.borrow()).clone()
    }

    pub fn core(&self) -> *mut CoreSurface {
        // This reads the private data directly (no wrapper/reference), so
        // it also works while the surface is finalized, when the core app
        // removes the surface.
        // SAFETY: the GObject surface owns (and outlives) this struct.
        let imp = unsafe { (*self.surface).imp() };
        // This asserts the non-optional because libghostty should only
        // be calling this for initialized surfaces.
        imp.core_surface.get().expect("core surface is initialized")
    }

    pub fn rt_app(&self) -> *mut apprt::App {
        Application::default().rt()
    }

    pub fn close(&self, process_active: bool) {
        let _ = process_active;
        self.borrow().close();
    }

    /// Zig forwards to `Surface.cgroupPath`, which the GObject surface
    /// never defines (the declaration is never referenced, so Zig never
    /// compiles it). The GTK apprt has no per-surface cgroup.
    pub fn cgroup(&self) -> Option<String> {
        None
    }

    pub fn get_title(&self) -> Option<&CStr> {
        let ptr = self.borrow().title_ptr()?;
        // SAFETY: the title is owned by the surface's private data and lives
        // until the next `set_title`, like the Zig slice.
        Some(unsafe { CStr::from_ptr(ptr) })
    }

    pub fn get_content_scale(&self) -> Result<ContentScale, apprt::Error> {
        Ok(self.borrow().get_content_scale())
    }

    pub fn get_size(&self) -> Result<SurfaceSize, apprt::Error> {
        Ok(self.borrow().get_size())
    }

    pub fn get_cursor_pos(&self) -> Result<CursorPos, apprt::Error> {
        Ok(self.borrow().get_cursor_pos())
    }

    pub fn supports_clipboard(&self, clipboard_type: Clipboard) -> bool {
        match clipboard_type {
            Clipboard::Standard | Clipboard::Selection | Clipboard::Primary => true,
        }
    }

    pub fn clipboard_request(
        &self,
        clipboard_type: Clipboard,
        state: &ClipboardRequest,
    ) -> Result<ClipboardReadResult, apprt::Error> {
        Ok(self.borrow().clipboard_request(clipboard_type, state))
    }

    pub fn set_clipboard(
        &self,
        clipboard_type: Clipboard,
        contents: &[ClipboardContent<'_>],
        confirm: bool,
    ) -> Result<(), apprt::Error> {
        self.borrow().set_clipboard(clipboard_type, contents, confirm);
        Ok(())
    }

    pub fn default_termio_env(&self) -> Result<EnvMap, apprt::Error> {
        self.borrow()
            .default_termio_env()
            .map_err(|e| apprt::Error(e.to_string()))
    }

    /// Redraw the inspector for our surface.
    pub fn redraw_inspector(&self) {
        self.borrow().redraw_inspector();
    }

    pub fn render_device_options(&self) -> Result<renderer::DeviceOptions, apprt::Error> {
        Ok(self.borrow().render_device_options())
    }
}
