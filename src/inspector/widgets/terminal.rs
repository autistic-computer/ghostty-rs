//! Port of `src/inspector/widgets/terminal.zig`.
//!
//! STATUS: ported (only compiled with the `inspector` feature)
//!
//! Deviation: `ScreenMap` (Zig: `std.EnumMap(ScreenSet.Key, screen.Info)`)
//! is an array of options indexed by the screen key.
#![cfg(feature = "inspector")]

use crate::inspector::widgets::termio::tag_name;
use crate::inspector::widgets::{self, help_marker, textf, ui, DetachableHeader};
use crate::pkg::dcimgui::c;
use crate::terminal::color::{DynamicRgb, Palette};
use crate::terminal::modes::Mode;
use crate::terminal::screen_set::Key as ScreenKey;
use crate::terminal::terminal::{MouseShiftCapture, Terminal};

/// All screen keys in declaration order (Zig: `std.meta.tags(Key)`).
const SCREEN_KEYS: [ScreenKey; 2] = [ScreenKey::Primary, ScreenKey::Alternate];

/// Terminal information inspector widget.
#[derive(Debug, Clone, Copy)]
pub struct Info {
    /// True if we're showing the 256-color palette window.
    pub show_palette: bool,

    /// The various detachable headers.
    pub misc_header: DetachableHeader,
    pub layout_header: DetachableHeader,
    pub mouse_header: DetachableHeader,
    pub color_header: DetachableHeader,
    pub modes_header: DetachableHeader,

    /// Screen detail windows for each screen key.
    pub screens: ScreenMap,
}

/// Screen detail windows for each screen key, indexed by the key.
pub type ScreenMap = [Option<widgets::screen::Info>; 2];

impl Info {
    pub fn empty() -> Info {
        Info {
            show_palette: false,
            misc_header: DetachableHeader::default(),
            layout_header: DetachableHeader::default(),
            mouse_header: DetachableHeader::default(),
            color_header: DetachableHeader::default(),
            modes_header: DetachableHeader::default(),
            screens: [None, None],
        }
    }

    /// Draw the terminal info window.
    pub fn draw(&mut self, open: bool, t: &mut Terminal) {
        // Draw our open state if we're open.
        if open {
            self.draw_open(t);
        }

        // Draw our detached state that draws regardless of if
        // we're open or not.
        if let Some(visible) = self.misc_header.window(c"Terminal Misc") {
            if visible {
                misc_table(t);
            }
            self.misc_header.window_end();
        }
        if let Some(visible) = self.layout_header.window(c"Terminal Layout") {
            if visible {
                layout_table(t);
            }
            self.layout_header.window_end();
        }
        if let Some(visible) = self.mouse_header.window(c"Terminal Mouse") {
            if visible {
                mouse_table(t);
            }
            self.mouse_header.window_end();
        }
        if let Some(visible) = self.color_header.window(c"Terminal Color") {
            if visible {
                color_table(t, &mut self.show_palette);
            }
            self.color_header.window_end();
        }
        if let Some(visible) = self.modes_header.window(c"Terminal Modes") {
            if visible {
                modes_table(t);
            }
            self.modes_header.window_end();
        }

        // SAFETY: requires the ImGui context the render callback set up.
        unsafe {
            // Palette pop-out window
            if self.show_palette {
                if c::ImGui_Begin(
                    c"256-Color Palette".as_ptr(),
                    &mut self.show_palette,
                    c::ImGuiWindowFlags_NoFocusOnAppearing as _,
                ) {
                    palette(c"palette", &t.colors.palette.current);
                }
                c::ImGui_End();
            }

            // Screen pop-out windows
            for key in SCREEN_KEYS {
                let Some(info) = self.screens[key as usize].as_mut() else {
                    continue;
                };
                let Some(mut screen) = t.screens.get(key) else {
                    // Could happen if we opened up a window for a screen
                    // and that screen was subsequently deinitialized. In
                    // this case, hide the window.
                    self.screens[key as usize] = None;
                    continue;
                };

                let title = ui::cstr(&format!("Screen: {}", tag_name(&key)));

                // Setup our next window so it has some size to it.
                let viewport = c::ImGui_GetMainViewport();
                c::ImGui_SetNextWindowSize(
                    ui::vec2(400f32.min((*viewport).Size.x), 300f32.min((*viewport).Size.y)),
                    c::ImGuiCond_FirstUseEver as _,
                );

                let mut screen_open: bool = true;
                let screen_draw = c::ImGui_Begin(
                    title.as_ptr(),
                    &mut screen_open,
                    c::ImGuiWindowFlags_NoFocusOnAppearing as _,
                );
                info.draw(
                    screen_draw,
                    widgets::screen::DrawData {
                        screen: &mut screen,
                        key,
                        active_key: t.screens.active_key,
                        modify_other_keys_2: t.flags.modify_other_keys_2,
                        color_palette: &t.colors.palette,
                    },
                );
                c::ImGui_End();

                // If the window was closed, remove it from our map so future
                // renders don't draw it.
                if !screen_open {
                    self.screens[key as usize] = None;
                }
            }
        }
    }

