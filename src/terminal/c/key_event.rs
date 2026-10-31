//! Port of `src/terminal/c/key_event.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: `key.Mods` is a `packed struct(u16)` in Zig and
//! crosses the C ABI as its backing integer (`GhosttyMods`); the Rust
//! functions take/return that `u16`. Enum arguments are taken as `c_int`
//! and validated like Zig's runtime safety check.

use std::ffi::c_int;

use super::result::Result;
use super::types::HandleAllocator;
use crate::input::key;
use crate::input::key_mods::ModsBacking;
use crate::lib::allocator::Allocator as CAllocator;

const LOG_TARGET: &str = "key_event";

/// Wrapper around KeyEvent that tracks the allocator for C API usage.
/// The UTF-8 text is not owned by this wrapper - the caller is responsible
/// for ensuring the lifetime of any UTF-8 text set via set_utf8.
pub struct KeyEventWrapper {
    pub event: key::KeyEvent<'static>,
    alloc: HandleAllocator,
}

/// C: GhosttyKeyEvent
pub type Event = *mut KeyEventWrapper;

/// C: ghostty_key_event_new
#[export_name = "ghostty_key_event_new"]
pub extern "C" fn new(alloc_: Option<&CAllocator>, result: &mut Event) -> Result {
    let alloc = HandleAllocator::new(alloc_);
    let ptr = alloc.create(KeyEventWrapper {
        event: key::KeyEvent::DEFAULT,
        alloc,
    });
    if ptr.is_null() {
        return Result::OutOfMemory;
    }
    *result = ptr;
    Result::Success
}

/// C: ghostty_key_event_free
///
/// # Safety
/// `event_` must be NULL or an event from [`new`] not used afterwards.
#[export_name = "ghostty_key_event_free"]
pub unsafe extern "C" fn free(event_: Event) {
    let Some(wrapper) = event_.as_ref() else {
        return;
    };
    let alloc = wrapper.alloc;
    alloc.destroy(event_);
}

/// Convert a C `GhosttyKeyAction`.
pub fn action_from_int(v: c_int) -> Option<key::Action> {
    [key::Action::Release, key::Action::Press, key::Action::Repeat]
        .into_iter()
        .find(|a| *a as c_int == v)
}

/// C: ghostty_key_event_set_action
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_set_action"]
pub unsafe extern "C" fn set_action(event_: Event, action: c_int) {
    let Some(action) = action_from_int(action) else {
        log::warn!(target: LOG_TARGET, "set_action invalid action value={action}");
        return;
    };
    (*event_).event.action = action;
}

/// C: ghostty_key_event_get_action
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_get_action"]
pub unsafe extern "C" fn get_action(event_: Event) -> key::Action {
    (*event_).event.action
}

/// C: ghostty_key_event_set_key
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_set_key"]
pub unsafe extern "C" fn set_key(event_: Event, k: c_int) {
    let Some(k) = key::Key::from_int(k) else {
        log::warn!(target: LOG_TARGET, "set_key invalid key value={k}");
        return;
    };
    (*event_).event.key = k;
}

/// C: ghostty_key_event_get_key
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_get_key"]
pub unsafe extern "C" fn get_key(event_: Event) -> key::Key {
    (*event_).event.key
}

/// C: ghostty_key_event_set_mods
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_set_mods"]
pub unsafe extern "C" fn set_mods(event_: Event, mods: ModsBacking) {
    (*event_).event.mods = key::Mods::from_int(mods);
}

/// C: ghostty_key_event_get_mods
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_get_mods"]
pub unsafe extern "C" fn get_mods(event_: Event) -> ModsBacking {
    (*event_).event.mods.int()
}

/// C: ghostty_key_event_set_consumed_mods
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_set_consumed_mods"]
pub unsafe extern "C" fn set_consumed_mods(event_: Event, consumed_mods: ModsBacking) {
    (*event_).event.consumed_mods = key::Mods::from_int(consumed_mods);
}

/// C: ghostty_key_event_get_consumed_mods
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_get_consumed_mods"]
pub unsafe extern "C" fn get_consumed_mods(event_: Event) -> ModsBacking {
    (*event_).event.consumed_mods.int()
}

/// C: ghostty_key_event_set_composing
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_set_composing"]
pub unsafe extern "C" fn set_composing(event_: Event, composing: bool) {
    (*event_).event.composing = composing;
}

/// C: ghostty_key_event_get_composing
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_get_composing"]
pub unsafe extern "C" fn get_composing(event_: Event) -> bool {
    (*event_).event.composing
}

/// C: ghostty_key_event_set_utf8
///
/// # Safety
/// `event_` must be a valid event; a non-null `utf8` must be valid for
/// `len` reads for as long as the event uses it.
#[export_name = "ghostty_key_event_set_utf8"]
pub unsafe extern "C" fn set_utf8(event_: Event, utf8: *const u8, len: usize) {
    (*event_).event.utf8 = if utf8.is_null() {
        b""
    } else {
        std::slice::from_raw_parts(utf8, len)
    };
}

