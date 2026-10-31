//! Port of `src/apprt/gtk/key.zig`.
//!
//! STATUS: ported
//!
//! Zig writes accelerator strings into a caller-provided fixed buffer
//! (`std.Io.Writer.fixed`), failing with `error.WriteFailed` when it is too
//! small; the port keeps that contract with [`FixedWriter`]. The keyval
//! helpers (`gdk_keyval_name`, `gdk_unicode_to_keyval`,
//! `gdk_keyval_to_unicode`) are called through the C API because, unlike
//! C, the gtk-rs wrappers insist on an initialized GTK.
#![cfg(feature = "gtk")]

use std::ffi::CStr;
use std::fmt::{self, Write as _};
use std::os::raw::c_uint;

use gtk4::gdk;
use gtk4::gdk::ffi;
use gtk4::gdk::prelude::*;
use gtk4::prelude::*;

use super::winproto;
use crate::input::binding::{Trigger, TriggerKey};
use crate::input::key::{Action, Key};
use crate::input::key_mods::{ModSide, Mods};

/// `error{WriteFailed}`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteFailed;

impl fmt::Display for WriteFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WriteFailed")
    }
}

impl std::error::Error for WriteFailed {}

/// `std.Io.Writer.fixed(buf)`: a writer into a fixed buffer that fails once
/// the buffer is full.
pub struct FixedWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> FixedWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        FixedWriter { buf, len: 0 }
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), WriteFailed> {
        let end = self.len + bytes.len();
        if end > self.buf.len() {
            return Err(WriteFailed);
        }
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }

    /// Writes the terminating NUL and returns the written string.
    fn finish_z(self) -> Result<&'a CStr, WriteFailed> {
        let FixedWriter { buf, len } = self;
        if len >= buf.len() {
            return Err(WriteFailed);
        }
        buf[len] = 0;
        // Keyval names and UTF-8 never contain NUL, so this cannot fail.
        CStr::from_bytes_with_nul(&buf[..=len]).map_err(|_| WriteFailed)
    }
}

impl fmt::Write for FixedWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_all(s.as_bytes()).map_err(|_| fmt::Error)
    }
}

/// `gdk.keyvalName`
fn keyval_name(keyval: c_uint) -> Option<&'static str> {
    // SAFETY: gdk_keyval_name returns a static string or NULL.
    unsafe {
        let ptr = ffi::gdk_keyval_name(keyval);
        if ptr.is_null() {
            return None;
        }
        CStr::from_ptr(ptr).to_str().ok()
    }
}

/// `gdk.unicodeToKeyval`
fn unicode_to_keyval(cp: u32) -> c_uint {
    // SAFETY: pure function.
    unsafe { ffi::gdk_unicode_to_keyval(cp) }
}

/// `gdk.keyvalToUnicode`
fn keyval_to_unicode(keyval: c_uint) -> u32 {
    // SAFETY: pure function.
    unsafe { ffi::gdk_keyval_to_unicode(keyval) }
}

/// Returns a GTK accelerator string from a trigger.
pub fn accel_from_trigger<'a>(
    buf: &'a mut [u8],
    trigger: Trigger,
) -> Result<Option<&'a CStr>, WriteFailed> {
    let mut writer = FixedWriter::new(buf);

    // Modifiers
    if trigger.mods.shift {
        writer.write_all(b"<Shift>")?;
    }
    if trigger.mods.ctrl {
        writer.write_all(b"<Ctrl>")?;
    }
    if trigger.mods.alt {
        writer.write_all(b"<Alt>")?;
    }
    if trigger.mods.super_ {
        writer.write_all(b"<Super>")?;
    }

    // Write our key
    if !write_trigger_key(&mut writer, trigger)? {
        return Ok(None);
    }

    // We need to make the string null terminated.
    writer.finish_z().map(Some)
}

