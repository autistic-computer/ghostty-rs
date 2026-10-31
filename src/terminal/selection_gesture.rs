//! Port of `src/terminal/SelectionGesture.zig`.
//!
//! STATUS: ported
//!
//! SelectionGesture manages gesture-based terminal text selection for one
//! pointer stream: press, drag, release, autoscroll, and pressure/deep-press
//! selection.
//!
//! This type owns only the state required to interpret a gesture. It does not
//! modify the terminal selection directly, except for scrolling the viewport
//! during `autoscroll_tick`. The caller feeds platform events into this type and
//! applies the returned `Selection` to the active screen when appropriate.
//!
//! A typical single-click drag flow looks like this:
//!
//! ```ignore
//! let selection = gesture.press(terminal, Press { ... })?;
//! terminal.screens.active.select(selection)?;
//! if let Some(selection) = gesture.drag(terminal, Drag { ... }) {
//!     terminal.screens.active.select(Some(selection))?;
//! }
//! gesture.release(terminal, Release { ... });
//! ```
//!
//! Double- and triple-click gestures use the same event flow. Repeated presses
//! inside `Press.repeat_interval` and within `Press.max_distance` increment the
//! internal click count up to three. `Press.behaviors` maps single-, double-,
//! and triple-clicks to behavior. By default, a single press returns null to
//! clear any existing selection, a double-click returns a word selection, and a
//! triple-click returns a line selection. Drags use the behavior selected by the
//! corresponding press. A new press that is too late, too far away, or on
//! another active screen starts a new single-click gesture.
//!
//! # Resetting and lifetime
//!
//! `release` ends the active drag/autoscroll phase but intentionally preserves
//! enough state for a subsequent press to become a double- or triple-click.
//! Call `reset` when the gesture is cancelled rather than released normally, or
//! when another subsystem takes ownership of pointer input. Examples include
//! enabling mouse reporting for an application, losing pointer/button state,
//! destroying the surface, switching to a mode that must not continue text
//! selection, or otherwise abandoning the current click sequence. Call `deinit`
//! once before discarding the gesture object so any tracked click pin is
//! released.
//!
//! # Terminal and screen changes
//!
//! The initial press pin is tracked in the active screen's `PageList`, so normal
//! terminal output and viewport scrolling can move rows without making the
//! gesture immediately stale. Selection results are computed against the current
//! terminal contents at the time of each call. For example, a double-click drag
//! selects word boundaries from the screen as it exists during `drag`, not from a
//! snapshot captured at `press`.
//!
//! The tracked pin is tied to both a `ScreenSet.Key` and that screen's
//! generation. If the active screen changes, or a screen is removed/recycled,
//! `validated_left_click_pin` returns null and drag-style operations stop producing
//! selections. `autoscroll_tick` treats this as cancellation and calls `reset` so
//! callers can stop their timers. This avoids exposing pins from inactive or
//! freed screens, but it does not make a historical snapshot of terminal data.
//!
//! # Concurrency
//!
//! SelectionGesture is not concurrency safe. It has mutable gesture state and
//! mutates/tracks pins inside the terminal page list without taking locks. The
//! caller must serialize all calls that touch the same gesture and terminal,
//! typically by holding the same terminal/renderer mutex used for other screen
//! mutations. Do not call `press`, `drag`, `release`, `reset`, `deinit`, or
//! `autoscroll_tick` concurrently with each other or with unrelated terminal
//! mutations unless the caller provides that synchronization.
//!
//! Notes on the port:
//! - `std.Io.Timestamp` (on the `.awake` clock) is [`Timestamp`], an alias
//!   of `std::time::Instant`.
//! - Nested Zig types are flattened: `Drag.Geometry` is [`DragGeometry`].
//! - `deinit` stays an explicit method because it needs the terminal.

use std::cmp::Ordering;

use crate::terminal::page_list::{Pin, PinPtr};
use crate::terminal::point;
use crate::terminal::screen::{Screen, SelectLine};
use crate::terminal::screen_set::{self, ScreenSet};
use crate::terminal::selection::Selection;
use crate::terminal::size::AllocError;
use crate::terminal::terminal::{ScrollViewport, Terminal};

/// A point in time on a monotonic clock (Zig: `std.Io.Timestamp`).
pub type Timestamp = std::time::Instant;

pub struct SelectionGesture {
    /// The tracked pin of the initial left click along with the screen
    /// that the pin is part of.
    pub left_click_pin: Option<PinPtr>,
    pub left_click_screen: screen_set::Key,
    pub left_click_screen_generation: usize,

    /// The count of clicks to count double and triple clicks and so on.
    /// The left click time was the last time the left click was done, if the
    /// caller could provide one. If this is null then we only support single clicks.
    /// (Zig: `u3`.)
    pub left_click_count: u8,
    pub left_click_time: Option<Timestamp>,

    /// The selection behavior chosen for the active left-click gesture.
    pub left_click_behavior: Behavior,

    /// The starting xpos/ypos of the left click. Note that if scrolling occurs,
    /// these will point to different cells, but the xpos/ypos will stay
    /// stable during scrolling relative to the surface.
    pub left_click_xpos: f64,
    pub left_click_ypos: f64,

    /// True once the active left-click gesture has moved away from the initially
    /// pressed cell. This is reset on every press that starts or continues a
    /// multi-click sequence, and is left available for callers to inspect while
    /// handling the corresponding release.
    pub left_click_dragged: bool,

    /// The current autoscroll state for the active left-click drag gesture.
    pub left_drag_autoscroll: Autoscroll,
}

/// The direction that selection dragging should autoscroll the viewport.
/// This is derived from the most recent drag position relative to the
/// surface bounds and reset whenever there is no active drag gesture.
///
/// When autoscroll is non-none, the caller should setup a timer
/// to periodically call autoscrollTick. The timer interval is up to the
/// caller but reasonable defaults are approximately every 15 milliseconds.
///
/// This is used to implement selection above/below the viewport that
/// wants to drag the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum Autoscroll {
    None,
    Up,
    Down,
}

/// The selection behavior for a click and subsequent drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum Behavior {
    /// Cell-granular drag selection. Press returns null to clear selection.
    Cell,

    /// Word selection on press and word-granular drag selection.
    Word,

    /// Line selection on press and line-granular drag selection.
    Line,

    /// Semantic command output selection on press and drag.
    Output,
}

/// Standard terminal selection behavior for single-, double-, and triple-clicks.
///
/// A single click uses cell behavior, which returns null on press so callers can
/// clear any existing selection and then drag by cell. A double-click selects and
/// drags by word. A triple-click selects and drags by line.
pub const DEFAULT_BEHAVIORS: [Behavior; 3] = [Behavior::Cell, Behavior::Word, Behavior::Line];

/// Distance from the top or bottom surface edge, in pixels, where dragging
/// should request autoscroll. This preserves the historical 1px buffer used
/// so fullscreen-edge drags can still trigger autoscroll.
const AUTOSCROLL_BUFFER: f64 = 1.0;

/// (Zig: `Press`.)
#[derive(Debug, Clone, Copy)]
pub struct Press<'a> {
    /// The time when the press event occurred. Prefer a monotonic timer;
    /// backwards timestamps reset the repeat sequence.
    /// This can be null if you're on a system that doesn't support
    /// time for some reason. In that case, we only support single clicks.
    pub time: Option<Timestamp>,

    /// The cell where the click was.
    ///
    /// `press` stores a tracked copy of this pin. The caller does not need to
    /// keep `p.pin` alive after the call returns, but the pin must belong to the
    /// terminal's active screen when passed in.
    pub pin: Pin,

    /// The x/y value of the click relative to the surface with (0,0) being
    /// top-left. This is used for distance detection for multi-clicks so
    /// double/triple clicks too far away from each other will reset the click
    /// count as well more accurate drag behaviors.
    pub xpos: f64,
    pub ypos: f64,

    /// Maximum distance a click can be from the original click to register
    /// as a repeat. If uncertain, set this to cell width.
    pub max_distance: f64,

    /// The maximum interval in nanoseconds that a press is considered
    /// a repeat e.g. to record double/triple clicks.
    pub repeat_interval: u64,

    /// The codepoints that delimit words for double-click selection.
    pub word_boundary_codepoints: &'a [u32],

    /// Selection behaviors for single-, double-, and triple-clicks.
    /// (Zig default: `&default_behaviors`.)
    pub behaviors: &'a [Behavior; 3],
}

