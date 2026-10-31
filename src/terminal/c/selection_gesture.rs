//! Port of `src/terminal/c/selection_gesture.zig`.
//!
//! STATUS: ported
//!
//! Notes on the port:
//! - The Rust `SelectionGesture` event payloads (`Press<'a>`, `Drag<'a>`,
//!   ...) borrow their word boundary codepoints and behaviors, which Zig
//!   points at storage inside the event wrapper. A self-referential
//!   wrapper is not expressible in safe Rust, so the wrapper stores the
//!   payload fields that it owns ([`EventPayload`]) next to the backing
//!   storage and builds the borrowed payload when the event is handled.
//!   An unset pin/geometry/viewport (Zig: `undefined`, guarded by the
//!   `event_validation` flags) is `None`.
//! - The C API receives timestamps as nanoseconds; `std.Io.Timestamp` is
//!   `std::time::Instant` in Rust, which cannot be built from a raw
//!   value, so [`instant_from_ns`] offsets a fixed process-wide base
//!   instant (the gesture only compares and subtracts timestamps).
//! - Enum arguments and enum values read from C (`Behavior` in
//!   [`Behaviors`]) cross the ABI as `c_int` and are validated; the C
//!   `Behavior`/`Autoscroll` enums mirror the core enums and are converted
//!   at the boundary. `bool` values are read as bytes.

use std::ffi::{c_int, c_void};
use std::sync::LazyLock;

use super::grid_ref;
use super::result::Result;
use super::selection as selection_c;
use super::terminal as terminal_c;
use super::types::{self, HandleAllocator};
use crate::lib::allocator::Allocator as CAllocator;
use crate::terminal::page_list::Pin;
use crate::terminal::point;
use crate::terminal::selection_codepoints;
use crate::terminal::selection_gesture::{self as gesturepkg, SelectionGesture, Timestamp};

const LOG_TARGET: &str = "selection_gesture_c";

/// C: GhosttySelectionGesture
pub type Gesture = *mut GestureWrapper;

/// C: GhosttySelectionGestureEvent
pub type Event = *mut EventWrapper;

pub struct GestureWrapper {
    alloc: HandleAllocator,
    gesture: SelectionGesture,
}

/// The owned fields of a `SelectionGesture.Press` (see the module notes).
#[derive(Debug, Clone, Copy)]
pub struct PressData {
    pub time: Option<Timestamp>,
    pub pin: Option<Pin>,
    pub xpos: f64,
    pub ypos: f64,
    pub max_distance: f64,
    pub repeat_interval: u64,
}

/// The owned fields of a `SelectionGesture.Drag`.
#[derive(Debug, Clone, Copy)]
pub struct DragData {
    pub pin: Option<Pin>,
    pub xpos: f64,
    pub ypos: f64,
    pub rectangle: bool,
    pub geometry: Option<gesturepkg::DragGeometry>,
}

/// The owned fields of a `SelectionGesture.AutoscrollTick`.
#[derive(Debug, Clone, Copy)]
pub struct AutoscrollTickData {
    pub viewport: Option<point::Coordinate>,
    pub xpos: f64,
    pub ypos: f64,
    pub rectangle: bool,
    pub geometry: Option<gesturepkg::DragGeometry>,
}

/// (Zig: the `union(EventType)` of `EventWrapper.event`.)
#[derive(Debug, Clone, Copy)]
pub enum EventPayload {
    Press(PressData),
    Release(gesturepkg::Release),
    Drag(DragData),
    AutoscrollTick(AutoscrollTickData),
    DeepPress,
}

/// Validation sidecar for required event fields that don't have safe
/// sentinels in the real SelectionGesture payloads. For example,
/// PageList.Pin contains a non-null node pointer and Geometry has no
/// meaningful zero value. Keep these as one-bit flags so dispatch can
/// reject incomplete C events instead of using undefined placeholder data.
#[derive(Debug, Clone, Copy, Default)]
pub struct EventValidation {
    pub press_pin_set: bool,
    pub drag_pin_set: bool,
    pub drag_geometry_set: bool,
    pub autoscroll_tick_viewport_set: bool,
    pub autoscroll_tick_geometry_set: bool,
}

pub struct EventWrapper {
    alloc: HandleAllocator,
    pub event: EventPayload,

    pub event_validation: EventValidation,

    /// Backing storage for Press/Drag/AutoscrollTick.word_boundary_codepoints.
    /// The C API receives codepoints as borrowed uint32_t values; we copy
    /// them into event-owned storage so the real payload can safely point
    /// at it until the event is changed or freed. `None` selects the
    /// default word boundaries.
    pub word_boundary_codepoints: Option<Vec<u32>>,

    /// Backing storage for Press.behaviors. The C API sets behaviors as a
    /// value struct, but SelectionGesture.Press stores a pointer to a
    /// [3]Behavior.
    pub behaviors: [gesturepkg::Behavior; 3],
}

impl EventWrapper {
    fn init(&mut self, event_type: EventType) {
        self.event = match event_type {
            EventType::Press => EventPayload::Press(self.default_press()),
            EventType::Release => EventPayload::Release(self.default_release()),
            EventType::Drag => EventPayload::Drag(self.default_drag()),
            EventType::AutoscrollTick => EventPayload::AutoscrollTick(self.default_autoscroll_tick()),
            EventType::DeepPress => EventPayload::DeepPress,
        };
    }

    fn default_press(&self) -> PressData {
        PressData {
            time: None,
            pin: None,
            xpos: 0.0,
            ypos: 0.0,
            max_distance: 0.0,
            repeat_interval: 0,
        }
    }

    fn default_release(&self) -> gesturepkg::Release {
        gesturepkg::Release { pin: None }
    }

    fn default_drag(&self) -> DragData {
        DragData {
            pin: None,
            xpos: 0.0,
            ypos: 0.0,
            rectangle: false,
            geometry: None,
        }
    }

    fn default_autoscroll_tick(&self) -> AutoscrollTickData {
        AutoscrollTickData {
            viewport: None,
            xpos: 0.0,
            ypos: 0.0,
            rectangle: false,
            geometry: None,
        }
    }

