//! Port of `src/surface_mouse.zig`.
//!
//! STATUS: ported
//!
//! SurfaceMouse represents mouse helper functionality for the core surface.
//!
//! It's currently small in scope; its purpose is to isolate mouse logic that
//! has gotten a bit complex (e.g. pointer shape handling for key events), but
//! the intention is to grow it later so that we can better test said logic).

use crate::input::key::Key;
use crate::input::key_mods::Mods;
use crate::terminal::mouse::{Event as MouseEvent, Shape as MouseShape};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceMouse {
    /// For processing key events; the key that was physically pressed on the
    /// keyboard.
    pub physical_key: Key,

    /// The mouse event tracking mode, if any.
    pub mouse_event: MouseEvent,

    /// The current terminal's mouse shape.
    pub mouse_shape: MouseShape,

    /// The last mods state when the last mouse button (whatever it was) was
    /// pressed or release.
    pub mods: Mods,

    /// True if the mouse position is currently over a link.
    pub over_link: bool,

    /// True if the mouse pointer is currently hidden.
    pub hidden: bool,
}

impl SurfaceMouse {
    /// Translates key state to mouse shape, called during key events. This mainly
    /// handles overrides on key presses depending on whether or not we are in
    /// mouse tracking mode, however it is also responsible for resetting cursor
    /// state on any particular key releases.
    ///
    /// null is returned when the mouse shape does not need changing.
    pub fn key_to_mouse_shape(&self) -> Option<MouseShape> {
        // Filter for appropriate key events
        if !eligible_mouse_shape_key_event(self.physical_key) {
            return None;
        }

        // Exceptions: link hover or hidden state overrides any other shape
        // processing and does not change state.
        //
        // TODO: As we unravel mouse state, we can fix this to be more explicit.
        if self.over_link || self.hidden {
            return None;
        }

        // Handle possible overrides depending on mouse tracking state.
        match self.mouse_event != MouseEvent::None {
            true => {
                // In mouse tracking mode
                if is_mouse_mode_override_state(self.mods) && is_rectangle_select_state(self.mods)
                {
                    // Crosshair (rectangle select), only set if we are also
                    // overriding (e.g. shift+ctrl+alt)
                    return Some(MouseShape::Crosshair);
                } else if is_mouse_mode_override_state(self.mods) {
                    // Normal override state
                    return Some(MouseShape::Text);
                }
            }

            false => {
                // Default terminal mode
                if is_rectangle_select_state(self.mods) {
                    // Crosshair (rectangle select)
                    return Some(MouseShape::Crosshair);
                } else if is_mouse_mode_override_state(self.mods) {
                    // Shift shows an I-beam so selection is obvious even when
                    // the application cursor is not text (OSC 22). Release
                    // restores mouse_shape below.
                    return Some(MouseShape::Text);
                }
            }
        }

        // No overrides means we just revert back to the stored terminal mouse
        // shape. Note that this may be different than what has been currently sent
        // to the apprt, so this will force the reset.
        Some(self.mouse_shape)
    }
}

fn eligible_mouse_shape_key_event(physical_key: Key) -> bool {
    physical_key.ctrl_or_super()
        || physical_key.left_or_right_shift()
        || physical_key.left_or_right_alt()
}

fn is_mouse_mode_override_state(mods: Mods) -> bool {
    mods.shift
}

