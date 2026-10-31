//! Port of `src/input/KeymapDarwin.zig`.
//!
//! STATUS: ported (macOS only; not compile-tested on Linux)
//!
//! Keymap is responsible for translating keyboard inputs into localized chars.
//!
//! For example, the physical key "S" on a US-layout keyboard might mean "O"
//! in Dvorak. On international keyboard layouts, it may require multiple
//! keystrokes to produce a single character that is otherwise a single
//! keystroke on a US-layout keyboard.
//!
//! This information is critical to know for many reasons. For keybindings,
//! if a user configures "ctrl+o" to do something, it should work with the
//! physical "ctrl+S" key on a Dvorak keyboard and so on.
//!
//! This is currently only implemented for macOS.

#![cfg(target_os = "macos")]

use std::ffi::{c_ulong, c_void};
use std::fmt;

use super::key::Mods;
use super::keycodes::ENTRIES as CODES;
use crate::pkg::macos::foundation;

pub struct Keymap {
    /// The current input source that is selected for the keyboard. This can
    /// and does change whenever the user selects a new keyboard layout. This
    /// change doesn't happen automatically; the user of this struct has to
    /// detect it and then call `reload` to update the keymap.
    source: *mut TISInputSource,

    /// The keyboard layout for the current input source.
    ///
    /// This doesn't need to be freed because its owned by the InputSource.
    unicode_layout: *const UCKeyboardLayout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    GetInputSourceFailed,
    TranslateFailed,
    /// `std.unicode.utf16LeToUtf8` failure (invalid UTF-16 or the output
    /// buffer is too small).
    InvalidUtf16,
    /// `Allocator.Error` from [`Keymap::source_id`].
    OutOfMemory,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}

/// The state that has to be passed in with each call to translate.
/// The contents of this are meant to mostly be opaque and can change
/// for platform-specific reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    pub dead_key: u32,
}

/// The result of a translation. The result of a translation can be multiple
/// states. For example, if the user types a dead key, the result will be
/// "composing" since they're still in the process of composing a full
/// character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Translation<'a> {
    /// The translation result. If this is a dead key state, then this will
    /// be pre-edit text that can be displayed but will ultimately be replaced.
    pub text: &'a [u8],

    /// Whether the text is still composing, i.e. this is a dead key state.
    pub composing: bool,

    /// The mods that were consumed to produce this translation
    pub mods: Mods,
}

impl Default for Translation<'_> {
    fn default() -> Self {
        Translation {
            text: b"",
            composing: false,
            mods: Mods::EMPTY,
        }
    }
}

impl Keymap {
    pub fn init() -> Result<Keymap, Error> {
        let mut keymap = Keymap {
            source: std::ptr::null_mut(),
            unicode_layout: std::ptr::null(),
        };
        keymap.reinit()?;
        Ok(keymap)
    }

    pub fn deinit(&self) {
        // SAFETY: source was returned by a Copy function and is owned by us.
        unsafe { foundation::cf_release(self.source as *mut c_void) };
    }

    /// Reload the keymap. This must be called if the user changes their
    /// keyboard layout.
    pub fn reload(&mut self) -> Result<(), Error> {
        // SAFETY: source was returned by a Copy function and is owned by us.
        unsafe { foundation::cf_release(self.source as *mut c_void) };
        self.reinit()
    }