    /// The word boundary codepoints of the payload (Zig: the payload's
    /// `word_boundary_codepoints` slice).
    pub fn word_boundary_codepoints(&self) -> &[u32] {
        match &self.word_boundary_codepoints {
            Some(cps) => cps,
            None => &selection_codepoints::DEFAULT_WORD_BOUNDARIES,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttySelectionGestureBehavior (Zig: `SelectionGesture.Behavior`).
    pub enum Behavior {
        Cell = 0,
        Word = 1,
        Line = 2,
        Output = 3,
    }
}

impl Behavior {
    pub fn from_zig(b: gesturepkg::Behavior) -> Behavior {
        match b {
            gesturepkg::Behavior::Cell => Behavior::Cell,
            gesturepkg::Behavior::Word => Behavior::Word,
            gesturepkg::Behavior::Line => Behavior::Line,
            gesturepkg::Behavior::Output => Behavior::Output,
        }
    }

    pub fn to_zig(self) -> gesturepkg::Behavior {
        match self {
            Behavior::Cell => gesturepkg::Behavior::Cell,
            Behavior::Word => gesturepkg::Behavior::Word,
            Behavior::Line => gesturepkg::Behavior::Line,
            Behavior::Output => gesturepkg::Behavior::Output,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttySelectionGestureAutoscroll (Zig: `SelectionGesture.Autoscroll`).
    pub enum Autoscroll {
        None = 0,
        Up = 1,
        Down = 2,
    }
}

impl Autoscroll {
    pub fn from_zig(a: gesturepkg::Autoscroll) -> Autoscroll {
        match a {
            gesturepkg::Autoscroll::None => Autoscroll::None,
            gesturepkg::Autoscroll::Up => Autoscroll::Up,
            gesturepkg::Autoscroll::Down => Autoscroll::Down,
        }
    }
}

/// C: GhosttySelectionGestureBehaviors
///
/// The fields are `GhosttySelectionGestureBehavior` values (C ints,
/// validated when set).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Behaviors {
    pub single_click: c_int,
    pub double_click: c_int,
    pub triple_click: c_int,
}

impl Behaviors {
    pub fn new(single_click: Behavior, double_click: Behavior, triple_click: Behavior) -> Behaviors {
        Behaviors {
            single_click: single_click as c_int,
            double_click: double_click as c_int,
            triple_click: triple_click as c_int,
        }
    }
}

crate::c_api_enum! {
    /// C: GhosttySelectionGestureData
    ///
    /// Output types: `click_count` → `u8`, `dragged` → `bool`,
    /// `autoscroll` → [`Autoscroll`], `behavior` → [`Behavior`], `anchor`
    /// → `GhosttyGridRef`.
    pub enum Data {
        ClickCount = 0,
        Dragged = 1,
        Autoscroll = 2,
        Behavior = 3,
        Anchor = 4,
    }
}

crate::c_api_enum! {
    /// C: GhosttySelectionGestureEventType
    pub enum EventType {
        Press = 0,
        Release = 1,
        Drag = 2,
        AutoscrollTick = 3,
        DeepPress = 4,
    }
}

crate::c_api_enum! {
    /// C: GhosttySelectionGestureEventOption
    ///
    /// Value types: `ref` → `GhosttyGridRef`, `position` →
    /// `GhosttySurfacePosition`, `repeat_distance` → `f64`,
    /// `time_ns`/`repeat_interval_ns` → `u64`, `word_boundary_codepoints`
    /// → `GhosttyCodepoints`, `behaviors` → [`Behaviors`], `rectangle` →
    /// `bool`, `geometry` → [`Geometry`], `viewport` →
    /// `GhosttyPointCoordinate`.
    pub enum EventOption {
        Ref = 0,
        Position = 1,
        RepeatDistance = 2,
        TimeNs = 3,
        RepeatIntervalNs = 4,
        WordBoundaryCodepoints = 5,
        Behaviors = 6,
        Rectangle = 7,
        Geometry = 8,
        Viewport = 9,
    }
}

/// C: GhosttySelectionGestureGeometry
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub columns: u32,
    pub cell_width: u32,
    pub padding_left: u32,
    pub screen_height: u32,
}

impl Geometry {
    fn to_zig(self) -> Option<gesturepkg::DragGeometry> {
        if self.columns == 0 {
            return None;
        }
        if self.cell_width == 0 {
            return None;
        }
        if self.screen_height == 0 {
            return None;
        }
        Some(gesturepkg::DragGeometry {
            columns: self.columns,
            cell_width: self.cell_width,
            padding_left: self.padding_left,
            screen_height: self.screen_height,
        })
    }
}

/// C: ghostty_selection_gesture_new
///
/// # Safety
/// `out_gesture` must be NULL or valid; the allocator (if any) must
/// outlive the gesture.
#[export_name = "ghostty_selection_gesture_new"]
pub unsafe extern "C" fn new(alloc_: Option<&CAllocator>, out_gesture: *mut Gesture) -> Result {
    let Some(out) = out_gesture.as_mut() else {
        return Result::InvalidValue;
    };

    let alloc = HandleAllocator::new(alloc_);
    let gesture = alloc.create(GestureWrapper {
        alloc,
        gesture: SelectionGesture::default(),
    });
    if gesture.is_null() {
        *out = std::ptr::null_mut();
        return Result::OutOfMemory;
    }
    *out = gesture;
    Result::Success
}

/// C: ghostty_selection_gesture_event_new
///
/// # Safety
/// `out_event` must be NULL or valid; the allocator (if any) must
/// outlive the event.
#[export_name = "ghostty_selection_gesture_event_new"]
pub unsafe extern "C" fn event_new(alloc_: Option<&CAllocator>, out_event: *mut Event, event_type: c_int) -> Result {
    let Some(out) = out_event.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(event_type) = EventType::from_int(event_type) else {
        return Result::InvalidValue;
    };

    let alloc = HandleAllocator::new(alloc_);
    let event = alloc.create(EventWrapper {
        alloc,
        event: EventPayload::DeepPress,
        event_validation: EventValidation::default(),
        word_boundary_codepoints: None,
        behaviors: gesturepkg::DEFAULT_BEHAVIORS,
    });
    if event.is_null() {
        *out = std::ptr::null_mut();
        return Result::OutOfMemory;
    }
    (*event).init(event_type);
    *out = event;
    Result::Success
}

/// C: ghostty_selection_gesture_free
///
/// # Safety
/// `gesture_` must be NULL or a gesture not used afterwards; `terminal`
/// must be NULL or live.
#[export_name = "ghostty_selection_gesture_free"]
pub unsafe extern "C" fn free(gesture_: Gesture, terminal: terminal_c::Terminal) {
    let Some(wrapper) = gesture_.as_mut() else {
        return;
    };
    if let Some(t) = terminal_c::zig_terminal(terminal) {
        wrapper.gesture.deinit(t);
    }
    let alloc = wrapper.alloc;
    alloc.destroy(gesture_);
}

/// C: ghostty_selection_gesture_event_free
///
/// # Safety
/// `event_` must be NULL or an event not used afterwards.
#[export_name = "ghostty_selection_gesture_event_free"]
pub unsafe extern "C" fn event_free(event_: Event) {
    let Some(event) = event_.as_ref() else {
        return;
    };
    let alloc = event.alloc;
    alloc.destroy(event_);
}

/// C: ghostty_selection_gesture_reset
///
/// # Safety
/// Handles must be NULL or live.
#[export_name = "ghostty_selection_gesture_reset"]
pub unsafe extern "C" fn reset(gesture_: Gesture, terminal: terminal_c::Terminal) {
    let Some(wrapper) = gesture_.as_mut() else {
        return;
    };
    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return;
    };
    wrapper.gesture.reset(t);
}

/// Write a gesture result (Zig: the `out_selection` handling repeated in
/// each `handleEvent` arm).
unsafe fn write_selection(
    sel: Option<crate::terminal::selection::Selection>,
    out_selection: *mut selection_c::CSelection,
) -> Result {
    let Some(sel) = sel else {
        return Result::NoValue;
    };
    if let Some(out) = out_selection.as_mut() {
        *out = selection_c::CSelection::from_zig(sel);
    }
    Result::Success
}

/// C: ghostty_selection_gesture_event
///
/// # Safety
/// Handles must be NULL or live; `out_selection` must be NULL or valid.
#[export_name = "ghostty_selection_gesture_event"]
pub unsafe extern "C" fn handle_event(
    gesture_: Gesture,
    terminal: terminal_c::Terminal,
    event_: Event,
    out_selection: *mut selection_c::CSelection,
) -> Result {
    let Some(wrapper) = gesture_.as_mut() else {
        return Result::InvalidValue;
    };
    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };
    let Some(event_wrapper) = event_.as_ref() else {
        return Result::InvalidValue;
    };
    let validation = event_wrapper.event_validation;
    let word_boundary_codepoints = event_wrapper.word_boundary_codepoints();