/// Returns true if our modifiers put us in a state where dragging
/// should cause a rectangle select.
pub fn is_rectangle_select_state(mods: Mods) -> bool {
    if cfg!(any(target_os = "macos", target_os = "ios")) {
        mods.alt
    } else {
        mods.ctrl_or_super() && mods.alt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, super_: bool, alt: bool, shift: bool) -> Mods {
        Mods {
            ctrl,
            super_,
            alt,
            shift,
            ..Mods::default()
        }
    }

    #[test]
    fn key_to_mouse_shape() {
        {
            // No specific key pressed
            let m = SurfaceMouse {
                physical_key: Key::Unidentified,
                mouse_event: MouseEvent::None,
                mouse_shape: MouseShape::Progress,
                mods: Mods::default(),
                over_link: false,
                hidden: false,
            };

            let got = m.key_to_mouse_shape();
            assert!(got.is_none());
        }

        {
            // Over a link. NOTE: This tests that we don't touch the inbound state,
            // not necessarily if we're over a link.
            let m = SurfaceMouse {
                physical_key: Key::ShiftLeft,
                mouse_event: MouseEvent::None,
                mouse_shape: MouseShape::Progress,
                mods: Mods::default(),
                over_link: true,
                hidden: false,
            };

            let got = m.key_to_mouse_shape();
            assert!(got.is_none());
        }

        {
            // Mouse is currently hidden
            let m = SurfaceMouse {
                physical_key: Key::ShiftLeft,
                mouse_event: MouseEvent::None,
                mouse_shape: MouseShape::Progress,
                mods: Mods::default(),
                over_link: true,
                hidden: true,
            };

            let got = m.key_to_mouse_shape();
            assert!(got.is_none());
        }

        {
            // default, no mods (mouse tracking)
            let m = SurfaceMouse {
                physical_key: Key::ShiftLeft,
                mouse_event: MouseEvent::X10,
                mouse_shape: MouseShape::Default,
                mods: Mods::default(),
                over_link: false,
                hidden: false,
            };

            let want = MouseShape::Default;
            let got = m.key_to_mouse_shape();
            assert_eq!(Some(want), got);
        }

        {
            // default -> crosshair (mouse tracking)
            let m = SurfaceMouse {
                physical_key: Key::AltLeft,
                mouse_event: MouseEvent::X10,
                mouse_shape: MouseShape::Default,
                mods: mods(true, true, true, true),
                over_link: false,
                hidden: false,
            };

            let want = MouseShape::Crosshair;
            let got = m.key_to_mouse_shape();
            assert_eq!(Some(want), got);
        }

        {
            // default -> text (mouse tracking)
            let m = SurfaceMouse {
                physical_key: Key::ShiftLeft,
                mouse_event: MouseEvent::X10,
                mouse_shape: MouseShape::Default,
                mods: mods(false, false, false, true),
                over_link: false,
                hidden: false,
            };

            let want = MouseShape::Text;
            let got = m.key_to_mouse_shape();
            assert_eq!(Some(want), got);
        }

        {
            // crosshair -> text (mouse tracking)
            let m = SurfaceMouse {
                physical_key: Key::AltLeft,
                mouse_event: MouseEvent::X10,
                mouse_shape: MouseShape::Crosshair,
                mods: mods(false, false, false, true),
                over_link: false,
                hidden: false,
            };

            let want = MouseShape::Text;
            let got = m.key_to_mouse_shape();
            assert_eq!(Some(want), got);
        }

        {
            // no override restores the application shape (mouse tracking)
            let m = SurfaceMouse {
                physical_key: Key::AltLeft,
                mouse_event: MouseEvent::X10,
                mouse_shape: MouseShape::Crosshair,
                mods: Mods::default(),
                over_link: false,
                hidden: false,
            };

            let want = MouseShape::Crosshair;
            let got = m.key_to_mouse_shape();
            assert_eq!(Some(want), got);
        }

        {
            // text -> crosshair (mouse tracking)
            let m = SurfaceMouse {
                physical_key: Key::AltLeft,
                mouse_event: MouseEvent::X10,
                mouse_shape: MouseShape::Text,
                mods: mods(true, true, true, true),
                over_link: false,
                hidden: false,
            };

            let want = MouseShape::Crosshair;
            let got = m.key_to_mouse_shape();
            assert_eq!(Some(want), got);
        }

        {
            // text, no mods (no mouse tracking)
            let m = SurfaceMouse {
                physical_key: Key::ShiftLeft,
                mouse_event: MouseEvent::None,
                mouse_shape: MouseShape::Text,
                mods: Mods::default(),
                over_link: false,
                hidden: false,
            };

            let want = MouseShape::Text;
            let got = m.key_to_mouse_shape();
            assert_eq!(Some(want), got);
        }

        {
            // text -> crosshair (no mouse tracking)
            let m = SurfaceMouse {
                physical_key: Key::AltLeft,
                mouse_event: MouseEvent::None,
                mouse_shape: MouseShape::Text,
                mods: mods(true, true, true, false),
                over_link: false,
                hidden: false,
            };

            let want = MouseShape::Crosshair;
            let got = m.key_to_mouse_shape();
            assert_eq!(Some(want), got);
        }

        {
            // no override restores the application shape (no mouse tracking)
            let m = SurfaceMouse {
                physical_key: Key::AltLeft,
                mouse_event: MouseEvent::None,
                mouse_shape: MouseShape::Crosshair,
                mods: Mods::default(),
                over_link: false,
                hidden: false,
            };

            let want = MouseShape::Crosshair;
            let got = m.key_to_mouse_shape();
            assert_eq!(Some(want), got);
        }
    }
}