/// Returns a XDG-compliant shortcuts string from a trigger.
/// Spec: https://specifications.freedesktop.org/shortcuts-spec/latest/
pub fn xdg_shortcut_from_trigger<'a>(
    buf: &'a mut [u8],
    trigger: Trigger,
) -> Result<Option<&'a CStr>, WriteFailed> {
    let mut writer = FixedWriter::new(buf);

    // Modifiers
    if trigger.mods.shift {
        writer.write_all(b"SHIFT+")?;
    }
    if trigger.mods.ctrl {
        writer.write_all(b"CTRL+")?;
    }
    if trigger.mods.alt {
        writer.write_all(b"ALT+")?;
    }
    if trigger.mods.super_ {
        writer.write_all(b"LOGO+")?;
    }

    // Write our key
    // NOTE: While the spec specifies that only libxkbcommon keysyms are
    // expected, using GTK's keysyms should still work as they are identical
    // to *X11's* keysyms (which I assume is a subset of libxkbcommon's).
    // I haven't been able to any evidence to back up that assumption but
    // this works for now
    if !write_trigger_key(&mut writer, trigger)? {
        return Ok(None);
    }

    // We need to make the string null terminated.
    writer.finish_z().map(Some)
}

/// Returns the keysym (identical to a GDK keyval) for a trigger key,
/// or null if the trigger has no keysym representation.
pub fn keysym_from_trigger(trigger: Trigger) -> Option<c_uint> {
    match trigger.key {
        TriggerKey::Physical(k) => keyval_from_key(k),
        TriggerKey::Unicode(cp) => Some(unicode_to_keyval(cp)),
        TriggerKey::CatchAll => None,
    }
}

/// `{u}` formatting of a codepoint (invalid ones become U+FFFD).
fn codepoint_str(cp: u32) -> String {
    char::from_u32(cp).unwrap_or('\u{FFFD}').to_string()
}

fn write_trigger_key(writer: &mut FixedWriter<'_>, trigger: Trigger) -> Result<bool, WriteFailed> {
    match trigger.key {
        TriggerKey::Physical(k) => {
            let Some(keyval) = keyval_from_key(k) else { return Ok(false) };
            let Some(name) = keyval_name(keyval) else { return Ok(false) };
            writer.write_all(name.as_bytes())?;
        }

        TriggerKey::Unicode(cp) => {
            if let Some(name) = keyval_name(cp) {
                writer.write_all(name.as_bytes())?;
            } else {
                writer.write_all(codepoint_str(cp).as_bytes())?;
            }
        }

        TriggerKey::CatchAll => return Ok(false),
    }

    Ok(true)
}

pub fn translate_mods(state: gdk::ModifierType) -> Mods {
    Mods {
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
        alt: state.contains(gdk::ModifierType::ALT_MASK),
        super_: state.contains(gdk::ModifierType::SUPER_MASK),
        // Lock is dependent on the X settings but we just assume caps lock.
        caps_lock: state.contains(gdk::ModifierType::LOCK_MASK),
        ..Mods::default()
    }
}

// Get the unshifted unicode value of the keyval. This is used
// by the Kitty keyboard protocol.
pub fn keyval_unicode_unshifted(
    widget: &impl IsA<gtk4::Widget>,
    event: &gdk::KeyEvent,
    keycode: u32,
) -> u32 {
    let display = widget.display();

    // We need to get the currently active keyboard layout so we know
    // what group to look at.
    let layout = event.layout();

    // Get all the possible keyboard mappings for this keycode. A keycode is the
    // physical key pressed.
    let Some(entries) = display.map_keycode(keycode) else { return 0 };

    for (key, keyval) in entries {
        if key.group() as u32 == layout && key.level() == 0 {
            let cp = keyval_to_unicode(keyval.into_glib());
            return if cp <= 0x1F_FFFF { cp } else { 0 };
        }
    }

    0
}

use glib::translate::IntoGlib as _;

/// Returns the mods to use a key event from a GTK event.
/// This requires a lot of context because the GdkEvent
/// doesn't contain enough on its own.
pub fn event_mods(
    event: &gdk::Event,
    physical_key: Key,
    gtk_mods: gdk::ModifierType,
    action: Action,
    app_winproto: &mut winproto::App,
) -> Mods {
    let device = event.device();

    let mut mods = app_winproto.event_mods(device.as_ref(), gtk_mods);
    mods.num_lock = device.as_ref().map(|d| d.is_num_locked()).unwrap_or(false);

    // We use the physical key to determine sided modifiers. As
    // far as I can tell there's no other way to reliably determine
    // this.
    //
    // We also set the main modifier to true if either side is true,
    // since on both X11/Wayland, GTK doesn't set the main modifier
    // if only the modifier key is pressed, but our core logic
    // relies on it.
    let pressed = action != Action::Release;
    match physical_key {
        Key::ShiftLeft => {
            mods.shift = pressed;
            mods.sides.shift = ModSide::Left;
        }

        Key::ShiftRight => {
            mods.shift = pressed;
            mods.sides.shift = ModSide::Right;
        }

        Key::ControlLeft => {
            mods.ctrl = pressed;
            mods.sides.ctrl = ModSide::Left;
        }

        Key::ControlRight => {
            mods.ctrl = pressed;
            mods.sides.ctrl = ModSide::Right;
        }

        Key::AltLeft => {
            mods.alt = pressed;
            mods.sides.alt = ModSide::Left;
        }

        Key::AltRight => {
            mods.alt = pressed;
            mods.sides.alt = ModSide::Right;
        }

        Key::MetaLeft => {
            mods.super_ = pressed;
            mods.sides.super_ = ModSide::Left;
        }

        Key::MetaRight => {
            mods.super_ = pressed;
            mods.sides.super_ = ModSide::Right;
        }

        _ => {}
    }

    mods
}