    match event_wrapper.event {
        EventPayload::Press(press) => {
            if !validation.press_pin_set {
                return Result::InvalidValue;
            }
            let Some(pin) = press.pin else {
                return Result::InvalidValue;
            };
            let sel = match wrapper.gesture.press(
                t,
                gesturepkg::Press {
                    time: press.time,
                    pin,
                    xpos: press.xpos,
                    ypos: press.ypos,
                    max_distance: press.max_distance,
                    repeat_interval: press.repeat_interval,
                    word_boundary_codepoints,
                    behaviors: &event_wrapper.behaviors,
                },
            ) {
                Ok(sel) => sel,
                Err(_) => return Result::OutOfMemory,
            };
            write_selection(sel, out_selection)
        }
        EventPayload::Release(release) => {
            wrapper.gesture.release(t, release);
            Result::NoValue
        }
        EventPayload::Drag(drag) => {
            if !validation.drag_pin_set {
                return Result::InvalidValue;
            }
            if !validation.drag_geometry_set {
                return Result::InvalidValue;
            }
            let (Some(pin), Some(geometry)) = (drag.pin, drag.geometry) else {
                return Result::InvalidValue;
            };
            let sel = wrapper.gesture.drag(
                t,
                gesturepkg::Drag {
                    pin,
                    xpos: drag.xpos,
                    ypos: drag.ypos,
                    rectangle: drag.rectangle,
                    word_boundary_codepoints,
                    geometry,
                },
            );
            write_selection(sel, out_selection)
        }
        EventPayload::AutoscrollTick(tick) => {
            if !validation.autoscroll_tick_viewport_set {
                return Result::InvalidValue;
            }
            if !validation.autoscroll_tick_geometry_set {
                return Result::InvalidValue;
            }
            let (Some(viewport), Some(geometry)) = (tick.viewport, tick.geometry) else {
                return Result::InvalidValue;
            };
            let sel = wrapper.gesture.autoscroll_tick(
                t,
                gesturepkg::AutoscrollTick {
                    viewport,
                    xpos: tick.xpos,
                    ypos: tick.ypos,
                    rectangle: tick.rectangle,
                    word_boundary_codepoints,
                    geometry,
                },
            );
            write_selection(sel, out_selection)
        }
        EventPayload::DeepPress => {
            let sel = wrapper.gesture.deep_press(t, gesturepkg::DeepPress { word_boundary_codepoints });
            write_selection(sel, out_selection)
        }
    }
}

/// C: ghostty_selection_gesture_event_set
///
/// # Safety
/// `event_` must be NULL or live; `value` must be NULL or point to the
/// option's value type.
#[export_name = "ghostty_selection_gesture_event_set"]
pub unsafe extern "C" fn event_set(event_: Event, option: c_int, value: *const c_void) -> Result {
    let Some(option) = EventOption::from_int(option) else {
        log::warn!(target: LOG_TARGET, "selection_gesture_event_set invalid option value={option}");
        return Result::InvalidValue;
    };

    event_set_typed(event_, option, value)
}

/// C: ghostty_selection_gesture_get
///
/// # Safety
/// Handles must be NULL or live; `out` must be NULL or point to the data
/// kind's output type.
#[export_name = "ghostty_selection_gesture_get"]
pub unsafe extern "C" fn get(gesture_: Gesture, terminal: terminal_c::Terminal, data: c_int, out: *mut c_void) -> Result {
    let Some(data) = Data::from_int(data) else {
        log::warn!(target: LOG_TARGET, "selection_gesture_get invalid data value={data}");
        return Result::InvalidValue;
    };

    if out.is_null() {
        return Result::InvalidValue;
    }
    get_typed(gesture_, terminal, data, out)
}

