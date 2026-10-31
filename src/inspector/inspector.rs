//! Port of `src/inspector/Inspector.zig`.
//!
//! STATUS: ported
//!
//! The Inspector is a development tool to debug the terminal. This is
//! useful for terminal application developers as well as people potentially
//! debugging issues in Ghostty itself.
//!
//! Deviations:
//! * `setup` and `render` (Dear ImGui) are only compiled with the
//!   `inspector` Cargo feature. The recording API (`record_key_event`,
//!   `record_pty_read`, `renderer_info`) is always available because the
//!   core surface, termio and renderer use it.
//! * `render` takes the core surface as a raw pointer because the
//!   inspector is owned by the surface it renders.

use crate::inspector::widgets;
use crate::inspector::KeyEvent;
use crate::terminal::terminal::Terminal;

#[cfg(feature = "inspector")]
use crate::pkg::dcimgui::{self as cimgui, c};
#[cfg(feature = "inspector")]
use crate::surface::Surface;

/// The error type of the recording functions.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug)]
pub struct Inspector {
    /// Mouse state that we track in addition to normal mouse states that
    /// Ghostty always knows about.
    pub mouse: widgets::surface::Mouse,

    // ImGui state
    pub gui: widgets::surface::Inspector,
}

impl Inspector {
    /// Setup the ImGui state. This requires an ImGui context to be set.
    #[cfg(feature = "inspector")]
    pub fn setup() {
        // SAFETY: the caller guarantees a current ImGui context.
        unsafe {
            let io: *mut c::ImGuiIO = c::ImGui_GetIO();

            // Enable docking, which we use heavily for the UI.
            (*io).ConfigFlags |= c::ImGuiConfigFlags_DockingEnable as c::ImGuiConfigFlags;

            // Our colorspace is sRGB.
            (*io).ConfigFlags |= c::ImGuiConfigFlags_IsSRGB as c::ImGuiConfigFlags;

            // Disable the ini file to save layout
            (*io).IniFilename = std::ptr::null();
            (*io).LogFilename = std::ptr::null();

            // Use our own embedded font
            {
                // TODO: This will have to be recalculated for different screen DPIs.
                // This is currently hardcoded to a 2x content scale.
                let font_size: f32 = 16.0 * 2.0;

                let mut font_config: c::ImFontConfig = std::mem::zeroed();
                cimgui::ext::ImFontConfig_ImFontConfig(&mut font_config);
                font_config.FontDataOwnedByAtlas = false;
                let regular = crate::font::embedded::REGULAR;
                c::ImFontAtlas_AddFontFromMemoryTTF(
                    (*io).Fonts,
                    regular.as_ptr() as *mut std::ffi::c_void,
                    regular.len() as _,
                    font_size,
                    &font_config,
                    std::ptr::null(),
                );
            }
        }
    }

    pub fn init() -> Inspector {
        Inspector {
            mouse: widgets::surface::Mouse::default(),
            gui: widgets::surface::Inspector::init(),
        }
    }

    /// Returns the renderer info panel. This is a convenience function
    /// to access and find this state to read and modify.
    pub fn renderer_info(&mut self) -> &mut widgets::renderer::Info {
        &mut self.gui.renderer_info
    }

    /// Record a keyboard event.
    pub fn record_key_event(&mut self, ev: KeyEvent) -> Result<(), Error> {
        const MAX_CAPACITY: usize = 50;

        let events: &mut widgets::key::EventRing = &mut self.gui.key_stream.events;
        widgets::termio::append_capped(events, ev, MAX_CAPACITY);
        Ok(())
    }

    /// Record data read from the pty.
    pub fn record_pty_read(&mut self, t: &mut Terminal, data: &[u8]) -> Result<(), Error> {
        self.gui.vt_stream.record_pty_read(t, data)
    }

    /// Render the frame.
    ///
    /// # Safety
    /// `surface` must be the live core surface that owns this inspector and
    /// an ImGui context must be current.
    #[cfg(feature = "inspector")]
    pub unsafe fn render(&mut self, surface: *mut Surface) {
        // Draw the UI
        let mouse = self.mouse;
        // SAFETY: forwarded from the caller.
        unsafe { self.gui.draw(surface, mouse) };

        // We always trigger a rebuild of the surface when the inspector
        // is focused because modifying the inspector can change the terminal
        // state. This is KIND OF expensive (wasted CPU if nothing was done)
        // but the inspector is a development tool and it expressly costs
        // more resources while open so its okay.
        // SAFETY: forwarded from the caller.
        let _ = unsafe { (*surface).renderer_thread.wakeup.notify() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input;

    #[test]
    fn record_key_event_capacity() {
        let mut insp = Inspector::init();
        for i in 0..60u32 {
            let mut ev = KeyEvent::init(&input::KeyEvent::default());
            ev.pty = i.to_le_bytes().to_vec();
            insp.record_key_event(ev).unwrap();
        }
        let events = &insp.gui.key_stream.events;
        assert_eq!(events.capacity(), 50);
        assert_eq!(events.len(), 50);
        assert_eq!(events.first().unwrap().pty, 10u32.to_le_bytes());
        assert_eq!(events.last().unwrap().pty, 59u32.to_le_bytes());
    }

    #[test]
    fn renderer_info_features() {
        let mut insp = Inspector::init();
        insp.renderer_info().put(crate::renderer::overlay::Feature::SemanticPrompts);
        assert_eq!(
            insp.renderer_info().overlay_features(),
            vec![crate::renderer::overlay::Feature::SemanticPrompts]
        );
    }

    #[test]
    fn record_pty_read() {
        let mut t = Terminal::init(crate::terminal::terminal::Options::new(10, 5)).unwrap();
        let mut insp = Inspector::init();
        insp.record_pty_read(&mut t, b"\x1b[H").unwrap();
        assert_eq!(insp.gui.vt_stream.events.len(), 1);
    }

    #[cfg(feature = "inspector")]
    #[test]
    fn setup_context() {
        // SAFETY: plain ImGui context lifecycle.
        unsafe {
            let ctx = c::ImGui_CreateContext(std::ptr::null_mut());
            Inspector::setup();
            let io = c::ImGui_GetIO();
            assert_ne!((*io).ConfigFlags & c::ImGuiConfigFlags_DockingEnable as c::ImGuiConfigFlags, 0);
            assert!((*io).IniFilename.is_null());
            c::ImGui_DestroyContext(ctx);
        }
    }
}