    fn draw_open(&mut self, t: &mut Terminal) {
        // Show our screens up top.
        screens_table(t, &mut self.screens);

        if self.misc_header.header(c"Misc") {
            misc_table(t);
        }
        if self.layout_header.header(c"Layout") {
            layout_table(t);
        }
        if self.mouse_header.header(c"Mouse") {
            mouse_table(t);
        }
        if self.color_header.header(c"Color") {
            color_table(t, &mut self.show_palette);
        }
        if self.modes_header.header(c"Modes") {
            modes_table(t);
        }
    }
}

/// Render the table of possible screens with various actions.
fn screens_table(t: &Terminal, map: &mut ScreenMap) {
    if !ui::begin_table(
        c"screens",
        3,
        c::ImGuiTableFlags_Borders | c::ImGuiTableFlags_RowBg | c::ImGuiTableFlags_SizingFixedFit,
    ) {
        return;
    }

    ui::setup_column(c"Screen", c::ImGuiTableColumnFlags_WidthFixed);
    ui::setup_column(c"Status", c::ImGuiTableColumnFlags_WidthFixed);
    ui::setup_column(c"", c::ImGuiTableColumnFlags_WidthFixed);

    // SAFETY: requires the ImGui context the render callback set up.
    unsafe {
        // Custom header row to include help marker before "Screen"
        {
            c::ImGui_TableNextRowEx(c::ImGuiTableRowFlags_Headers as _, 0.0);
            {
                ui::col(0);
                c::ImGui_PushStyleVarImVec2(c::ImGuiStyleVar_FramePadding as _, ui::vec2(0.0, 0.0));
                help_marker(
                    "A terminal can have multiple screens, only one of which is active at \
                     a time. Each screen has its own grid, contents, and other state. \
                     This section allows you to inspect the different screens managed by \
                     the terminal.",
                );
                c::ImGui_PopStyleVar();
                c::ImGui_SameLineEx(0.0, (*c::ImGui_GetStyle()).ItemInnerSpacing.x);
                c::ImGui_TableHeader(c"Screen".as_ptr());
            }
            {
                ui::col(1);
                c::ImGui_TableHeader(c"Status".as_ptr());
            }
            {
                ui::col(2);
                c::ImGui_TableHeader(c"".as_ptr());
            }
        }

        for key in SCREEN_KEYS {
            let is_initialized = t.screens.get(key).is_some();
            let is_active = t.screens.active_key == key;

            ui::row();
            {
                ui::col(0);
                ui::text(&tag_name(&key));
            }
            {
                ui::col(1);
                if is_active {
                    ui::text_colored(ui::vec4(0.4, 1.0, 0.4, 1.0), "active");
                } else if is_initialized {
                    ui::text_colored(ui::vec4(0.6, 0.6, 0.6, 1.0), "initialized");
                } else {
                    ui::text_colored(ui::vec4(0.4, 0.4, 0.4, 1.0), "(not initialized)");
                }
            }
            {
                ui::col(2);
                c::ImGui_PushIDInt(key as i32);
                c::ImGui_BeginDisabled(!is_initialized);
                if c::ImGui_Button(c"View".as_ptr()) && map[key as usize].is_none() {
                    map[key as usize] = Some(widgets::screen::Info::EMPTY);
                }
                c::ImGui_EndDisabled();
                c::ImGui_PopID();
            }
        }
    }

    ui::end_table();
}

/// Table of miscellaneous terminal information.
fn misc_table(t: &Terminal) {
    ui::begin_table(c"table_misc", 2, c::ImGuiTableFlags_None);

    {
        ui::row();
        ui::col(0);
        ui::text("Working Directory");
        ui::same_line();
        help_marker("The current working directory reported by the shell.");
        ui::col(1);
        if !t.pwd.is_empty() {
            ui::text(&String::from_utf8_lossy(&t.pwd));
        } else {
            ui::text_disabled("(none)");
        }
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Focused");
        ui::same_line();
        help_marker("Whether the terminal itself is currently focused.");
        ui::col(1);
        ui::checkbox(c"##focused", t.flags.focused);
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Previous Char");
        ui::same_line();
        help_marker("The previously printed character, used only for the REP sequence.");
        ui::col(1);
        if let Some(c) = t.previous_char {
            textf!("U+{c:04X}");
        } else {
            ui::text_disabled("(none)");
        }
    }

    ui::end_table();
}

