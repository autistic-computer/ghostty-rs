//! Port of `src/inspector/widgets/key.zig`.
//!
//! STATUS: ported
//!
//! Deviations:
//! * [`Event`] owns its UTF-8 text in [`Event::utf8`]; `event.utf8` is
//!   always empty (Zig points `event.utf8` at an allocated copy, which a
//!   Rust `KeyEvent<'static>` can't borrow from its owner). Use
//!   [`Event::utf8`] to read it.
//! * `Event.label` and `modsTooltip` return a `String` instead of writing
//!   into a caller buffer.
//! * Drawing is only compiled with the `inspector` feature.

use crate::datastruct::circ_buf::CircBuf;
use crate::input;
use crate::input::binding;

#[cfg(feature = "inspector")]
use crate::datastruct::circ_buf::Direction;
#[cfg(feature = "inspector")]
use crate::inspector::widgets::{textf, ui};
#[cfg(feature = "inspector")]
use crate::pkg::dcimgui::c;

/// Circular buffer of key events.
pub type EventRing = CircBuf<Event>;

/// GUI state of an [`Event`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ImguiState {
    pub selected: bool,
}

/// Represents a recorded keyboard event.
#[derive(Debug, Clone, Default)]
pub struct Event {
    /// The input event. Its `utf8` is always empty: the text is owned by
    /// [`Event::utf8`].
    pub event: input::KeyEvent<'static>,

    /// The event text (Zig: an allocated copy stored in `event.utf8`).
    pub utf8: Vec<u8>,

    /// The binding that was triggered as a result of this event.
    /// Multiple bindings are possible if they are chained.
    pub binding: Vec<binding::Action>,

    /// The data sent to the pty as a result of this keyboard event.
    /// This is allocated using the inspector allocator.
    pub pty: Vec<u8>,

    /// State for the inspector GUI. Do not set this unless you're the inspector.
    pub imgui_state: ImguiState,
}

impl Event {
    pub fn init(ev: &input::KeyEvent<'_>) -> Event {
        let copy = input::KeyEvent {
            action: ev.action,
            key: ev.key,
            mods: ev.mods,
            consumed_mods: ev.consumed_mods,
            composing: ev.composing,
            utf8: b"",
            unshifted_codepoint: ev.unshifted_codepoint,
        };
        Event {
            event: copy,
            utf8: ev.utf8.to_vec(),
            ..Default::default()
        }
    }

    /// Returns a label that can be used for this event.
    pub fn label(&self) -> String {
        let mut s = String::new();
        s.push_str(match self.event.action {
            input::Action::Press => "Press: ",
            input::Action::Release => "Release: ",
            input::Action::Repeat => "Repeat: ",
        });

        if self.event.mods.shift {
            s.push_str("Shift+");
        }
        if self.event.mods.ctrl {
            s.push_str("Ctrl+");
        }
        if self.event.mods.alt {
            s.push_str("Alt+");
        }
        if self.event.mods.super_ {
            s.push_str("Super+");
        }

        // Write our key. If we have an invalid key we attempt to write
        // the utf8 associated with it if we have it to handle non-ascii.
        s.push_str(&key_name(self.event.key, &self.utf8));

        // Deadkey
        if self.event.composing {
            s.push_str(" (composing)");
        }

        s
    }
}

/// The display name of a key: its tag name, or the UTF-8 text for
/// unidentified keys.
fn key_name(key: input::Key, utf8: &[u8]) -> String {
    match key {
        input::Key::Unidentified if !utf8.is_empty() => String::from_utf8_lossy(utf8).into_owned(),
        _ => key.as_str().to_string(),
    }
}

/// Format UTF-8 text as a codepoint sequence ("U+41 U+42 ").
fn format_codepoints(utf8: &[u8], invalid: &str) -> String {
    match std::str::from_utf8(utf8) {
        Ok(s) => s.chars().map(|cp| format!("U+{:X} ", cp as u32)).collect(),
        Err(_) => invalid.to_string(),
    }
}

/// Format pty bytes with ESC and non-printable bytes spelled out.
fn format_pty(pty: &[u8]) -> String {
    let mut s = String::new();
    for &byte in pty {
        // Print ESC special because its so common
        if byte == 0x1B {
            s.push_str("ESC ");
            continue;
        }

        // Print ASCII as-is
        if byte > 0x20 && byte < 0x7F {
            s.push(byte as char);
            continue;
        }

        // Everything else as a hex byte
        s.push_str(&format!("0x{byte:X} "));
    }
    s
}