/// (Zig: `Drag`.)
#[derive(Debug, Clone, Copy)]
pub struct Drag<'a> {
    /// The cell where the current drag position is. This is used
    /// synchronously to calculate the selection and is not tracked.
    pub pin: Pin,

    /// The x/y value of the drag relative to the surface with (0,0) being
    /// top-left.
    pub xpos: f64,
    pub ypos: f64,

    /// True if the current drag should produce a rectangular selection.
    pub rectangle: bool,

    /// The codepoints that delimit words for double-click drag selection.
    pub word_boundary_codepoints: &'a [u32],

    /// Geometry required for selection threshold and autoscroll calculations.
    pub geometry: DragGeometry,
}

/// Display geometry needed to translate surface-relative pointer positions
/// into selection behavior. (Zig: `Drag.Geometry`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragGeometry {
    /// The number of columns in the rendered terminal grid.
    pub columns: u32,

    /// The width of one terminal cell in surface pixels.
    pub cell_width: u32,

    /// The left padding before the terminal grid begins, in surface pixels.
    pub padding_left: u32,

    /// The height of the rendered terminal surface in surface pixels.
    pub screen_height: u32,
}

/// (Zig: `AutoscrollTick`.)
#[derive(Debug, Clone, Copy)]
pub struct AutoscrollTick<'a> {
    /// The viewport cell where the current drag position is. This is resolved
    /// after the viewport is scrolled so the selection tracks the newly visible
    /// row under the pointer.
    pub viewport: point::Coordinate,

    /// The x/y value of the drag relative to the surface with (0,0) being
    /// top-left.
    pub xpos: f64,
    pub ypos: f64,

    /// True if the current drag should produce a rectangular selection.
    pub rectangle: bool,

    /// The codepoints that delimit words for double-click drag selection.
    pub word_boundary_codepoints: &'a [u32],

    /// Geometry required for selection threshold and autoscroll calculations.
    pub geometry: DragGeometry,
}

/// A pressure-based activation during an existing left-click gesture.
///
/// This is the terminal gesture model for platform features such as macOS
/// force click / deep click on pressure-sensitive trackpads. It is not a
/// distinct mouse button and it is not part of the normal single/double/triple
/// click count sequence; it can only occur after a left press is already
/// active.
#[derive(Debug, Clone, Copy)]
pub struct DeepPress<'a> {
    /// The codepoints that delimit words for the word selection produced by
    /// the deep press.
    pub word_boundary_codepoints: &'a [u32],
}

/// (Zig: `Release`.)
#[derive(Debug, Clone, Copy)]
pub struct Release {
    /// The cell where the release occurred, if the release position mapped to
    /// a valid cell. This is used synchronously to update gesture state and is
    /// not tracked.
    pub pin: Option<Pin>,
}

/// (Zig: `error{PressRequiresReset}`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PressRepeatError {
    PressRequiresReset,
}

impl Default for SelectionGesture {
    fn default() -> Self {
        SelectionGesture::INIT
    }
}

impl SelectionGesture {
    pub const INIT: SelectionGesture = SelectionGesture {
        left_click_pin: None,
        left_click_count: 0,
        left_click_time: None,
        left_click_behavior: Behavior::Cell,
        left_click_screen: screen_set::Key::Primary,
        left_click_screen_generation: 0,
        left_click_xpos: 0.0,
        left_click_ypos: 0.0,
        left_click_dragged: false,
        left_drag_autoscroll: Autoscroll::None,
    };

    pub fn deinit(&mut self, t: &mut Terminal) {
        // Grab our pagelist that is associated with the pin. If it doesn't
        // exist anymore then our tracked pin is already free.
        let Some(pin) = self.left_click_pin else { return };
        if t.screens.generation(self.left_click_screen) != self.left_click_screen_generation {
            return;
        }
        let Some(mut screen) = t.screens.get(self.left_click_screen) else { return };
        screen.pages.untrack_pin(pin);
    }

    /// Reset any active gesture state and untrack the tracked click pin.
    ///
    /// Use this for cancellation/abandonment, not for the ordinary left-button
    /// release path. `release` deliberately keeps the last press time/count so a
    /// following press can become a double- or triple-click; `reset` clears that
    /// sequence and makes the next press a fresh single click.
    ///
    /// Examples of reset-worthy events are: mouse reporting taking over, pointer
    /// capture being lost, a surface/window being torn down, or another interaction
    /// mode deciding that text selection must stop immediately. If the active screen
    /// was already removed or recycled, this safely drops the stale reference without
    /// trying to untrack a pin from the wrong screen generation.
    pub fn reset(&mut self, t: &mut Terminal) {
        self.left_click_count = 0;
        self.left_click_time = None;
        self.left_click_behavior = Behavior::Cell;
        self.left_click_dragged = false;
        self.left_drag_autoscroll = Autoscroll::None;
        self.untrack_pin(t);
    }

    /// Return the tracked left-click pin only if it still belongs to the current
    /// active screen instance.
    ///
    /// This validates both the screen key and generation so a pin from a removed,
    /// recycled, or inactive screen is never exposed to callers. A null result means
    /// callers should treat the in-progress gesture as temporarily or permanently
    /// unable to produce a selection. For a normal drag this usually means "do
    /// nothing for this event"; for autoscroll it is treated as cancellation because
    /// a timer should not continue firing for a gesture that no longer has a valid
    /// anchor.
    pub fn validated_left_click_pin(&self, screens: &ScreenSet) -> Option<PinPtr> {
        let pin = self.left_click_pin?;
        if self.left_click_screen != screens.active_key {
            return None;
        }
        if screens.generation(self.left_click_screen) != self.left_click_screen_generation {
            return None;
        }
        let _ = screens.get(self.left_click_screen)?;
        Some(pin)
    }

    /// Record a press event and return the standard selection for this click.
    ///
    /// If this press continues the existing click sequence, the click count is
    /// incremented up to three and the original anchor pin is kept. Otherwise, the
    /// previous gesture state is cleared and this press becomes the new anchor.
    /// The returned selection is untracked and represents the standard terminal
    /// click behavior for the resulting click count. The caller is responsible for
    /// applying it to the screen, usually with `Screen.select`, and for arranging
    /// any copy-on-select behavior.
    ///
    /// Examples:
    ///
    /// * first press: `left_click_count == 1`, defaults to cell behavior;
    /// * second nearby press within the repeat interval: `left_click_count == 2`,
    ///   defaults to word behavior;
    /// * third nearby press within the repeat interval: `left_click_count == 3`,
    ///   defaults to line behavior;
    /// * press after the interval, too far away, or after a screen generation
    ///   change: starts over at `left_click_count == 1` and returns null.
    pub fn press(&mut self, t: &mut Terminal, p: Press<'_>) -> Result<Option<Selection>, AllocError> {
        if self.left_click_count > 0 {
            match self.press_repeat(t, &p) {
                // Successful repeat.
                Ok(()) => return Ok(self.press_selection(&t.screens.active, &p)),
                Err(PressRepeatError::PressRequiresReset) => {}
            }
        }

        // Initial click or the repeat failed for some reason such as
        // the subsequent click being too far away.
        self.press_initial(t, &p)?;
        Ok(self.press_selection(&t.screens.active, &p))
    }

    /// Record a drag event and return the current untracked drag selection.
    ///
    /// The returned selection is untracked and represents the best selection for the
    /// terminal contents at the time of this call. The caller is responsible for
    /// applying it to the screen, usually with `Screen.select`, and for arranging any
    /// copy-on-select behavior. A null result means either there is no active
    /// selection gesture, the original press is no longer valid for the active
    /// screen, or the drag has not crossed the threshold required to select a cell.
    ///
    /// This method also updates `left_click_dragged` and `left_drag_autoscroll`.
    /// If `left_drag_autoscroll` becomes `.up` or `.down`, the caller should start or
    /// keep a timer that calls `autoscrollTick` while the button remains pressed. If
    /// it becomes `.none`, the caller should stop that timer.
    ///
    /// Normal terminal output and viewport movement between drag events are allowed:
    /// the tracked press pin follows the page list, and the drag pin is used only
    /// synchronously. Content-sensitive selections such as word and line selection
    /// are recalculated from the current active screen every time.
    pub fn drag(&mut self, t: &mut Terminal, d: Drag<'_>) -> Option<Selection> {
        // If we aren't currently clicked then we don't do any dragging
        // behavior.
        if self.left_click_count == 0 {
            debug_assert!(self.left_drag_autoscroll == Autoscroll::None);
            return None;
        }

        // Get our click pin. We get a validated pin because if our
        // screen changed out from under us then we aren't actually
        // clicking anymore.
        let click_pin_ptr = self.validated_left_click_pin(&t.screens)?;
        // SAFETY: the pin is tracked by the active screen (validated above).
        let click_pin: Pin = unsafe { *click_pin_ptr.as_ptr() };
        if !d.pin.eql(click_pin) {
            self.left_click_dragged = true;
        }

        // Determine if we should autoscroll. If our drag position is above
        // the top, we go up. If its below the bottom we go down. Easy.
        let max_y: f64 = d.geometry.screen_height as f64;
        self.left_drag_autoscroll = if d.ypos <= AUTOSCROLL_BUFFER {
            Autoscroll::Up
        } else if d.ypos > max_y - AUTOSCROLL_BUFFER {
            Autoscroll::Down
        } else {
            Autoscroll::None
        };

        let screen: &Screen = &t.screens.active;
        let selection = match self.left_click_behavior {
            Behavior::Cell => drag_selection(
                click_pin,
                d.pin,
                pixel_from_float(self.left_click_xpos),
                pixel_from_float(d.xpos),
                d.rectangle,
                d.geometry,
            ),

            Behavior::Word => drag_selection_word(screen, click_pin, d.pin, d.word_boundary_codepoints),

            Behavior::Line => drag_selection_line(screen, click_pin, d.pin),

            Behavior::Output => drag_selection_output(screen, click_pin, d.pin),
        };

        // Same-cell cell selections can still become real selections when the drag
        // crosses the within-cell threshold. Treat those as drags so callers don't
        // also process click-only actions such as opening links.
        if self.left_click_behavior == Behavior::Cell && selection.is_some() {
            self.left_click_dragged = true;
        }

        selection
    }

