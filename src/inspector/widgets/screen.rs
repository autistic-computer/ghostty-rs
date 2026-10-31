//! Port of `src/inspector/widgets/screen.zig`.
//!
//! STATUS: ported (only compiled with the `inspector` feature)
#![cfg(feature = "inspector")]

use std::ffi::CStr;

use crate::inspector::units;
use crate::inspector::widgets::termio::tag_name;
use crate::inspector::widgets::{self, help_marker, textf, ui};
use crate::pkg::dcimgui::{self as cimgui, c};
use crate::terminal::color::{DynamicPalette, Palette};
use crate::terminal::kitty::graphics_storage::ImageStorage;
use crate::terminal::page_list::LimitKey;
use crate::terminal::screen::{Cursor, Screen, SemanticClick, SemanticPrompt};
use crate::terminal::screen_set::Key as ScreenKey;

/// Window names for the screen dockspace.
const WINDOW_INFO: &CStr = c"Info";
const WINDOW_CELL: &CStr = c"Cell";
const WINDOW_PAGELIST: &CStr = c"PageList";

/// The data [`Info::draw`] inspects.
pub struct DrawData<'a> {
    /// The screen that we're inspecting.
    pub screen: &'a mut Screen,

    /// Which screen key we're viewing.
    pub key: ScreenKey,

    /// Which screen is active (primary or alternate).
    pub active_key: ScreenKey,

    /// Whether xterm modify other keys mode 2 is enabled.
    pub modify_other_keys_2: bool,

    /// Color palette for cursor color resolution.
    pub color_palette: &'a DynamicPalette,
}

/// Screen information inspector widget.
#[derive(Debug, Clone, Copy)]
pub struct Info {
    pub pagelist: widgets::pagelist::Inspector,
    pub cell_chooser: widgets::pagelist::CellChooser,
}

impl Info {
    pub const EMPTY: Info = Info {
        pagelist: widgets::pagelist::Inspector::EMPTY,
        cell_chooser: widgets::pagelist::CellChooser::EMPTY,
    };

    /// Draw the screen info contents.
    pub fn draw(&mut self, open: bool, data: DrawData<'_>) {
        // SAFETY: requires the ImGui context the render callback set up.
        unsafe {
            // Create the dockspace for this screen
            let dockspace_id = c::ImGui_GetID(c"Screen Dockspace".as_ptr());
            let _ = Self::create_dock_space(dockspace_id);

            let screen = data.screen;

            // Info window
            if c::ImGui_Begin(
                WINDOW_INFO.as_ptr(),
                std::ptr::null_mut(),
                c::ImGuiWindowFlags_NoFocusOnAppearing as _,
            ) {
                if ui::collapsing_header(c"Cursor", c::ImGuiTreeNodeFlags_DefaultOpen) {
                    cursor_table(&screen.cursor);
                    ui::separator();
                    cursor_style(&screen.cursor, Some(&data.color_palette.current));
                }

                if ui::collapsing_header(c"Keyboard", c::ImGuiTreeNodeFlags_DefaultOpen) {
                    keyboard_table(screen, data.modify_other_keys_2);
                }

                if ui::collapsing_header(c"Semantic Prompt", c::ImGuiTreeNodeFlags_DefaultOpen) {
                    semantic_prompt_table(&screen.semantic_prompt);
                }

                if ui::collapsing_header(c"Kitty Graphics", c::ImGuiTreeNodeFlags_DefaultOpen) {
                    kitty_graphics_table(&screen.kitty_images);
                }

                if ui::collapsing_header(c"Other Screen State", c::ImGuiTreeNodeFlags_DefaultOpen) {
                    internal_state_table(screen);
                }
            }
            c::ImGui_End();

            // Cell window
            if c::ImGui_Begin(
                WINDOW_CELL.as_ptr(),
                std::ptr::null_mut(),
                c::ImGuiWindowFlags_NoFocusOnAppearing as _,
            ) {
                self.cell_chooser.draw(&screen.pages);
            }
            c::ImGui_End();

            // PageList window
            if c::ImGui_Begin(
                WINDOW_PAGELIST.as_ptr(),
                std::ptr::null_mut(),
                c::ImGuiWindowFlags_NoFocusOnAppearing as _,
            ) {
                self.pagelist.draw(&mut screen.pages);
            }
            c::ImGui_End();
        }

        // The remainder is the open state
        if !open {
            return;
        }

        // Show warning if viewing an inactive screen
        if data.key != data.active_key {
            ui::text_colored(ui::vec4(1.0, 0.8, 0.0, 1.0), "⚠ Viewing inactive screen");
            ui::separator();
        }
    }

