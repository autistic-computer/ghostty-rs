//! Port of `src/inspector/widgets/surface.zig`.
//!
//! STATUS: ported
//!
//! Deviation: without the `inspector` feature the GUI state only holds the
//! recorded data (key events, VT events, renderer overlay features); the
//! purely visual state (`surface_info`, `terminal_info`,
//! `show_demo_window`) and all drawing need the feature.

use crate::inspector::widgets;
use crate::terminal::page_list::Pin;

#[cfg(feature = "inspector")]
use crate::inspector::widgets::{textf, ui};
#[cfg(feature = "inspector")]
use crate::pkg::dcimgui::{self as cimgui, c};
#[cfg(feature = "inspector")]
use crate::surface::Surface;

/// This is discovered via the hardcoded string in the ImGui demo window.
#[allow(dead_code)]
const WINDOW_IMGUI_DEMO: &std::ffi::CStr = c"Dear ImGui Demo";
#[allow(dead_code)]
const WINDOW_KEYBOARD: &std::ffi::CStr = c"Keyboard";
#[allow(dead_code)]
const WINDOW_TERMINAL: &std::ffi::CStr = c"Terminal";
#[allow(dead_code)]
const WINDOW_SURFACE: &std::ffi::CStr = c"Surface";
#[allow(dead_code)]
const WINDOW_TERMIO: &std::ffi::CStr = c"Terminal IO";
#[allow(dead_code)]
const WINDOW_RENDERER: &std::ffi::CStr = c"Renderer";

#[derive(Debug)]
pub struct Inspector {
    /// Internal GUI state
    #[cfg(feature = "inspector")]
    pub surface_info: Info,
    pub key_stream: widgets::key::Stream,
    #[cfg(feature = "inspector")]
    pub terminal_info: widgets::terminal::Info,
    pub vt_stream: widgets::termio::Stream,
    pub renderer_info: widgets::renderer::Info,
    #[cfg(feature = "inspector")]
    pub show_demo_window: bool,
}

impl Inspector {
    pub fn init() -> Inspector {
        Inspector {
            #[cfg(feature = "inspector")]
            surface_info: Info::EMPTY,
            key_stream: widgets::key::Stream::init(),
            #[cfg(feature = "inspector")]
            terminal_info: widgets::terminal::Info::empty(),
            vt_stream: widgets::termio::Stream::init(),
            renderer_info: widgets::renderer::Info::EMPTY,
            #[cfg(feature = "inspector")]
            show_demo_window: true,
        }
    }
}

#[cfg(feature = "inspector")]
impl Inspector {
    /// Draw the inspector.
    ///
    /// `surface` is a raw pointer because the inspector itself is owned by
    /// the surface (`surface.inspector`).
    ///
    /// # Safety
    /// `surface` must be a live core surface and an ImGui context must be
    /// current.
    pub unsafe fn draw(&mut self, surface: *mut Surface, mouse: Mouse) {
        // SAFETY: the caller guarantees the surface is live.
        let surface = unsafe { &mut *surface };

        // SAFETY: the caller guarantees an ImGui context.
        unsafe {
            // Create our dockspace first. If we had to setup our dockspace,
            // then it is a first render.
            let dockspace_id = c::ImGui_GetID(c"Main Dockspace".as_ptr());
            let first_render = create_dock_space(dockspace_id);

            // Draw everything that requires the terminal state mutex.
            {
                let mutex = surface.renderer_state.mutex;
                mutex.as_ref().lock_uncancelable(crate::lib::tinyio::TinyIo);
                let t: &mut crate::terminal::terminal::Terminal = &mut *surface.renderer_state.terminal.as_ptr();

                // Terminal info window
                {
                    let open = c::ImGui_Begin(
                        WINDOW_TERMINAL.as_ptr(),
                        std::ptr::null_mut(),
                        c::ImGuiWindowFlags_NoFocusOnAppearing as _,
                    );
                    self.terminal_info.draw(open, t);
                    c::ImGui_End();
                }

                // Surface info window
                {
                    let open = c::ImGui_Begin(
                        WINDOW_SURFACE.as_ptr(),
                        std::ptr::null_mut(),
                        c::ImGuiWindowFlags_NoFocusOnAppearing as _,
                    );
                    self.surface_info.draw(open, surface, t, &mouse);
                    c::ImGui_End();
                }

                // Keyboard info window
                {
                    let open = c::ImGui_Begin(
                        WINDOW_KEYBOARD.as_ptr(),
                        std::ptr::null_mut(),
                        c::ImGuiWindowFlags_NoFocusOnAppearing as _,
                    );
                    self.key_stream.draw(open);
                    c::ImGui_End();
                }

                // Terminal IO window
                {
                    let open = c::ImGui_Begin(
                        WINDOW_TERMIO.as_ptr(),
                        std::ptr::null_mut(),
                        c::ImGuiWindowFlags_NoFocusOnAppearing as _,
                    );
                    if open {
                        let palette = t.colors.palette.current;
                        self.vt_stream.draw(surface, &palette);
                    }
                    c::ImGui_End();
                }

                // Renderer info window
                {
                    let open = c::ImGui_Begin(
                        WINDOW_RENDERER.as_ptr(),
                        std::ptr::null_mut(),
                        c::ImGuiWindowFlags_NoFocusOnAppearing as _,
                    );
                    self.renderer_info.draw(open);
                    c::ImGui_End();
                }

                mutex.as_ref().unlock(crate::lib::tinyio::TinyIo);
            }

            // In debug we show the ImGui demo window so we can easily view
            // available widgets and such.
            if cfg!(debug_assertions) && self.show_demo_window {
                c::ImGui_ShowDemoWindow(&mut self.show_demo_window);
            }

            if first_render {
                // On first render, setup our initial focus state. We only
                // do this on first render so that we can let the user change
                // focus afterward without it snapping back.
                c::ImGui_SetWindowFocusStr(WINDOW_TERMINAL.as_ptr());
            }
        }
    }
}