/// C: ghostty_selection_gesture_get_multi
///
/// # Safety
/// `keys` and `values` must be NULL or valid for `count` entries; see
/// [`get`].
#[export_name = "ghostty_selection_gesture_get_multi"]
pub unsafe extern "C" fn get_multi(
    gesture_: Gesture,
    terminal: terminal_c::Terminal,
    count: usize,
    keys: *const c_int,
    values: *const *mut c_void,
    out_written: *mut usize,
) -> Result {
    if keys.is_null() || values.is_null() {
        return Result::InvalidValue;
    }

    for i in 0..count {
        let result = get(gesture_, terminal, *keys.add(i), *values.add(i));
        if result != Result::Success {
            if let Some(w) = out_written.as_mut() {
                *w = i;
            }
            return result;
        }
    }
    if let Some(w) = out_written.as_mut() {
        *w = count;
    }
    Result::Success
}

unsafe fn get_typed(gesture_: Gesture, terminal: terminal_c::Terminal, data: Data, out: *mut c_void) -> Result {
    let Some(wrapper) = gesture_.as_ref() else {
        return Result::InvalidValue;
    };
    let Some(t) = terminal_c::zig_terminal(terminal) else {
        return Result::InvalidValue;
    };

    match data {
        Data::ClickCount => *(out as *mut u8) = wrapper.gesture.left_click_count,
        Data::Dragged => *(out as *mut bool) = wrapper.gesture.left_click_dragged,
        Data::Autoscroll => *(out as *mut Autoscroll) = Autoscroll::from_zig(wrapper.gesture.left_drag_autoscroll),
        Data::Behavior => *(out as *mut Behavior) = Behavior::from_zig(wrapper.gesture.left_click_behavior),
        Data::Anchor => {
            let Some(pin) = wrapper.gesture.validated_left_click_pin(&t.screens) else {
                return Result::NoValue;
            };
            *(out as *mut grid_ref::CGridRef) = grid_ref::CGridRef::from_pin(*pin.as_ptr());
        }
    }

    Result::Success
}

unsafe fn event_set_typed(event_: Event, option: EventOption, value: *const c_void) -> Result {
    let Some(event) = event_.as_mut() else {
        return Result::InvalidValue;
    };
    match event.event {
        EventPayload::Press(mut press) => {
            let r = press_set_typed(event, &mut press, option, value);
            event.event = EventPayload::Press(press);
            r
        }
        EventPayload::Release(mut release) => {
            let r = release_set_typed(&mut release, option, value);
            event.event = EventPayload::Release(release);
            r
        }
        EventPayload::Drag(mut drag) => {
            let r = drag_set_typed(event, &mut drag, option, value);
            event.event = EventPayload::Drag(drag);
            r
        }
        EventPayload::AutoscrollTick(mut tick) => {
            let r = autoscroll_tick_set_typed(event, &mut tick, option, value);
            event.event = EventPayload::AutoscrollTick(tick);
            r
        }
        EventPayload::DeepPress => deep_press_set_typed(event, option, value),
    }
}

/// Read a `bool` from C (as a byte, so any nonzero value is true).
unsafe fn read_bool(value: *const c_void) -> bool {
    *(value as *const u8) != 0
}

unsafe fn press_set_typed(event: &mut EventWrapper, press: &mut PressData, option: EventOption, value: *const c_void) -> Result {
    if value.is_null() {
        match option {
            EventOption::Ref => event.event_validation.press_pin_set = false,
            EventOption::Position => {
                press.xpos = 0.0;
                press.ypos = 0.0;
            }
            EventOption::RepeatDistance => press.max_distance = 0.0,
            EventOption::TimeNs => press.time = None,
            EventOption::RepeatIntervalNs => press.repeat_interval = 0,
            EventOption::WordBoundaryCodepoints => clear_word_boundary_codepoints(event),
            EventOption::Behaviors => event.behaviors = gesturepkg::DEFAULT_BEHAVIORS,
            EventOption::Rectangle | EventOption::Geometry | EventOption::Viewport => return Result::InvalidValue,
        }
        return Result::Success;
    }

    match option {
        EventOption::Ref => {
            let v = &*(value as *const grid_ref::CGridRef);
            let Some(pin) = v.to_pin() else {
                return Result::InvalidValue;
            };
            press.pin = Some(pin);
            event.event_validation.press_pin_set = true;
        }
        EventOption::Position => {
            let v = &*(value as *const types::SurfacePosition);
            press.xpos = v.x;
            press.ypos = v.y;
        }
        EventOption::RepeatDistance => press.max_distance = *(value as *const f64),
        EventOption::TimeNs => press.time = Some(instant_from_ns(*(value as *const u64))),
        EventOption::RepeatIntervalNs => press.repeat_interval = *(value as *const u64),
        EventOption::WordBoundaryCodepoints => {
            return try_set_word_boundary_codepoints(event, &*(value as *const types::Codepoints));
        }
        EventOption::Behaviors => {
            let v = &*(value as *const Behaviors);
            let (Some(single), Some(double), Some(triple)) = (
                valid_behavior(v.single_click),
                valid_behavior(v.double_click),
                valid_behavior(v.triple_click),
            ) else {
                return Result::InvalidValue;
            };
            event.behaviors = [single.to_zig(), double.to_zig(), triple.to_zig()];
        }
        EventOption::Rectangle | EventOption::Geometry | EventOption::Viewport => return Result::InvalidValue,
    }

    Result::Success
}

unsafe fn release_set_typed(release: &mut gesturepkg::Release, option: EventOption, value: *const c_void) -> Result {
    match option {
        EventOption::Ref => {
            let Some(v) = (value as *const grid_ref::CGridRef).as_ref() else {
                release.pin = None;
                return Result::Success;
            };
            let Some(pin) = v.to_pin() else {
                return Result::InvalidValue;
            };
            release.pin = Some(pin);
        }

        EventOption::Position
        | EventOption::RepeatDistance
        | EventOption::TimeNs
        | EventOption::RepeatIntervalNs
        | EventOption::WordBoundaryCodepoints
        | EventOption::Behaviors
        | EventOption::Rectangle
        | EventOption::Geometry
        | EventOption::Viewport => return Result::InvalidValue,
    }

    Result::Success
}