#[cfg(feature = "inspector")]
impl Event {
    /// Render this event in the inspector GUI.
    pub fn render(&self) {
        // SAFETY: requires the ImGui context the render callback set up.
        unsafe {
            c::ImGui_BeginTable(c"##event".as_ptr(), 2, c::ImGuiTableFlags_None as _);

            if !self.binding.is_empty() {
                ui::row();
                ui::col(0);
                ui::text("Triggered Binding");
                ui::col(1);

                let height: f32 = {
                    let item_count = self.binding.len().min(5) as f32;
                    let padding = (*c::ImGui_GetStyle()).FramePadding.y * 2.0;
                    c::ImGui_GetTextLineHeightWithSpacing() * item_count + padding
                };
                if c::ImGui_BeginListBox(c"##bindings".as_ptr(), ui::vec2(0.0, height)) {
                    for action in &self.binding {
                        ui::selectable(&ui::cstr(action.key().as_str()));
                    }
                    c::ImGui_EndListBox();
                }
            }

            {
                ui::row();
                ui::col(0);
                ui::text("Encoding to Pty");
                ui::col(1);
                if self.pty.is_empty() {
                    ui::text_disabled("(no data)");
                } else {
                    self.render_pty();
                }
            }

            {
                ui::row();
                ui::col(0);
                ui::text("Action");
                ui::col(1);
                ui::text(self.event.action.as_str());
            }
            {
                ui::row();
                ui::col(0);
                ui::text("Key");
                ui::col(1);
                ui::text(self.event.key.as_str());
            }
            if !self.event.mods.empty() {
                ui::row();
                ui::col(0);
                ui::text("Mods");
                ui::col(1);
                if self.event.mods.shift {
                    ui::text("shift ");
                }
                if self.event.mods.ctrl {
                    ui::text("ctrl ");
                }
                if self.event.mods.alt {
                    ui::text("alt ");
                }
                if self.event.mods.super_ {
                    ui::text("super ");
                }
            }
            if self.event.composing {
                ui::row();
                ui::col(0);
                ui::text("Composing");
                ui::col(1);
                ui::text("true");
            }
            {
                ui::row();
                ui::col(0);
                ui::text("UTF-8");
                ui::col(1);
                if self.utf8.is_empty() {
                    ui::text_disabled("(empty)");
                } else {
                    self.render_utf8(&self.utf8);
                }
            }

            c::ImGui_EndTable();
        }
    }

    fn render_utf8(&self, utf8: &[u8]) {
        // Format the codepoint sequence
        let text = format_codepoints(utf8, "(invalid utf-8)");

        // Render as a textbox
        read_only_input(c"##utf8", &text);
    }

    fn render_pty(&self) {
        // Format the codepoint sequence
        let text = format_pty(&self.pty);

        // Render as a textbox
        read_only_input(c"##pty", &text);
    }
}

/// A read-only `ImGui_InputText` showing `text` (Zig formats into a
/// 1024-byte buffer and fails if it doesn't fit; the port truncates).
#[cfg(feature = "inspector")]
fn read_only_input(label: &std::ffi::CStr, text: &str) {
    let mut buf = [0u8; 1024];
    let len = text.len().min(buf.len() - 1);
    buf[..len].copy_from_slice(&text.as_bytes()[..len]);
    // SAFETY: the buffer is NUL-terminated and `len` excludes the NUL.
    unsafe {
        c::ImGui_InputText(
            label.as_ptr(),
            buf.as_mut_ptr().cast(),
            len,
            c::ImGuiInputTextFlags_ReadOnly as _,
        );
    }
}

fn mods_tooltip(mods: &input::Mods) -> String {
    let mut s = String::new();
    let mut first = true;
    if mods.shift {
        s.push_str("Shift");
        first = false;
    }
    if mods.ctrl {
        if !first {
            s.push('+');
        }
        s.push_str("Ctrl");
        first = false;
    }
    if mods.alt {
        if !first {
            s.push('+');
        }
        s.push_str("Alt");
        first = false;
    }
    if mods.super_ {
        if !first {
            s.push('+');
        }
        s.push_str("Super");
    }
    s
}

/// Keyboard event stream inspector widget.
#[derive(Debug)]
pub struct Stream {
    pub events: EventRing,
}

impl Stream {
    pub fn init() -> Stream {
        Stream {
            events: EventRing::init(2, Event::default()),
        }
    }
}

