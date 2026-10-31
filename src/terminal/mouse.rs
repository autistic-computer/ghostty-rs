//! Port of `src/terminal/mouse.zig`.
//!
//! STATUS: ported
//!
//! Deviations: `Shape.getGObjectType` (GObject enum registration for the GTK
//! apprt) belongs to the GTK apprt port and is not provided here; the
//! `ghostty.h MouseShape` test compares against the C header and is not
//! ported (the C header is not part of the Rust crate).

/// The event types that can be reported for mouse-related activities.
/// These are all mutually exclusive (hence in a single enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Event {
    #[default]
    None = 0,
    /// 9
    X10 = 1,
    /// 1000
    Normal = 2,
    /// 1002
    Button = 3,
    /// 1003
    Any = 4,
}

/// Returns true if this event sends motion events.
pub fn event_sends_motion(event: Event) -> bool {
    event == Event::Button || event == Event::Any
}

/// The format of mouse events when enabled.
/// These are all mutually exclusive (hence in a single enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Format {
    #[default]
    X10 = 0,
    /// 1005
    Utf8 = 1,
    /// 1006
    Sgr = 2,
    /// 1015
    Urxvt = 3,
    /// 1016
    SgrPixels = 4,
}

/// The possible cursor shapes. Not all app runtimes support these shapes.
/// The shapes are always based on the W3C supported cursor styles so we
/// can have a cross platform list.
//
// Must be kept in sync with ghostty_cursor_shape_e
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub enum Shape {
    #[default]
    Default,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    AllScroll,
    ColResize,
    RowResize,
    NResize,
    EResize,
    SResize,
    WResize,
    NeResize,
    NwResize,
    SeResize,
    SwResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ZoomIn,
    ZoomOut,
}

impl Shape {
    /// Build cursor shape from string or null if its unknown.
    pub fn from_string(v: &[u8]) -> Option<Shape> {
        STRING_MAP
            .iter()
            .find(|(k, _)| k.as_bytes() == v)
            .map(|&(_, shape)| shape)
    }
}

static STRING_MAP: &[(&str, Shape)] = &[
    // W3C
    ("default", Shape::Default),
    ("context-menu", Shape::ContextMenu),
    ("help", Shape::Help),
    ("pointer", Shape::Pointer),
    ("progress", Shape::Progress),
    ("wait", Shape::Wait),
    ("cell", Shape::Cell),
    ("crosshair", Shape::Crosshair),
    ("text", Shape::Text),
    ("vertical-text", Shape::VerticalText),
    ("alias", Shape::Alias),
    ("copy", Shape::Copy),
    ("move", Shape::Move),
    ("no-drop", Shape::NoDrop),
    ("not-allowed", Shape::NotAllowed),
    ("grab", Shape::Grab),
    ("grabbing", Shape::Grabbing),
    ("all-scroll", Shape::AllScroll),
    ("col-resize", Shape::ColResize),
    ("row-resize", Shape::RowResize),
    ("n-resize", Shape::NResize),
    ("e-resize", Shape::EResize),
    ("s-resize", Shape::SResize),
    ("w-resize", Shape::WResize),
    ("ne-resize", Shape::NeResize),
    ("nw-resize", Shape::NwResize),
    ("se-resize", Shape::SeResize),
    ("sw-resize", Shape::SwResize),
    ("ew-resize", Shape::EwResize),
    ("ns-resize", Shape::NsResize),
    ("nesw-resize", Shape::NeswResize),
    ("nwse-resize", Shape::NwseResize),
    ("zoom-in", Shape::ZoomIn),
    ("zoom-out", Shape::ZoomOut),
    // xterm/foot
    ("left_ptr", Shape::Default),
    ("question_arrow", Shape::Help),
    ("hand", Shape::Pointer),
    ("left_ptr_watch", Shape::Progress),
    ("watch", Shape::Wait),
    ("cross", Shape::Crosshair),
    ("xterm", Shape::Text),
    ("dnd-link", Shape::Alias),
    ("dnd-copy", Shape::Copy),
    ("dnd-move", Shape::Move),
    ("dnd-no-drop", Shape::NoDrop),
    ("crossed_circle", Shape::NotAllowed),
    ("hand1", Shape::Grab),
    ("right_side", Shape::EResize),
    ("top_side", Shape::NResize),
    ("top_right_corner", Shape::NeResize),
    ("top_left_corner", Shape::NwResize),
    ("bottom_side", Shape::SResize),
    ("bottom_right_corner", Shape::SeResize),
    ("bottom_left_corner", Shape::SwResize),
    ("left_side", Shape::WResize),
    ("fleur", Shape::AllScroll),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_shape_from_string() {
        assert_eq!(Shape::Default, Shape::from_string(b"default").unwrap());
        assert_eq!(Shape::NwseResize, Shape::from_string(b"nwse-resize").unwrap());
        assert_eq!(Shape::AllScroll, Shape::from_string(b"fleur").unwrap());
        assert_eq!(None, Shape::from_string(b"bogus"));
    }

    #[test]
    fn shape_c_values() {
        // Must be kept in sync with ghostty_cursor_shape_e.
        assert_eq!(std::mem::size_of::<Shape>(), std::mem::size_of::<std::ffi::c_int>());
        assert_eq!(Shape::Default as i32, 0);
        assert_eq!(Shape::ZoomOut as i32, 33);
    }

    #[test]
    fn sends_motion() {
        assert!(event_sends_motion(Event::Button));
        assert!(event_sends_motion(Event::Any));
        assert!(!event_sends_motion(Event::Normal));
        assert!(!event_sends_motion(Event::None));
    }
}