    /// Create the dock space for the screen inspector. This creates
    /// a dedicated dock space for the screen inspector windows. But they
    /// can of course be undocked and moved around as desired.
    fn create_dock_space(dockspace_id: c::ImGuiID) -> bool {
        // SAFETY: requires the ImGui context the render callback set up.
        unsafe {
            // Check if we need to set up the dockspace
            let setup = cimgui::ImGui_DockBuilderGetNode(dockspace_id).is_null();

            if setup {
                // Register our dockspace node
                let id = cimgui::ImGui_DockBuilderAddNodeEx(
                    dockspace_id,
                    cimgui::ImGuiDockNodeFlagsPrivate::DockSpace as _,
                );
                debug_assert_eq!(id, dockspace_id);

                // Dock windows into the space
                cimgui::ImGui_DockBuilderDockWindow(WINDOW_INFO.as_ptr(), dockspace_id);
                cimgui::ImGui_DockBuilderDockWindow(WINDOW_CELL.as_ptr(), dockspace_id);
                cimgui::ImGui_DockBuilderDockWindow(WINDOW_PAGELIST.as_ptr(), dockspace_id);
                cimgui::ImGui_DockBuilderFinish(dockspace_id);
            }

            // Create the dockspace
            let id = c::ImGui_DockSpaceEx(
                dockspace_id,
                ui::vec2(0.0, 0.0),
                c::ImGuiDockNodeFlags_None as _,
                std::ptr::null(),
            );
            debug_assert_eq!(id, dockspace_id);
            setup
        }
    }
}

/// Render cursor state with a table of cursor-specific fields.
pub fn cursor_table(cursor: &Cursor) {
    if !ui::begin_table(c"table_cursor", 2, c::ImGuiTableFlags_None) {
        return;
    }

    ui::row();
    ui::col(0);
    ui::text("Position (x, y)");
    ui::same_line();
    help_marker("The current cursor position in the terminal grid (0-indexed).");
    ui::col(1);
    textf!("({}, {})", cursor.x, cursor.y);

    ui::row();
    ui::col(0);
    ui::text("Hyperlink");
    ui::same_line();
    help_marker("The active OSC8 hyperlink for newly printed characters.");
    ui::col(1);
    if let Some(link) = &cursor.hyperlink {
        ui::text(&String::from_utf8_lossy(&link.uri));
    } else {
        ui::text_disabled("(none)");
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Pending Wrap");
        ui::same_line();
        help_marker(
            "The 'last column flag' (LCF). If set, the next character will force a soft-wrap to the next line.",
        );
        ui::col(1);
        ui::checkbox(c"##pending_wrap", cursor.pending_wrap);
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Protected");
        ui::same_line();
        help_marker(
            "If enabled, new characters will have the protected attribute set, preventing erasure by certain sequences.",
        );
        ui::col(1);
        ui::checkbox(c"##protected", cursor.protected);
    }

    ui::end_table();
}

/// Render cursor style information using the shared style table.
pub fn cursor_style(cursor: &Cursor, palette: Option<&Palette>) {
    widgets::style::table(cursor.style, palette);
}