/// Table of terminal layout information.
fn layout_table(t: &mut Terminal) {
    ui::begin_table(c"table_layout", 2, c::ImGuiTableFlags_None);

    {
        ui::row();
        ui::col(0);
        ui::text("Grid");
        ui::same_line();
        help_marker("The size of the terminal grid in columns and rows.");
        ui::col(1);
        textf!("{}c x {}r", t.cols, t.rows);
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Pixels");
        ui::same_line();
        help_marker("The size of the terminal grid in pixels.");
        ui::col(1);
        textf!("{}w x {}h", t.width_px, t.height_px);
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Scroll Region");
        ui::same_line();
        help_marker("The scrolling region boundaries (top, bottom, left, right).");
        ui::col(1);

        // SAFETY: requires the ImGui context the render callback set up.
        unsafe {
            c::ImGui_PushItemWidth(c::ImGui_CalcTextSize(c"00000".as_ptr()).x);

            let mut over = t.scrolling_region;
            let mut changed = false;

            c::ImGui_AlignTextToFramePadding();
            ui::text("T:");
            c::ImGui_SameLine();
            if c::ImGui_InputScalar(
                c"##scroll_top".as_ptr(),
                c::ImGuiDataType_U16 as _,
                (&mut over.top as *mut u16).cast(),
            ) {
                over.top = over.top.min(t.rows.saturating_sub(1));
                changed = true;
            }

            c::ImGui_SameLine();
            ui::text("B:");
            c::ImGui_SameLine();
            if c::ImGui_InputScalar(
                c"##scroll_bottom".as_ptr(),
                c::ImGuiDataType_U16 as _,
                (&mut over.bottom as *mut u16).cast(),
            ) {
                over.bottom = over.bottom.min(t.rows.saturating_sub(1));
                changed = true;
            }

            c::ImGui_SameLine();
            ui::text("L:");
            c::ImGui_SameLine();
            if c::ImGui_InputScalar(
                c"##scroll_left".as_ptr(),
                c::ImGuiDataType_U16 as _,
                (&mut over.left as *mut u16).cast(),
            ) {
                over.left = over.left.min(t.cols.saturating_sub(1));
                changed = true;
            }

            c::ImGui_SameLine();
            ui::text("R:");
            c::ImGui_SameLine();
            if c::ImGui_InputScalar(
                c"##scroll_right".as_ptr(),
                c::ImGuiDataType_U16 as _,
                (&mut over.right as *mut u16).cast(),
            ) {
                over.right = over.right.min(t.cols.saturating_sub(1));
                changed = true;
            }

            if changed && over.top < over.bottom && over.left < over.right {
                t.scrolling_region = over;
            }

            c::ImGui_PopItemWidth();
        }
    }

    ui::end_table();
}

/// Table of mouse-related terminal information.
fn mouse_table(t: &Terminal) {
    ui::begin_table(c"table_mouse", 2, c::ImGuiTableFlags_None);

    {
        ui::row();
        ui::col(0);
        ui::text("Event Mode");
        ui::same_line();
        help_marker("The mouse event reporting mode set by the application.");
        ui::col(1);
        ui::text(&tag_name(&t.flags.mouse_event));
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Format");
        ui::same_line();
        help_marker("The mouse event encoding format.");
        ui::col(1);
        ui::text(&tag_name(&t.flags.mouse_format));
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Shape");
        ui::same_line();
        help_marker("The current mouse cursor shape set by the application.");
        ui::col(1);
        ui::text(&tag_name(&t.mouse_shape));
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Shift Capture");
        ui::same_line();
        help_marker("XTSHIFTESCAPE state for capturing shift in mouse protocol.");
        ui::col(1);
        if t.flags.mouse_shift_capture == MouseShiftCapture::Null {
            ui::text_disabled("(unset)");
        } else {
            ui::text(&tag_name(&t.flags.mouse_shift_capture));
        }
    }

    ui::end_table();
}

