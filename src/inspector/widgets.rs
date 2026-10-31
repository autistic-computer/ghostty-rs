//! Port of `src/inspector/widgets.zig`.
//!
//! STATUS: ported
//!
//! The widget data models (key events, VT events, renderer info, mouse
//! state) are always compiled because the core surface, termio and
//! renderer record into them. Everything that draws with Dear ImGui is
//! only compiled with the `inspector` Cargo feature, as are the purely
//! visual widget modules (`page`, `pagelist`, `screen`, `style`,
//! `terminal`).
//!
//! Deviation: the Zig code calls the printf-style ImGui functions directly
//! (`ImGui_Text("%d", x)`). The port formats with Rust's `format!` and
//! renders through [`ui`] (unformatted or `"%.*s"` calls), which renders
//! identically.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "widgets/key.rs"]
pub mod key;
#[path = "widgets/page.rs"]
pub mod page;
#[path = "widgets/pagelist.rs"]
pub mod pagelist;
#[path = "widgets/renderer.rs"]
pub mod renderer;
#[path = "widgets/screen.rs"]
pub mod screen;
#[path = "widgets/style.rs"]
pub mod style;
#[path = "widgets/surface.rs"]
pub mod surface;
#[path = "widgets/terminal.rs"]
pub mod terminal;
#[path = "widgets/termio.rs"]
pub mod termio;
// @generated-mods-end

#[cfg(feature = "inspector")]
use std::ffi::CStr;

#[cfg(feature = "inspector")]
use crate::pkg::dcimgui::{self as cimgui, c};

/// Draws a "(?)" disabled text marker that shows some help text
/// on hover.
#[cfg(feature = "inspector")]
pub fn help_marker(text: &str) {
    // SAFETY: ImGui calls require a current context, which every caller
    // of the inspector widgets (the render callback) guarantees.
    unsafe {
        c::ImGui_TextDisabled(c"(?)".as_ptr());
        if !c::ImGui_BeginItemTooltip() {
            return;
        }

        c::ImGui_PushTextWrapPos(c::ImGui_GetFontSize() * 35.0);
        ui::text(text);
        c::ImGui_PopTextWrapPos();

        c::ImGui_EndTooltip();
    }
}

/// DetachableHeader allows rendering a collapsing header that can be
/// detached into its own window.
#[derive(Debug, Clone, Copy)]
pub struct DetachableHeader {
    /// Set whether the window is detached.
    pub detached: bool,

    /// If true, detaching will move the item into a docking position
    /// to the right.
    pub dock: bool,

    // Internal state do not touch.
    pub window_first: bool,
}

impl Default for DetachableHeader {
    fn default() -> Self {
        DetachableHeader {
            detached: false,
            dock: true,
            window_first: true,
        }
    }
}

#[cfg(feature = "inspector")]
impl DetachableHeader {
    pub fn window_end(&mut self) {
        // If we started the window, we need to end it.
        // SAFETY: see `help_marker`.
        unsafe { c::ImGui_End() };
    }