    /// Record a selection autoscroll tick for the active left-click drag gesture.
    ///
    /// This scrolls the viewport in the active autoscroll direction and then
    /// continues the drag at the provided viewport position. The viewport position
    /// is resolved to a pin after scrolling so the drag applies to the row now under
    /// the pointer.
    ///
    /// This always scrolls the viewport by exactly one row in the current
    /// autoscroll direction. If you want to scroll by more, increase your
    /// tick rate.
    ///
    /// If the original press pin no longer belongs to the active screen, this calls
    /// `reset` and returns null. That is a signal for the caller to stop its
    /// autoscroll timer and leave any existing terminal selection alone unless some
    /// other event says otherwise.
    pub fn autoscroll_tick(&mut self, t: &mut Terminal, tick: AutoscrollTick<'_>) -> Option<Selection> {
        if self.left_click_count == 0 {
            debug_assert!(self.left_drag_autoscroll == Autoscroll::None);
            return None;
        }

        let delta: isize = match self.left_drag_autoscroll {
            Autoscroll::None => return None,
            Autoscroll::Up => -1,
            Autoscroll::Down => 1,
        };

        // If our click pin no longer belongs to the active screen, the gesture is
        // no longer valid. Stop it so callers can stop their autoscroll timer
        // without clearing the current selection as if this were a real drag.
        if self.validated_left_click_pin(&t.screens).is_none() {
            self.reset(t);
            return None;
        }

        t.scroll_viewport(ScrollViewport::Delta(delta));

        let pin = t.screens.active.pages.pin(point::Point::Viewport(tick.viewport))?;
        self.drag(
            t,
            Drag {
                pin,
                xpos: tick.xpos,
                ypos: tick.ypos,
                rectangle: tick.rectangle,
                word_boundary_codepoints: tick.word_boundary_codepoints,
                geometry: tick.geometry,
            },
        )
    }

    /// Record a deep press event for the active left-click gesture.
    ///
    /// A deep press is a force/pressure activation while the primary pointer is
    /// already down. Ghostty treats it like the platform text-selection affordance:
    /// select the word under the original press, then consume the gesture so
    /// further cursor movement while the button remains pressed does not drag or
    /// autoscroll the selection.
    ///
    /// After a successful deep press, the click sequence is cleared and the tracked
    /// pin is untracked. The returned selection should be applied by the caller. A
    /// null result means there was no valid active left-click anchor, commonly
    /// because the screen changed or the gesture had already been cancelled.
    pub fn deep_press(&mut self, t: &mut Terminal, p: DeepPress<'_>) -> Option<Selection> {
        let click_pin_ptr = self.validated_left_click_pin(&t.screens)?;
        // SAFETY: the pin is tracked by the active screen (validated above).
        let click_pin: Pin = unsafe { *click_pin_ptr.as_ptr() };
        let sel = t.screens.active.select_word(click_pin, p.word_boundary_codepoints);

        self.left_click_count = 0;
        self.left_click_time = None;
        self.left_click_behavior = Behavior::Cell;
        self.left_click_dragged = true;
        self.left_drag_autoscroll = Autoscroll::None;
        self.untrack_pin(t);

        sel
    }

    /// Record a release event for the active left-click gesture.
    ///
    /// This stops autoscroll and updates `left_click_dragged`, but it does not clear
    /// the click count or time. Keeping that state is what lets the next nearby press
    /// become a double- or triple-click. Call `reset` instead if the release should
    /// cancel the click sequence entirely.
    ///
    /// Pass the release pin when the pointer position maps to a valid terminal cell.
    /// If it does not, pass null; the gesture then conservatively records that the
    /// pointer moved away from the original pressed cell. This is useful for callers
    /// that use `left_click_dragged` after release to decide whether a click should
    /// activate links or other hit targets.
    pub fn release(&mut self, t: &mut Terminal, r: Release) {
        if self.left_click_count == 0 {
            debug_assert!(self.left_drag_autoscroll == Autoscroll::None);
            return;
        }

        if let Some(release_pin) = r.pin {
            if let Some(click_pin) = self.validated_left_click_pin(&t.screens) {
                // SAFETY: the pin is tracked by the active screen.
                if !release_pin.eql(unsafe { *click_pin.as_ptr() }) {
                    self.left_click_dragged = true;
                }
            } else {
                // If the original anchor is no longer valid, conservatively treat
                // this as a drag/cancelled click so callers don't perform click-only
                // actions on a different or recycled screen.
                self.left_click_dragged = true;
            }
        } else {
            self.left_click_dragged = true;
        }
        self.left_drag_autoscroll = Autoscroll::None;
    }

    fn press_initial(&mut self, t: &mut Terminal, p: &Press<'_>) -> Result<(), AllocError> {
        // Setup our pin first, reusing our existing pin if we can.
        if let Some(pin) = self.left_click_pin {
            if cfg!(debug_assertions) {
                assert!(self.left_click_screen == t.screens.active_key);
                assert!(self.left_click_screen_generation == t.screens.generation(t.screens.active_key));
            }
            // SAFETY: the pin is tracked by the active screen.
            unsafe { *pin.as_ptr() = p.pin };
        } else {
            let screens: &mut ScreenSet = &mut t.screens;
            self.left_click_pin = Some(screens.active.pages.track_pin(p.pin)?);
            self.left_click_screen = screens.active_key;
            self.left_click_screen_generation = screens.generation(screens.active_key);
        }
        self.left_click_count = 1;
        self.left_click_behavior = p.behaviors[0];
        self.left_click_xpos = p.xpos;
        self.left_click_ypos = p.ypos;
        self.left_click_time = p.time;
        self.left_click_dragged = false;
        self.left_drag_autoscroll = Autoscroll::None;
        Ok(())
    }

    fn press_repeat(&mut self, t: &mut Terminal, p: &Press<'_>) -> Result<(), PressRepeatError> {
        let result = self.press_repeat_inner(t, p);
        if result.is_err() {
            // Zig: errdefer
            self.left_click_count = 0;
            self.left_click_behavior = Behavior::Cell;
            self.untrack_pin(t);
        }
        result
    }

    fn press_repeat_inner(&mut self, t: &Terminal, p: &Press<'_>) -> Result<(), PressRepeatError> {
        // If too much time has passed then we always reset.
        let time = p.time.ok_or(PressRepeatError::PressRequiresReset)?;
        {
            let prev_time = self.left_click_time.ok_or(PressRepeatError::PressRequiresReset)?;
            let since = time_since(time, prev_time).ok_or(PressRepeatError::PressRequiresReset)?;
            if since > p.repeat_interval {
                return Err(PressRepeatError::PressRequiresReset);
            }
        }

        // If the click is too far away from the initial click we can't continue.
        let distance = ((p.xpos - self.left_click_xpos).powi(2) + (p.ypos - self.left_click_ypos).powi(2)).sqrt();
        if distance > p.max_distance {
            return Err(PressRepeatError::PressRequiresReset);
        }

        // If our prior click was on another screen then free and reset. "Another screen"
        // doesn't just mean alt vs primary, it could mean an alt screen that was
        // recycled since we free tracked pins on recycle.
        let screens: &ScreenSet = &t.screens;
        if self.left_click_screen != screens.active_key
            || screens.generation(self.left_click_screen) != self.left_click_screen_generation
        {
            // The error return will trigger the top-level errdefer which
            // will reset our pin.
            return Err(PressRepeatError::PressRequiresReset);
        }

        self.left_click_time = Some(time);
        self.left_click_dragged = false;
        self.left_drag_autoscroll = Autoscroll::None;
        self.left_click_count = (self.left_click_count + 1).min(
            3, // We only support triple clicks max
        );
        self.left_click_behavior = p.behaviors[self.left_click_count as usize - 1];
        Ok(())
    }