unsafe fn drag_set_typed(event: &mut EventWrapper, drag: &mut DragData, option: EventOption, value: *const c_void) -> Result {
    if value.is_null() {
        match option {
            EventOption::Ref => event.event_validation.drag_pin_set = false,
            EventOption::Position => {
                drag.xpos = 0.0;
                drag.ypos = 0.0;
            }
            EventOption::WordBoundaryCodepoints => clear_word_boundary_codepoints(event),
            EventOption::Rectangle => drag.rectangle = false,
            EventOption::Geometry => event.event_validation.drag_geometry_set = false,
            EventOption::Viewport => return Result::InvalidValue,

            EventOption::RepeatDistance | EventOption::TimeNs | EventOption::RepeatIntervalNs | EventOption::Behaviors => {
                return Result::InvalidValue
            }
        }
        return Result::Success;
    }

    match option {
        EventOption::Ref => {
            let v = &*(value as *const grid_ref::CGridRef);
            let Some(pin) = v.to_pin() else {
                return Result::InvalidValue;
            };
            drag.pin = Some(pin);
            event.event_validation.drag_pin_set = true;
        }
        EventOption::Position => {
            let v = &*(value as *const types::SurfacePosition);
            drag.xpos = v.x;
            drag.ypos = v.y;
        }
        EventOption::WordBoundaryCodepoints => {
            return try_set_word_boundary_codepoints(event, &*(value as *const types::Codepoints));
        }
        EventOption::Rectangle => drag.rectangle = read_bool(value),
        EventOption::Geometry => {
            let Some(geometry) = (*(value as *const Geometry)).to_zig() else {
                return Result::InvalidValue;
            };
            drag.geometry = Some(geometry);
            event.event_validation.drag_geometry_set = true;
        }
        EventOption::Viewport => return Result::InvalidValue,

        EventOption::RepeatDistance | EventOption::TimeNs | EventOption::RepeatIntervalNs | EventOption::Behaviors => {
            return Result::InvalidValue
        }
    }

    Result::Success
}

unsafe fn autoscroll_tick_set_typed(
    event: &mut EventWrapper,
    tick: &mut AutoscrollTickData,
    option: EventOption,
    value: *const c_void,
) -> Result {
    if value.is_null() {
        match option {
            EventOption::Viewport => event.event_validation.autoscroll_tick_viewport_set = false,
            EventOption::Position => {
                tick.xpos = 0.0;
                tick.ypos = 0.0;
            }
            EventOption::WordBoundaryCodepoints => clear_word_boundary_codepoints(event),
            EventOption::Rectangle => tick.rectangle = false,
            EventOption::Geometry => event.event_validation.autoscroll_tick_geometry_set = false,

            EventOption::Ref
            | EventOption::RepeatDistance
            | EventOption::TimeNs
            | EventOption::RepeatIntervalNs
            | EventOption::Behaviors => return Result::InvalidValue,
        }
        return Result::Success;
    }

    match option {
        EventOption::Viewport => {
            tick.viewport = Some(*(value as *const point::Coordinate));
            event.event_validation.autoscroll_tick_viewport_set = true;
        }
        EventOption::Position => {
            let v = &*(value as *const types::SurfacePosition);
            tick.xpos = v.x;
            tick.ypos = v.y;
        }
        EventOption::WordBoundaryCodepoints => {
            return try_set_word_boundary_codepoints(event, &*(value as *const types::Codepoints));
        }
        EventOption::Rectangle => tick.rectangle = read_bool(value),
        EventOption::Geometry => {
            let Some(geometry) = (*(value as *const Geometry)).to_zig() else {
                return Result::InvalidValue;
            };
            tick.geometry = Some(geometry);
            event.event_validation.autoscroll_tick_geometry_set = true;
        }

        EventOption::Ref
        | EventOption::RepeatDistance
        | EventOption::TimeNs
        | EventOption::RepeatIntervalNs
        | EventOption::Behaviors => return Result::InvalidValue,
    }

    Result::Success
}

unsafe fn deep_press_set_typed(event: &mut EventWrapper, option: EventOption, value: *const c_void) -> Result {
    if value.is_null() {
        match option {
            EventOption::WordBoundaryCodepoints => clear_word_boundary_codepoints(event),

            EventOption::Ref
            | EventOption::Position
            | EventOption::RepeatDistance
            | EventOption::TimeNs
            | EventOption::RepeatIntervalNs
            | EventOption::Behaviors
            | EventOption::Rectangle
            | EventOption::Geometry
            | EventOption::Viewport => return Result::InvalidValue,
        }
        return Result::Success;
    }

    match option {
        EventOption::WordBoundaryCodepoints => {
            try_set_word_boundary_codepoints(event, &*(value as *const types::Codepoints))
        }

        EventOption::Ref
        | EventOption::Position
        | EventOption::RepeatDistance
        | EventOption::TimeNs
        | EventOption::RepeatIntervalNs
        | EventOption::Behaviors
        | EventOption::Rectangle
        | EventOption::Geometry
        | EventOption::Viewport => Result::InvalidValue,
    }
}

/// The largest `u21` (Zig: `std.math.cast(u21, cp)`).
const MAX_U21: u32 = (1 << 21) - 1;

unsafe fn try_set_word_boundary_codepoints(event: &mut EventWrapper, value: &types::Codepoints) -> Result {
    if value.len > 0 && value.ptr.is_null() {
        return Result::InvalidValue;
    }
    clear_word_boundary_codepoints(event);
    if value.ptr.is_null() {
        event.word_boundary_codepoints = Some(Vec::new());
        return Result::Success;
    }
    let src = std::slice::from_raw_parts(value.ptr, value.len);
    let mut copy = Vec::with_capacity(value.len);
    for &cp in src {
        if cp > MAX_U21 {
            return Result::InvalidValue;
        }
        copy.push(cp);
    }
    event.word_boundary_codepoints = Some(copy);
    Result::Success
}

fn clear_word_boundary_codepoints(event: &mut EventWrapper) {
    event.word_boundary_codepoints = None;
}

/// The base of [`instant_from_ns`]: an arbitrary fixed instant.
static INSTANT_BASE: LazyLock<Timestamp> = LazyLock::new(Timestamp::now);

/// `std.Io.Timestamp.fromNanoseconds` (see the module notes).
fn instant_from_ns(ns: u64) -> Timestamp {
    let base = *INSTANT_BASE;
    base.checked_add(std::time::Duration::from_nanos(ns)).unwrap_or(base)
}