#[cfg(feature = "inspector")]
impl Stream {
    pub fn draw(&mut self, open: bool) {
        if !open {
            return;
        }

        if self.events.empty() {
            ui::text("No recorded key events. Press a key with the terminal focused to record it.");
            return;
        }

        // SAFETY: requires the ImGui context the render callback set up.
        unsafe {
            if c::ImGui_Button(c"Clear".as_ptr()) {
                self.events.clear();
            }

            c::ImGui_Separator();

            let table_flags = c::ImGuiTableFlags_Borders
                | c::ImGuiTableFlags_Resizable
                | c::ImGuiTableFlags_ScrollY
                | c::ImGuiTableFlags_SizingFixedFit;

            if !c::ImGui_BeginTable(c"table_key_events".as_ptr(), 6, table_flags as _) {
                return;
            }

            c::ImGui_TableSetupScrollFreeze(0, 1);
            let fixed = c::ImGuiTableColumnFlags_WidthFixed as _;
            let stretch = c::ImGuiTableColumnFlags_WidthStretch as _;
            c::ImGui_TableSetupColumnEx(c"Action".as_ptr(), fixed, 80.0, 0);
            c::ImGui_TableSetupColumnEx(c"Key".as_ptr(), fixed, 160.0, 0);
            c::ImGui_TableSetupColumnEx(c"Mods".as_ptr(), fixed, 150.0, 0);
            c::ImGui_TableSetupColumnEx(c"UTF-8".as_ptr(), fixed, 80.0, 0);
            c::ImGui_TableSetupColumnEx(c"PTY Encoding".as_ptr(), stretch, 0.0, 0);
            c::ImGui_TableSetupColumnEx(c"Binding".as_ptr(), stretch, 0.0, 0);
            c::ImGui_TableHeadersRow();

            let mut it = self.events.iterator(Direction::Reverse);
            while let Some(ev) = it.next() {
                c::ImGui_PushIDPtr((ev as *const Event).cast());

                c::ImGui_TableNextRow();
                let row_min_y = c::ImGui_GetCursorScreenPos().y;

                // Set row background color based on action
                c::ImGui_TableSetBgColor(
                    c::ImGuiTableBgTarget_RowBg0 as _,
                    action_color(ev.event.action),
                    -1,
                );

                // Action column with colored text
                ui::col(0);
                let action_text_color = match ev.event.action {
                    input::Action::Press => ui::vec4(0.4, 1.0, 0.4, 1.0),   // Green
                    input::Action::Release => ui::vec4(0.6, 0.6, 1.0, 1.0), // Blue
                    input::Action::Repeat => ui::vec4(1.0, 1.0, 0.4, 1.0),  // Yellow
                };
                ui::text_colored(action_text_color, ev.event.action.as_str());

                // Key column with consistent key coloring
                ui::col(1);
                let key_name = key_name(ev.event.key, &ev.utf8);
                let key_rgba = key_color(ev.event.key);
                let key_color = ui::vec4(
                    (key_rgba & 0xFF) as f32 / 255.0,
                    ((key_rgba >> 8) & 0xFF) as f32 / 255.0,
                    ((key_rgba >> 16) & 0xFF) as f32 / 255.0,
                    1.0,
                );
                ui::text_colored(key_color, &key_name);

                // Composing indicator
                if ev.event.composing {
                    c::ImGui_SameLine();
                    ui::text_colored(ui::vec4(1.0, 0.6, 0.0, 1.0), "*");
                    if c::ImGui_IsItemHovered(c::ImGuiHoveredFlags_None as _) {
                        ui::set_tooltip("Composing (dead key)");
                    }
                }

                // Mods
                ui::col(2);
                if ev.event.mods.empty() {
                    ui::text_disabled("-");
                } else {
                    let mut any_hovered = false;
                    let mods = ev.event.mods;
                    for (set, label) in [
                        (mods.shift, c"S"),
                        (mods.ctrl, c"C"),
                        (mods.alt, c"A"),
                        (mods.super_, c"M"),
                    ] {
                        if set {
                            c::ImGui_SmallButton(label.as_ptr());
                            any_hovered = any_hovered || c::ImGui_IsItemHovered(c::ImGuiHoveredFlags_None as _);
                            c::ImGui_SameLine();
                        }
                    }
                    c::ImGui_NewLine();

                    if any_hovered {
                        ui::set_tooltip(&mods_tooltip(&ev.event.mods));
                    }
                }

                // UTF-8
                ui::col(3);
                if ev.utf8.is_empty() {
                    ui::text_disabled("-");
                } else {
                    let mut s = format_codepoints(&ev.utf8, "?");
                    truncate(&mut s, 127);
                    ui::text(&s);
                }

                // PTY
                ui::col(4);
                if ev.pty.is_empty() {
                    ui::text_disabled("-");
                } else {
                    let mut s = format_pty(&ev.pty);
                    truncate(&mut s, 255);
                    ui::text(&s);
                }

                // Binding
                ui::col(5);
                if ev.binding.is_empty() {
                    ui::text_disabled("-");
                } else {
                    let mut s = String::new();
                    for (i, action) in ev.binding.iter().enumerate() {
                        if i > 0 {
                            s.push_str(", ");
                        }
                        s.push_str(action.key().as_str());
                    }
                    truncate(&mut s, 255);
                    textf!("{s}");
                }

                // Row hover highlight
                let row_max_y = c::ImGui_GetCursorScreenPos().y;
                let mouse_pos = c::ImGui_GetMousePos();
                if mouse_pos.y >= row_min_y && mouse_pos.y < row_max_y {
                    c::ImGui_TableSetBgColor(c::ImGuiTableBgTarget_RowBg1 as _, 0x1AFFFFFF, -1);
                }

                c::ImGui_PopID();
            }

            c::ImGui_EndTable();
        }
    }
}

