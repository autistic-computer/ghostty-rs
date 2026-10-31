//! Port of `src/renderer/State.zig`.
//!
//! STATUS: ported
//!
//! This is the render state that is given to a renderer.
//!
//! Deviations:
//! * `std.Io.Mutex` is [`crate::lib::tinyio::Mutex`] and `std.Io` is
//!   [`TinyIo`]. The mutex, terminal and inspector are raw pointers like in
//!   Zig: the owner (the core surface) keeps them alive for the lifetime of
//!   the state.
//! * `Preedit.codepoints` is an owned `Vec` (Zig: an allocated slice with
//!   explicit `deinit`/`clone`).

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::input::key_mods::Mods;
use crate::lib::tinyio::{Mutex, Timeout, TinyIo};
use crate::terminal::point::Coordinate;
use crate::terminal::size::CellCountInt;

use crate::inspector::Inspector;
use crate::terminal::terminal::Terminal;

#[derive(Debug)]
pub struct State {
    /// The mutex that must be held while reading any of the data in the
    /// members of this state. Note that the state itself is NOT protected
    /// by the mutex and is NOT thread-safe, only the members values of the
    /// state (i.e. the terminal, devmode, etc. values).
    pub mutex: NonNull<Mutex>,

    /// The terminal data.
    pub terminal: NonNull<Terminal>,

    /// The terminal inspector, if any. This will be null while the inspector
    /// is not active and will be set when it is active.
    pub inspector: Option<NonNull<Inspector>>,

    /// Dead key state. This will render the current dead key preedit text
    /// over the cursor. This currently only ever renders a single codepoint.
    /// Preedit can in theory be multiple codepoints long but that is left as
    /// a future exercise.
    pub preedit: Option<Preedit>,

    /// Mouse state. This only contains state relevant to what renderers
    /// need about the mouse.
    pub mouse: Mouse,

    /// The number of threads currently waiting to acquire `mutex` via
    /// `lockDemand`. This is not protected by the mutex; it is read by
    /// hot lock/unlock loops (the IO parse thread) in `yieldToDemand` to
    /// decide whether to hand the mutex off before relocking it.
    pub demand: AtomicU32,

    /// Handoff generation counter. Incremented (with a futex wake) by
    /// `unlockDemand` after a demanding waiter releases the mutex, so that
    /// `yieldToDemand` knows the waiter had its turn.
    pub handoff_gen: AtomicU32,
}

// SAFETY: the pointed-to mutex is thread-safe and the terminal/inspector
// are only accessed while holding it (the contract documented on `mutex`).
unsafe impl Send for State {}
unsafe impl Sync for State {}

/// How long `yieldToDemand` sleeps waiting for a demanding waiter to
/// take its turn before giving up. This bounds how long the IO parse
/// thread can stall if a wake is lost or the waiter is descheduled; a
/// demanding critical section (the renderer's frame snapshot) is
/// microseconds, so one millisecond is generous.
const HANDOFF_TIMEOUT_NS: u64 = 1_000_000;

impl State {
    /// Create a state with the default values for the optional members.
    pub fn new(mutex: NonNull<Mutex>, terminal: NonNull<Terminal>) -> State {
        State {
            mutex,
            terminal,
            inspector: None,
            preedit: None,
            mouse: Mouse::default(),
            demand: AtomicU32::new(0),
            handoff_gen: AtomicU32::new(0),
        }
    }

    fn mutex(&self) -> &Mutex {
        // SAFETY: the owner keeps the mutex alive for the lifetime of the
        // state.
        unsafe { self.mutex.as_ref() }
    }

    /// Acquire `mutex` while signaling demand for it. Use this instead of
    /// locking the mutex directly on threads that must not be starved by
    /// a hot lock/unlock loop (the renderer's frame snapshot). Must be
    /// released with `unlockDemand`; releasing with `mutex.unlock` keeps
    /// the data safe but makes parked `yieldToDemand` callers wait out
    /// their full timeout.
    ///
    /// Both `std.Thread.Mutex` and os_unfair_lock are unfair: a running
    /// thread that unlocks and immediately relocks beats a sleeping
    /// waiter every time, because the waiter must first be woken and
    /// scheduled. Under sustained pty output the IO parse thread is
    /// exactly such a loop, so without this signal the renderer can
    /// starve for as long as the output lasts.
    pub fn lock_demand(&self, io: TinyIo) {
        self.demand.fetch_add(1, Ordering::Relaxed);
        self.mutex().lock_uncancelable(io);
        let prev = self.demand.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(prev > 0);
    }

    /// Release `mutex` acquired via `lockDemand` and notify hot loops
    /// parked in `yieldToDemand` that the demanding waiter had its turn.
    pub fn unlock_demand(&self, io: TinyIo) {
        self.mutex().unlock(io);
        self.handoff_gen.fetch_add(1, Ordering::Relaxed);
        io.futex_wake(&self.handoff_gen, 1);
    }

