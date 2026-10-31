//! Port of `src/inspector/widgets/style.zig`.
//!
//! STATUS: ported (only compiled with the `inspector` feature)
#![cfg(feature = "inspector")]

use std::ffi::CStr;

use crate::inspector::widgets::{help_marker, textf, ui};
use crate::pkg::dcimgui::c;
use crate::terminal::color::Palette;
use crate::terminal::style::{Color, Style};

/// Render a style as a table.
pub fn table(st: Style, palette: Option<&Palette>) {
    {
        ui::begin_table(c"style", 2, c::ImGuiTableFlags_None);
        {
            ui::row();
            ui::col(0);
            ui::text("Foreground");
            ui::same_line();
            help_marker("The foreground (text) color");
            ui::col(1);
            color(c"fg", st.fg_color, palette);
        }

        {
            ui::row();
            ui::col(0);
            ui::text("Background");
            ui::same_line();
            help_marker("The background (cell) color");
            ui::col(1);
            color(c"bg", st.bg_color, palette);
        }

        {
            ui::row();
            ui::col(0);
            ui::text("Underline");
            ui::same_line();
            help_marker("The underline color, if underlines are enabled.");
            ui::col(1);
            color(c"underline", st.underline_color, palette);
        }

        let style_flags: [(&str, &str, bool); 7] = [
            ("bold", "Text will be rendered with bold weight.", st.flags.bold),
            ("italic", "Text will be rendered in italic style.", st.flags.italic),
            ("faint", "Text will be rendered with reduced intensity.", st.flags.faint),
            ("blink", "Text will blink.", st.flags.blink),
            ("inverse", "Foreground and background colors are swapped.", st.flags.inverse),
            ("invisible", "Text will be invisible (hidden).", st.flags.invisible),
            ("strikethrough", "Text will have a line through it.", st.flags.strikethrough),
        ];
        for (style, help, set) in style_flags {
            if !set {
                continue;
            }

            ui::row();
            {
                ui::col(0);
                ui::text(style);
                ui::same_line();
                help_marker(help);
            }
            {
                ui::col(1);
                ui::text("true");
            }
        }
        ui::end_table();
    }

    ui::text_disabled("(Any styles not shown are not currently set)");
}

/// Render a style color.
pub fn color(id: &CStr, c: Color, palette: Option<&Palette>) {
    // SAFETY: requires the ImGui context the render callback set up.
    unsafe { c::ImGui_PushID(id.as_ptr()) };

    let flags = c::ImGuiColorEditFlags_DisplayHex | c::ImGuiColorEditFlags_NoPicker | c::ImGuiColorEditFlags_NoLabel;
    match c {
        Color::None => ui::text("default"),

        Color::Palette(idx) => {
            textf!("Palette {idx}");
            if let Some(p) = palette {
                let rgb = p[idx as usize];
                ui::color_edit3(c"color_fg", rgb.r, rgb.g, rgb.b, flags);
            }
        }

        Color::Rgb(rgb) => {
            ui::color_edit3(c"color_fg", rgb.r, rgb.g, rgb.b, flags);
        }
    }

    // SAFETY: see above.
    unsafe { c::ImGui_PopID() };
}