/// Returns an input key from a keyval or null if we don't have a mapping.
pub fn key_from_keyval(keyval: c_uint) -> Option<Key> {
    for entry in KEYMAP {
        if entry.0 == keyval {
            return Some(entry.1);
        }
    }

    None
}

/// Returns the logical key after applying any eligible XKB remapping.
///
/// Modifier keyvals that Ghostty doesn't represent must not fall back to the
/// physical key. XKB can turn any physical key into a modifier, and using the
/// original key in that case would encode an unrelated key press.
pub fn remap_key(physical_key: Key, keyval: c_uint, is_modifier: bool) -> Key {
    if let Some(remapped) = key_from_keyval(keyval) {
        if physical_key.should_be_remappable() || remapped.should_be_remappable() {
            return remapped;
        }
    }

    if is_modifier || is_modifier_keyval(keyval) {
        return Key::Unidentified;
    }
    physical_key
}

/// Returns whether a keyval has modifier semantics that GDK may not expose
/// through `gdk_key_event_is_modifier`.
///
/// In particular, XKB can report ISO level and group modifiers as keyvals
/// such as `Mode_switch` while GDK reports that the event is not a modifier.
fn is_modifier_keyval(keyval: c_uint) -> bool {
    const MODIFIERS: &[c_uint] = &[
        k(ffi::GDK_KEY_Shift_L),
        k(ffi::GDK_KEY_Shift_R),
        k(ffi::GDK_KEY_Shift_Lock),
        k(ffi::GDK_KEY_Control_L),
        k(ffi::GDK_KEY_Control_R),
        k(ffi::GDK_KEY_Caps_Lock),
        k(ffi::GDK_KEY_Meta_L),
        k(ffi::GDK_KEY_Meta_R),
        k(ffi::GDK_KEY_Alt_L),
        k(ffi::GDK_KEY_Alt_R),
        k(ffi::GDK_KEY_Super_L),
        k(ffi::GDK_KEY_Super_R),
        k(ffi::GDK_KEY_Hyper_L),
        k(ffi::GDK_KEY_Hyper_R),
        k(ffi::GDK_KEY_Num_Lock),
        k(ffi::GDK_KEY_Mode_switch),
        k(ffi::GDK_KEY_ISO_Lock),
        k(ffi::GDK_KEY_ISO_Level2_Latch),
        k(ffi::GDK_KEY_ISO_Level3_Shift),
        k(ffi::GDK_KEY_ISO_Level3_Latch),
        k(ffi::GDK_KEY_ISO_Level3_Lock),
        k(ffi::GDK_KEY_ISO_Level5_Shift),
        k(ffi::GDK_KEY_ISO_Level5_Latch),
        k(ffi::GDK_KEY_ISO_Level5_Lock),
        k(ffi::GDK_KEY_ISO_Group_Latch),
        k(ffi::GDK_KEY_ISO_Group_Lock),
    ];
    MODIFIERS.contains(&keyval)
}

/// Returns a keyval from an input key or null if we don't have a mapping.
///
/// (Zig builds a per-key comptime lookup; the port searches the table.)
pub fn keyval_from_key(key: Key) -> Option<c_uint> {
    for entry in KEYMAP {
        if entry.1 == key {
            return Some(entry.0);
        }
    }

    None
}