    /// Called by hot lock/unlock loops between critical sections, with
    /// `mutex` NOT held: if a `lockDemand` waiter exists, sleep until it
    /// has acquired and released the mutex (or the timeout passes). This
    /// is the handoff that unfair mutexes never do on their own.
    ///
    /// The orderings here are all monotonic because these atomics are a
    /// scheduling heuristic, not a synchronization boundary: the mutex
    /// itself orders the protected data, and the timeout bounds any
    /// staleness.
    pub fn yield_to_demand(&self, io: TinyIo) {
        if self.demand.load(Ordering::Relaxed) == 0 {
            return;
        }

        // Snapshot the generation before rechecking demand: if the waiter
        // acquires and releases between our check and the wait below, the
        // generation no longer matches and timedWait returns immediately.
        let gen = self.handoff_gen.load(Ordering::Relaxed);
        if self.demand.load(Ordering::Relaxed) == 0 {
            return;
        }
        let _ = io.futex_wait(
            &self.handoff_gen,
            gen,
            Timeout::Duration(Duration::from_nanos(HANDOFF_TIMEOUT_NS)),
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mouse {
    /// The point on the viewport where the mouse currently is. We use
    /// viewport points to avoid the complexity of mapping the mouse to
    /// the renderer state.
    pub point: Option<Coordinate>,

    /// The mods that are currently active for the last mouse event.
    /// This could really just be mods in general and we probably will
    /// move it out of mouse state at some point.
    pub mods: Mods,
}

/// The pre-edit state. See Surface.preeditCallback for more information.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Preedit {
    /// The codepoints to render as preedit text.
    pub codepoints: Vec<Codepoint>,
}

/// A single codepoint to render as preedit text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Codepoint {
    /// Zig: `u21`.
    pub codepoint: u32,
    pub wide: bool,
}

/// The result of [`Preedit::range`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreeditRange {
    pub start: CellCountInt,
    pub end: CellCountInt,
    pub cp_offset: usize,
}

impl Preedit {
    /// Deinit this preedit. (Frees the codepoints; equivalent to drop.)
    pub fn deinit(self) {
        drop(self);
    }

    /// Allocate a copy of this preedit.
    pub fn clone_preedit(&self) -> Preedit {
        self.clone()
    }

    /// The width in cells of all codepoints in the preedit.
    pub fn width(&self) -> usize {
        let mut result: usize = 0;
        for cp in &self.codepoints {
            result += if cp.wide { 2 } else { 1 };
        }

        result
    }

    /// Range returns the start and end x position of the preedit text
    /// along with any codepoint offset necessary to fit the preedit
    /// into the available space.
    pub fn range(&self, start: CellCountInt, max: CellCountInt) -> PreeditRange {
        // If our width is greater than the number of cells we have
        // then we need to adjust our codepoint start to a point where
        // our width would be less than the number of cells we have.
        let (w, cp_offset) = 'width: {
            // max is inclusive, so we need to add 1 to it.
            let max_width = max - start + 1;

            // Rebuild our width in reverse order. This is because we want
            // to offset by the end cells, not the start cells (if we have to).
            let mut w: CellCountInt = 0;
            for i in 0..self.codepoints.len() {
                let reverse_i = self.codepoints.len() - i - 1;
                let cp = self.codepoints[reverse_i];
                w += if cp.wide { 2 } else { 1 };
                if w > max_width {
                    break 'width (w, reverse_i);
                }
            }

            // Width fit in the max width so no offset necessary.
            (w, 0)
        };

        // If our preedit goes off the end of the screen, we adjust it so
        // that it shifts left.
        let end = if w > 0 { start + (w - 1) } else { start };
        let start_offset = if end > max { end - max } else { 0 };
        PreeditRange {
            start: start.saturating_sub(start_offset),
            end: end.saturating_sub(start_offset),
            cp_offset,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_HANGUL_GA: u32 = 0xAC00; // U+AC00 HANGUL SYLLABLE GA

    #[test]
    fn preedit_range_covers_exact_cell_width() {
        {
            let p = Preedit {
                codepoints: vec![Codepoint {
                    codepoint: 'a' as u32,
                    wide: false,
                }],
            };
            let range = p.range(2, 9);
            assert_eq!(2 as CellCountInt, range.start);
            assert_eq!(2 as CellCountInt, range.end);
            assert_eq!(0usize, range.cp_offset);
        }

        {
            let p = Preedit {
                codepoints: vec![Codepoint {
                    codepoint: TEST_HANGUL_GA,
                    wide: true,
                }],
            };
            let range = p.range(2, 9);
            assert_eq!(2 as CellCountInt, range.start);
            assert_eq!(3 as CellCountInt, range.end);
            assert_eq!(0usize, range.cp_offset);
        }
    }

    #[test]
    fn preedit_range_shifts_left_at_right_edge() {
        let p = Preedit {
            codepoints: vec![Codepoint {
                codepoint: TEST_HANGUL_GA,
                wide: true,
            }],
        };
        let range = p.range(9, 9);
        assert_eq!(8 as CellCountInt, range.start);
        assert_eq!(9 as CellCountInt, range.end);
        assert_eq!(0usize, range.cp_offset);
    }

    // Extra (not in Zig): the demand handoff protocol.
    #[test]
    fn lock_demand_handoff() {
        let mut mutex = Mutex::new();
        let mut terminal = Terminal::init(crate::terminal::terminal::Options::new(80, 24)).unwrap();
        let state = State::new(NonNull::from(&mut mutex), NonNull::from(&mut terminal));
        let io = TinyIo;

        // No demand: yield returns immediately.
        state.yield_to_demand(io);

        state.lock_demand(io);
        assert_eq!(0, state.demand.load(Ordering::Relaxed));
        state.unlock_demand(io);
        assert_eq!(1, state.handoff_gen.load(Ordering::Relaxed));

        // Simulated demand with no waiter: yield times out.
        state.demand.store(1, Ordering::Relaxed);
        state.yield_to_demand(io);
        state.demand.store(0, Ordering::Relaxed);

        let p = Preedit {
            codepoints: vec![
                Codepoint {
                    codepoint: 'a' as u32,
                    wide: false,
                },
                Codepoint {
                    codepoint: TEST_HANGUL_GA,
                    wide: true,
                },
            ],
        };
        assert_eq!(3, p.width());
        assert_eq!(p, p.clone_preedit());
    }
}