/// Render keyboard information with a table.
fn keyboard_table(screen: &Screen, modify_other_keys_2: bool) {
    if !ui::begin_table(c"table_keyboard", 2, c::ImGuiTableFlags_None) {
        return;
    }

    let kitty_flags = screen.kitty_keyboard.current();

    {
        ui::row();
        ui::col(0);
        ui::text("Mode");
        ui::col(1);
        let mode = if kitty_flags.int() != 0 { "kitty" } else { "legacy" };
        ui::text(mode);
    }

    if kitty_flags.int() != 0 {
        let fields = [
            ("disambiguate", kitty_flags.disambiguate),
            ("report_events", kitty_flags.report_events),
            ("report_alternates", kitty_flags.report_alternates),
            ("report_all", kitty_flags.report_all),
            ("report_associated", kitty_flags.report_associated),
        ];
        for (field_name, value) in fields {
            ui::row();
            ui::col(0);
            ui::text(field_name);
            ui::col(1);
            ui::text(if value { "true" } else { "false" });
        }
    } else {
        ui::row();
        ui::col(0);
        ui::text("Xterm modify keys");
        ui::col(1);
        ui::text(if modify_other_keys_2 { "true" } else { "false" });
    } // keyboard mode info

    ui::end_table();
}

/// Render kitty graphics information table.
pub fn kitty_graphics_table(kitty_images: &ImageStorage) {
    if !kitty_images.enabled() {
        ui::text_disabled("(Kitty graphics are disabled)");
        return;
    }

    if !ui::begin_table(c"##kitty_graphics", 2, c::ImGuiTableFlags_None) {
        return;
    }

    ui::row();
    ui::col(0);
    ui::text("Memory Usage");
    ui::col(1);
    textf!(
        "{} bytes ({} KiB)",
        kitty_images.total_bytes,
        units::to_kibi_bytes(kitty_images.total_bytes)
    );

    ui::row();
    ui::col(0);
    ui::text("Memory Limit");
    ui::col(1);
    textf!(
        "{} bytes ({} KiB)",
        kitty_images.total_limit,
        units::to_kibi_bytes(kitty_images.total_limit)
    );

    ui::row();
    ui::col(0);
    ui::text("Image Count");
    ui::col(1);
    textf!("{}", kitty_images.images.len());

    ui::row();
    ui::col(0);
    ui::text("Placement Count");
    ui::col(1);
    textf!("{}", kitty_images.placements.len());

    ui::row();
    ui::col(0);
    ui::text("Image Loading");
    ui::col(1);
    ui::text(if kitty_images.loading.is_some() { "true" } else { "false" });

    ui::end_table();
}

/// Render internal terminal state table.
pub fn internal_state_table(screen: &Screen) {
    let pages = &screen.pages;

    if !ui::begin_table(c"##terminal_state", 2, c::ImGuiTableFlags_None) {
        return;
    }

    ui::row();
    ui::col(0);
    ui::text("Memory Usage");
    ui::col(1);
    textf!(
        "{} bytes ({} KiB)",
        pages.page_size,
        units::to_kibi_bytes(pages.page_size)
    );

    ui::row();
    ui::col(0);
    ui::text("Memory Limit");
    ui::col(1);
    let max_bytes = pages.limits.max(LimitKey::Bytes);
    textf!("{} bytes ({} KiB)", max_bytes, units::to_kibi_bytes(max_bytes));

    ui::row();
    ui::col(0);
    ui::text("Viewport Location");
    ui::col(1);
    ui::text(&tag_name(&pages.viewport));

    ui::end_table();
}

/// Render semantic prompt state table.
pub fn semantic_prompt_table(semantic_prompt: &SemanticPrompt) {
    if !ui::begin_table(c"##semantic_prompt", 2, c::ImGuiTableFlags_None) {
        return;
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Seen");
        ui::same_line();
        help_marker("Whether any semantic prompt markers (OSC 133) have been seen in this screen.");
        ui::col(1);
        ui::checkbox(c"##seen", semantic_prompt.seen);
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Click Handling");
        ui::same_line();
        help_marker("How click events are handled in prompts. Set via 'cl' or 'click_events' options in OSC 133.");
        ui::col(1);
        match semantic_prompt.click {
            SemanticClick::None => ui::text_disabled("(none)"),
            SemanticClick::ClickEvents(click_events) => textf!("click_events={}", tag_name(&click_events)),
            SemanticClick::Cl(cl) => textf!("cl={}", tag_name(&cl)),
        }
    }

    ui::end_table();
}
