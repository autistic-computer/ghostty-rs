//! Port of `src/terminal/c/mouse_event.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port: enum arguments are taken as `c_int` and validated
//! like Zig's runtime safety check; `Mods` crosses the ABI as its `u16`
//! backing integer.

use std::ffi::c_int;

use super::result::Result;
use super::types::HandleAllocator;
use crate::input::key_mods::{Mods as KeyMods, ModsBacking};
use crate::input::mouse;
use crate::input::mouse_encode;
use crate::lib::allocator::Allocator as CAllocator;

const LOG_TARGET: &str = "mouse_event";

/// Wrapper around mouse event that tracks the allocator for C API usage.
pub struct MouseEventWrapper {
    pub event: mouse_encode::Event,
    alloc: HandleAllocator,
}

/// C: GhosttyMouseEvent
pub type Event = *mut MouseEventWrapper;

/// C: GhosttyMouseAction
pub type Action = mouse::Action;

/// C: GhosttyMouseButton
pub type Button = mouse::Button;

/// C: GhosttyMousePosition
pub type Position = mouse_encode::Pos;

/// C: GhosttyMods
pub type Mods = ModsBacking;

/// Convert a C `GhosttyMouseAction`.
pub fn action_from_int(v: c_int) -> Option<Action> {
    [Action::Press, Action::Release, Action::Motion]
        .into_iter()
        .find(|a| *a as c_int == v)
}

/// Convert a C `GhosttyMouseButton`.
pub fn button_from_int(v: c_int) -> Option<Button> {
    Button::ALL.into_iter().find(|b| *b as c_int == v)
}

/// C: ghostty_mouse_event_new
#[export_name = "ghostty_mouse_event_new"]
pub extern "C" fn new(alloc_: Option<&CAllocator>, result: &mut Event) -> Result {
    let alloc = HandleAllocator::new(alloc_);
    let ptr = alloc.create(MouseEventWrapper {
        event: mouse_encode::Event::default(),
        alloc,
    });
    if ptr.is_null() {
        return Result::OutOfMemory;
    }
    *result = ptr;
    Result::Success
}

/// C: ghostty_mouse_event_free
///
/// # Safety
/// `event_` must be NULL or an event from [`new`] not used afterwards.
#[export_name = "ghostty_mouse_event_free"]
pub unsafe extern "C" fn free(event_: Event) {
    let Some(wrapper) = event_.as_ref() else {
        return;
    };
    let alloc = wrapper.alloc;
    alloc.destroy(event_);
}

/// C: ghostty_mouse_event_set_action
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_mouse_event_set_action"]
pub unsafe extern "C" fn set_action(event_: Event, action: c_int) {
    let Some(action) = action_from_int(action) else {
        log::warn!(target: LOG_TARGET, "set_action invalid action value={action}");
        return;
    };
    (*event_).event.action = action;
}

/// C: ghostty_mouse_event_get_action
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_mouse_event_get_action"]
pub unsafe extern "C" fn get_action(event_: Event) -> Action {
    (*event_).event.action
}

/// C: ghostty_mouse_event_set_button
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_mouse_event_set_button"]
pub unsafe extern "C" fn set_button(event_: Event, button: c_int) {
    let Some(button) = button_from_int(button) else {
        log::warn!(target: LOG_TARGET, "set_button invalid button value={button}");
        return;
    };
    (*event_).event.button = Some(button);
}

/// C: ghostty_mouse_event_clear_button
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_mouse_event_clear_button"]
pub unsafe extern "C" fn clear_button(event_: Event) {
    (*event_).event.button = None;
}

/// C: ghostty_mouse_event_get_button
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_mouse_event_get_button"]
pub unsafe extern "C" fn get_button(event_: Event, out: Option<&mut Button>) -> bool {
    if let Some(button) = (*event_).event.button {
        if let Some(ptr) = out {
            *ptr = button;
        }
        return true;
    }
    false
}

/// C: ghostty_mouse_event_set_mods
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_mouse_event_set_mods"]
pub unsafe extern "C" fn set_mods(event_: Event, mods: Mods) {
    (*event_).event.mods = KeyMods::from_int(mods);
}

/// C: ghostty_mouse_event_get_mods
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_mouse_event_get_mods"]
pub unsafe extern "C" fn get_mods(event_: Event) -> Mods {
    (*event_).event.mods.int()
}

/// C: ghostty_mouse_event_set_position
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_mouse_event_set_position"]
pub unsafe extern "C" fn set_position(event_: Event, pos: Position) {
    (*event_).event.pos = pos;
}

/// C: ghostty_mouse_event_get_position
///
/// # Safety
/// `event_` must be a valid event.
#[export_name = "ghostty_mouse_event_get_position"]
pub unsafe extern "C" fn get_position(event_: Event) -> Position {
    (*event_).event.pos
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;

    #[test]
    fn alloc() {
        let mut e: Event = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));
        unsafe { free(e) };
    }

    #[test]
    fn free_null() {
        unsafe { free(std::ptr::null_mut()) };
    }

    #[test]
    fn set_get() {
        let mut e: Event = std::ptr::null_mut();
        assert_eq!(Result::Success, new(Some(&C_ALLOCATOR), &mut e));

        unsafe {
            // Action
            set_action(e, Action::Motion as c_int);
            assert_eq!(Action::Motion, get_action(e));

            // Button
            set_button(e, Button::Left as c_int);
            let mut button = Button::Unknown;
            assert!(get_button(e, Some(&mut button)));
            assert_eq!(Button::Left, button);
            assert!(get_button(e, None));
            clear_button(e);
            assert!(!get_button(e, Some(&mut button)));

            // Mods
            let mut mods = KeyMods::default();
            mods.shift = true;
            mods.ctrl = true;
            set_mods(e, mods.int());
            let got_mods = KeyMods::from_int(get_mods(e));
            assert!(got_mods.shift);
            assert!(got_mods.ctrl);
            assert!(!got_mods.alt);

            // Position
            set_position(e, Position { x: 12.5, y: -4.0 });
            let pos = get_position(e);
            assert_eq!(12.5f32, pos.x);
            assert_eq!(-4.0f32, pos.y);

            free(e);
        }
    }
}