    /// Get the input source ID for the current keyboard layout. The input
    /// source ID is a unique identifier for the keyboard layout which is uniquely
    /// defined by Apple.
    ///
    /// This is macOS-only. Other platforms don't have an equivalent of this
    /// so this isn't expected to be generally implemented.
    pub fn source_id<'a>(&self, buf: &'a mut [u8]) -> Result<&'a [u8], Error> {
        // Get the raw CFStringRef
        // SAFETY: source is a valid input source.
        let id_raw = unsafe { TISGetInputSourceProperty(self.source, kTISPropertyInputSourceID) };
        if id_raw.is_null() {
            return Err(Error::OutOfMemory);
        }

        // Convert the CFStringRef to a C string into our buffer.
        // SAFETY: the input source ID property is a CFStringRef.
        let id = unsafe { foundation::String::from_ptr(id_raw) }.ok_or(Error::OutOfMemory)?;
        id.cstring(buf, foundation::StringEncoding::Utf8)
            .ok_or(Error::OutOfMemory)
    }

    /// Reinit reinitializes the keymap. It assumes that all the memory associated
    /// with the keymap is already freed.
    fn reinit(&mut self) -> Result<(), Error> {
        // SAFETY: plain FFI call.
        self.source = unsafe { TISCopyCurrentKeyboardLayoutInputSource() };
        if self.source.is_null() {
            return Err(Error::GetInputSourceFailed);
        }

        self.unicode_layout = {
            // This returns a CFDataRef
            // SAFETY: source is a valid input source.
            let data_raw =
                unsafe { TISGetInputSourceProperty(self.source, kTISPropertyUnicodeKeyLayoutData) };
            if data_raw.is_null() {
                return Err(Error::GetInputSourceFailed);
            }

            // The CFDataRef contains a UCKeyboardLayout pointer
            // SAFETY: data_raw is a CFDataRef owned by the input source.
            let data = unsafe { foundation::Data::from_ptr(data_raw) }
                .ok_or(Error::GetInputSourceFailed)?;
            data.get_pointer() as *const UCKeyboardLayout
        };

        if cfg!(debug_assertions) {
            let mut buf = [0u8; 256];
            if let Ok(id) = self.source_id(&mut buf) {
                log::debug!("keyboard layout={}", String::from_utf8_lossy(id));
            }
        }

        Ok(())
    }

    /// Translate a single key input into a utf8 sequence.
    pub fn translate<'a>(
        &self,
        out: &'a mut [u8],
        state: &mut State,
        code: u16,
        input_mods: Mods,
    ) -> Result<Translation<'a>, Error> {
        // On macOS we strip ctrl because UCKeyTranslate
        // converts to the masked values (i.e. ctrl+c becomes 3)
        // and we don't want that behavior in Ghostty ever. This makes
        // this file not a general-purpose keymap implementation.
        let mods: Mods = {
            let mut v = input_mods;
            v.ctrl = false;
            v
        };

        // Get the keycode for the space key, using comptime.
        const CODE_SPACE: u16 = {
            let mut i = 0;
            loop {
                if i >= CODES.len() {
                    panic!("space code not found");
                }
                let c = CODES[i].code.as_bytes();
                if c.len() == 5
                    && c[0] == b'S'
                    && c[1] == b'p'
                    && c[2] == b'a'
                    && c[3] == b'c'
                    && c[4] == b'e'
                {
                    break CODES[i].native as u16;
                }
                i += 1;
            }
        };

        // Convert our mods from our format to the Carbon API format
        let modifier_state: u32 = CarbonMods {
            alt: mods.alt,
            ctrl: mods.ctrl,
            meta: mods.super_,
            shift: mods.shift,
            caps_lock: mods.caps_lock,
        }
        .uc_key_translate();

        // We use 4 here because the Chromium source code uses 4 and Chrome
        // works pretty well. They have a todo to look into longer sequences
        // but given how mature that software is I think this is fine.
        //
        // From Chromium:
        // Per Apple docs, the buffer length can be up to 255 but is rarely more than 4.
        // https://developer.apple.com/documentation/coreservices/1390584-uckeytranslate
        let mut chars: [u16; 4] = [0; 4];
        let mut char_count: c_ulong = 0;
        // SAFETY: all pointers are valid for the duration of the call.
        if unsafe {
            UCKeyTranslate(
                self.unicode_layout,
                code,
                K_UC_KEY_ACTION_DOWN,
                modifier_state,
                LMGetKbdType() as u32,
                K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_BIT,
                &mut state.dead_key,
                chars.len() as c_ulong,
                &mut char_count,
                chars.as_mut_ptr(),
            )
        } != 0
        {
            return Err(Error::TranslateFailed);
        }

        // If we got a dead key, then we translate again with "space"
        // in order to get the pre-edit text.
        let composing = if state.dead_key != 0 && char_count == 0 {
            // We need to copy our dead key state so that it isn't modified.
            let mut dead_key_ignore: u32 = state.dead_key;
            // SAFETY: all pointers are valid for the duration of the call.
            if unsafe {
                UCKeyTranslate(
                    self.unicode_layout,
                    CODE_SPACE,
                    K_UC_KEY_ACTION_DOWN,
                    modifier_state,
                    LMGetKbdType() as u32,
                    K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_MASK,
                    &mut dead_key_ignore,
                    chars.len() as c_ulong,
                    &mut char_count,
                    chars.as_mut_ptr(),
                )
            } != 0
            {
                return Err(Error::TranslateFailed);
            }
            true
        } else {
            false
        };

        // Convert the utf16 to utf8
        let mut len = 0usize;
        for r in char::decode_utf16(chars[..char_count as usize].iter().copied()) {
            let ch = r.map_err(|_| Error::InvalidUtf16)?;
            let n = ch.len_utf8();
            if len + n > out.len() {
                return Err(Error::InvalidUtf16);
            }
            ch.encode_utf8(&mut out[len..len + n]);
            len += n;
        }
        Ok(Translation {
            text: &out[..len],
            composing,
            mods,
        })
    }
}