/// Converts a trigger to a human-readable label for display in UI.
///
/// Uses GTK accelerator-style formatting (e.g., "Ctrl+Shift+A").
/// Returns false if the trigger cannot be formatted (e.g., catch_all).
pub fn label_from_trigger(writer: &mut impl fmt::Write, trigger: Trigger) -> Result<bool, fmt::Error> {
    // Modifiers first, using human-readable format
    if trigger.mods.super_ {
        writer.write_str("Super+")?;
    }
    if trigger.mods.ctrl {
        writer.write_str("Ctrl+")?;
    }
    if trigger.mods.alt {
        writer.write_str("Alt+")?;
    }
    if trigger.mods.shift {
        writer.write_str("Shift+")?;
    }

    // Write the key
    write_trigger_key_label(writer, trigger)
}

/// Writes a keyval name, capitalizing the first letter for nicer display.
fn write_capitalized(writer: &mut impl fmt::Write, span: &str) -> fmt::Result {
    let bytes = span.as_bytes();
    if !bytes.is_empty() {
        if bytes[0].is_ascii_lowercase() {
            writer.write_char((bytes[0] - b'a' + b'A') as char)?;
            if bytes.len() > 1 {
                writer.write_str(&span[1..])?;
            }
        } else {
            writer.write_str(span)?;
        }
    }
    Ok(())
}

/// Writes the key portion of a trigger in human-readable format.
fn write_trigger_key_label(writer: &mut impl fmt::Write, trigger: Trigger) -> Result<bool, fmt::Error> {
    match trigger.key {
        TriggerKey::Physical(k) => {
            let Some(keyval) = keyval_from_key(k) else { return Ok(false) };
            let Some(name) = keyval_name(keyval) else { return Ok(false) };
            // Capitalize the first letter for nicer display
            write_capitalized(writer, name)?;
        }

        TriggerKey::Unicode(cp) => {
            // Try to get a nice name from GDK first
            if let Some(name) = keyval_name(cp) {
                // Capitalize the first letter for nicer display
                write_capitalized(writer, name)?;
            } else {
                // Fall back to printing the character
                writer.write_str(&codepoint_str(cp))?;
            }
        }

        TriggerKey::CatchAll => return Ok(false),
    }

    Ok(true)
}

const fn k(v: std::os::raw::c_int) -> c_uint {
    v as c_uint
}

/// A raw entry in the keymap. Our keymap contains mappings between
/// GDK keys and our own key enum.
type RawEntry = (c_uint, Key);