/// Table of color-related terminal information.
fn color_table(t: &Terminal, show_palette: &mut bool) {
    ui::text_wrapped(
        "Color state for the terminal. Note these colors only apply \
         to the palette and unstyled colors. Many modern terminal \
         applications use direct RGB colors which are not reflected here.",
    );
    ui::separator();

    ui::begin_table(c"table_color", 2, c::ImGuiTableFlags_None);

    {
        ui::row();
        ui::col(0);
        ui::text("Background");
        ui::same_line();
        help_marker("Unstyled cell background color.");
        ui::col(1);
        dynamic_rgb(c"bg_color", &t.colors.background);
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Foreground");
        ui::same_line();
        help_marker("Unstyled cell foreground color.");
        ui::col(1);
        dynamic_rgb(c"fg_color", &t.colors.foreground);
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Cursor");
        ui::same_line();
        help_marker("Cursor coloring set by escape sequences.");
        ui::col(1);
        dynamic_rgb(c"cursor_color", &t.colors.cursor);
    }

    {
        ui::row();
        ui::col(0);
        ui::text("Palette");
        ui::same_line();
        help_marker("The 256-color palette.");
        ui::col(1);
        // SAFETY: requires the ImGui context the render callback set up.
        if unsafe { c::ImGui_Button(c"View".as_ptr()) } {
            *show_palette = true;
        }
    }

    ui::end_table();
}

/// Table of terminal modes.
fn modes_table(t: &Terminal) {
    ui::begin_table(
        c"table_modes",
        3,
        c::ImGuiTableFlags_SizingFixedFit | c::ImGuiTableFlags_RowBg,
    );

    {
        ui::setup_column(c"", c::ImGuiTableColumnFlags_NoResize);
        ui::setup_column(c"Number", c::ImGuiTableColumnFlags_PreferSortAscending);
        ui::setup_column(c"Name", c::ImGuiTableColumnFlags_WidthStretch);
        // SAFETY: requires an open table.
        unsafe { c::ImGui_TableHeadersRow() };
    }

    for mode in Mode::all() {
        let tag = mode.tag();

        ui::row();
        // SAFETY: requires the ImGui context the render callback set up.
        unsafe { c::ImGui_PushIDInt(mode as u16 as i32) };
        {
            ui::col(0);
            ui::checkbox(c"##checkbox", t.modes.get(mode));
        }
        {
            ui::col(1);
            textf!("{}{}", if tag.ansi { "" } else { "?" }, tag.value as u32);
        }
        {
            ui::col(2);
            ui::text(mode.name());
        }
        // SAFETY: paired with PushIDInt.
        unsafe { c::ImGui_PopID() };
    }

    ui::end_table();
}

/// Render a DynamicRGB color.
fn dynamic_rgb(label: &std::ffi::CStr, rgb: &DynamicRgb) -> bool {
    ui::begin_table(
        label,
        if rgb.r#override.is_some() { 2 } else { 1 },
        c::ImGuiTableFlags_SizingFixedFit,
    );

    if rgb.r#override.is_some() {
        ui::setup_column(c"##label", c::ImGuiTableColumnFlags_WidthFixed);
    }
    ui::setup_column(c"##value", c::ImGuiTableColumnFlags_WidthStretch);

    if let Some(c) = rgb.r#override {
        ui::row();
        ui::col(0);
        ui::text("override:");
        ui::same_line();
        help_marker("Overridden color set by escape sequences.");

        ui::col(1);
        ui::color_edit3(c"##override", c.r, c.g, c.b, c::ImGuiColorEditFlags_None);
    }

    ui::row();
    ui::col(0);
    if let Some(c) = rgb.default {
        if rgb.r#override.is_some() {
            ui::text("default:");
            ui::same_line();
            help_marker("Default color from configuration.");

            ui::col(1);
        }

        ui::color_edit3(c"##default", c.r, c.g, c.b, c::ImGuiColorEditFlags_None);
    } else {
        ui::text_disabled("(unset)");
    }

    ui::end_table();
    false
}

/// Render a color palette as a 16x16 grid of color buttons.
fn palette(label: &std::ffi::CStr, pal: &Palette) {
    // SAFETY: requires the ImGui context the render callback set up.
    unsafe {
        c::ImGui_PushID(label.as_ptr());

        for row in 0..16usize {
            for col in 0..16usize {
                let idx = row * 16 + col;
                let rgb = pal[idx];

                if col > 0 {
                    c::ImGui_SameLine();
                }

                c::ImGui_PushIDInt(idx as i32);
                ui::color_edit3(c"##color", rgb.r, rgb.g, rgb.b, c::ImGuiColorEditFlags_NoInputs);
                if c::ImGui_IsItemHovered(c::ImGuiHoveredFlags_DelayShort as _) {
                    ui::set_tooltip(&format!("{}: #{:02X}{:02X}{:02X}", idx, rgb.r, rgb.g, rgb.b));
                }
                c::ImGui_PopID();
            }
        }

        c::ImGui_PopID();
    }
}