fn valid_behavior(behavior: c_int) -> Option<Behavior> {
    Behavior::from_int(behavior)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lib::allocator::C_ALLOCATOR;
    use crate::terminal::point::{Coordinate, Point};

    fn vp<T>(v: &mut T) -> *mut c_void {
        v as *mut T as *mut c_void
    }

    fn cp<T>(v: &T) -> *const c_void {
        v as *const T as *const c_void
    }

    fn new_terminal() -> terminal_c::Terminal {
        let mut t: terminal_c::Terminal = std::ptr::null_mut();
        assert_eq!(Result::Success, terminal_c::new(Some(&C_ALLOCATOR), &mut t, 5, 2));
        t
    }

    fn write(t: terminal_c::Terminal, s: &[u8]) {
        unsafe { terminal_c::vt_write(t, s.as_ptr(), s.len()) };
    }

    fn new_gesture() -> Gesture {
        let mut g: Gesture = std::ptr::null_mut();
        assert_eq!(Result::Success, unsafe { new(Some(&C_ALLOCATOR), &mut g) });
        g
    }

    fn new_event(event_type: EventType) -> Event {
        let mut e: Event = std::ptr::null_mut();
        assert_eq!(Result::Success, unsafe { event_new(Some(&C_ALLOCATOR), &mut e, event_type as c_int) });
        e
    }

    unsafe fn set_<T>(event: Event, option: EventOption, value: &T) -> Result {
        event_set(event, option as c_int, cp(value))
    }

    unsafe fn clear_(event: Event, option: EventOption) -> Result {
        event_set(event, option as c_int, std::ptr::null())
    }

    unsafe fn get_<T>(gesture: Gesture, terminal: terminal_c::Terminal, data: Data, out: &mut T) -> Result {
        get(gesture, terminal, data as c_int, vp(out))
    }

    fn grid_ref_at(t: terminal_c::Terminal, x: u16, y: u32) -> grid_ref::CGridRef {
        let mut r = grid_ref::CGridRef::default();
        assert_eq!(Result::Success, unsafe {
            terminal_c::grid_ref(t, Point::Active(Coordinate { x, y }).cval(), &mut r)
        });
        r
    }

    fn press_data(event: Event) -> PressData {
        match unsafe { (*event).event } {
            EventPayload::Press(p) => p,
            _ => panic!("not a press event"),
        }
    }

    fn empty_sel() -> selection_c::CSelection {
        selection_c::CSelection::new(Default::default(), Default::default())
    }

    #[test]
    fn selection_gesture_lifecycle_and_get() {
        let terminal = new_terminal();
        let gesture = new_gesture();
        unsafe {
            let mut click_count: u8 = 255;
            assert_eq!(Result::Success, get_(gesture, terminal, Data::ClickCount, &mut click_count));
            assert_eq!(0, click_count);

            let mut dragged = true;
            assert_eq!(Result::Success, get_(gesture, terminal, Data::Dragged, &mut dragged));
            assert!(!dragged);

            let mut autoscroll = Autoscroll::Up;
            assert_eq!(Result::Success, get_(gesture, terminal, Data::Autoscroll, &mut autoscroll));
            assert_eq!(Autoscroll::None, autoscroll);

            let mut behavior = Behavior::Word;
            assert_eq!(Result::Success, get_(gesture, terminal, Data::Behavior, &mut behavior));
            assert_eq!(Behavior::Cell, behavior);

            let mut anchor = grid_ref::CGridRef::default();
            assert_eq!(Result::NoValue, get_(gesture, terminal, Data::Anchor, &mut anchor));
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_get_multi() {
        let terminal = new_terminal();
        let gesture = new_gesture();
        unsafe {
            let keys = [Data::ClickCount, Data::Dragged, Data::Autoscroll, Data::Behavior].map(|k| k as c_int);
            let mut click_count: u8 = 255;
            let mut dragged = true;
            let mut autoscroll = Autoscroll::Up;
            let mut behavior = Behavior::Word;
            let values = [vp(&mut click_count), vp(&mut dragged), vp(&mut autoscroll), vp(&mut behavior)];
            let mut written: usize = 0;

            assert_eq!(
                Result::Success,
                get_multi(gesture, terminal, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(keys.len(), written);
            assert_eq!(0, click_count);
            assert!(!dragged);
            assert_eq!(Autoscroll::None, autoscroll);
            assert_eq!(Behavior::Cell, behavior);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_get_multi_returns_first_failing_index() {
        let terminal = new_terminal();
        let gesture = new_gesture();
        unsafe {
            let keys = [Data::ClickCount, Data::Anchor, Data::Dragged].map(|k| k as c_int);
            let mut click_count: u8 = 255;
            let mut anchor = grid_ref::CGridRef::default();
            let mut dragged = true;
            let values = [vp(&mut click_count), vp(&mut anchor), vp(&mut dragged)];
            let mut written: usize = 0;

            assert_eq!(
                Result::NoValue,
                get_multi(gesture, terminal, keys.len(), keys.as_ptr(), values.as_ptr(), &mut written)
            );
            assert_eq!(1, written);
            assert_eq!(0, click_count);
            assert!(dragged);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_event_set_clear_and_free() {
        let event = new_event(EventType::Press);
        unsafe {
            let in_pos = types::SurfacePosition { x: 12.5, y: -3.25 };
            assert_eq!(Result::Success, set_(event, EventOption::Position, &in_pos));
            assert_eq!(12.5, press_data(event).xpos);
            assert_eq!(-3.25, press_data(event).ypos);

            assert_eq!(Result::Success, clear_(event, EventOption::Position));
            assert_eq!(0.0, press_data(event).xpos);
            assert_eq!(0.0, press_data(event).ypos);

            let repeat_distance: f64 = 4.0;
            assert_eq!(Result::Success, set_(event, EventOption::RepeatDistance, &repeat_distance));
            assert_eq!(repeat_distance, press_data(event).max_distance);
            event_free(event);
        }
    }

    #[test]
    fn selection_gesture_event_copies_clears_and_frees_codepoints() {
        let event = new_event(EventType::Press);
        unsafe {
            let mut values = [' ' as u32, '\t' as u32];
            let input = types::Codepoints {
                ptr: values.as_ptr(),
                len: values.len(),
            };
            assert_eq!(Result::Success, set_(event, EventOption::WordBoundaryCodepoints, &input));

            values[0] = 'x' as u32;
            let _ = values;

            let cps = (*event).word_boundary_codepoints();
            assert_eq!(2, cps.len());
            assert_eq!(' ' as u32, cps[0]);
            assert_eq!('\t' as u32, cps[1]);

            let invalid = types::Codepoints {
                ptr: std::ptr::null(),
                len: 1,
            };
            assert_eq!(Result::InvalidValue, set_(event, EventOption::WordBoundaryCodepoints, &invalid));

            assert_eq!(Result::Success, clear_(event, EventOption::WordBoundaryCodepoints));
            assert_eq!(
                selection_codepoints::DEFAULT_WORD_BOUNDARIES.len(),
                (*event).word_boundary_codepoints().len()
            );

            let empty = types::Codepoints {
                ptr: std::ptr::null(),
                len: 0,
            };
            assert_eq!(Result::Success, set_(event, EventOption::WordBoundaryCodepoints, &empty));
            assert_eq!(0, (*event).word_boundary_codepoints().len());
            event_free(event);
        }
    }

    #[test]
    fn selection_gesture_event_behaviors() {
        let event = new_event(EventType::Press);
        unsafe {
            let input = Behaviors::new(Behavior::Cell, Behavior::Word, Behavior::Line);
            assert_eq!(Result::Success, set_(event, EventOption::Behaviors, &input));
            assert_eq!(gesturepkg::Behavior::Cell, (*event).behaviors[0]);
            assert_eq!(gesturepkg::Behavior::Word, (*event).behaviors[1]);
            assert_eq!(gesturepkg::Behavior::Line, (*event).behaviors[2]);
            event_free(event);
        }
    }

    #[test]
    fn selection_gesture_event_applies_press() {
        let terminal = new_terminal();
        let gesture = new_gesture();
        let press_event = new_event(EventType::Press);
        unsafe {
            write(terminal, b"abc");

            let r = grid_ref_at(terminal, 1, 0);
            assert_eq!(Result::Success, set_(press_event, EventOption::Ref, &r));
            let behaviors = Behaviors::new(Behavior::Word, Behavior::Word, Behavior::Line);
            assert_eq!(Result::Success, set_(press_event, EventOption::Behaviors, &behaviors));

            let mut sel = empty_sel();
            assert_eq!(Result::Success, handle_event(gesture, terminal, press_event, &mut sel));
            assert_eq!(0, sel.start.to_pin().unwrap().x);
            assert_eq!(2, sel.end.to_pin().unwrap().x);

            assert_eq!(Result::Success, handle_event(gesture, terminal, press_event, std::ptr::null_mut()));
            event_free(press_event);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_event_press_requires_ref() {
        let terminal = new_terminal();
        let gesture = new_gesture();
        let press_event = new_event(EventType::Press);
        unsafe {
            let mut sel = empty_sel();
            assert_eq!(Result::InvalidValue, handle_event(gesture, terminal, press_event, &mut sel));
            event_free(press_event);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_event_null_output_still_reports_no_selection() {
        let terminal = new_terminal();
        let gesture = new_gesture();
        let press_event = new_event(EventType::Press);
        unsafe {
            let r = grid_ref_at(terminal, 1, 0);
            assert_eq!(Result::Success, set_(press_event, EventOption::Ref, &r));

            assert_eq!(Result::NoValue, handle_event(gesture, terminal, press_event, std::ptr::null_mut()));
            event_free(press_event);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_event_applies_release() {
        let terminal = new_terminal();
        let gesture = new_gesture();
        let press_event = new_event(EventType::Press);
        let release_event = new_event(EventType::Release);
        unsafe {
            let r = grid_ref_at(terminal, 1, 0);
            assert_eq!(Result::Success, set_(press_event, EventOption::Ref, &r));
            assert_eq!(Result::Success, set_(release_event, EventOption::Ref, &r));

            assert_eq!(Result::NoValue, handle_event(gesture, terminal, press_event, std::ptr::null_mut()));
            assert_eq!(Result::NoValue, handle_event(gesture, terminal, release_event, std::ptr::null_mut()));

            let mut dragged = true;
            assert_eq!(Result::Success, get_(gesture, terminal, Data::Dragged, &mut dragged));
            assert!(!dragged);

            let pos = types::SurfacePosition { x: 0.0, y: 0.0 };
            assert_eq!(Result::InvalidValue, set_(release_event, EventOption::Position, &pos));
            event_free(release_event);
            event_free(press_event);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_release_without_ref_marks_dragged() {
        let terminal = new_terminal();
        let gesture = new_gesture();
        let press_event = new_event(EventType::Press);
        let release_event = new_event(EventType::Release);
        unsafe {
            let r = grid_ref_at(terminal, 1, 0);
            assert_eq!(Result::Success, set_(press_event, EventOption::Ref, &r));

            assert_eq!(Result::NoValue, handle_event(gesture, terminal, press_event, std::ptr::null_mut()));
            assert_eq!(Result::NoValue, handle_event(gesture, terminal, release_event, std::ptr::null_mut()));

            let mut dragged = false;
            assert_eq!(Result::Success, get_(gesture, terminal, Data::Dragged, &mut dragged));
            assert!(dragged);
            event_free(release_event);
            event_free(press_event);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_event_applies_drag() {
        let terminal = new_terminal();
        write(terminal, b"abcde");
        let gesture = new_gesture();
        let press_event = new_event(EventType::Press);
        let drag_event = new_event(EventType::Drag);
        unsafe {
            let press_ref = grid_ref_at(terminal, 1, 0);
            assert_eq!(Result::Success, set_(press_event, EventOption::Ref, &press_ref));

            let press_pos = types::SurfacePosition { x: 10.0, y: 10.0 };
            assert_eq!(Result::Success, set_(press_event, EventOption::Position, &press_pos));
            assert_eq!(Result::NoValue, handle_event(gesture, terminal, press_event, std::ptr::null_mut()));

            let drag_ref = grid_ref_at(terminal, 3, 0);
            assert_eq!(Result::Success, set_(drag_event, EventOption::Ref, &drag_ref));

            let drag_pos = types::SurfacePosition { x: 36.0, y: 10.0 };
            assert_eq!(Result::Success, set_(drag_event, EventOption::Position, &drag_pos));
            let geometry = Geometry {
                columns: 5,
                cell_width: 10,
                padding_left: 0,
                screen_height: 20,
            };
            assert_eq!(Result::Success, set_(drag_event, EventOption::Geometry, &geometry));

            let mut sel = empty_sel();
            assert_eq!(Result::Success, handle_event(gesture, terminal, drag_event, &mut sel));
            assert_eq!(1, sel.start.to_pin().unwrap().x);
            assert_eq!(3, sel.end.to_pin().unwrap().x);

            let mut dragged = false;
            assert_eq!(Result::Success, get_(gesture, terminal, Data::Dragged, &mut dragged));
            assert!(dragged);
            event_free(drag_event);
            event_free(press_event);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_drag_requires_ref_and_geometry() {
        let terminal = new_terminal();
        let gesture = new_gesture();
        let drag_event = new_event(EventType::Drag);
        unsafe {
            let mut sel = empty_sel();
            assert_eq!(Result::InvalidValue, handle_event(gesture, terminal, drag_event, &mut sel));

            let r = grid_ref_at(terminal, 1, 0);
            assert_eq!(Result::Success, set_(drag_event, EventOption::Ref, &r));
            assert_eq!(Result::InvalidValue, handle_event(gesture, terminal, drag_event, &mut sel));

            let invalid_geometry = Geometry {
                columns: 5,
                cell_width: 0,
                padding_left: 0,
                screen_height: 20,
            };
            assert_eq!(Result::InvalidValue, set_(drag_event, EventOption::Geometry, &invalid_geometry));
            event_free(drag_event);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_event_applies_autoscroll_tick() {
        let terminal = new_terminal();
        write(terminal, b"abcde\r\nfghij");
        let gesture = new_gesture();
        let press_event = new_event(EventType::Press);
        let drag_event = new_event(EventType::Drag);
        let tick_event = new_event(EventType::AutoscrollTick);
        unsafe {
            let geometry = Geometry {
                columns: 5,
                cell_width: 10,
                padding_left: 0,
                screen_height: 20,
            };

            let press_ref = grid_ref_at(terminal, 1, 0);
            assert_eq!(Result::Success, set_(press_event, EventOption::Ref, &press_ref));
            let press_pos = types::SurfacePosition { x: 10.0, y: 10.0 };
            assert_eq!(Result::Success, set_(press_event, EventOption::Position, &press_pos));
            assert_eq!(Result::NoValue, handle_event(gesture, terminal, press_event, std::ptr::null_mut()));

            let drag_ref = grid_ref_at(terminal, 3, 1);
            assert_eq!(Result::Success, set_(drag_event, EventOption::Ref, &drag_ref));
            let drag_pos = types::SurfacePosition { x: 36.0, y: 20.0 };
            assert_eq!(Result::Success, set_(drag_event, EventOption::Position, &drag_pos));
            assert_eq!(Result::Success, set_(drag_event, EventOption::Geometry, &geometry));
            let mut sel = empty_sel();
            assert_eq!(Result::Success, handle_event(gesture, terminal, drag_event, &mut sel));

            let mut autoscroll = Autoscroll::None;
            assert_eq!(Result::Success, get_(gesture, terminal, Data::Autoscroll, &mut autoscroll));
            assert_eq!(Autoscroll::Down, autoscroll);

            let viewport = point::Coordinate { x: 3, y: 1 };
            assert_eq!(Result::Success, set_(tick_event, EventOption::Viewport, &viewport));
            assert_eq!(Result::Success, set_(tick_event, EventOption::Position, &drag_pos));
            assert_eq!(Result::Success, set_(tick_event, EventOption::Geometry, &geometry));

            assert_eq!(Result::Success, handle_event(gesture, terminal, tick_event, &mut sel));
            event_free(tick_event);
            event_free(drag_event);
            event_free(press_event);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_autoscroll_tick_requires_viewport_and_geometry() {
        let terminal = new_terminal();
        let gesture = new_gesture();
        let tick_event = new_event(EventType::AutoscrollTick);
        unsafe {
            let mut sel = empty_sel();
            assert_eq!(Result::InvalidValue, handle_event(gesture, terminal, tick_event, &mut sel));

            let viewport = point::Coordinate { x: 1, y: 0 };
            assert_eq!(Result::Success, set_(tick_event, EventOption::Viewport, &viewport));
            assert_eq!(Result::InvalidValue, handle_event(gesture, terminal, tick_event, &mut sel));

            let r = grid_ref_at(terminal, 1, 0);
            assert_eq!(Result::InvalidValue, set_(tick_event, EventOption::Ref, &r));
            event_free(tick_event);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_event_applies_deep_press() {
        let terminal = new_terminal();
        write(terminal, b"abcde");
        let gesture = new_gesture();
        let press_event = new_event(EventType::Press);
        let deep_press_event = new_event(EventType::DeepPress);
        unsafe {
            let r = grid_ref_at(terminal, 2, 0);
            assert_eq!(Result::Success, set_(press_event, EventOption::Ref, &r));
            assert_eq!(Result::NoValue, handle_event(gesture, terminal, press_event, std::ptr::null_mut()));

            let mut sel = empty_sel();
            assert_eq!(Result::Success, handle_event(gesture, terminal, deep_press_event, &mut sel));
            assert_eq!(0, sel.start.to_pin().unwrap().x);
            assert_eq!(4, sel.end.to_pin().unwrap().x);

            let mut dragged = false;
            assert_eq!(Result::Success, get_(gesture, terminal, Data::Dragged, &mut dragged));
            assert!(dragged);

            let pos = types::SurfacePosition { x: 0.0, y: 0.0 };
            assert_eq!(Result::InvalidValue, set_(deep_press_event, EventOption::Position, &pos));
            event_free(deep_press_event);
            event_free(press_event);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_deep_press_without_active_anchor_returns_no_value() {
        let terminal = new_terminal();
        let gesture = new_gesture();
        let deep_press_event = new_event(EventType::DeepPress);
        unsafe {
            let mut sel = empty_sel();
            assert_eq!(Result::NoValue, handle_event(gesture, terminal, deep_press_event, &mut sel));
            event_free(deep_press_event);
            free(gesture, terminal);
            terminal_c::free(terminal);
        }
    }

    #[test]
    fn selection_gesture_free_null() {
        unsafe { free(std::ptr::null_mut(), std::ptr::null_mut()) };
    }

    #[test]
    fn selection_gesture_event_free_null() {
        unsafe { event_free(std::ptr::null_mut()) };
    }
}