const KEYMAP: &[RawEntry] = &[
    (k(ffi::GDK_KEY_a), Key::KeyA),
    (k(ffi::GDK_KEY_b), Key::KeyB),
    (k(ffi::GDK_KEY_c), Key::KeyC),
    (k(ffi::GDK_KEY_d), Key::KeyD),
    (k(ffi::GDK_KEY_e), Key::KeyE),
    (k(ffi::GDK_KEY_f), Key::KeyF),
    (k(ffi::GDK_KEY_g), Key::KeyG),
    (k(ffi::GDK_KEY_h), Key::KeyH),
    (k(ffi::GDK_KEY_i), Key::KeyI),
    (k(ffi::GDK_KEY_j), Key::KeyJ),
    (k(ffi::GDK_KEY_k), Key::KeyK),
    (k(ffi::GDK_KEY_l), Key::KeyL),
    (k(ffi::GDK_KEY_m), Key::KeyM),
    (k(ffi::GDK_KEY_n), Key::KeyN),
    (k(ffi::GDK_KEY_o), Key::KeyO),
    (k(ffi::GDK_KEY_p), Key::KeyP),
    (k(ffi::GDK_KEY_q), Key::KeyQ),
    (k(ffi::GDK_KEY_r), Key::KeyR),
    (k(ffi::GDK_KEY_s), Key::KeyS),
    (k(ffi::GDK_KEY_t), Key::KeyT),
    (k(ffi::GDK_KEY_u), Key::KeyU),
    (k(ffi::GDK_KEY_v), Key::KeyV),
    (k(ffi::GDK_KEY_w), Key::KeyW),
    (k(ffi::GDK_KEY_x), Key::KeyX),
    (k(ffi::GDK_KEY_y), Key::KeyY),
    (k(ffi::GDK_KEY_z), Key::KeyZ),

    (k(ffi::GDK_KEY_0), Key::Digit0),
    (k(ffi::GDK_KEY_1), Key::Digit1),
    (k(ffi::GDK_KEY_2), Key::Digit2),
    (k(ffi::GDK_KEY_3), Key::Digit3),
    (k(ffi::GDK_KEY_4), Key::Digit4),
    (k(ffi::GDK_KEY_5), Key::Digit5),
    (k(ffi::GDK_KEY_6), Key::Digit6),
    (k(ffi::GDK_KEY_7), Key::Digit7),
    (k(ffi::GDK_KEY_8), Key::Digit8),
    (k(ffi::GDK_KEY_9), Key::Digit9),

    (k(ffi::GDK_KEY_semicolon), Key::Semicolon),
    (k(ffi::GDK_KEY_space), Key::Space),
    (k(ffi::GDK_KEY_apostrophe), Key::Quote),
    (k(ffi::GDK_KEY_comma), Key::Comma),
    (k(ffi::GDK_KEY_grave), Key::Backquote),
    (k(ffi::GDK_KEY_period), Key::Period),
    (k(ffi::GDK_KEY_slash), Key::Slash),
    (k(ffi::GDK_KEY_minus), Key::Minus),
    (k(ffi::GDK_KEY_equal), Key::Equal),
    (k(ffi::GDK_KEY_bracketleft), Key::BracketLeft),
    (k(ffi::GDK_KEY_bracketright), Key::BracketRight),
    (k(ffi::GDK_KEY_backslash), Key::Backslash),

    (k(ffi::GDK_KEY_Up), Key::ArrowUp),
    (k(ffi::GDK_KEY_Down), Key::ArrowDown),
    (k(ffi::GDK_KEY_Right), Key::ArrowRight),
    (k(ffi::GDK_KEY_Left), Key::ArrowLeft),
    (k(ffi::GDK_KEY_Home), Key::Home),
    (k(ffi::GDK_KEY_End), Key::End),
    (k(ffi::GDK_KEY_Insert), Key::Insert),
    (k(ffi::GDK_KEY_Delete), Key::Delete),
    (k(ffi::GDK_KEY_Caps_Lock), Key::CapsLock),
    (k(ffi::GDK_KEY_Scroll_Lock), Key::ScrollLock),
    (k(ffi::GDK_KEY_Num_Lock), Key::NumLock),
    (k(ffi::GDK_KEY_Page_Up), Key::PageUp),
    (k(ffi::GDK_KEY_Page_Down), Key::PageDown),
    (k(ffi::GDK_KEY_Escape), Key::Escape),
    (k(ffi::GDK_KEY_Return), Key::Enter),
    (k(ffi::GDK_KEY_Tab), Key::Tab),
    (k(ffi::GDK_KEY_BackSpace), Key::Backspace),
    (k(ffi::GDK_KEY_Print), Key::PrintScreen),
    (k(ffi::GDK_KEY_Pause), Key::Pause),

    (k(ffi::GDK_KEY_F1), Key::F1),
    (k(ffi::GDK_KEY_F2), Key::F2),
    (k(ffi::GDK_KEY_F3), Key::F3),
    (k(ffi::GDK_KEY_F4), Key::F4),
    (k(ffi::GDK_KEY_F5), Key::F5),
    (k(ffi::GDK_KEY_F6), Key::F6),
    (k(ffi::GDK_KEY_F7), Key::F7),
    (k(ffi::GDK_KEY_F8), Key::F8),
    (k(ffi::GDK_KEY_F9), Key::F9),
    (k(ffi::GDK_KEY_F10), Key::F10),
    (k(ffi::GDK_KEY_F11), Key::F11),
    (k(ffi::GDK_KEY_F12), Key::F12),
    (k(ffi::GDK_KEY_F13), Key::F13),
    (k(ffi::GDK_KEY_F14), Key::F14),
    (k(ffi::GDK_KEY_F15), Key::F15),
    (k(ffi::GDK_KEY_F16), Key::F16),
    (k(ffi::GDK_KEY_F17), Key::F17),
    (k(ffi::GDK_KEY_F18), Key::F18),
    (k(ffi::GDK_KEY_F19), Key::F19),
    (k(ffi::GDK_KEY_F20), Key::F20),
    (k(ffi::GDK_KEY_F21), Key::F21),
    (k(ffi::GDK_KEY_F22), Key::F22),
    (k(ffi::GDK_KEY_F23), Key::F23),
    (k(ffi::GDK_KEY_F24), Key::F24),
    (k(ffi::GDK_KEY_F25), Key::F25),

    (k(ffi::GDK_KEY_KP_0), Key::Numpad0),
    (k(ffi::GDK_KEY_KP_1), Key::Numpad1),
    (k(ffi::GDK_KEY_KP_2), Key::Numpad2),
    (k(ffi::GDK_KEY_KP_3), Key::Numpad3),
    (k(ffi::GDK_KEY_KP_4), Key::Numpad4),
    (k(ffi::GDK_KEY_KP_5), Key::Numpad5),
    (k(ffi::GDK_KEY_KP_6), Key::Numpad6),
    (k(ffi::GDK_KEY_KP_7), Key::Numpad7),
    (k(ffi::GDK_KEY_KP_8), Key::Numpad8),
    (k(ffi::GDK_KEY_KP_9), Key::Numpad9),
    (k(ffi::GDK_KEY_KP_Decimal), Key::NumpadDecimal),
    (k(ffi::GDK_KEY_KP_Divide), Key::NumpadDivide),
    (k(ffi::GDK_KEY_KP_Multiply), Key::NumpadMultiply),
    (k(ffi::GDK_KEY_KP_Subtract), Key::NumpadSubtract),
    (k(ffi::GDK_KEY_KP_Add), Key::NumpadAdd),
    (k(ffi::GDK_KEY_KP_Enter), Key::NumpadEnter),
    (k(ffi::GDK_KEY_KP_Equal), Key::NumpadEqual),

    (k(ffi::GDK_KEY_KP_Separator), Key::NumpadSeparator),
    (k(ffi::GDK_KEY_KP_Left), Key::NumpadLeft),
    (k(ffi::GDK_KEY_KP_Right), Key::NumpadRight),
    (k(ffi::GDK_KEY_KP_Up), Key::NumpadUp),
    (k(ffi::GDK_KEY_KP_Down), Key::NumpadDown),
    (k(ffi::GDK_KEY_KP_Page_Up), Key::NumpadPageUp),
    (k(ffi::GDK_KEY_KP_Page_Down), Key::NumpadPageDown),
    (k(ffi::GDK_KEY_KP_Home), Key::NumpadHome),
    (k(ffi::GDK_KEY_KP_End), Key::NumpadEnd),
    (k(ffi::GDK_KEY_KP_Insert), Key::NumpadInsert),
    (k(ffi::GDK_KEY_KP_Delete), Key::NumpadDelete),
    (k(ffi::GDK_KEY_KP_Begin), Key::NumpadBegin),

    (k(ffi::GDK_KEY_Copy), Key::Copy),
    (k(ffi::GDK_KEY_Cut), Key::Cut),
    (k(ffi::GDK_KEY_Paste), Key::Paste),

    (k(ffi::GDK_KEY_Shift_L), Key::ShiftLeft),
    (k(ffi::GDK_KEY_Control_L), Key::ControlLeft),
    (k(ffi::GDK_KEY_Alt_L), Key::AltLeft),
    (k(ffi::GDK_KEY_Super_L), Key::MetaLeft),
    (k(ffi::GDK_KEY_Shift_R), Key::ShiftRight),
    (k(ffi::GDK_KEY_Control_R), Key::ControlRight),
    (k(ffi::GDK_KEY_Alt_R), Key::AltRight),
    (k(ffi::GDK_KEY_Super_R), Key::MetaRight),

    // TODO: media keys
];

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(mods: Mods, key: TriggerKey) -> Trigger {
        Trigger { key, mods }
    }

    fn mods(shift: bool, ctrl: bool, alt: bool, super_: bool) -> Mods {
        Mods {
            shift,
            ctrl,
            alt,
            super_,
            ..Mods::default()
        }
    }

    #[test]
    fn remap_key_() {
        // XKB remaps between non-writing-system keys are honored.
        assert_eq!(Key::Escape, remap_key(Key::CapsLock, k(ffi::GDK_KEY_Escape), false));

        // Writing-system keys retain their physical identity.
        assert_eq!(Key::KeyA, remap_key(Key::KeyA, k(ffi::GDK_KEY_b), false));

        // GDK doesn't identify Mode_switch as a modifier, but it must not fall
        // back to the physical key.
        assert_eq!(
            Key::Unidentified,
            remap_key(Key::Delete, k(ffi::GDK_KEY_Mode_switch), false)
        );

        // GDK's modifier flag also covers custom modifier actions whose keyval
        // Ghostty doesn't represent.
        assert_eq!(
            Key::Unidentified,
            remap_key(Key::Delete, k(ffi::GDK_KEY_VoidSymbol), true)
        );
    }

    #[test]
    fn accel_from_trigger_() {
        let mut buf = [0u8; 256];

        assert_eq!(
            c"<Super>q",
            accel_from_trigger(
                &mut buf,
                trigger(mods(false, false, false, true), TriggerKey::Unicode('q' as u32))
            )
            .unwrap()
            .unwrap()
        );

        let mut buf = [0u8; 256];
        assert_eq!(
            c"<Shift><Ctrl><Alt><Super>backslash",
            accel_from_trigger(
                &mut buf,
                trigger(mods(true, true, true, true), TriggerKey::Unicode(92))
            )
            .unwrap()
            .unwrap()
        );

        // A buffer that is too small fails like Zig's fixed writer.
        let mut small = [0u8; 8];
        assert_eq!(
            Err(WriteFailed),
            accel_from_trigger(
                &mut small,
                trigger(mods(false, false, false, true), TriggerKey::Unicode('q' as u32))
            )
        );
    }

    #[test]
    fn xdg_shortcut_from_trigger_() {
        let mut buf = [0u8; 256];

        assert_eq!(
            c"LOGO+q",
            xdg_shortcut_from_trigger(
                &mut buf,
                trigger(mods(false, false, false, true), TriggerKey::Unicode('q' as u32))
            )
            .unwrap()
            .unwrap()
        );

        let mut buf = [0u8; 256];
        assert_eq!(
            c"SHIFT+CTRL+ALT+LOGO+backslash",
            xdg_shortcut_from_trigger(
                &mut buf,
                trigger(mods(true, true, true, true), TriggerKey::Unicode(92))
            )
            .unwrap()
            .unwrap()
        );
    }

    #[test]
    fn keysym_from_trigger_() {
        // Unicode keys use the keysym numbering (latin-1 maps directly).
        assert_eq!(
            Some('q' as c_uint),
            keysym_from_trigger(trigger(
                mods(false, false, false, true),
                TriggerKey::Unicode('q' as u32)
            ))
        );

        // Physical keys map through our keymap.
        assert_eq!(
            Some(k(ffi::GDK_KEY_a)),
            keysym_from_trigger(trigger(Mods::default(), TriggerKey::Physical(Key::KeyA)))
        );

        // catch_all has no keysym representation.
        assert_eq!(None, keysym_from_trigger(trigger(Mods::default(), TriggerKey::CatchAll)));
    }

    #[test]
    fn label_from_trigger_() {
        // Simple unicode key with modifier
        {
            let mut buf = String::new();
            assert!(label_from_trigger(
                &mut buf,
                trigger(mods(false, false, false, true), TriggerKey::Unicode('q' as u32))
            )
            .unwrap());
            assert_eq!("Super+Q", buf);
        }

        // Multiple modifiers
        {
            let mut buf = String::new();
            assert!(label_from_trigger(
                &mut buf,
                trigger(mods(true, true, true, true), TriggerKey::Unicode(92))
            )
            .unwrap());
            assert_eq!("Super+Ctrl+Alt+Shift+Backslash", buf);
        }

        // Physical key
        {
            let mut buf = String::new();
            assert!(label_from_trigger(
                &mut buf,
                trigger(mods(false, true, false, false), TriggerKey::Physical(Key::KeyA))
            )
            .unwrap());
            assert_eq!("Ctrl+A", buf);
        }

        // No modifiers
        {
            let mut buf = String::new();
            assert!(label_from_trigger(
                &mut buf,
                trigger(Mods::default(), TriggerKey::Physical(Key::Escape))
            )
            .unwrap());
            assert_eq!("Escape", buf);
        }

        // catch_all returns false
        {
            let mut buf = String::new();
            assert!(!label_from_trigger(&mut buf, trigger(Mods::default(), TriggerKey::CatchAll)).unwrap());
        }
    }

    #[test]
    fn keymap_round_trips() {
        for (keyval, key) in KEYMAP {
            assert_eq!(keyval_from_key(*key), Some(*keyval));
            assert_eq!(key_from_keyval(*keyval), Some(*key));
        }
        let m = translate_mods(gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::LOCK_MASK);
        assert!(m.shift && m.caps_lock && !m.ctrl);
    }
}