/// Create the global dock space for the inspector. A dock space
/// is a special area where windows can be docked into. The global
/// dock space fills the entire main viewport.
///
/// Returns true if this was the first time the dock space was created.
#[cfg(feature = "inspector")]
fn create_dock_space(dockspace_id: c::ImGuiID) -> bool {
    // SAFETY: requires the ImGui context the render callback set up.
    unsafe {
        let viewport: *mut c::ImGuiViewport = c::ImGui_GetMainViewport();

        // Initial Docking setup
        let setup = cimgui::ImGui_DockBuilderGetNode(dockspace_id).is_null();
        if setup {
            // Register our dockspace node
            let id = cimgui::ImGui_DockBuilderAddNodeEx(
                dockspace_id,
                cimgui::ImGuiDockNodeFlagsPrivate::DockSpace as _,
            );
            debug_assert_eq!(id, dockspace_id);

            // Ensure it is the full size of the viewport
            cimgui::ImGui_DockBuilderSetNodeSize(dockspace_id, (*viewport).Size);

            // We only initialize one central docking point now but
            // this is the point we'd pre-split and so on for the initial
            // layout.
            let dock_id_main: c::ImGuiID = dockspace_id;
            cimgui::ImGui_DockBuilderDockWindow(WINDOW_IMGUI_DEMO.as_ptr(), dock_id_main);
            cimgui::ImGui_DockBuilderDockWindow(WINDOW_TERMINAL.as_ptr(), dock_id_main);
            cimgui::ImGui_DockBuilderDockWindow(WINDOW_SURFACE.as_ptr(), dock_id_main);
            cimgui::ImGui_DockBuilderDockWindow(WINDOW_KEYBOARD.as_ptr(), dock_id_main);
            cimgui::ImGui_DockBuilderDockWindow(WINDOW_TERMIO.as_ptr(), dock_id_main);
            cimgui::ImGui_DockBuilderDockWindow(WINDOW_RENDERER.as_ptr(), dock_id_main);
            cimgui::ImGui_DockBuilderFinish(dockspace_id);
        }

        // Put the dockspace over the viewport.
        let id = c::ImGui_DockSpaceOverViewportEx(
            dockspace_id,
            viewport,
            c::ImGuiDockNodeFlags_PassthruCentralNode as _,
            std::ptr::null(),
        );
        debug_assert_eq!(id, dockspace_id);
        setup
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Mouse {
    /// Last hovered x/y
    pub last_xpos: f64,
    pub last_ypos: f64,

    // Last hovered screen point
    pub last_point: Option<Pin>,
}

/// Surface information inspector widget.
#[cfg(feature = "inspector")]
#[derive(Debug, Default, Clone, Copy)]
pub struct Info;

#[cfg(feature = "inspector")]
impl Info {
    pub const EMPTY: Info = Info;

    /// Draw the surface info window. `t` is the surface's terminal, which
    /// the caller has locked.
    pub fn draw(
        &mut self,
        open: bool,
        surface: &Surface,
        t: &crate::terminal::terminal::Terminal,
        mouse: &Mouse,
    ) {
        if !open {
            return;
        }

        if ui::collapsing_header(c"Help", c::ImGuiTreeNodeFlags_None) {
            ui::text_wrapped(
                "This window displays information about the surface (window). \
                 A surface is the graphical area that displays the terminal \
                 content. It includes dimensions, font sizing, and mouse state \
                 information specific to this window instance.",
            );
        }

        ui::separator_text(c"Dimensions");
        dimensions_table(surface);

        ui::separator_text(c"Font");
        font_table(surface);

        ui::separator_text(c"Mouse");
        mouse_table(surface, t, mouse);
    }
}

#[cfg(feature = "inspector")]
fn dimensions_table(surface: &Surface) {
    ui::begin_table(c"table_size", 2, c::ImGuiTableFlags_None);

    // Screen Size
    ui::row();
    ui::col(0);
    ui::text("Screen Size");
    ui::col(1);
    textf!(
        "{}px x {}px",
        surface.size.screen.width,
        surface.size.screen.height
    );

    // Grid Size
    ui::row();
    ui::col(0);
    ui::text("Grid Size");
    ui::col(1);
    let grid_size = surface.size.grid();
    textf!("{}c x {}r", grid_size.columns, grid_size.rows);

    // Cell Size
    ui::row();
    ui::col(0);
    ui::text("Cell Size");
    ui::col(1);
    textf!("{}px x {}px", surface.size.cell.width, surface.size.cell.height);

    // Padding
    ui::row();
    ui::col(0);
    ui::text("Window Padding");
    ui::col(1);
    textf!(
        "T={} B={} L={} R={} px",
        surface.size.padding.top,
        surface.size.padding.bottom,
        surface.size.padding.left,
        surface.size.padding.right,
    );

    ui::end_table();
}

#[cfg(feature = "inspector")]
fn font_table(surface: &Surface) {
    ui::begin_table(c"table_font", 2, c::ImGuiTableFlags_None);

    ui::row();
    ui::col(0);
    ui::text("Size (Points)");
    ui::col(1);
    textf!("{:.2} pt", surface.font_size.points);

    ui::row();
    ui::col(0);
    ui::text("Size (Pixels)");
    ui::col(1);
    textf!("{:.2} px", surface.font_size.pixels());

    ui::end_table();
}

#[cfg(feature = "inspector")]
fn mouse_table(surface: &Surface, t: &crate::terminal::terminal::Terminal, mouse: &Mouse) {
    use crate::input::{MouseButton, MouseButtonState};
    use crate::renderer::size::{Coordinate, CoordinateSurface, CoordinateTag};
    use crate::terminal::point::{Coordinate as PointCoordinate, Tag};

    ui::begin_table(c"table_mouse", 2, c::ImGuiTableFlags_None);

    let surface_mouse = &surface.mouse;

    {
        let hover_point: PointCoordinate = mouse
            .last_point
            .and_then(|p| t.screens.active.pages.point_from_pin(Tag::Active, p))
            .map(|pt| pt.coord())
            .unwrap_or_default();

        ui::row();
        ui::col(0);
        ui::text("Hover Grid");
        ui::col(1);
        textf!("row={}, col={}", hover_point.y, hover_point.x);
    }

    {
        let coord = match (Coordinate::Surface(CoordinateSurface {
            x: mouse.last_xpos,
            y: mouse.last_ypos,
        }))
        .convert(CoordinateTag::Terminal, surface.size)
        {
            Coordinate::Terminal(v) => v,
            _ => unreachable!(),
        };

        ui::row();
        ui::col(0);
        ui::text("Hover Point");
        ui::col(1);
        textf!("({}px, {}px)", coord.x as i64, coord.y as i64);
    }

    let any_click = surface_mouse
        .click_state
        .iter()
        .any(|state| *state == MouseButtonState::Press);

    {
        ui::row();
        ui::col(0);
        ui::text("Click State");
        ui::col(1);
        if !any_click {
            ui::text("none");
        } else {
            for (i, state) in surface_mouse.click_state.iter().enumerate() {
                if *state != MouseButtonState::Press {
                    continue;
                }
                let button = MouseButton::ALL.iter().copied().find(|b| *b as usize == i);
                ui::same_line();
                ui::text(match button {
                    None | Some(MouseButton::Unknown) => "?",
                    Some(MouseButton::Left) => "L",
                    Some(MouseButton::Middle) => "M",
                    Some(MouseButton::Right) => "R",
                    Some(MouseButton::Four) => "{4}",
                    Some(MouseButton::Five) => "{5}",
                    Some(MouseButton::Six) => "{6}",
                    Some(MouseButton::Seven) => "{7}",
                    Some(MouseButton::Eight) => "{8}",
                    Some(MouseButton::Nine) => "{9}",
                    Some(MouseButton::Ten) => "{10}",
                    Some(MouseButton::Eleven) => "{11}",
                });
            }
        }
    }

    {
        let left_click_point: PointCoordinate = surface_mouse
            .selection_gesture
            .validated_left_click_pin(&t.screens)
            // SAFETY: a validated pin is tracked by the active screen.
            .and_then(|p| t.screens.active.pages.point_from_pin(Tag::Active, unsafe { *p.as_ptr() }))
            .map(|pt| pt.coord())
            .unwrap_or_default();

        ui::row();
        ui::col(0);
        ui::text("Click Grid");
        ui::col(1);
        textf!("row={}, col={}", left_click_point.y, left_click_point.x);
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Click Point");
        ui::col(1);
        textf!(
            "({}px, {}px)",
            surface_mouse.selection_gesture.left_click_xpos as u32,
            surface_mouse.selection_gesture.left_click_ypos as u32,
        );
    }

    ui::end_table();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspector_init() {
        let insp = Inspector::init();
        assert!(insp.key_stream.events.empty());
        assert!(insp.vt_stream.events.empty());
        assert!(insp.renderer_info.features.is_empty());
        let m = Mouse::default();
        assert!(m.last_point.is_none());
    }
}