    /// Returns None if there is no window created (not detached).
    /// Otherwise returns whether the window is open.
    pub fn window(&mut self, label: &CStr) -> Option<bool> {
        // If we're not detached, we don't create a window.
        if !self.detached {
            self.window_first = true;
            return None;
        }

        // SAFETY: see `help_marker`.
        unsafe {
            // If this is our first time showing the window then we need to
            // setup docking. We only do this on the first time because we
            // don't want to reset a user's docking behavior later.
            if self.window_first {
                self.window_first = false;
                'dock: {
                    if !self.dock {
                        break 'dock;
                    }
                    let dock_id = c::ImGui_GetWindowDockID();
                    if dock_id == 0 {
                        break 'dock;
                    }
                    let mut dock_id_right: c::ImGuiID = 0;
                    let mut dock_id_left: c::ImGuiID = 0;
                    cimgui::ImGui_DockBuilderSplitNode(
                        dock_id,
                        c::ImGuiDir_Right as _,
                        0.4,
                        &mut dock_id_right,
                        &mut dock_id_left,
                    );
                    cimgui::ImGui_DockBuilderDockWindow(label.as_ptr(), dock_id_right);
                    cimgui::ImGui_DockBuilderFinish(dock_id);
                }
            }

            Some(c::ImGui_Begin(
                label.as_ptr(),
                &mut self.detached,
                c::ImGuiWindowFlags_NoFocusOnAppearing as _,
            ))
        }
    }

    pub fn header(&mut self, label: &CStr) -> bool {
        // If we're detached, create a separate window.
        if self.detached {
            return false;
        }

        // SAFETY: see `help_marker`.
        unsafe {
            // Make sure all headers have a unique ID in the stack. We only
            // need to do this for the header side because creating a window
            // automatically creates an ID.
            c::ImGui_PushID(label.as_ptr());

            // Create the collapsing header with the pop out button overlaid.
            c::ImGui_SetNextItemAllowOverlap();
            let is_open = c::ImGui_CollapsingHeader(label.as_ptr(), c::ImGuiTreeNodeFlags_None as _);

            // Place pop-out button inside the header bar
            let header_max = c::ImGui_GetItemRectMax();
            let header_min = c::ImGui_GetItemRectMin();
            let frame_height = c::ImGui_GetFrameHeight();
            let button_size = frame_height - 4.0;
            let padding = 4.0;

            c::ImGui_SameLine();
            c::ImGui_SetCursorScreenPos(ui::vec2(
                header_max.x - button_size - padding,
                header_min.y + 2.0,
            ));
            {
                c::ImGui_PushStyleVarImVec2(c::ImGuiStyleVar_FramePadding as _, ui::vec2(0.0, 0.0));
                if c::ImGui_ButtonEx(c">>##detach".as_ptr(), ui::vec2(button_size, button_size)) {
                    self.detached = true;
                }
                c::ImGui_PopStyleVar();
            }

            if c::ImGui_IsItemHovered(c::ImGuiHoveredFlags_DelayShort as _) {
                ui::set_tooltip("Detach into separate window");
            }

            c::ImGui_PopID();
            is_open
        }
    }
}

/// Small helpers over the Dear ImGui C API used by all widgets. All of
/// them require a current ImGui context.
#[cfg(feature = "inspector")]
pub mod ui {
    use std::ffi::{c_int, CStr, CString};

    use crate::pkg::dcimgui::c;

    #[inline]
    pub fn vec2(x: f32, y: f32) -> c::ImVec2 {
        c::ImVec2 { x, y }
    }

    #[inline]
    pub fn vec4(x: f32, y: f32, z: f32, w: f32) -> c::ImVec4 {
        c::ImVec4 { x, y, z, w }
    }

    /// `ImGui_Text`.
    pub fn text(s: &str) {
        let r = s.as_bytes().as_ptr_range();
        // SAFETY: the range is valid for the call.
        unsafe { c::ImGui_TextUnformattedEx(r.start.cast(), r.end.cast()) };
    }

    /// `ImGui_TextDisabled`.
    pub fn text_disabled(s: &str) {
        // SAFETY: "%.*s" reads exactly `len` bytes.
        unsafe { c::ImGui_TextDisabled(c"%.*s".as_ptr(), s.len() as c_int, s.as_ptr()) };
    }

    /// `ImGui_TextColored`.
    pub fn text_colored(col: c::ImVec4, s: &str) {
        // SAFETY: as `text_disabled`.
        unsafe { c::ImGui_TextColored(col, c"%.*s".as_ptr(), s.len() as c_int, s.as_ptr()) };
    }

    /// `ImGui_TextWrapped`.
    pub fn text_wrapped(s: &str) {
        // SAFETY: as `text_disabled`.
        unsafe { c::ImGui_TextWrapped(c"%.*s".as_ptr(), s.len() as c_int, s.as_ptr()) };
    }

    /// `ImGui_SetTooltip`.
    pub fn set_tooltip(s: &str) {
        // SAFETY: as `text_disabled`.
        unsafe { c::ImGui_SetTooltip(c"%.*s".as_ptr(), s.len() as c_int, s.as_ptr()) };
    }

