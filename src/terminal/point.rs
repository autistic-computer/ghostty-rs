//! Port of `src/terminal/point.zig`.
//!
//! STATUS: ported

use crate::terminal::size;

/// The possible reference locations for a point. When someone says "(42, 80)"
/// in the context of a terminal, that could mean multiple things: it is in the
/// current visible viewport? the current active area of the screen where the
/// cursor is? the entire scrollback history? etc.
///
/// This tag is used to differentiate those cases.
///
/// (Zig: `lib.Enum(lib.target, ...)`, a C-ABI compatible enum.)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tag {
    /// Top-left is part of the active area where a running program can
    /// jump the cursor and make changes. The active area is the "editable"
    /// part of the screen.
    ///
    /// The bottom-right of the active tag differs from all other tags
    /// because it includes the full height (rows) of the screen, including
    /// rows that may not be written yet. This is required because the active
    /// area is fully "addressable" by the running program (see below) whereas
    /// the other tags are used primarily for reading/modifying past-written
    /// data so they can't address unwritten rows.
    ///
    /// Note for those less familiar with terminal functionality: there
    /// are escape sequences to move the cursor to any position on
    /// the screen, but it is limited to the size of the viewport and
    /// the bottommost part of the screen. Terminal programs can't --
    /// with sequences at the time of writing this comment -- modify
    /// anything in the scrollback, visible viewport (if it differs
    /// from the active area), etc.
    Active = 0,

    /// Top-left is the visible viewport. This means that if the user
    /// has scrolled in any direction, top-left changes. The bottom-right
    /// is the last written row from the top-left.
    Viewport = 1,

    /// Top-left is the furthest back in the scrollback history
    /// supported by the screen and the bottom-right is the bottom-right
    /// of the last written row. Note this last point is important: the
    /// bottom right is NOT necessarily the same as "active" because
    /// "active" always allows referencing the full rows tall of the
    /// screen whereas "screen" only contains written rows.
    Screen = 2,

    /// The top-left is the same as "screen" but the bottom-right is
    /// the line just before the top of "active". This contains only
    /// the scrollback history.
    History = 3,
}

impl Tag {
    /// The Zig tag name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Tag::Active => "active",
            Tag::Viewport => "viewport",
            Tag::Screen => "screen",
            Tag::History => "history",
        }
    }
}

/// An x/y point in the terminal for some definition of location (tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Point {
    Active(Coordinate),
    Viewport(Coordinate),
    Screen(Coordinate),
    History(Coordinate),
}

/// C ABI value union for [`Point`] (Zig: `Point.CValue`).
///
/// Padding: largest variant is Coordinate (u16 + u32 = 6 bytes).
/// Use [2]u64 (16 bytes) for future expansion.
#[repr(C)]
#[derive(Clone, Copy)]
pub union PointCValue {
    pub active: Coordinate,
    pub viewport: Coordinate,
    pub screen: Coordinate,
    pub history: Coordinate,
    pub _padding: [u64; 2],
}

/// C ABI tagged union for [`Point`] (Zig: `Point.C`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PointC {
    pub tag: Tag,
    pub value: PointCValue,
}

impl Point {
    /// The tag of this point.
    #[inline]
    pub fn tag(self) -> Tag {
        match self {
            Point::Active(_) => Tag::Active,
            Point::Viewport(_) => Tag::Viewport,
            Point::Screen(_) => Tag::Screen,
            Point::History(_) => Tag::History,
        }
    }

    #[inline]
    pub fn coord(self) -> Coordinate {
        match self {
            Point::Active(v) | Point::Viewport(v) | Point::Screen(v) | Point::History(v) => v,
        }
    }

    /// Convert to the C ABI tagged union.
    pub fn cval(self) -> PointC {
        let value = match self {
            Point::Active(v) => PointCValue { active: v },
            Point::Viewport(v) => PointCValue { viewport: v },
            Point::Screen(v) => PointCValue { screen: v },
            Point::History(v) => PointCValue { history: v },
        };
        PointC {
            tag: self.tag(),
            value,
        }
    }

    /// Convert a C ABI point into the native tagged union.
    pub fn from_c(pt: PointC) -> Point {
        // SAFETY: the tag determines which union field is active; every
        // field has the same type anyway.
        unsafe {
            match pt.tag {
                Tag::Active => Point::Active(pt.value.active),
                Tag::Viewport => Point::Viewport(pt.value.viewport),
                Tag::Screen => Point::Screen(pt.value.screen),
                Tag::History => Point::History(pt.value.history),
            }
        }
    }
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Coordinate {
    /// x can use size.CellCountInt because the number of columns
    /// can't ever be more than a valid number of columns in a Page.
    pub x: size::CellCountInt,

    /// y does not use size.CellCountInt because certain coordinate
    /// usage such as screen/history can have more rows than are possible
    /// in a single page.
    pub y: u32,
}

impl Coordinate {
    pub fn eql(self, other: Coordinate) -> bool {
        self.x == other.x && self.y == other.y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // point.zig has no tests; these check the C ABI conversion round-trip.
    #[test]
    fn c_abi_roundtrip() {
        assert_eq!(core::mem::size_of::<PointCValue>(), 16);
        let pts = [
            Point::Active(Coordinate { x: 1, y: 2 }),
            Point::Viewport(Coordinate { x: 3, y: 4 }),
            Point::Screen(Coordinate { x: 5, y: 6 }),
            Point::History(Coordinate { x: 7, y: 8 }),
        ];
        for pt in pts {
            assert_eq!(pt, Point::from_c(pt.cval()));
            assert!(pt.coord().eql(Point::from_c(pt.cval()).coord()));
        }
    }
}