    fn press_selection(&self, screen: &Screen, p: &Press<'_>) -> Option<Selection> {
        match self.left_click_behavior {
            Behavior::Cell => None,
            Behavior::Word => screen.select_word(p.pin, p.word_boundary_codepoints),
            Behavior::Line => screen.select_line(SelectLine::new(p.pin)),
            Behavior::Output => screen.select_output(p.pin),
        }
    }

    fn untrack_pin(&mut self, t: &mut Terminal) {
        // Can't untrack unless we have a pin.
        let Some(pin) = self.left_click_pin.take() else { return };

        // If the generation changed our pin is already invalid.
        let screens: &ScreenSet = &t.screens;
        if screens.generation(self.left_click_screen) != self.left_click_screen_generation {
            return;
        }

        // If we can't get a screen then its already freed.
        let Some(mut screen) = screens.get(self.left_click_screen) else { return };
        screen.pages.untrack_pin(pin);
    }
}

fn time_since(time: Timestamp, prev_time: Timestamp) -> Option<u64> {
    // A negative delta (time before prev_time) is null.
    let delta = time.checked_duration_since(prev_time)?;
    Some(u64::try_from(delta.as_nanos()).unwrap_or(u64::MAX))
}

/// Convert a caller-provided floating-point position to a pixel coordinate.
/// Negative and NaN values clamp to the origin, matching the drag behavior
/// outside the left edge of the surface.
fn pixel_from_float(value: f64) -> u32 {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }

    // @intFromFloat requires a value representable by the destination type.
    // Saturate first so positive infinity and oversized coordinates are safe.
    let max: f64 = u32::MAX as f64;
    if value >= max {
        return u32::MAX;
    }
    value as u32
}

/// Calculates the appropriate selection given pins and pixel x positions for
/// the click point and the drag point, as well as selection mode and geometry.
fn drag_selection(
    click_pin: Pin,
    drag_pin: Pin,
    click_x: u32,
    drag_x: u32,
    rectangle_selection: bool,
    geometry: DragGeometry,
) -> Option<Selection> {
    // Explanation:
    //
    // # Normal selections
    //
    // ## Left-to-right selections
    // - The clicked cell is included if it was clicked to the left of its
    //   threshold point and the drag location is right of the threshold point.
    // - The cell under the cursor (the "drag cell") is included if the drag
    //   location is right of its threshold point.
    //
    // ## Right-to-left selections
    // - The clicked cell is included if it was clicked to the right of its
    //   threshold point and the drag location is left of the threshold point.
    // - The cell under the cursor (the "drag cell") is included if the drag
    //   location is left of its threshold point.
    //
    // # Rectangular selections
    //
    // Rectangular selections are handled similarly, except that
    // entire columns are considered rather than individual cells.

    if geometry.columns == 0 || geometry.cell_width == 0 {
        return None;
    }

    // We only include cells in the selection if the threshold point lies
    // between the start and end points of the selection. A threshold of
    // 60% of the cell width was chosen empirically because it felt good.
    let threshold_point: u32 = (geometry.cell_width as f64 * 0.6).round() as u32;

    // We use this to clamp the pixel positions below.
    let pixel_span = geometry.columns.checked_mul(geometry.cell_width).unwrap_or(u32::MAX);
    let max_x = pixel_span - 1;

    // We need to know how far across in the cell the drag pos is, so
    // we subtract the padding and then take it modulo the cell width.
    let drag_x_frac = max_x.min(drag_x.saturating_sub(geometry.padding_left)) % geometry.cell_width;

    // We figure out the fractional part of the click x position similarly.
    let click_x_frac = max_x.min(click_x.saturating_sub(geometry.padding_left)) % geometry.cell_width;

    // Whether the click pin and drag pin are equal.
    let same_pin = drag_pin.eql(click_pin);

    // Whether or not the end point of our selection is before the start point.
    let end_before_start = 'ebs: {
        if same_pin {
            break 'ebs drag_x_frac < click_x_frac;
        }

        // Special handling for rectangular selections, we only use x position.
        if rectangle_selection {
            break 'ebs match drag_pin.x.cmp(&click_pin.x) {
                Ordering::Equal => drag_x_frac < click_x_frac,
                Ordering::Less => true,
                Ordering::Greater => false,
            };
        }

        drag_pin.before(click_pin)
    };

    // Whether or not the click pin cell
    // should be included in the selection.
    let include_click_cell = if end_before_start {
        click_x_frac >= threshold_point
    } else {
        click_x_frac < threshold_point
    };

    // Whether or not the drag pin cell
    // should be included in the selection.
    let include_drag_cell = if end_before_start {
        drag_x_frac < threshold_point
    } else {
        drag_x_frac >= threshold_point
    };

    // If the click cell should be included in the selection then it's the
    // start, otherwise we get the previous or next cell to it depending on
    // the type and direction of the selection.
    let start_pin = if include_click_cell {
        click_pin
    } else if end_before_start {
        if rectangle_selection {
            click_pin.left_clamp(1)
        } else {
            click_pin.left_wrap(1).unwrap_or(click_pin)
        }
    } else if rectangle_selection {
        click_pin.right_clamp(1)
    } else {
        click_pin.right_wrap(1).unwrap_or(click_pin)
    };

    // Likewise for the end pin with the drag cell.
    let end_pin = if include_drag_cell {
        drag_pin
    } else if end_before_start {
        if rectangle_selection {
            drag_pin.right_clamp(1)
        } else {
            drag_pin.right_wrap(1).unwrap_or(drag_pin)
        }
    } else if rectangle_selection {
        drag_pin.left_clamp(1)
    } else {
        drag_pin.left_wrap(1).unwrap_or(drag_pin)
    };

    // If the click cell is the same as the drag cell and the click cell
    // shouldn't be included, or if the cells are adjacent such that the
    // start or end pin becomes the other cell, and that cell should not
    // be included, then we have no selection, so we set it to null.
    //
    // If in rectangular selection mode, we compare columns as well.
    //
    // TODO(qwerasd): this can/should probably be refactored, it's a bit
    //                repetitive and does excess work in rectangle mode.
    if (!include_click_cell && same_pin)
        || (!include_click_cell && rectangle_selection && click_pin.x == drag_pin.x)
        || (!include_click_cell && end_pin.eql(click_pin))
        || (!include_click_cell && rectangle_selection && end_pin.x == click_pin.x)
        || (!include_drag_cell && start_pin.eql(drag_pin))
        || (!include_drag_cell && rectangle_selection && start_pin.x == drag_pin.x)
    {
        return None;
    }

    // TODO: Clamp selection to the screen area, don't
    //       let it extend past the last written row.

    Some(Selection::init(start_pin, end_pin, rectangle_selection))
}

/// Calculates the appropriate word-wise selection for a double-click drag.
fn drag_selection_word(
    screen: &Screen,
    click_pin: Pin,
    drag_pin: Pin,
    boundary_codepoints: &[u32],
) -> Option<Selection> {
    // Get the word closest to our starting click.
    let word_start = screen.select_word_between(click_pin, drag_pin, boundary_codepoints)?;

    // Get the word closest to our current point.
    let word_current = screen.select_word_between(drag_pin, click_pin, boundary_codepoints)?;

    // If our current mouse position is before the starting position,
    // then the selection start is the word nearest our current position.
    if drag_pin.before(click_pin) {
        Some(Selection::init(word_current.start(), word_start.end(), false))
    } else {
        Some(Selection::init(word_start.start(), word_current.end(), false))
    }
}

/// Calculates the appropriate line-wise selection for a triple-click drag.
fn drag_selection_line(screen: &Screen, click_pin: Pin, drag_pin: Pin) -> Option<Selection> {
    // Get the line selection under our current drag point. If there isn't a
    // line, do nothing.
    let line = screen.select_line(SelectLine::new(drag_pin))?;

    // Get the selection under our click point. We first try to trim
    // whitespace if we've selected a word. But if no word exists then
    // we select the blank line.
    let sel_ = screen.select_line(SelectLine::new(click_pin)).or_else(|| {
        screen.select_line(SelectLine {
            whitespace: None,
            ..SelectLine::new(click_pin)
        })
    });

    let mut sel = sel_?;
    if drag_pin.before(click_pin) {
        *sel.start_ptr() = line.start();
    } else {
        *sel.end_ptr() = line.end();
    }
    Some(sel)
}