    /// A NUL-terminated copy of `s` for ImGui labels and IDs (Zig uses
    /// sentinel-terminated strings).
    pub fn cstr(s: &str) -> CString {
        CString::new(s.replace('\0', "")).unwrap_or_default()
    }

    /// `ImGui_TableNextRow`.
    pub fn row() {
        // SAFETY: requires an open table.
        unsafe { c::ImGui_TableNextRow() };
    }

    /// `ImGui_TableSetColumnIndex`.
    pub fn col(n: i32) {
        // SAFETY: requires an open table.
        unsafe {
            c::ImGui_TableSetColumnIndex(n);
        }
    }

    /// `ImGui_TableNextColumn`.
    pub fn next_col() {
        // SAFETY: requires an open table.
        unsafe {
            c::ImGui_TableNextColumn();
        }
    }

    /// `ImGui_TableSetupColumn`.
    pub fn setup_column(label: &CStr, flags: c::ImGuiTableColumnFlags_) {
        // SAFETY: requires an open table.
        unsafe { c::ImGui_TableSetupColumn(label.as_ptr(), flags as _) };
    }

    /// `ImGui_BeginTable`.
    pub fn begin_table(id: &CStr, columns: i32, flags: c::ImGuiTableFlags_) -> bool {
        // SAFETY: plain ImGui call.
        unsafe { c::ImGui_BeginTable(id.as_ptr(), columns, flags as _) }
    }

    /// `ImGui_EndTable`.
    pub fn end_table() {
        // SAFETY: requires an open table.
        unsafe { c::ImGui_EndTable() };
    }

    /// `ImGui_SameLine`.
    pub fn same_line() {
        // SAFETY: plain ImGui call.
        unsafe { c::ImGui_SameLine() };
    }

    /// `ImGui_SeparatorText`.
    pub fn separator_text(label: &CStr) {
        // SAFETY: plain ImGui call.
        unsafe { c::ImGui_SeparatorText(label.as_ptr()) };
    }

    /// `ImGui_Separator`.
    pub fn separator() {
        // SAFETY: plain ImGui call.
        unsafe { c::ImGui_Separator() };
    }

    /// `ImGui_CollapsingHeader`.
    pub fn collapsing_header(label: &CStr, flags: c::ImGuiTreeNodeFlags_) -> bool {
        // SAFETY: plain ImGui call.
        unsafe { c::ImGui_CollapsingHeader(label.as_ptr(), flags as _) }
    }

    /// A checkbox showing a value. The Zig code copies the value into a
    /// local and ignores the result, so edits are discarded.
    pub fn checkbox(label: &CStr, value: bool) {
        let mut v = value;
        // SAFETY: plain ImGui call.
        unsafe { c::ImGui_Checkbox(label.as_ptr(), &mut v) };
    }

    /// `ImGui_ColorEdit3` on a copy of an RGB color.
    pub fn color_edit3(label: &CStr, r: u8, g: u8, b: u8, flags: c::ImGuiColorEditFlags_) {
        let mut data = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0];
        // SAFETY: plain ImGui call; `data` has 3 floats.
        unsafe { c::ImGui_ColorEdit3(label.as_ptr(), data.as_mut_ptr(), flags as _) };
    }

    /// `ImGui_SelectableEx(label, false, None, .{0, 0})`.
    pub fn selectable(label: &CStr) -> bool {
        // SAFETY: plain ImGui call.
        unsafe {
            c::ImGui_SelectableEx(
                label.as_ptr(),
                false,
                c::ImGuiSelectableFlags_None as _,
                vec2(0.0, 0.0),
            )
        }
    }
}

/// `format!` + [`ui::text`] (Zig: `ImGui_Text(fmt, args...)`).
#[cfg(feature = "inspector")]
macro_rules! textf {
    ($($arg:tt)*) => { $crate::inspector::widgets::ui::text(&format!($($arg)*)) };
}
#[cfg(feature = "inspector")]
pub(crate) use textf;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detachable_header_defaults() {
        let h = DetachableHeader::default();
        assert!(!h.detached);
        assert!(h.dock);
        assert!(h.window_first);
    }
}
