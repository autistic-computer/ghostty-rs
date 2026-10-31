//! Port of `src/input/mouse.zig`.
//!
//! STATUS: ported

/// The type of action associated with a mouse event. This is different
/// from ButtonState because button state is simply the current state
/// of a mouse button but an action is something that triggers via
/// an GUI event and supports more.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Press,
    Release,
    Motion,
}

/// The state of a mouse button.
///
/// This is backed by a c_int so we can use this as-is for our embedding API.
///
/// IMPORTANT: Any changes here update include/ghostty.h
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ButtonState {
    Release,
    Press,
}

/// Possible mouse buttons. We only track up to 11 because that's the maximum
/// button input that terminal mouse tracking handles without becoming
/// ambiguous.
///
/// Its a bit silly to name numbers like this but given its a restricted
/// set, it feels better than passing around raw numeric literals.
///
/// This is backed by a c_int so we can use this as-is for our embedding API.
///
/// IMPORTANT: Any changes here update include/ghostty.h
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Button {
    Unknown = 0,
    Left = 1,
    Right = 2,
    Middle = 3,
    Four = 4,
    Five = 5,
    Six = 6,
    Seven = 7,
    Eight = 8,
    Nine = 9,
    Ten = 10,
    Eleven = 11,
}

impl Button {
    /// All buttons in declaration order.
    pub const ALL: [Button; 12] = [
        Button::Unknown,
        Button::Left,
        Button::Right,
        Button::Middle,
        Button::Four,
        Button::Five,
        Button::Six,
        Button::Seven,
        Button::Eight,
        Button::Nine,
        Button::Ten,
        Button::Eleven,
    ];

    /// The maximum value in this enum. Since the values start at zero, a
    /// densely packed array indexed by this enum needs `max + 1` elements.
    pub const MAX: usize = {
        let mut cur = 0;
        let mut i = 0;
        while i < Button::ALL.len() {
            let value = Button::ALL[i] as usize;
            if value > cur {
                cur = value;
            }
            i += 1;
        }

        cur
    };
}

/// The "momentum" of a mouse scroll event. This matches the macOS events
/// because it is the only reliable source right now of momentum events.
/// This is used to handle "inertial scrolling" (i.e. flicking).
///
/// https://developer.apple.com/documentation/appkit/nseventphase
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Momentum {
    #[default]
    None = 0,
    Began = 1,
    Stationary = 2,
    Changed = 3,
    Ended = 4,
    Cancelled = 5,
    MayBegin = 6,
}

impl Momentum {
    /// Convert from the `u3` backing value. Returns `None` for 7, which is
    /// not a valid tag.
    pub const fn from_int(v: u8) -> Option<Momentum> {
        Some(match v {
            0 => Momentum::None,
            1 => Momentum::Began,
            2 => Momentum::Stationary,
            3 => Momentum::Changed,
            4 => Momentum::Ended,
            5 => Momentum::Cancelled,
            6 => Momentum::MayBegin,
            _ => return None,
        })
    }
}

/// The pressure stage of a pressure-sensitive input device.
///
/// This currently only supports the stages that macOS supports.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PressureStage {
    /// The input device is unpressed.
    #[default]
    None = 0,

    /// The input device is pressed a normal amount. On macOS trackpads,
    /// this is after a "click".
    Normal = 1,

    /// The input device is pressed a deep amount. On macOS trackpads,
    /// this is after a "force click".
    Deep = 2,
}

/// The bitmask for mods for scroll events.
///
/// This is a `packed struct(u8)` in Zig: bit 0 is `precision`, bits 1..4
/// are `momentum` and the remaining bits are padding. Use
/// [`ScrollMods::to_bits`] / [`ScrollMods::from_bits`] for the C ABI value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ScrollMods {
    /// True if this is a high-precision scroll event. For example, Apple
    /// devices such as Magic Mouse, trackpads, etc. are high-precision
    /// and send very detailed scroll events.
    pub precision: bool,

    /// The momentum phase (if available, supported) of the scroll event.
    /// This is used to handle "inertial scrolling" (i.e. flicking).
    pub momentum: Momentum,
}

impl ScrollMods {
    /// The packed `u8` representation.
    pub const fn to_bits(self) -> u8 {
        (self.precision as u8) | ((self.momentum as u8) << 1)
    }

    /// Inverse of [`ScrollMods::to_bits`]. An invalid momentum value maps
    /// to [`Momentum::None`].
    pub const fn from_bits(v: u8) -> ScrollMods {
        ScrollMods {
            precision: v & 1 != 0,
            momentum: match Momentum::from_int((v >> 1) & 0b111) {
                Some(m) => m,
                None => Momentum::None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // For our own understanding
    #[test]
    fn scroll_mods() {
        assert_eq!(ScrollMods::default().to_bits(), 0b0);
        assert_eq!(
            ScrollMods {
                precision: true,
                ..Default::default()
            }
            .to_bits(),
            0b0000_0001,
        );
    }

    #[test]
    fn button_max() {
        assert_eq!(Button::MAX, 11);
    }
}