/// Truncate to at most `max` bytes on a char boundary (Zig writes into
/// fixed buffers and stops when they are full).
#[cfg(feature = "inspector")]
fn truncate(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Returns row background color for an action (ABGR format for ImGui)
fn action_color(action: input::Action) -> u32 {
    match action {
        input::Action::Press => 0x1A4A6F4A,   // Muted sage green
        input::Action::Release => 0x1A6A5A5A, // Muted slate gray
        input::Action::Repeat => 0x1A4A5A6F,  // Muted warm brown
    }
}

/// Generate a consistent color for a key based on its enum value.
/// Uses HSV color space with fixed saturation and value for pleasing colors.
fn key_color(key: input::Key) -> u32 {
    let key_int: u32 = key as u32;
    let hue: f32 = key_int.wrapping_mul(47) as f32 / 256.0;
    hsv_to_rgba(hue, 0.5, 0.9, 1.0)
}

/// Convert HSV (hue 0-1, saturation 0-1, value 0-1) to RGBA u32.
fn hsv_to_rgba(h: f32, s: f32, v: f32, a: f32) -> u32 {
    let i: u32 = (h * 6.0) as u32;
    let f = h * 6.0 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);

    let (r, g, b) = match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };

    let ri: u32 = (r * 255.0) as u32;
    let gi: u32 = (g * 255.0) as u32;
    let bi: u32 = (b * 255.0) as u32;
    let ai: u32 = (a * 255.0) as u32;

    (ai << 24) | (bi << 16) | (gi << 8) | ri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_label() {
        let ev = Event::init(&input::KeyEvent {
            action: input::Action::Press,
            key: input::Key::KeyA,
            mods: input::Mods {
                shift: true,
                ctrl: true,
                ..Default::default()
            },
            utf8: b"A",
            ..Default::default()
        });
        assert_eq!(ev.utf8, b"A");
        assert!(ev.event.utf8.is_empty());
        assert_eq!(ev.label(), "Press: Shift+Ctrl+key_a");

        let ev = Event::init(&input::KeyEvent {
            action: input::Action::Release,
            key: input::Key::Unidentified,
            utf8: "é".as_bytes(),
            composing: true,
            ..Default::default()
        });
        assert_eq!(ev.label(), "Release: é (composing)");
    }

    #[test]
    fn mods_tooltip_format() {
        let mods = input::Mods {
            shift: true,
            alt: true,
            super_: true,
            ..Default::default()
        };
        assert_eq!(mods_tooltip(&mods), "Shift+Alt+Super");
        assert_eq!(mods_tooltip(&input::Mods::default()), "");
    }

    #[test]
    fn pty_and_utf8_format() {
        assert_eq!(format_pty(b"\x1b[Aa\x01"), "ESC [Aa0x1 ");
        assert_eq!(format_codepoints("ab".as_bytes(), "?"), "U+61 U+62 ");
        assert_eq!(format_codepoints(&[0xFF], "?"), "?");
    }

    #[test]
    fn hsv() {
        assert_eq!(hsv_to_rgba(0.0, 0.0, 1.0, 1.0), 0xFFFFFFFF);
        assert_eq!(hsv_to_rgba(0.0, 1.0, 1.0, 1.0), 0xFF0000FF);
        assert_eq!(action_color(input::Action::Press), 0x1A4A6F4A);
        // Key colors are stable.
        assert_eq!(key_color(input::Key::KeyA), key_color(input::Key::KeyA));
    }

    #[test]
    fn stream_init() {
        let s = Stream::init();
        assert!(s.events.empty());
        assert_eq!(s.events.capacity(), 2);
    }
}