/// C: ghostty_key_event_get_utf8
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_get_utf8"]
pub unsafe extern "C" fn get_utf8(event_: Event, len: Option<&mut usize>) -> *const u8 {
    let event = &(*event_).event;
    if let Some(l) = len {
        *l = event.utf8.len();
    }
    if event.utf8.is_empty() {
        std::ptr::null()
    } else {
        event.utf8.as_ptr()
    }
}

/// C: ghostty_key_event_set_unshifted_codepoint
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_set_unshifted_codepoint"]
pub unsafe extern "C" fn set_unshifted_codepoint(event_: Event, codepoint: u32) {
    // Zig: `@truncate` to u21.
    (*event_).event.unshifted_codepoint = codepoint & 0x1F_FFFF;
}

/// C: ghostty_key_event_get_unshifted_codepoint
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_key_event_get_unshifted_codepoint"]
pub unsafe extern "C" fn get_unshifted_codepoint(event_: Event) -> u32 {
    (*event_).event.unshifted_codepoint
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;

    fn mods(f: impl FnOnce(&mut key::Mods)) -> ModsBacking {
        let mut m = key::Mods::default();
        f(&mut m);
        m.int()
    }

    #[test]
    fn alloc() {
        let mut e: Event = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));
        unsafe { free(e) };
    }

    #[test]
    fn set() {
        let mut e: Event = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));

        unsafe {
            let ev = |e: Event| &(*e).event;

            // Test action
            set_action(e, key::Action::Press as c_int);
            assert_eq!(key::Action::Press, ev(e).action);

            // Test key
            set_key(e, key::Key::KeyA as c_int);
            assert_eq!(key::Key::KeyA, ev(e).key);

            // Test mods
            set_mods(
                e,
                mods(|m| {
                    m.shift = true;
                    m.ctrl = true;
                }),
            );
            assert!(ev(e).mods.shift);
            assert!(ev(e).mods.ctrl);

            // Test consumed mods
            set_consumed_mods(e, mods(|m| m.shift = true));
            assert!(ev(e).consumed_mods.shift);
            assert!(!ev(e).consumed_mods.ctrl);

            // Test composing
            set_composing(e, true);
            assert!(ev(e).composing);

            // Test UTF-8
            let text = b"hello";
            set_utf8(e, text.as_ptr(), text.len());
            assert_eq!(text, ev(e).utf8);

            // Test UTF-8 null
            set_utf8(e, std::ptr::null(), 0);
            assert_eq!(b"", ev(e).utf8);

            // Test unshifted codepoint
            set_unshifted_codepoint(e, 'a' as u32);
            assert_eq!('a' as u32, ev(e).unshifted_codepoint);

            free(e);
        }
    }

    #[test]
    fn get() {
        let mut e: Event = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));

        unsafe {
            // Set some values
            set_action(e, key::Action::Repeat as c_int);
            set_key(e, key::Key::KeyZ as c_int);
            set_mods(
                e,
                mods(|m| {
                    m.alt = true;
                    m.super_ = true;
                }),
            );
            set_consumed_mods(e, mods(|m| m.alt = true));
            set_composing(e, true);
            let text = b"test";
            set_utf8(e, text.as_ptr(), text.len());
            set_unshifted_codepoint(e, 'z' as u32);

            // Get them back
            assert_eq!(key::Action::Repeat, get_action(e));
            assert_eq!(key::Key::KeyZ, get_key(e));
            let got_mods = key::Mods::from_int(get_mods(e));
            assert!(got_mods.alt);
            assert!(got_mods.super_);
            let got_consumed = key::Mods::from_int(get_consumed_mods(e));
            assert!(got_consumed.alt);
            assert!(!got_consumed.super_);
            assert!(get_composing(e));

            let mut utf8_len: usize = 0;
            let got_utf8 = get_utf8(e, Some(&mut utf8_len));
            assert!(!got_utf8.is_null());
            assert_eq!(4, utf8_len);
            assert_eq!(b"test", std::slice::from_raw_parts(got_utf8, utf8_len));
            assert_eq!('z' as u32, get_unshifted_codepoint(e));

            free(e);
        }
    }

    #[test]
    fn complete_key_event() {
        let mut e: Event = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));

        unsafe {
            // Build a complete key event for shift+a
            set_action(e, key::Action::Press as c_int);
            set_key(e, key::Key::KeyA as c_int);
            set_mods(e, mods(|m| m.shift = true));
            set_consumed_mods(e, mods(|m| m.shift = true));
            let text = b"A";
            set_utf8(e, text.as_ptr(), text.len());
            set_unshifted_codepoint(e, 'a' as u32);

            // Verify all fields
            let ev = &(*e).event;
            assert_eq!(key::Action::Press, ev.action);
            assert_eq!(key::Key::KeyA, ev.key);
            assert!(ev.mods.shift);
            assert!(ev.consumed_mods.shift);
            assert_eq!(b"A", ev.utf8);
            assert_eq!('a' as u32, ev.unshifted_codepoint);

            // Also test the getter
            let mut utf8_len: usize = 0;
            let got_utf8 = get_utf8(e, Some(&mut utf8_len));
            assert!(!got_utf8.is_null());
            assert_eq!(1, utf8_len);
            assert_eq!(b"A", std::slice::from_raw_parts(got_utf8, utf8_len));

            free(e);
        }
    }
}