/// Map to the modifiers format used by the UCKeyTranslate function.
/// We use a u32 here because our bit arithmetic is all u32 anyways.
///
/// (Zig: `packed struct(u32)` with 8 bits of padding, then meta, shift,
/// caps_lock, alt, ctrl.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct CarbonMods {
    meta: bool,
    shift: bool,
    caps_lock: bool,
    alt: bool,
    ctrl: bool,
}

impl CarbonMods {
    const fn to_bits(self) -> u32 {
        ((self.meta as u32) << 8)
            | ((self.shift as u32) << 9)
            | ((self.caps_lock as u32) << 10)
            | ((self.alt as u32) << 11)
            | ((self.ctrl as u32) << 12)
    }

    /// Translate NSEventModifierFlags into the format used by UCKeyTranslate.
    fn uc_key_translate(self) -> u32 {
        let int: u32 = self.to_bits();
        (int >> 8) & 0xFF
    }
}

// The documentation for all of these types and functions is in the macOS SDK:
// Carbon.framework/Versions/A/Frameworks/HIToolbox.framework/Versions/A/Headers/TextInputSources.h
#[repr(C)]
struct TISInputSource {
    _private: [u8; 0],
}
#[repr(C)]
struct UCKeyboardLayout {
    _private: [u8; 0],
}

type CFStringRef = *const foundation::String;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut TISInputSource;
    fn TISGetInputSourceProperty(source: *mut TISInputSource, key: CFStringRef) -> *mut c_void;
    fn LMGetKbdLast() -> u8;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: *const UCKeyboardLayout,
        code: u16,
        action: u16,
        modifier_state: u32,
        keyboard_type: u32,
        options: u32,
        dead_key_state: *mut u32,
        max_length: c_ulong,
        actual_length: *mut c_ulong,
        unicode_string: *mut u16,
    ) -> i32;
    static kTISPropertyLocalizedName: CFStringRef;
    static kTISPropertyUnicodeKeyLayoutData: CFStringRef;
    static kTISPropertyInputSourceID: CFStringRef;
}

const K_UC_KEY_ACTION_DOWN: u16 = 0;
const K_UC_KEY_ACTION_UP: u16 = 1;
const K_UC_KEY_ACTION_AUTO_KEY: u16 = 2;
const K_UC_KEY_ACTION_DISPLAY: u16 = 3;
const K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_BIT: u32 = 0;
const K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_MASK: u32 = 1 << K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_BIT;

#[cfg(test)]
mod tests {
    use super::*;

    // We got this from dumping the values out of another program since
    // I can't find the header for this anywhere. I find various modifier
    // headers but they do not match this! 🥺
    #[test]
    fn expected_values() {
        let c = |f: fn(&mut CarbonMods)| {
            let mut m = CarbonMods::default();
            f(&mut m);
            m.to_bits()
        };
        assert_eq!(0x100u32, c(|m| m.meta = true));
        assert_eq!(0x200u32, c(|m| m.shift = true));
        assert_eq!(0x400u32, c(|m| m.caps_lock = true));
        assert_eq!(0x800u32, c(|m| m.alt = true));
        assert_eq!(0x1000u32, c(|m| m.ctrl = true));
    }

    #[test]
    fn init_deinit() {
        let keymap = Keymap::init().unwrap();
        keymap.deinit();

        // These tests are all commented because they depend on the user-selected
        // keyboard layout...
        //
        // // Single quote ' which is fine on US, but dead on US-International
        // var buf: [4]u8 = undefined;
        // var state: State = .{};
        // {
        //     const result = try keymap.translate(&buf, &state, 0x27, .{});
        //     std.log.warn("map: text={s} dead={}", .{ result.text, result.composing });
        // }
        //
        // // Then type "a" which should combine with the dead key to make á
        // {
        //     const result = try keymap.translate(&buf, &state, 0x00, .{});
        //     std.log.warn("map: text={s} dead={}", .{ result.text, result.composing });
        // }
        //
        // // Shift+1 = ! on US
        // {
        //     const result = try keymap.translate(&buf, &state, 0x12, .{ .shift = true });
        //     std.log.warn("map: text={s} dead={}", .{ result.text, result.composing });
        // }
        //
        // // Scratch space
        // {
        //     const result = try keymap.translate(&buf, &state, 0x00, .{ .ctrl = true });
        //     std.log.warn("map: text={s} dead={}", .{ result.text, result.composing });
        // }
    }
}