/// Calculates the appropriate semantic-output-wise selection for an output
/// drag. This expands from the output block under the click point to the output
/// block under the current drag point. If the drag point is not output, keep the
/// original output selection.
fn drag_selection_output(screen: &Screen, click_pin: Pin, drag_pin: Pin) -> Option<Selection> {
    let mut sel = screen.select_output(click_pin)?;
    let Some(current) = screen.select_output(drag_pin) else { return Some(sel) };

    if drag_pin.before(click_pin) {
        *sel.start_ptr() = current.start();
    } else {
        *sel.end_ptr() = current.end();
    }
    Some(sel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::osc::semantic_prompt as osc_sp;
    use crate::terminal::screen::{InputClear, Options as ScreenOptions, SemanticContentUpdate};
    use crate::terminal::selection::Bounds;
    use crate::terminal::terminal::Options as TerminalOptions;
    use std::time::Duration;

    fn term(cols: u16, rows: u16) -> Terminal {
        Terminal::init(TerminalOptions::new(cols, rows)).unwrap()
    }

    fn now() -> Timestamp {
        Timestamp::now()
    }

    fn coord(x: u16, y: u32) -> point::Coordinate {
        point::Coordinate { x, y }
    }

    fn test_press(t: &Terminal, x: u16, y: u32, time: Option<Timestamp>) -> Press<'static> {
        Press {
            time,
            pin: t.screens.active.pages.pin(point::Point::Active(coord(x, y))).unwrap(),
            xpos: x as f64,
            ypos: y as f64,
            max_distance: 1.0,
            repeat_interval: u64::MAX,
            word_boundary_codepoints: &[],
            behaviors: &DEFAULT_BEHAVIORS,
        }
    }

    fn test_drag(t: &Terminal, x: u16, y: u32, xpos: f64, ypos: f64) -> Drag<'static> {
        Drag {
            pin: t.screens.active.pages.pin(point::Point::Active(coord(x, y))).unwrap(),
            xpos,
            ypos,
            rectangle: false,
            word_boundary_codepoints: &[],
            geometry: DragGeometry {
                columns: 5,
                cell_width: 10,
                padding_left: 0,
                screen_height: 100,
            },
        }
    }

    fn test_autoscroll_tick(viewport: point::Coordinate, xpos: f64, ypos: f64) -> AutoscrollTick<'static> {
        AutoscrollTick {
            viewport,
            xpos,
            ypos,
            rectangle: false,
            word_boundary_codepoints: &[],
            geometry: DragGeometry {
                columns: 5,
                cell_width: 10,
                padding_left: 0,
                screen_height: 100,
            },
        }
    }

    fn test_pin(t: &Terminal, x: u16, y: u32) -> Pin {
        t.screens.active.pages.pin(point::Point::Active(coord(x, y))).unwrap()
    }

    fn alt_screen(t: &mut Terminal) {
        let opts = ScreenOptions {
            cols: t.cols,
            rows: t.rows,
            ..ScreenOptions::default_value()
        };
        t.screens.get_init(screen_set::Key::Alternate, opts).unwrap();
        t.screens.switch_to(screen_set::Key::Alternate);
    }

    /// The test screen of `test_drag_selection`.
    fn drag_test_screen() -> Screen {
        Screen::init(ScreenOptions {
            cols: 10,
            rows: 5,
            max_scrollback_bytes: Some(0),
            ..ScreenOptions::default_value()
        })
        .unwrap()
    }

    const DRAG_TEST_GEOMETRY: DragGeometry = DragGeometry {
        columns: 10,
        cell_width: 10,
        padding_left: 5,
        screen_height: 110,
    };

    fn vp_pin(screen: &Screen, x: u16, y: u32) -> Pin {
        screen.pages.pin(point::Point::Viewport(coord(x, y))).unwrap()
    }

    fn x_pos(x: f64) -> u32 {
        let cell_width_f64 = DRAG_TEST_GEOMETRY.cell_width as f64;
        (x * cell_width_f64).floor() as u32 + DRAG_TEST_GEOMETRY.padding_left
    }

    /// Utility function for the unit tests for drag selection logic.
    ///
    /// Tests a click and drag on a 10x5 cell grid, x positions are given in
    /// fractional cells, e.g. 3.1 would be 10% through the cell at x = 3.
    ///
    /// NOTE: The geometry tested with has 10px wide cells, meaning only one digit
    ///       after the decimal place has any meaning, e.g. 3.14 is equal to 3.1.
    ///
    /// The provided start_x/y and end_x/y are the expected start and end points
    /// of the resulting selection.
    #[allow(clippy::too_many_arguments)]
    fn test_drag_selection(
        click_x: f64,
        click_y: u32,
        drag_x: f64,
        drag_y: u32,
        start_x: u16,
        start_y: u32,
        end_x: u16,
        end_y: u32,
        rect: bool,
    ) {
        // Our screen size is 10x5 cells that are
        // 10x20 px, with 5px padding on all sides.
        let screen = drag_test_screen();

        let click_pin = vp_pin(&screen, click_x.floor() as u16, click_y);
        let drag_pin = vp_pin(&screen, drag_x.floor() as u16, drag_y);

        let start_pin = vp_pin(&screen, start_x, start_y);
        let end_pin = vp_pin(&screen, end_x, end_y);

        assert_eq!(
            Some(Selection {
                bounds: Bounds::Untracked {
                    start: start_pin,
                    end: end_pin,
                },
                rectangle: rect,
            }),
            drag_selection(click_pin, drag_pin, x_pos(click_x), x_pos(drag_x), rect, DRAG_TEST_GEOMETRY),
        );
    }

    /// Like `test_drag_selection` but checks that the resulting selection is null.
    ///
    /// See `test_drag_selection` for more details.
    fn test_drag_selection_is_null(click_x: f64, click_y: u32, drag_x: f64, drag_y: u32, rect: bool) {
        // Our screen size is 10x5 cells that are
        // 10x20 px, with 5px padding on all sides.
        let screen = drag_test_screen();

        let click_pin = vp_pin(&screen, click_x.floor() as u16, click_y);
        let drag_pin = vp_pin(&screen, drag_x.floor() as u16, drag_y);

        assert_eq!(
            None,
            drag_selection(click_pin, drag_pin, x_pos(click_x), x_pos(drag_x), rect, DRAG_TEST_GEOMETRY),
        );
    }

    #[rustfmt::skip]
    #[test]
    fn selection_gesture_drag_selection_logic() {
        // -- LTR
        // single cell selection
        test_drag_selection(3.0, 3, 3.9, 3, 3, 3, 3, 3, false);
        // including click and drag pin cells
        test_drag_selection(3.0, 3, 5.9, 3, 3, 3, 5, 3, false);
        // including click pin cell but not drag pin cell
        test_drag_selection(3.0, 3, 5.0, 3, 3, 3, 4, 3, false);
        // including drag pin cell but not click pin cell
        test_drag_selection(3.9, 3, 5.9, 3, 4, 3, 5, 3, false);
        // including neither click nor drag pin cells
        test_drag_selection(3.9, 3, 5.0, 3, 4, 3, 4, 3, false);
        // empty selection (single cell on only left half)
        test_drag_selection_is_null(3.0, 3, 3.1, 3, false);
        // empty selection (single cell on only right half)
        test_drag_selection_is_null(3.8, 3, 3.9, 3, false);
        // empty selection (between two cells, not crossing threshold)
        test_drag_selection_is_null(3.9, 3, 4.0, 3, false);

        // -- RTL
        // single cell selection
        test_drag_selection(3.9, 3, 3.0, 3, 3, 3, 3, 3, false);
        // including click and drag pin cells
        test_drag_selection(5.9, 3, 3.0, 3, 5, 3, 3, 3, false);
        // including click pin cell but not drag pin cell
        test_drag_selection(5.9, 3, 3.9, 3, 5, 3, 4, 3, false);
        // including drag pin cell but not click pin cell
        test_drag_selection(5.0, 3, 3.0, 3, 4, 3, 3, 3, false);
        // including neither click nor drag pin cells
        test_drag_selection(5.0, 3, 3.9, 3, 4, 3, 4, 3, false);
        // empty selection (single cell on only left half)
        test_drag_selection_is_null(3.1, 3, 3.0, 3, false);
        // empty selection (single cell on only right half)
        test_drag_selection_is_null(3.9, 3, 3.8, 3, false);
        // empty selection (between two cells, not crossing threshold)
        test_drag_selection_is_null(4.0, 3, 3.9, 3, false);

        // -- Wrapping
        // LTR, wrap excluded cells
        test_drag_selection(9.9, 2, 0.0, 4, 0, 3, 9, 3, false);
        // RTL, wrap excluded cells
        test_drag_selection(0.0, 4, 9.9, 2, 9, 3, 0, 3, false);
    }

    #[rustfmt::skip]
    #[test]
    fn selection_gesture_rectangle_drag_selection_logic() {
        // -- LTR
        // single column selection
        test_drag_selection(3.0, 2, 3.9, 4, 3, 2, 3, 4, true);
        // including click and drag pin columns
        test_drag_selection(3.0, 2, 5.9, 4, 3, 2, 5, 4, true);
        // including click pin column but not drag pin column
        test_drag_selection(3.0, 2, 5.0, 4, 3, 2, 4, 4, true);
        // including drag pin column but not click pin column
        test_drag_selection(3.9, 2, 5.9, 4, 4, 2, 5, 4, true);
        // including neither click nor drag pin columns
        test_drag_selection(3.9, 2, 5.0, 4, 4, 2, 4, 4, true);
        // empty selection (single column on only left half)
        test_drag_selection_is_null(3.0, 2, 3.1, 4, true);
        // empty selection (single column on only right half)
        test_drag_selection_is_null(3.8, 2, 3.9, 4, true);
        // empty selection (between two columns, not crossing threshold)
        test_drag_selection_is_null(3.9, 2, 4.0, 4, true);

        // -- RTL
        // single column selection
        test_drag_selection(3.9, 2, 3.0, 4, 3, 2, 3, 4, true);
        // including click and drag pin columns
        test_drag_selection(5.9, 2, 3.0, 4, 5, 2, 3, 4, true);
        // including click pin column but not drag pin column
        test_drag_selection(5.9, 2, 3.9, 4, 5, 2, 4, 4, true);
        // including drag pin column but not click pin column
        test_drag_selection(5.0, 2, 3.0, 4, 4, 2, 3, 4, true);
        // including neither click nor drag pin columns
        test_drag_selection(5.0, 2, 3.9, 4, 4, 2, 4, 4, true);
        // empty selection (single column on only left half)
        test_drag_selection_is_null(3.1, 2, 3.0, 4, true);
        // empty selection (single column on only right half)
        test_drag_selection_is_null(3.9, 2, 3.8, 4, true);
        // empty selection (between two columns, not crossing threshold)
        test_drag_selection_is_null(4.0, 2, 3.9, 4, true);

        // -- Wrapping
        // LTR, do not wrap
        test_drag_selection(9.9, 2, 0.0, 4, 9, 2, 0, 4, true);
        // RTL, do not wrap
        test_drag_selection(0.0, 4, 9.9, 2, 0, 4, 9, 2, true);
    }

    #[test]
    fn selection_gesture_press_records_initial_click() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let time = now();
        let ev = test_press(&t, 1, 2, Some(time));
        let _ = gesture.press(&mut t, ev).unwrap();

        assert_eq!(1, gesture.left_click_count);
        assert_eq!(time, gesture.left_click_time.unwrap());
        assert_eq!(1.0, gesture.left_click_xpos);
        assert_eq!(2.0, gesture.left_click_ypos);
        assert!(!gesture.left_click_dragged);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_press_returns_standard_click_selections() {
        let mut t = term(20, 5);
        t.print_string("alpha beta\none two").unwrap();
        let mut gesture = SelectionGesture::INIT;

        let time = now();
        let mut event = test_press(&t, 1, 0, Some(time));
        event.word_boundary_codepoints = &[' ' as u32];

        assert_eq!(None, gesture.press(&mut t, event).unwrap());

        assert_eq!(
            Selection::init(test_pin(&t, 0, 0), test_pin(&t, 4, 0), false),
            gesture.press(&mut t, event).unwrap().unwrap()
        );

        assert_eq!(
            Selection::init(test_pin(&t, 0, 0), test_pin(&t, 9, 0), false),
            gesture.press(&mut t, event).unwrap().unwrap()
        );

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_press_behaviors_choose_press_and_drag_behavior() {
        let mut t = term(20, 5);
        t.print_string("alpha beta\none two\nthree four").unwrap();
        let mut gesture = SelectionGesture::INIT;

        let time = now();
        let behaviors = [Behavior::Cell, Behavior::Line, Behavior::Word];
        let mut event = test_press(&t, 1, 0, Some(time));
        event.behaviors = &behaviors;
        event.word_boundary_codepoints = &[' ' as u32];

        let _ = gesture.press(&mut t, event).unwrap();
        assert_eq!(Behavior::Cell, gesture.left_click_behavior);

        let double_click = gesture.press(&mut t, event).unwrap().unwrap();
        assert_eq!(Behavior::Line, gesture.left_click_behavior);
        assert_eq!(
            Selection::init(test_pin(&t, 0, 0), test_pin(&t, 9, 0), false),
            double_click
        );

        let d = test_drag(&t, 2, 2, 20.0, 50.0);
        let line_drag = gesture.drag(&mut t, d).unwrap();
        assert_eq!(Selection::init(test_pin(&t, 0, 0), test_pin(&t, 9, 2), false), line_drag);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_output_behavior_selects_and_drags_semantic_output() {
        let mut t = term(10, 6);

        {
            let screen = &mut t.screens.active;
            screen.cursor_set_semantic_content(SemanticContentUpdate::Output);
            screen.test_write_string("out1\n").unwrap();
            screen.cursor_set_semantic_content(SemanticContentUpdate::Prompt(osc_sp::PromptKind::Initial));
            screen.test_write_string("$ ").unwrap();
            screen.cursor_set_semantic_content(SemanticContentUpdate::Input(InputClear::ClearExplicit));
            screen.test_write_string("cmd\n").unwrap();
            screen.cursor_set_semantic_content(SemanticContentUpdate::Output);
            screen.test_write_string("out2").unwrap();
        }

        let mut gesture = SelectionGesture::INIT;

        let behaviors = [Behavior::Output, Behavior::Word, Behavior::Line];
        let mut event = test_press(&t, 1, 0, Some(now()));
        event.behaviors = &behaviors;

        let press_selection = gesture.press(&mut t, event).unwrap().unwrap();
        assert_eq!(Behavior::Output, gesture.left_click_behavior);
        assert_eq!(
            Selection::init(test_pin(&t, 0, 0), test_pin(&t, 3, 0), false),
            press_selection
        );

        let d = test_drag(&t, 1, 2, 10.0, 50.0);
        let output_drag = gesture.drag(&mut t, d).unwrap();
        assert_eq!(Selection::init(test_pin(&t, 0, 0), test_pin(&t, 3, 2), false), output_drag);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_drag_returns_selection_and_records_autoscroll() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let mut press_event = test_press(&t, 1, 1, Some(now()));
        press_event.xpos = 10.0;
        let _ = gesture.press(&mut t, press_event).unwrap();

        let d = test_drag(&t, 3, 1, 39.0, 50.0);
        let sel = gesture.drag(&mut t, d).unwrap();
        assert_eq!(Autoscroll::None, gesture.left_drag_autoscroll);
        assert!(gesture.left_click_dragged);

        assert_eq!(Selection::init(test_pin(&t, 1, 1), test_pin(&t, 3, 1), false), sel);

        let d = test_drag(&t, 3, 1, 39.0, 1.0);
        let _ = gesture.drag(&mut t, d);
        assert_eq!(Autoscroll::Up, gesture.left_drag_autoscroll);

        let d = test_drag(&t, 3, 1, 39.0, 100.0);
        let _ = gesture.drag(&mut t, d);
        assert_eq!(Autoscroll::Down, gesture.left_drag_autoscroll);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_drag_clamps_unrepresentable_positions() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let mut press_event = test_press(&t, 1, 1, Some(now()));
        press_event.xpos = 10.0;
        let _ = gesture.press(&mut t, press_event).unwrap();

        let d = test_drag(&t, 1, 1, f64::INFINITY, 50.0);
        let positive = gesture.drag(&mut t, d).unwrap();
        assert!(test_pin(&t, 1, 1).eql(positive.start()));
        assert!(test_pin(&t, 1, 1).eql(positive.end()));

        let d = test_drag(&t, 1, 1, f64::NAN, 50.0);
        assert_eq!(None, gesture.drag(&mut t, d));

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_drag_saturates_overflowing_geometry() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let ev = test_press(&t, 1, 1, Some(now()));
        let _ = gesture.press(&mut t, ev).unwrap();
        let mut drag_event = test_drag(&t, 1, 1, 10.0, 50.0);
        drag_event.geometry.columns = u32::MAX;
        drag_event.geometry.cell_width = u32::MAX;
        assert_eq!(None, gesture.drag(&mut t, drag_event));

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_drag_rejects_empty_geometry() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let ev = test_press(&t, 1, 1, Some(now()));
        let _ = gesture.press(&mut t, ev).unwrap();
        let mut drag_event = test_drag(&t, 1, 1, 10.0, 50.0);
        drag_event.geometry.columns = 0;
        drag_event.geometry.cell_width = 0;
        assert_eq!(None, gesture.drag(&mut t, drag_event));

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_release_clears_autoscroll_and_records_drag() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let ev = test_press(&t, 1, 1, Some(now()));
        let _ = gesture.press(&mut t, ev).unwrap();
        assert!(!gesture.left_click_dragged);

        let d = test_drag(&t, 1, 1, 10.0, 1.0);
        let _ = gesture.drag(&mut t, d);
        assert_eq!(Autoscroll::Up, gesture.left_drag_autoscroll);
        assert!(!gesture.left_click_dragged);

        let r = Release {
            pin: Some(test_pin(&t, 2, 1)),
        };
        gesture.release(&mut t, r);
        assert_eq!(Autoscroll::None, gesture.left_drag_autoscroll);
        assert!(gesture.left_click_dragged);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_release_with_invalidated_click_records_drag() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let ev = test_press(&t, 1, 1, Some(now()));
        let _ = gesture.press(&mut t, ev).unwrap();
        assert!(!gesture.left_click_dragged);

        alt_screen(&mut t);

        let r = Release {
            pin: Some(test_pin(&t, 1, 1)),
        };
        gesture.release(&mut t, r);
        assert!(gesture.left_click_dragged);
        assert_eq!(Autoscroll::None, gesture.left_drag_autoscroll);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_same_cell_threshold_selection_records_drag() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let mut press_event = test_press(&t, 1, 1, Some(now()));
        press_event.xpos = 10.0;
        let _ = gesture.press(&mut t, press_event).unwrap();
        assert!(!gesture.left_click_dragged);

        let d = test_drag(&t, 1, 1, 19.0, 50.0);
        let sel = gesture.drag(&mut t, d).unwrap();
        assert!(gesture.left_click_dragged);
        assert_eq!(Selection::init(test_pin(&t, 1, 1), test_pin(&t, 1, 1), false), sel);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_drag_without_press_returns_null() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let d = test_drag(&t, 1, 1, 10.0, 50.0);
        assert_eq!(None, gesture.drag(&mut t, d));
        assert_eq!(Autoscroll::None, gesture.left_drag_autoscroll);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_drag_autoscroll_edge_boundaries() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let mut press_event = test_press(&t, 1, 1, Some(now()));
        press_event.xpos = 10.0;
        let _ = gesture.press(&mut t, press_event).unwrap();

        let d = test_drag(&t, 2, 1, 20.0, 1.0);
        let _ = gesture.drag(&mut t, d);
        assert_eq!(Autoscroll::Up, gesture.left_drag_autoscroll);

        let d = test_drag(&t, 2, 1, 20.0, 1.1);
        let _ = gesture.drag(&mut t, d);
        assert_eq!(Autoscroll::None, gesture.left_drag_autoscroll);

        let d = test_drag(&t, 2, 1, 20.0, 99.0);
        let _ = gesture.drag(&mut t, d);
        assert_eq!(Autoscroll::None, gesture.left_drag_autoscroll);

        let d = test_drag(&t, 2, 1, 20.0, 99.1);
        let _ = gesture.drag(&mut t, d);
        assert_eq!(Autoscroll::Down, gesture.left_drag_autoscroll);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_autoscroll_tick_scrolls_and_continues_drag() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let mut press_event = test_press(&t, 1, 1, Some(now()));
        press_event.xpos = 10.0;
        let _ = gesture.press(&mut t, press_event).unwrap();

        let d = test_drag(&t, 3, 1, 39.0, 100.0);
        let _ = gesture.drag(&mut t, d);
        assert_eq!(Autoscroll::Down, gesture.left_drag_autoscroll);

        let sel = gesture
            .autoscroll_tick(&mut t, test_autoscroll_tick(coord(3, 2), 39.0, 100.0))
            .unwrap();
        assert_eq!(Autoscroll::Down, gesture.left_drag_autoscroll);
        assert!(gesture.left_click_dragged);
        assert_eq!(Selection::init(test_pin(&t, 1, 1), test_pin(&t, 3, 2), false), sel);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_autoscroll_tick_resolves_drag_pin_after_scrolling() {
        let mut t = Terminal::init(TerminalOptions {
            max_scrollback_bytes: Some(10),
            ..TerminalOptions::new(5, 3)
        })
        .unwrap();
        t.print_string("1111\n2222\n3333\n4444\n5555").unwrap();
        t.scroll_viewport(ScrollViewport::Delta(-2));

        let mut gesture = SelectionGesture::INIT;

        let mut press_event = test_press(&t, 1, 1, Some(now()));
        press_event.xpos = 10.0;
        let _ = gesture.press(&mut t, press_event).unwrap();

        let d = test_drag(&t, 3, 2, 39.0, 100.0);
        let _ = gesture.drag(&mut t, d);
        assert_eq!(Autoscroll::Down, gesture.left_drag_autoscroll);

        let viewport = coord(3, 2);
        let pre_scroll_pin = t.screens.active.pages.pin(point::Point::Viewport(viewport)).unwrap();
        let sel = gesture
            .autoscroll_tick(&mut t, test_autoscroll_tick(viewport, 39.0, 100.0))
            .unwrap();
        let post_scroll_pin = t.screens.active.pages.pin(point::Point::Viewport(viewport)).unwrap();

        assert!(!pre_scroll_pin.eql(post_scroll_pin));
        assert_eq!(Selection::init(test_pin(&t, 1, 1), post_scroll_pin, false), sel);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_autoscroll_tick_stops_with_invalidated_click() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let mut press_event = test_press(&t, 1, 1, Some(now()));
        press_event.xpos = 10.0;
        let _ = gesture.press(&mut t, press_event).unwrap();

        let d = test_drag(&t, 2, 1, 20.0, 1.0);
        let _ = gesture.drag(&mut t, d);
        assert_eq!(Autoscroll::Up, gesture.left_drag_autoscroll);

        alt_screen(&mut t);

        assert_eq!(
            None,
            gesture.autoscroll_tick(&mut t, test_autoscroll_tick(coord(2, 1), 20.0, 1.0))
        );
        assert_eq!(Autoscroll::None, gesture.left_drag_autoscroll);
        assert_eq!(0, gesture.left_click_count);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_deep_press_selects_word_and_consumes_drag() {
        let mut t = term(20, 5);
        t.print_string("alpha beta").unwrap();
        let mut gesture = SelectionGesture::INIT;

        let ev = test_press(&t, 1, 0, Some(now()));
        let _ = gesture.press(&mut t, ev).unwrap();
        let d = test_drag(&t, 1, 0, 10.0, 1.0);
        let _ = gesture.drag(&mut t, d);
        assert_eq!(Autoscroll::Up, gesture.left_drag_autoscroll);

        let sel = gesture
            .deep_press(
                &mut t,
                DeepPress {
                    word_boundary_codepoints: &[' ' as u32],
                },
            )
            .unwrap();

        assert_eq!(Selection::init(test_pin(&t, 0, 0), test_pin(&t, 4, 0), false), sel);
        assert_eq!(0, gesture.left_click_count);
        assert_eq!(None, gesture.left_click_time);
        assert!(gesture.left_click_dragged);
        assert_eq!(Autoscroll::None, gesture.left_drag_autoscroll);
        assert!(gesture.left_click_pin.is_none());

        let d = test_drag(&t, 7, 0, 70.0, 50.0);
        assert_eq!(None, gesture.drag(&mut t, d));
        let r = Release {
            pin: Some(test_pin(&t, 7, 0)),
        };
        gesture.release(&mut t, r);
        assert!(gesture.left_click_dragged);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_drag_with_invalidated_click_returns_null() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let mut press_event = test_press(&t, 1, 1, Some(now()));
        press_event.xpos = 10.0;
        let _ = gesture.press(&mut t, press_event).unwrap();

        let d = test_drag(&t, 2, 1, 20.0, 1.0);
        let _ = gesture.drag(&mut t, d);
        assert_eq!(Autoscroll::Up, gesture.left_drag_autoscroll);

        alt_screen(&mut t);

        let d = test_drag(&t, 2, 1, 20.0, 50.0);
        assert_eq!(None, gesture.drag(&mut t, d));
        assert_eq!(Autoscroll::Up, gesture.left_drag_autoscroll);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_double_click_drag_selects_by_word() {
        let mut t = term(20, 5);
        t.print_string("alpha beta gamma").unwrap();
        let mut gesture = SelectionGesture::INIT;

        let time = now();
        let ev = test_press(&t, 1, 0, Some(time));
        let _ = gesture.press(&mut t, ev).unwrap();
        let _ = gesture.press(&mut t, ev).unwrap();

        let mut drag_event = test_drag(&t, 7, 0, 70.0, 50.0);
        drag_event.word_boundary_codepoints = &[' ' as u32];
        let sel = gesture.drag(&mut t, drag_event).unwrap();

        assert_eq!(Selection::init(test_pin(&t, 0, 0), test_pin(&t, 9, 0), false), sel);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_double_click_drag_selects_by_word_backwards() {
        let mut t = term(20, 5);
        t.print_string("alpha beta gamma").unwrap();
        let mut gesture = SelectionGesture::INIT;

        let time = now();
        let ev = test_press(&t, 7, 0, Some(time));
        let _ = gesture.press(&mut t, ev).unwrap();
        let _ = gesture.press(&mut t, ev).unwrap();

        let mut drag_event = test_drag(&t, 1, 0, 10.0, 50.0);
        drag_event.word_boundary_codepoints = &[' ' as u32];
        let sel = gesture.drag(&mut t, drag_event).unwrap();

        assert_eq!(Selection::init(test_pin(&t, 0, 0), test_pin(&t, 9, 0), false), sel);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_double_click_drag_on_empty_cell_selects_nearest_word() {
        let mut t = term(20, 5);
        t.print_string("alpha beta").unwrap();
        let mut gesture = SelectionGesture::INIT;

        let time = now();
        let ev = test_press(&t, 1, 0, Some(time));
        let _ = gesture.press(&mut t, ev).unwrap();
        let _ = gesture.press(&mut t, ev).unwrap();

        let mut drag_event = test_drag(&t, 15, 0, 150.0, 50.0);
        drag_event.word_boundary_codepoints = &[' ' as u32];
        let sel = gesture.drag(&mut t, drag_event).unwrap();

        assert_eq!(Selection::init(test_pin(&t, 0, 0), test_pin(&t, 9, 0), false), sel);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_triple_click_drag_selects_by_line() {
        let mut t = term(20, 5);
        t.print_string("alpha beta\none two\nthree four").unwrap();
        let mut gesture = SelectionGesture::INIT;

        let time = now();
        let ev = test_press(&t, 1, 0, Some(time));
        let _ = gesture.press(&mut t, ev).unwrap();
        let _ = gesture.press(&mut t, ev).unwrap();
        let _ = gesture.press(&mut t, ev).unwrap();

        let d = test_drag(&t, 2, 2, 20.0, 50.0);
        let sel = gesture.drag(&mut t, d).unwrap();

        assert_eq!(Selection::init(test_pin(&t, 0, 0), test_pin(&t, 9, 2), false), sel);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_triple_click_drag_selects_by_line_backwards() {
        let mut t = term(20, 5);
        t.print_string("alpha beta\none two\nthree four").unwrap();
        let mut gesture = SelectionGesture::INIT;

        let time = now();
        let ev = test_press(&t, 2, 2, Some(time));
        let _ = gesture.press(&mut t, ev).unwrap();
        let _ = gesture.press(&mut t, ev).unwrap();
        let _ = gesture.press(&mut t, ev).unwrap();

        let d = test_drag(&t, 1, 0, 10.0, 50.0);
        let sel = gesture.drag(&mut t, d).unwrap();

        assert_eq!(Selection::init(test_pin(&t, 0, 0), test_pin(&t, 9, 2), false), sel);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_repeat_increments_click_count() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let time = now();
        let ev = test_press(&t, 1, 1, Some(time));
        let _ = gesture.press(&mut t, ev).unwrap();
        let _ = gesture.press(&mut t, ev).unwrap();

        assert_eq!(2, gesture.left_click_count);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_repeat_clamps_at_triple_click() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let time = now();
        for _ in 0..4 {
            let ev = test_press(&t, 1, 1, Some(time));
            let _ = gesture.press(&mut t, ev).unwrap();
        }

        assert_eq!(3, gesture.left_click_count);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_null_initial_time_stays_single_click() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let ev = test_press(&t, 1, 1, None);
        let _ = gesture.press(&mut t, ev).unwrap();
        let ev = test_press(&t, 1, 1, Some(now()));
        let _ = gesture.press(&mut t, ev).unwrap();

        assert_eq!(1, gesture.left_click_count);
        assert!(gesture.left_click_time.is_some());

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_null_repeat_time_stays_single_click() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let ev = test_press(&t, 1, 1, Some(now()));
        let _ = gesture.press(&mut t, ev).unwrap();
        let ev = test_press(&t, 1, 1, None);
        let _ = gesture.press(&mut t, ev).unwrap();

        assert_eq!(1, gesture.left_click_count);
        assert_eq!(None, gesture.left_click_time);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_distant_press_resets_click_count() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let time = now();
        let ev = test_press(&t, 1, 1, Some(time));
        let _ = gesture.press(&mut t, ev).unwrap();
        let ev = test_press(&t, 4, 1, Some(time));
        let _ = gesture.press(&mut t, ev).unwrap();

        assert_eq!(1, gesture.left_click_count);
        assert_eq!(4.0, gesture.left_click_xpos);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_expired_repeat_resets_click_count() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let mut event = test_press(&t, 1, 1, Some(now()));
        event.repeat_interval = 0;
        let _ = gesture.press(&mut t, event).unwrap();

        std::thread::sleep(Duration::from_millis(1));
        event.time = Some(now());
        let _ = gesture.press(&mut t, event).unwrap();

        assert_eq!(1, gesture.left_click_count);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_backwards_repeat_time_resets_click_count() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let n = now();
        let earlier = n + Duration::from_secs(1);
        let later = n + Duration::from_secs(2);
        let ev = test_press(&t, 1, 1, Some(later));
        let _ = gesture.press(&mut t, ev).unwrap();
        let ev = test_press(&t, 1, 1, Some(earlier));
        let _ = gesture.press(&mut t, ev).unwrap();

        assert_eq!(1, gesture.left_click_count);
        assert_eq!(earlier, gesture.left_click_time.unwrap());

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_screen_switch_resets_click_count() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        let time = now();
        let primary_tracked = t.screens.active.pages.count_tracked_pins();
        let ev = test_press(&t, 1, 1, Some(time));
        let _ = gesture.press(&mut t, ev).unwrap();

        alt_screen(&mut t);
        let ev = test_press(&t, 1, 1, Some(time));
        let _ = gesture.press(&mut t, ev).unwrap();

        assert_eq!(1, gesture.left_click_count);
        assert_eq!(screen_set::Key::Alternate, gesture.left_click_screen);
        assert_eq!(
            primary_tracked,
            t.screens.get(screen_set::Key::Primary).unwrap().pages.count_tracked_pins()
        );

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_removed_screen_resets_without_untracking_stale_pin() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;

        alt_screen(&mut t);
        let ev = test_press(&t, 1, 1, Some(now()));
        let _ = gesture.press(&mut t, ev).unwrap();

        t.screens.switch_to(screen_set::Key::Primary);
        t.screens.remove(screen_set::Key::Alternate);
        let ev = test_press(&t, 1, 1, Some(now()));
        let _ = gesture.press(&mut t, ev).unwrap();

        assert_eq!(1, gesture.left_click_count);
        assert_eq!(screen_set::Key::Primary, gesture.left_click_screen);

        gesture.deinit(&mut t);
    }

    #[test]
    fn selection_gesture_deinit_untracks_pin() {
        let mut t = term(5, 5);
        let mut gesture = SelectionGesture::INIT;
        let tracked = t.screens.active.pages.count_tracked_pins();
        let ev = test_press(&t, 1, 1, Some(now()));
        let _ = gesture.press(&mut t, ev).unwrap();
        assert_eq!(tracked + 1, t.screens.active.pages.count_tracked_pins());

        gesture.deinit(&mut t);
        assert_eq!(tracked, t.screens.active.pages.count_tracked_pins());
    }
}
